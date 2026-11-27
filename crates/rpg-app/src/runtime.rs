//! Background runtime: drains the event inbox through the engine, polls GitHub,
//! and hot-reloads rules. All `engine::process` calls happen on one thread.
use anyhow::Result;
use chrono::{DateTime, FixedOffset, Local, Offset, TimeZone, Utc};
use notify::{RecursiveMode, Watcher};
use rpg_core::config::Config;
use rpg_core::engine::{self, Outcome};
use rpg_core::enrich::{self, FileAge};
use rpg_core::event::{Event, EventKind};
use rpg_core::flavor::TextGen;
use rpg_core::paths::Paths;
use rpg_core::rules::{self, Rules};
use rpg_core::quests;
use rpg_core::quests::state::{Offer, QuestStatus, QuestView};
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
/// How long a quest past its deadline waits for the early GitHub poll.
const CHECK_CAP: chrono::Duration = chrono::Duration::seconds(30);
/// Grace beyond `offer_seconds` before a pending offer the overlay never answered is dropped
/// (its clock only starts once the banner is on screen).
const OFFER_GRACE_SECS: i64 = 300;
/// After a failed quest expiry, how long until it is tried again.
const EXPIRE_RETRY: chrono::Duration = chrono::Duration::seconds(60);

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
    /// Whether an offer could be shown right now (not in DND, quests on).
    fn offers_allowed(&self) -> bool;
    /// A new quest offer; answer it with `Runtime::answer_offer` or `Runtime::drop_offer`.
    fn quest_offer(&self, id: u64, offer: Offer);
    /// The active quest changed: accepted, progressed, checking GitHub, or resolved.
    fn quest_update(&self, view: QuestView);
    /// Offer `id` is no longer pending (quests turned off, or it went stale); stop showing it.
    fn quest_offer_withdrawn(&self, id: u64);
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
    let (quests, errors) = quests::load_dir(&paths.quests());
    for e in &errors {
        log(paths, &format!("quest {} skipped: {}", e.file.display(), e.message));
    }
    rules.quests = quests;
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
    /// A GitHub poll finished (its events, if any, are already in the inbox).
    Polled,
    /// The user answered offer `id`: accept (`true`) or pass.
    Answer(u64, bool),
    /// Offer `id` went away unanswered (not a pass).
    DropOffer(u64),
    /// Tray "Quests" toggle.
    SetQuests(bool),
}

/// Handle to the inbox thread, for the overlay and tray. Managed by the Tauri app.
#[derive(Clone)]
pub struct Runtime {
    tx: Sender<Msg>,
}

impl Runtime {
    /// Accept or pass offer `id`. Ignored unless `id` is the pending offer.
    pub fn answer_offer(&self, id: u64, accept: bool) {
        let _ = self.tx.send(Msg::Answer(id, accept));
    }

    /// Forget offer `id` without counting a pass.
    pub fn drop_offer(&self, id: u64) {
        let _ = self.tx.send(Msg::DropOffer(id));
    }

    /// Turn quests on or off for this session. Off cancels the active quest and withdraws an offer.
    pub fn set_quests_enabled(&self, on: bool) {
        let _ = self.tx.send(Msg::SetQuests(on));
    }
}

/// Quest bookkeeping owned by the inbox thread.
pub(crate) struct QuestRuntime {
    /// Quests on for this session (tray toggle; starts from `[quests] enabled`).
    enabled: bool,
    next_id: u64,
    /// The offer waiting for an answer: id, offer, when it was raised.
    pending: Option<(u64, Offer, DateTime<Utc>)>,
    /// Row id of the quest that already had its deadline GitHub check.
    checked: Option<i64>,
    /// When the running GitHub check started.
    checking_since: Option<DateTime<Utc>>,
    /// A poll finished since the check started.
    polled: bool,
    /// Wake the GitHub poller now.
    poke: Box<dyn Fn() + Send>,
    /// Expiry failed (error or panic): don't try again before this. Logged once per streak.
    expire_retry_at: Option<DateTime<Utc>>,
}

