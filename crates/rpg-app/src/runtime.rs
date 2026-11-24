//! Background runtime: drains the event inbox through the engine, polls GitHub,
//! and hot-reloads rules. All `engine::process` calls happen on one thread.
use anyhow::Result;
use chrono::{FixedOffset, Local, Offset, TimeZone};
use notify::{RecursiveMode, Watcher};
use rpg_core::config::Config;
use rpg_core::engine::{self, Outcome};
use rpg_core::enrich::{self, FileAge};
use rpg_core::event::{Event, EventKind};
use rpg_core::flavor::TextGen;
use rpg_core::paths::Paths;
use rpg_core::rules::{self, Rules};
use rpg_core::skills;
use rpg_core::store::Store;
use rpg_core::{github, inbox};
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Fallback inbox scan interval, in case a filesystem notification is missed.
const TICK: Duration = Duration::from_secs(30);
/// Coalesce bursts of filesystem events (e.g. tmp write + rename) into one pass.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Max files per commit to look up ages for.
const FILE_AGE_CAP: usize = 50;
const BACKFILL_KEY: &str = "github_backfill_done";
const BACKFILL_PRS: u32 = 100;
const LIVE_PRS: u32 = 30;

/// Shared app state. The presenter lives inside the overlay's `OutcomeSink` (Task 10).
#[derive(Clone)]
pub struct AppState {
    pub paths: Paths,
    pub store: Arc<Store>,
    pub rules: Arc<RwLock<Rules>>,
    pub cfg: Arc<RwLock<Config>>,
    pub gen: Option<Arc<dyn TextGen>>,
}

/// Receives processed batches. `away` is true for the startup batch (events
/// that arrived while the app was closed).
pub trait OutcomeSink: Send + Sync {
    fn present(&self, batch: Vec<(Event, Outcome)>, away: bool);
}

/// Append one line to `<data>/rpg.log`. Never fails.
pub fn log(paths: &Paths, msg: &str) {
    let _ = std::fs::create_dir_all(&paths.data);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(paths.log()) {
        let _ = writeln!(f, "{} rpg-app: {}", Local::now().to_rfc3339(), msg.replace('\n', " "));
    }
}

fn read<T: Clone>(lock: &RwLock<T>) -> T {
    lock.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Load achievement rules from `<config>/rules` and skills from `<config>/rules/skills`,
/// logging every bad file.
pub fn load_rules(paths: &Paths) -> Rules {
    let (mut rules, errors) = rules::load_dir(&paths.rules());
    for e in &errors {
        log(paths, &format!("rule {} skipped: {}", e.file.display(), e.message));
    }
    let (skills, errors) = skills::load_dir(&paths.skills());
    for e in &errors {
        log(paths, &format!("skill {} skipped: {}", e.file.display(), e.message));
    }
    rules.skills = skills;
    rules
}

/// The local UTC offset in effect at `ts` (DST-aware per event).
pub fn local_offset(ts: chrono::DateTime<chrono::Utc>) -> FixedOffset {
    Local.from_utc_datetime(&ts.naive_utc()).offset().fix()
}

pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

type ProcessFn<'a> = &'a dyn Fn(&Store, &Rules, &Config, &Event, &[FileAge], FixedOffset) -> Result<Outcome>;

/// Drain the inbox in order, run each event through the engine, and delete each
/// processed file. A failing event is logged and moved to `bad/`; the rest still
/// run. Flavor text is rendered later, by the overlay, only for unlocks that get a banner.
pub fn process_inbox_once(state: &AppState) -> Result<Vec<(Event, Outcome)>> {
    process_inbox_with(state, &engine::process)
}

fn process_inbox_with(state: &AppState, process: ProcessFn) -> Result<Vec<(Event, Outcome)>> {
    let items = inbox::drain(&state.paths.inbox())?;
    if items.is_empty() {
        return Ok(vec![]);
    }
    // Snapshots, so a rules reload never waits on processing.
    let rules = read(&state.rules);
    let cfg = read(&state.cfg);
    let mut out = Vec::new();
    for (path, ev) in items {
        let tz = local_offset(ev.ts);
        let res = catch_unwind(AssertUnwindSafe(|| {
            let ages = file_ages(state, &ev);
            process(&state.store, &rules, &cfg, &ev, &ages, tz)
        }));
        let outcome = match res {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                log(&state.paths, &format!("event {} ({}) failed: {e:#}", ev.id, ev.kind.as_str()));
                quarantine(&state.paths, &path);
                continue;
            }
            Err(p) => {
                // The panic may have left the engine's transaction open.
                let _ = state.store.rollback_tx();
                log(&state.paths, &format!("event {} ({}) panicked: {}", ev.id, ev.kind.as_str(), panic_message(&*p)));
                quarantine(&state.paths, &path);
                continue;
            }
        };
        if let Err(e) = std::fs::remove_file(&path) {
            // Harmless if it lingers: the engine ignores already-recorded event ids.
            log(&state.paths, &format!("could not delete {}: {e}", path.display()));
        }
        out.push((ev, outcome));
    }
    Ok(out)
}

/// Move a failed inbox file to `bad/`. If even that fails, delete it so it is
/// not retried forever.
fn quarantine(paths: &Paths, file: &Path) {
    if let Err(e) = inbox::quarantine(&paths.inbox(), file) {
        log(paths, &format!("quarantine failed ({e:#}); deleting {}", file.display()));
        let _ = std::fs::remove_file(file);
    }
}

/// `(repo, hash, files)` for a commit event's file-age lookup.
fn commit_enrich_args(ev: &Event) -> Option<(PathBuf, String, Vec<String>)> {
    if ev.kind != EventKind::Commit {
        return None;
    }
    let repo = ev.data["repo_path"].as_str().or(ev.repo.as_deref())?;
    let hash = ev.data["hash"].as_str()?;
    let files = ev.data["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["path"].as_str().or(f.as_str()).map(String::from))
        .collect();
    Some((PathBuf::from(repo), hash.to_string(), files))
}

/// Enrichment is best-effort: on failure the event still counts, without ages.
fn file_ages(state: &AppState, ev: &Event) -> Vec<FileAge> {
    let Some((repo, hash, files)) = commit_enrich_args(ev) else { return vec![] };
    enrich::file_ages(&repo, &hash, &files, FILE_AGE_CAP).unwrap_or_else(|e| {
        log(&state.paths, &format!("file ages for {hash} in {} failed: {e:#}", repo.display()));
        vec![]
    })
}

enum Msg {
    /// Something changed in the inbox (or a poll wrote events).
    Wake,
    /// Set a meta key on the processing thread, outside any engine transaction.
    SetMeta(&'static str, String),
}

/// Start the background threads: inbox processing (the only thread that calls
/// the engine), the GitHub poller, and the rules watcher.
pub fn spawn(state: AppState, sink: Arc<dyn OutcomeSink>) {
    let (tx, rx) = mpsc::channel::<Msg>();

    let wake = tx.clone();
    let inbox_watcher = watch(&state.paths, &state.paths.inbox(), move || {
        let _ = wake.send(Msg::Wake);
    });
    let st = state.clone();
    std::thread::Builder::new()
        .name("rpg-inbox".into())
        .spawn(move || {
            let _keep = inbox_watcher;
            process_loop(&st, &*sink, &rx);
        })
        .expect("spawning inbox thread");

    // `gh` has no timeout, so polling lives on its own thread.
    if read(&state.cfg).github.enabled {
        let st = state.clone();
        std::thread::Builder::new()
            .name("rpg-github".into())
            .spawn(move || poll_loop(&st, &tx))
            .expect("spawning github thread");
    }

    let (rtx, rrx) = mpsc::channel::<()>();
    let stx = rtx.clone();
    let rules_watcher = watch(&state.paths, &state.paths.rules(), move || {
        let _ = rtx.send(());
    });
    let skills_watcher = watch(&state.paths, &state.paths.skills(), move || {
        let _ = stx.send(());
    });
    std::thread::Builder::new()
        .name("rpg-rules".into())
        .spawn(move || {
            let _keep = (rules_watcher, skills_watcher);
            while rrx.recv().is_ok() {
                std::thread::sleep(DEBOUNCE);
                while rrx.try_recv().is_ok() {}
                let rules = load_rules(&state.paths);
                log(&state.paths, &format!("rules reloaded: {} achievements, {} skills", rules.list.len(), rules.skills.len()));
                *state.rules.write().unwrap_or_else(|p| p.into_inner()) = rules;
            }
        })
        .expect("spawning rules thread");
}

/// Watch `dir` (non-recursively), calling `on_change` for every notification.
/// Returns the watcher, which must be kept alive; `None` (logged) on failure.
fn watch(paths: &Paths, dir: &Path, on_change: impl Fn() + Send + 'static) -> Option<notify::RecommendedWatcher> {
    let res = notify::recommended_watcher(move |r: notify::Result<notify::Event>| {
        if r.is_ok() {
            on_change();
        }
    })
    .and_then(|mut w| w.watch(dir, RecursiveMode::NonRecursive).map(|_| w));
    match res {
        Ok(w) => Some(w),
        Err(e) => {
            log(paths, &format!("cannot watch {}: {e}", dir.display()));
            None
        }
    }
}

fn run_once(state: &AppState, sink: &dyn OutcomeSink, away: bool) {
    match process_inbox_once(state) {
        Ok(batch) if !batch.is_empty() => {
            // A presenter bug must not kill the inbox thread.
            if let Err(p) = catch_unwind(AssertUnwindSafe(|| sink.present(batch, away))) {
                log(&state.paths, &format!("presenting a batch panicked: {}", panic_message(&*p)));
            }
        }
        Ok(_) => {}
        Err(e) => log(&state.paths, &format!("inbox drain failed: {e:#}")),
    }
}

fn apply(state: &AppState, msg: Msg) {
    if let Msg::SetMeta(k, v) = msg {
        if let Err(e) = state.store.meta_set(k, &v) {
            log(&state.paths, &format!("meta_set {k} failed: {e:#}"));
        }
    }
}

fn process_loop(state: &AppState, sink: &dyn OutcomeSink, rx: &Receiver<Msg>) {
    run_once(state, sink, true);
    loop {
        match rx.recv_timeout(TICK) {
            Ok(m) => apply(state, m),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        std::thread::sleep(DEBOUNCE);
        // Fold queued wakes into this one pass.
        while let Ok(m) = rx.try_recv() {
            apply(state, m);
        }
        run_once(state, sink, false);
    }
}

/// One GitHub poll: fetch, parse, and write events into the inbox (the single
/// path into the engine). Backfill runs once, until `github_backfill_done` is set; it holds
/// the RFC 3339 time backfill completed, and live events from before then count as backfill.
fn poll_github(state: &AppState, tx: &Sender<Msg>) -> Result<usize> {
    let done = state.store.meta_get(BACKFILL_KEY)?;
    let backfill = done.is_none();
    let json = github::fetch(if backfill { BACKFILL_PRS } else { LIVE_PRS })?;
    let mut events = github::parse(&json, backfill)?;
    if let Some(raw) = done {
        let cutoff = github::parse_backfill_done(&raw).unwrap_or_else(|| {
            // Not a timestamp (older build): treat everything up to now as history from here on.
            let now = chrono::Utc::now();
            let _ = tx.send(Msg::SetMeta(BACKFILL_KEY, now.to_rfc3339()));
            now
        });
        github::mark_pre_backfill(&mut events, cutoff);
    }
    for ev in &events {
        inbox::write_event(&state.paths.inbox(), ev)?;
    }
    if backfill {
        let _ = tx.send(Msg::SetMeta(BACKFILL_KEY, chrono::Utc::now().to_rfc3339()));
    }
    let _ = tx.send(Msg::Wake);
    Ok(events.len())
}

/// Poll every `poll_minutes`. Errors (no `gh`, logged out, API errors) are
/// logged once per failure streak and the poll is skipped until the next interval.
fn poll_loop(state: &AppState, tx: &Sender<Msg>) {
    let mut failing = false;
    loop {
        match poll_github(state, tx) {
            Ok(n) => {
                if failing {
                    log(&state.paths, &format!("github poll recovered ({n} events)"));
                }
                failing = false;
            }
            Err(e) => {
                if !failing {
                    log(&state.paths, &format!("github poll failed; skipping until it recovers: {e:#}"));
                }
                failing = true;
            }
        }
        let minutes = read(&state.cfg).github.poll_minutes.max(1);
        std::thread::sleep(Duration::from_secs(minutes * 60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use rpg_core::event::{EventKind, Source};
    use rpg_core::inbox;
    use std::path::Path;

    struct Fixture {
        _tmp: tempfile::TempDir,
        state: AppState,
    }

    fn fixture(rules: Rules, gen: Option<Arc<dyn TextGen>>) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(Some(tmp.path().to_path_buf())).unwrap();
        std::fs::create_dir_all(paths.inbox()).unwrap();
        let state = AppState {
            paths,
            store: Arc::new(Store::open_in_memory().unwrap()),
            rules: Arc::new(RwLock::new(rules)),
            cfg: Arc::new(RwLock::new(Config::default())),
            gen,
        };
        Fixture { _tmp: tmp, state }
    }

    fn push(n: u32) -> Event {
        Event::new(EventKind::Push, Source::Git, Some("r".into()), serde_json::json!({ "n": n }))
    }

    fn inbox_json_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_file())
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn bad_files(dir: &Path) -> usize {
        let bad = dir.join("bad");
        if !bad.exists() {
            return 0;
        }
        std::fs::read_dir(bad).unwrap().count()
    }

    #[test]
    fn process_inbox_once_processes_in_order_and_deletes() {
        let f = fixture(Rules::default(), None);
        let inbox_dir = f.state.paths.inbox();
        let evs = [push(1), push(2), push(3)];
        for e in &evs {
            inbox::write_event(&inbox_dir, e).unwrap();
        }
        let out = process_inbox_once(&f.state).unwrap();
        assert_eq!(out.len(), 3);
        let ids: Vec<&str> = out.iter().map(|(e, _)| e.id.as_str()).collect();
        assert_eq!(ids, evs.iter().map(|e| e.id.as_str()).collect::<Vec<_>>());
        let totals: Vec<u64> = out.iter().map(|(_, o)| o.total_xp).collect();
        assert_eq!(totals, vec![25, 50, 75]);
        assert!(inbox_json_files(&inbox_dir).is_empty(), "inbox emptied");
        assert_eq!(bad_files(&inbox_dir), 0);
    }

    #[test]
    fn bad_event_moves_to_bad_and_continues() {
        let f = fixture(Rules::default(), None);
        let inbox_dir = f.state.paths.inbox();
        let (a, b, c) = (push(1), push(2), push(3));
        inbox::write_event(&inbox_dir, &a).unwrap();
        inbox::write_event(&inbox_dir, &b).unwrap();
        inbox::write_event(&inbox_dir, &c).unwrap();
        std::fs::write(inbox_dir.join("0-garbage.json"), "{not json").unwrap();
        let fail_id = b.id.clone();
        let process = move |s: &Store, r: &Rules, cfg: &Config, ev: &Event, fa: &[FileAge], tz: FixedOffset| {
            if ev.id == fail_id {
                return Err(anyhow!("engine exploded"));
            }
            engine::process(s, r, cfg, ev, fa, tz)
        };
        let out = process_inbox_with(&f.state, &process).unwrap();
        let ids: Vec<&str> = out.iter().map(|(e, _)| e.id.as_str()).collect();
        assert_eq!(ids, vec![a.id.as_str(), c.id.as_str()]);
        assert!(inbox_json_files(&inbox_dir).is_empty(), "inbox emptied");
        assert_eq!(bad_files(&inbox_dir), 2, "garbage file and failed event quarantined");
        let log = std::fs::read_to_string(f.state.paths.log()).unwrap();
        assert!(log.contains("engine exploded"), "failure logged: {log}");
    }

    #[test]
    fn panicking_event_rolls_back_and_continues() {
        let f = fixture(Rules::default(), None);
        let inbox_dir = f.state.paths.inbox();
        let (a, b, c) = (push(1), push(2), push(3));
        for e in [&a, &b, &c] {
            inbox::write_event(&inbox_dir, e).unwrap();
        }
        let fail_id = b.id.clone();
        let process = move |s: &Store, r: &Rules, cfg: &Config, ev: &Event, fa: &[FileAge], tz: FixedOffset| {
            if ev.id == fail_id {
                s.begin_tx().unwrap();
                s.add_xp(&ev.id, 1000, "leaked", ev.ts).unwrap();
                panic!("engine panicked");
            }
            engine::process(s, r, cfg, ev, fa, tz)
        };
        let out = process_inbox_with(&f.state, &process).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].0.id, c.id);
        assert_eq!(out[1].1.total_xp, 50, "the panicked event's writes were rolled back");
        assert_eq!(f.state.store.total_xp().unwrap(), 50);
        assert_eq!(bad_files(&inbox_dir), 1);
    }

    struct PanicSink;
    impl OutcomeSink for PanicSink {
        fn present(&self, _batch: Vec<(Event, Outcome)>, _away: bool) {
            panic!("presenter bug");
        }
    }

    #[test]
    fn presenter_panic_is_logged_not_fatal() {
        let f = fixture(Rules::default(), None);
        inbox::write_event(&f.state.paths.inbox(), &push(1)).unwrap();
        run_once(&f.state, &PanicSink, false);
        let log = std::fs::read_to_string(f.state.paths.log()).unwrap();
        assert!(log.contains("presenting a batch panicked: presenter bug"), "{log}");
    }

    #[test]
    fn commit_file_ages_input_from_event_data() {
        let ev = Event::new(
            EventKind::Commit,
            Source::Git,
            Some("C:/repo".into()),
            serde_json::json!({ "hash": "abc", "repo_path": "C:/top", "files": [{ "path": "a.rs" }, { "path": "b/c.rs" }] }),
        );
        let (repo, hash, files) = commit_enrich_args(&ev).unwrap();
        assert_eq!(repo, Path::new("C:/top"));
        assert_eq!(hash, "abc");
        assert_eq!(files, vec!["a.rs".to_string(), "b/c.rs".to_string()]);
        assert!(commit_enrich_args(&push(1)).is_none());
    }
}