impl QuestRuntime {
    pub(crate) fn new(enabled: bool, poke: Box<dyn Fn() + Send>) -> QuestRuntime {
        QuestRuntime {
            enabled,
            next_id: 0,
            pending: None,
            checked: None,
            checking_since: None,
            polled: false,
            poke,
            expire_retry_at: None,
        }
    }
}

/// Start the background threads: inbox processing (the only thread that calls
/// the engine), the GitHub poller, and the rules watcher.
pub fn spawn(state: AppState, sink: Arc<dyn OutcomeSink>) -> Runtime {
    let (tx, rx) = mpsc::channel::<Msg>();
    let (poke_tx, poke_rx) = mpsc::channel::<()>();
    let cfg = read(&state.cfg);

    let wake = tx.clone();
    let inbox_watcher = watch(&state.paths, &state.paths.inbox(), move || {
        let _ = wake.send(Msg::Wake);
    });
    let st = state.clone();
    let mut quest = QuestRuntime::new(
        cfg.quests.enabled,
        Box::new(move || {
            let _ = poke_tx.send(());
        }),
    );
    std::thread::Builder::new()
        .name("rpg-inbox".into())
        .spawn(move || {
            let _keep = inbox_watcher;
            process_loop(&st, &*sink, &rx, &mut quest);
        })
        .expect("spawning inbox thread");

    // `gh` has no timeout, so polling lives on its own thread.
    if cfg.github.enabled {
        let st = state.clone();
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("rpg-github".into())
            .spawn(move || poll_loop(&st, &tx, &poke_rx))
            .expect("spawning github thread");
    }

    let (rtx, rrx) = mpsc::channel::<()>();
    let stx = rtx.clone();
    let rtx2 = rtx.clone();
    let rules_watcher = watch(&state.paths, &state.paths.rules(), move || {
        let _ = rtx.send(());
    });
    let skills_watcher = watch(&state.paths, &state.paths.skills(), move || {
        let _ = stx.send(());
    });
    let qtx = rtx2;
    let quests_watcher = watch(&state.paths, &state.paths.quests(), move || {
        let _ = qtx.send(());
    });
    std::thread::Builder::new()
        .name("rpg-rules".into())
        .spawn(move || {
            let _keep = (rules_watcher, skills_watcher, quests_watcher);
            while rrx.recv().is_ok() {
                std::thread::sleep(DEBOUNCE);
                while rrx.try_recv().is_ok() {}
                let rules = load_rules(&state.paths);
                log(&state.paths, &format!("rules reloaded: {} achievements, {} skills, {} quests", rules.list.len(), rules.skills.len(), rules.quests.len()));
                *state.rules.write().unwrap_or_else(|p| p.into_inner()) = rules;
            }
        })
        .expect("spawning rules thread");
    Runtime { tx }
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

/// Call into the sink; a presenter bug must not kill the inbox thread.
fn guarded(state: &AppState, what: &str, f: impl FnOnce()) {
    if let Err(p) = catch_unwind(AssertUnwindSafe(f)) {
        log(&state.paths, &format!("{what} panicked: {}", panic_message(&*p)));
    }
}

/// Run a store write that may run user rules. A panic rolls back an open transaction (as
/// `process_inbox_with` does) and becomes an error, so the inbox thread survives it.
fn store_call<T>(state: &AppState, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            // Harmless if no transaction was open.
            let _ = state.store.rollback_tx();
            Err(anyhow::anyhow!("panicked: {}", panic_message(&*p)))
        }
    }
}

/// Process the inbox and present the batch, then forward quest progress and, for a live
/// batch, maybe raise an offer.
fn run_once(state: &AppState, sink: &dyn OutcomeSink, away: bool, quest: &mut QuestRuntime) {
    match process_inbox_once(state) {
        Ok(batch) if !batch.is_empty() => {
            let updates: Vec<QuestView> = batch.iter().filter_map(|(_, o)| o.quest.clone()).collect();
            let events: Vec<Event> = if away { vec![] } else { batch.iter().map(|(e, _)| e.clone()).collect() };
            guarded(state, "presenting a batch", || sink.present(batch, away));
            for view in updates {
                guarded(state, "quest update", || sink.quest_update(view));
            }
            // Never from the startup batch: those events happened while the app was closed.
            if !away {
                maybe_offer(state, sink, quest, &events, Utc::now());
            }
        }
        Ok(_) => {}
        Err(e) => log(&state.paths, &format!("inbox drain failed: {e:#}")),
    }
}

/// Offer the first quest one of `events` (in order) triggers, if an offer may be shown now.
fn maybe_offer(state: &AppState, sink: &dyn OutcomeSink, quest: &mut QuestRuntime, events: &[Event], now: DateTime<Utc>) {
    if !quest.enabled || quest.pending.is_some() || events.is_empty() || !sink.offers_allowed() {
        return;
    }
    let rules = state.rules.read().unwrap_or_else(|p| p.into_inner());
    let mut cfg = read(&state.cfg);
    // The session toggle, not the file, decides (they start equal).
    cfg.quests.enabled = true;
    let tz = local_offset(now);
    for ev in events {
        match store_call(state, || quests::state::offer_for(&state.store, &rules.quests, &cfg, ev, now, tz, &mut || fastrand::f64())) {
            Ok(Some(offer)) => {
                quest.next_id += 1;
                let id = quest.next_id;
                quest.pending = Some((id, offer.clone(), now));
                guarded(state, "quest offer", || sink.quest_offer(id, offer));
                return;
            }
            Ok(None) => {}
            Err(e) => log(&state.paths, &format!("quest offer for {} failed: {e:#}", ev.id)),
        }
    }
}

fn handle(state: &AppState, sink: &dyn OutcomeSink, quest: &mut QuestRuntime, msg: Msg, now: DateTime<Utc>) {
    match msg {
        Msg::Wake => {}
        Msg::SetMeta(k, v) => {
            if let Err(e) = state.store.meta_set(k, &v) {
                log(&state.paths, &format!("meta_set {k} failed: {e:#}"));
            }
        }
        Msg::Polled => quest.polled = true,
        Msg::Answer(id, yes) => answer(state, sink, quest, id, yes, now),
        Msg::DropOffer(id) => {
            if take_pending(quest, id).is_some() {
                log(&state.paths, &format!("quest offer {id} dropped unanswered"));
            }
        }
        Msg::SetQuests(on) => set_quests(state, sink, quest, on, now),
    }
}

/// Take the pending offer if it is `id`.
fn take_pending(quest: &mut QuestRuntime, id: u64) -> Option<Offer> {
    match &quest.pending {
        Some((p, _, _)) if *p == id => quest.pending.take().map(|(_, o, _)| o),
        _ => None,
    }
}

fn answer(state: &AppState, sink: &dyn OutcomeSink, quest: &mut QuestRuntime, id: u64, yes: bool, now: DateTime<Utc>) {
    let Some(offer) = take_pending(quest, id) else {
        log(&state.paths, &format!("quest offer {id} answered but not pending; ignored"));
        return;
    };
    let cfg = read(&state.cfg);
    if !yes {
        match store_call(state, || quests::state::pass(&state.store, &cfg.quests)) {
            Ok(true) => log(&state.paths, &format!("quest {} passed; chain broken", offer.quest_id)),
            Ok(false) => {}
            Err(e) => log(&state.paths, &format!("quest pass failed: {e:#}")),
        }
        return;
    }
    match state.store.active_quest() {
        Ok(None) => {}
        Ok(Some(row)) => {
            log(&state.paths, &format!("quest {} accepted while {} is active; ignored", offer.quest_id, row.quest_id));
            return;
        }
        Err(e) => {
            log(&state.paths, &format!("quest accept failed: {e:#}"));
            return;
        }
    }
    match store_call(state, || quests::state::accept(&state.store, &offer, now)) {
        Ok(view) => {
            quest.checked = None;
            quest.checking_since = None;
            let github = view.has_github_step;
            guarded(state, "quest update", || sink.quest_update(view));
            if github && cfg.github.enabled {
                (quest.poke)();
            }
        }
        Err(e) => log(&state.paths, &format!("quest accept failed: {e:#}")),
    }
}

fn set_quests(state: &AppState, sink: &dyn OutcomeSink, quest: &mut QuestRuntime, on: bool, now: DateTime<Utc>) {
    quest.enabled = on;
    if on {
        return;
    }
    if let Some((id, _, _)) = quest.pending.take() {
        guarded(state, "quest offer withdrawal", || sink.quest_offer_withdrawn(id));
    }
    quest.checking_since = None;
    match store_call(state, || quests::state::cancel(&state.store, now)) {
        Ok(Some(view)) => guarded(state, "quest update", || sink.quest_update(view)),
        Ok(None) => {}
        Err(e) => log(&state.paths, &format!("quest cancel failed: {e:#}")),
    }
}

/// Whether a step is left that a GitHub poll could fill. The view doesn't say which steps
/// are GitHub kinds, so any unfilled step of a quest that has one counts.
fn github_step_left(view: &QuestView) -> bool {
    view.has_github_step && view.steps.iter().any(|s| s.have < s.need)
}

fn offer_ttl(cfg: &Config) -> chrono::Duration {
    chrono::Duration::seconds(cfg.quests.offer_seconds as i64 + OFFER_GRACE_SECS)
}

/// Drop a stale pending offer, and resolve the active quest once its deadline has passed:
/// first one early GitHub poll if a GitHub step is left (`Checking`), then `expire_quest`.
fn deadline_check(state: &AppState, sink: &dyn OutcomeSink, quest: &mut QuestRuntime, now: DateTime<Utc>) {
    deadline_check_with(state, sink, quest, now, &engine::expire_quest)
}

type ExpireFn<'a> = &'a dyn Fn(&Store, &Rules, &Config, DateTime<Utc>, FixedOffset) -> Result<Option<(Event, Outcome, QuestView)>>;

fn deadline_check_with(state: &AppState, sink: &dyn OutcomeSink, quest: &mut QuestRuntime, now: DateTime<Utc>, expire: ExpireFn) {
    let cfg = read(&state.cfg);
    if let Some((id, _, at)) = &quest.pending {
        if now - *at > offer_ttl(&cfg) {
            let id = *id;
            quest.pending = None;
            log(&state.paths, &format!("quest offer {id} went stale; dropped"));
            guarded(state, "quest offer withdrawal", || sink.quest_offer_withdrawn(id));
        }
    }
    let view = match quests::state::active_view(&state.store) {
        Ok(Some(v)) => v,
        Ok(None) => {
            quest.checking_since = None;
            quest.expire_retry_at = None;
            return;
        }
        Err(e) => {
            log(&state.paths, &format!("reading the active quest failed: {e:#}"));
            return;
        }
    };
    if view.deadline >= now || quest.expire_retry_at.is_some_and(|t| now < t) {
        return;
    }
    if let Some(since) = quest.checking_since {
        if !quest.polled && now - since < CHECK_CAP {
            return;
        }
    } else if cfg.github.enabled && quest.checked != Some(view.row_id) && github_step_left(&view) {
        quest.checked = Some(view.row_id);
        quest.checking_since = Some(now);
        quest.polled = false;
        let checking = QuestView { status: QuestStatus::Checking, ..view };
        guarded(state, "quest update", || sink.quest_update(checking));
        (quest.poke)();
        return;
    }
    quest.checking_since = None;
    let rules = state.rules.read().unwrap_or_else(|p| p.into_inner());
    let res = store_call(state, || expire(&state.store, &rules, &cfg, now, local_offset(now)));
    drop(rules);
    match res {
        Ok(resolved) => {
            if quest.expire_retry_at.take().is_some() {
                log(&state.paths, "quest expiry recovered");
            }
            if let Some((ev, out, view)) = resolved {
                guarded(state, "quest update", || sink.quest_update(view));
                guarded(state, "presenting a batch", || sink.present(vec![(ev, out)], false));
            }
        }
        Err(e) => {
            if quest.expire_retry_at.is_none() {
                log(&state.paths, &format!("quest expiry failed; retrying every minute until it works: {e:#}"));
            }
            quest.expire_retry_at = Some(now + EXPIRE_RETRY);
        }
    }
}

/// How long the inbox thread may sleep: `TICK`, or less if an offer goes stale, a deadline
/// passes or a GitHub check runs out sooner. At least a second, so a failing expiry can't spin.
fn next_wait(state: &AppState, quest: &QuestRuntime, now: DateTime<Utc>) -> Duration {
    let mut wait = chrono::Duration::from_std(TICK).expect("TICK fits");
    if let Some((_, _, at)) = &quest.pending {
        wait = wait.min(*at + offer_ttl(&read(&state.cfg)) - now);
    }
    if let Some(since) = quest.checking_since {
        wait = wait.min(since + CHECK_CAP - now);
    } else if let Some(retry) = quest.expire_retry_at {
        wait = wait.min(retry - now);
    } else if let Ok(Some(row)) = state.store.active_quest() {
        wait = wait.min(row.deadline - now);
    }
    wait.to_std().unwrap_or(Duration::ZERO).max(Duration::from_secs(1))
}

/// The startup pass: the away batch (never offers), quests off from config ends an active
/// quest, a quest that ran out while the app was closed is resolved, and the active quest's
/// view is sent again for the overlay.
fn startup(state: &AppState, sink: &dyn OutcomeSink, quest: &mut QuestRuntime, now: DateTime<Utc>) {
    run_once(state, sink, true, quest);
    if !quest.enabled {
        set_quests(state, sink, quest, false, now);
    }
    deadline_check(state, sink, quest, now);
    if quest.checking_since.is_none() {
        match quests::state::active_view(&state.store) {
            Ok(Some(view)) => guarded(state, "quest update", || sink.quest_update(view)),
            Ok(None) => {}
            Err(e) => log(&state.paths, &format!("reading the active quest failed: {e:#}")),
        }
    }
}

fn process_loop(state: &AppState, sink: &dyn OutcomeSink, rx: &Receiver<Msg>, quest: &mut QuestRuntime) {
    startup(state, sink, quest, Utc::now());
    loop {
        match rx.recv_timeout(next_wait(state, quest, Utc::now())) {
            Ok(m) => handle(state, sink, quest, m, Utc::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        std::thread::sleep(DEBOUNCE);
        // Fold queued wakes into this one pass.
        while let Ok(m) = rx.try_recv() {
            handle(state, sink, quest, m, Utc::now());
        }
        run_once(state, sink, false, quest);
        deadline_check(state, sink, quest, Utc::now());
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

/// Poll every `poll_minutes`, or right away when poked (quest accepted or at its deadline).
/// Errors (no `gh`, logged out, API errors) are logged once per failure streak and the poll
/// is skipped until the next interval. Every finished poll, failed or not, sends `Polled`.
fn poll_loop(state: &AppState, tx: &Sender<Msg>, poke: &Receiver<()>) {
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
        let _ = tx.send(Msg::Polled);
        let interval = Duration::from_secs(read(&state.cfg).github.poll_minutes.max(1) * 60);
        match poke.recv_timeout(interval) {
            // Fold pokes that piled up during the poll into this one.
            Ok(()) => while poke.try_recv().is_ok() {},
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(interval),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use rpg_core::event::{EventKind, Source};
    use rpg_core::inbox;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::Mutex;

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
                s.add_xp(&ev.id, 1000, "leaked", None, ev.ts).unwrap();
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
        fn offers_allowed(&self) -> bool {
            false
        }
        fn quest_offer(&self, _id: u64, _offer: Offer) {}
        fn quest_update(&self, _view: QuestView) {}
        fn quest_offer_withdrawn(&self, _id: u64) {}
    }

    /// Records what the runtime sends; `blocked` closes the offer gate.
    #[derive(Default)]
    struct RecSink {
        blocked: bool,
        offers: Mutex<Vec<(u64, String)>>,
        updates: Mutex<Vec<QuestStatus>>,
        withdrawn: Mutex<Vec<u64>>,
        presented: Mutex<Vec<(usize, bool)>>,
    }
    impl OutcomeSink for RecSink {
        fn present(&self, batch: Vec<(Event, Outcome)>, away: bool) {
            self.presented.lock().unwrap().push((batch.len(), away));
        }
        fn offers_allowed(&self) -> bool {
            !self.blocked
        }
        fn quest_offer(&self, id: u64, offer: Offer) {
            self.offers.lock().unwrap().push((id, offer.quest_id));
        }
        fn quest_update(&self, view: QuestView) {
            self.updates.lock().unwrap().push(view.status);
        }
        fn quest_offer_withdrawn(&self, id: u64) {
            self.withdrawn.lock().unwrap().push(id);
        }
    }
    impl RecSink {
        fn offers(&self) -> Vec<(u64, String)> {
            self.offers.lock().unwrap().clone()
        }
        fn updates(&self) -> Vec<QuestStatus> {
            self.updates.lock().unwrap().clone()
        }
    }

    const QUEST: &str = r#"
id = "sure_thing"
name = "Sure Thing"
tier = "bronze"
trigger = ["commit"]
chance = 1.0
cooldown_hours = 0
minutes = 1
steps = [{ kind = "commit", count = 3 }]
fallback = "Commit more."
"#;
    const COMMIT_STEP: &str = r#"steps = [{ kind = "commit", count = 3 }]"#;

    /// A fixture with one quest (`chance = 1.0`, no cooldowns), its steps replaced by `steps`.
    fn quest_fixture_with(steps: &str) -> Fixture {
        let rules = Rules { quests: vec![rpg_core::quests::parse_quest(&QUEST.replace(COMMIT_STEP, steps)).unwrap()], ..Rules::default() };
        let f = fixture(rules, None);
        f.state.cfg.write().unwrap().quests.min_gap_minutes = 0;
        f
    }

    fn quest_fixture() -> Fixture {
        quest_fixture_with(COMMIT_STEP)
    }

    fn commit() -> Event {
        Event::new(EventKind::Commit, Source::Git, Some("C:/repo".into()), serde_json::json!({ "lines": 10 }))
    }

    fn quiet() -> QuestRuntime {
        QuestRuntime::new(true, Box::new(|| {}))
    }

    fn counting() -> (QuestRuntime, Arc<AtomicUsize>) {
        let pokes = Arc::new(AtomicUsize::new(0));
        let p = pokes.clone();
        let q = QuestRuntime::new(true, Box::new(move || {
            p.fetch_add(1, SeqCst);
        }));
        (q, pokes)
    }

    /// Write a commit to the inbox and run one live pass.
    fn live_commit(f: &Fixture, sink: &RecSink, q: &mut QuestRuntime) {
        inbox::write_event(&f.state.paths.inbox(), &commit()).unwrap();
        run_once(&f.state, sink, false, q);
    }

    fn passes(f: &Fixture) -> u32 {
        quests::state::chain(&f.state.store).unwrap().passes
    }

    #[test]
    fn live_event_raises_one_offer() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let offers = sink.offers();
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].1, "sure_thing");
        live_commit(&f, &sink, &mut q);
        assert_eq!(sink.offers().len(), 1, "no second offer while one is pending");
    }

    #[test]
    fn away_batch_never_offers() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        inbox::write_event(&f.state.paths.inbox(), &commit()).unwrap();
        run_once(&f.state, &sink, true, &mut q);
        assert_eq!(*sink.presented.lock().unwrap(), vec![(1, true)]);
        assert!(sink.offers().is_empty());
        assert!(q.pending.is_none());
    }

    #[test]
    fn offers_respect_sink_gate() {
        let f = quest_fixture();
        let sink = RecSink { blocked: true, ..RecSink::default() };
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        assert!(sink.offers().is_empty());
        // Quests toggled off for the session: no offer either.
        let sink = RecSink::default();
        let mut q = QuestRuntime::new(false, Box::new(|| {}));
        live_commit(&f, &sink, &mut q);
        assert!(sink.offers().is_empty());
    }

    #[test]
    fn accept_then_expire_reports_failed() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let (mut q, pokes) = counting();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        assert_eq!(sink.updates(), vec![QuestStatus::Active]);
        assert_eq!(pokes.load(SeqCst), 0, "no GitHub step, no poll");
        deadline_check(&f.state, &sink, &mut q, Utc::now());
        assert_eq!(sink.updates(), vec![QuestStatus::Active], "not due yet");
        // Two minutes later the one-minute quest has run out.
        deadline_check(&f.state, &sink, &mut q, Utc::now() + chrono::Duration::minutes(2));
        assert_eq!(sink.updates(), vec![QuestStatus::Active, QuestStatus::Failed]);
        assert!(quests::state::active_view(&f.state.store).unwrap().is_none());
        assert_eq!(sink.presented.lock().unwrap().last(), Some(&(1, false)), "the quest_failed event is presented");
    }

    #[test]
    fn github_step_checks_once_before_failing() {
        let f = quest_fixture_with(r#"steps = [{ kind = "pr_opened" }]"#);
        let sink = RecSink::default();
        let (mut q, pokes) = counting();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        assert_eq!(pokes.load(SeqCst), 1, "polled on accept");
        let late = Utc::now() + chrono::Duration::minutes(2);
        deadline_check(&f.state, &sink, &mut q, late);
        assert_eq!(sink.updates(), vec![QuestStatus::Active, QuestStatus::Checking]);
        assert_eq!(pokes.load(SeqCst), 2, "poked at the deadline");
        deadline_check(&f.state, &sink, &mut q, late);
        assert_eq!(sink.updates().len(), 2, "still waiting for the poll");
        handle(&f.state, &sink, &mut q, Msg::Polled, late);
        deadline_check(&f.state, &sink, &mut q, late);
        assert_eq!(sink.updates(), vec![QuestStatus::Active, QuestStatus::Checking, QuestStatus::Failed]);
        assert_eq!(pokes.load(SeqCst), 2, "only one check per quest");
    }

    #[test]
    fn github_check_gives_up_after_the_cap() {
        let f = quest_fixture_with(r#"steps = [{ kind = "pr_merged" }]"#);
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        let late = Utc::now() + chrono::Duration::minutes(2);
        deadline_check(&f.state, &sink, &mut q, late);
        deadline_check(&f.state, &sink, &mut q, late + chrono::Duration::seconds(10));
        assert_eq!(sink.updates().len(), 2, "within the cap");
        deadline_check(&f.state, &sink, &mut q, late + chrono::Duration::seconds(31));
        assert_eq!(sink.updates(), vec![QuestStatus::Active, QuestStatus::Checking, QuestStatus::Failed]);
    }

    #[test]
    fn no_github_check_when_github_is_off() {
        let f = quest_fixture_with(r#"steps = [{ kind = "pr_opened" }]"#);
        f.state.cfg.write().unwrap().github.enabled = false;
        let sink = RecSink::default();
        let (mut q, pokes) = counting();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        deadline_check(&f.state, &sink, &mut q, Utc::now() + chrono::Duration::minutes(2));
        assert_eq!(sink.updates(), vec![QuestStatus::Active, QuestStatus::Failed]);
        assert_eq!(pokes.load(SeqCst), 0);
    }

    #[test]
    fn ignored_offer_is_a_pass_dropped_offer_is_not() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, false), Utc::now());
        assert_eq!(passes(&f), 1);
        assert!(q.pending.is_none());
        // A second offer, dropped (e.g. the overlay never showed it): no pass.
        live_commit(&f, &sink, &mut q);
        let id2 = sink.offers()[1].0;
        assert_ne!(id, id2);
        handle(&f.state, &sink, &mut q, Msg::DropOffer(id2), Utc::now());
        assert!(q.pending.is_none());
        assert_eq!(passes(&f), 1);
        // Answering an offer that is no longer pending does nothing.
        handle(&f.state, &sink, &mut q, Msg::Answer(id2, false), Utc::now());
        assert_eq!(passes(&f), 1);
    }

    #[test]
    fn stale_pending_offer_is_dropped_without_a_pass() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        deadline_check(&f.state, &sink, &mut q, Utc::now() + chrono::Duration::seconds(60 + 250));
        assert!(q.pending.is_some(), "still within offer_seconds + 300 s");
        deadline_check(&f.state, &sink, &mut q, Utc::now() + chrono::Duration::seconds(60 + 301));
        assert!(q.pending.is_none());
        assert_eq!(*sink.withdrawn.lock().unwrap(), vec![id]);
        assert_eq!(passes(&f), 0);
    }

    #[test]
    fn disabling_cancels_active_quest() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        live_commit(&f, &sink, &mut q);
        let n = quests::state::chain(&f.state.store).unwrap().n;
        handle(&f.state, &sink, &mut q, Msg::SetQuests(false), Utc::now());
        assert_eq!(sink.updates().last(), Some(&QuestStatus::Cancelled));
        assert_eq!(quests::state::chain(&f.state.store).unwrap().n, n);
        assert!(quests::state::active_view(&f.state.store).unwrap().is_none());
        let before = sink.offers().len();
        live_commit(&f, &sink, &mut q);
        assert_eq!(sink.offers().len(), before, "off: commits raise nothing");
    }

    #[test]
    fn disabling_withdraws_pending_offer() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::SetQuests(false), Utc::now());
        assert_eq!(*sink.withdrawn.lock().unwrap(), vec![id]);
        assert!(q.pending.is_none());
        assert_eq!(passes(&f), 0);
    }

    #[test]
    fn quest_progress_from_a_batch_is_forwarded() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        live_commit(&f, &sink, &mut q);
        assert_eq!(sink.updates(), vec![QuestStatus::Active, QuestStatus::Active], "step progress sent");
    }

    #[test]
    fn panicking_expiry_rolls_back_and_backs_off() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        let calls = AtomicUsize::new(0);
        let boom = |s: &Store, _: &Rules, _: &Config, _: DateTime<Utc>, _: FixedOffset| -> Result<Option<(Event, Outcome, QuestView)>> {
            calls.fetch_add(1, SeqCst);
            s.begin_tx().unwrap();
            panic!("rule exploded");
        };
        let late = Utc::now() + chrono::Duration::minutes(2);
        deadline_check_with(&f.state, &sink, &mut q, late, &boom);
        assert_eq!(calls.load(SeqCst), 1);
        // Not left mid-transaction: a new one can start.
        f.state.store.begin_tx().expect("transaction was rolled back");
        f.state.store.rollback_tx().unwrap();
        assert!(quests::state::active_view(&f.state.store).unwrap().is_some(), "still active, to retry");
        // Backs off: no retry (or log line) on every pass.
        deadline_check_with(&f.state, &sink, &mut q, late + chrono::Duration::seconds(5), &boom);
        assert_eq!(calls.load(SeqCst), 1, "not retried within the back-off");
        deadline_check_with(&f.state, &sink, &mut q, late + chrono::Duration::seconds(61), &boom);
        assert_eq!(calls.load(SeqCst), 2, "retried after the back-off");
        let log = std::fs::read_to_string(f.state.paths.log()).unwrap();
        assert_eq!(log.matches("quest expiry failed").count(), 1, "logged once per streak: {log}");
        assert!(log.contains("panicked: rule exploded"), "{log}");
        // Once expiry works again, the quest resolves.
        deadline_check(&f.state, &sink, &mut q, late + chrono::Duration::seconds(122));
        assert_eq!(sink.updates(), vec![QuestStatus::Active, QuestStatus::Failed]);
        assert!(std::fs::read_to_string(f.state.paths.log()).unwrap().contains("quest expiry recovered"));
    }

    #[test]
    fn panicking_store_call_rolls_back() {
        let f = quest_fixture();
        let r: Result<()> = store_call(&f.state, || {
            f.state.store.begin_tx().unwrap();
            panic!("accept exploded");
        });
        assert!(format!("{:#}", r.unwrap_err()).contains("panicked: accept exploded"));
        f.state.store.begin_tx().expect("transaction was rolled back");
        f.state.store.rollback_tx().unwrap();
    }

    #[test]
    fn startup_resends_the_active_quest() {
        let f = quest_fixture();
        let sink = RecSink::default();
        let mut q = quiet();
        live_commit(&f, &sink, &mut q);
        let id = sink.offers()[0].0;
        handle(&f.state, &sink, &mut q, Msg::Answer(id, true), Utc::now());
        // A fresh runtime, as after a restart.
        let sink = RecSink::default();
        let mut q = quiet();
        startup(&f.state, &sink, &mut q, Utc::now());
        assert_eq!(sink.updates(), vec![QuestStatus::Active]);
        assert!(sink.offers().is_empty());
    }

    #[test]
    fn presenter_panic_is_logged_not_fatal() {
        let f = fixture(Rules::default(), None);
        inbox::write_event(&f.state.paths.inbox(), &push(1)).unwrap();
        run_once(&f.state, &PanicSink, false, &mut quiet());
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
