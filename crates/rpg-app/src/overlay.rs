//! The overlay: a corner window for "+N exp" toasts and a top-center window for
//! the Gold Banner. Drives the `Presenter` and resolves banner flavor text lazily.
use crate::presenter::{Banner, BannerKind, Presenter, Toast};
use crate::runtime::{local_offset, log, panic_message, AppState, OutcomeSink};
use crate::tray::Toggles;
use crate::{platform, sound};
use rpg_core::engine::{Outcome, Unlock};
use rpg_core::event::Event;
use rpg_core::flavor;
use rpg_core::paths::Paths;
use rpg_core::rules::Tier;
use serde::{Deserialize, Serialize};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Listener, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const TOAST: &str = "toast";
const BANNER: &str = "banner";
const TOAST_SIZE: (f64, f64) = (360.0, 400.0);
const BANNER_SIZE: (f64, f64) = (560.0, 220.0);
/// Margin from the screen edge, in logical pixels.
const MARGIN: f64 = 16.0;
/// Hide the toast window this long after the last toast (pills fade over 3s).
const TOAST_HIDE_AFTER: Duration = Duration::from_millis(3600);
/// If the banner page never reports `banner-done` (it does after ~8s), move on anyway.
const BANNER_WATCHDOG: Duration = Duration::from_secs(15);

#[derive(Serialize, Clone)]
struct BannerPayload {
    id: u64,
    kind: &'static str,
    title: String,
    name: String,
    text: String,
    reward: String,
    tier: Option<&'static str>,
}

#[derive(Serialize, Clone)]
struct ToastPayload {
    amount: u64,
    label: String,
    notice: bool,
}

#[derive(Deserialize)]
struct DonePayload {
    id: u64,
}

#[derive(Deserialize)]
struct ReadyPayload {
    window: String,
}

/// The banner taken from the presenter: resolving its text (`shown: None`) or on screen.
struct Current {
    id: u64,
    shown: Option<Banner>,
}

/// Presenter plus window state. Every show/hide of an overlay window happens
/// while holding this lock, so the decision and the visibility change are atomic.
#[derive(Default)]
struct Flow {
    presenter: Presenter,
    toast_ready: bool,
    banner_ready: bool,
    current: Option<Current>,
    next_id: u64,
    /// Bumped on every toast batch; the hide timer only hides its own batch.
    toast_gen: u64,
}

impl Flow {
    fn current_id(&self) -> Option<u64> {
        self.current.as_ref().map(|c| c.id)
    }
}

struct Inner {
    app: AppHandle,
    state: AppState,
    toggles: Arc<Toggles>,
    flow: Mutex<Flow>,
}

/// `OutcomeSink` that shows batches in the overlay windows.
pub struct OverlaySink(Arc<Inner>);

impl OverlaySink {
    /// Create the (hidden) toast and banner windows and wire up their events.
    pub fn new(app: &AppHandle, state: AppState, toggles: Arc<Toggles>) -> tauri::Result<OverlaySink> {
        let inner = Arc::new(Inner {
            app: app.clone(),
            state,
            toggles,
            flow: Mutex::new(Flow { presenter: Presenter::new(), ..Flow::default() }),
        });
        let i = inner.clone();
        app.listen_any("overlay-ready", move |e| {
            let Ok(p) = serde_json::from_str::<ReadyPayload>(e.payload()) else { return };
            {
                let mut f = i.flow();
                match p.window.as_str() {
                    TOAST => f.toast_ready = true,
                    BANNER => f.banner_ready = true,
                    _ => return,
                }
            }
            log(&i.state.paths, &format!("overlay {} ready", p.window));
            pump(&i);
        });
        let i = inner.clone();
        app.listen_any("banner-done", move |e| {
            if let Ok(p) = serde_json::from_str::<DonePayload>(e.payload()) {
                finish_banner(&i, p.id);
            }
        });

        // Only now create the pages: they report `overlay-ready` as soon as they load.
        let paths = &inner.state.paths;
        let toast = build_window(app, paths, TOAST, "overlay/toast.html", TOAST_SIZE)?;
        toast.set_ignore_cursor_events(true)?;
        build_window(app, paths, BANNER, "overlay/banner.html", BANNER_SIZE)?;
        place_windows(app);
        Ok(OverlaySink(inner))
    }

    /// Tray "Do not disturb": hold banners and drop toasts while on. A banner
    /// on screen is hidden and goes back to the head of the queue; one still
    /// resolving its text is requeued by its worker (`Presenter::confirm_show`).
    pub fn set_dnd(&self, on: bool) {
        {
            let mut f = self.0.flow();
            f.presenter.set_dnd(on);
            if on && f.current.as_ref().is_some_and(|c| c.shown.is_some()) {
                let shown = f.current.take().and_then(|c| c.shown).expect("checked above");
                f.presenter.requeue_front(shown);
                if let Some(w) = self.0.window(BANNER) {
                    let _ = w.hide();
                }
            }
        }
        if !on {
            pump(&self.0);
        }
    }
}

impl OverlaySink {
    /// Show the result of a user action (see `Presenter::notice`).
    pub fn notice(&self, ok: bool, title: &str, detail: &str) {
        self.0.flow().presenter.notice(ok, title, detail);
        pump(&self.0);
    }
}

impl OutcomeSink for OverlaySink {
    fn present(&self, batch: Vec<(Event, Outcome)>, away: bool) {
        self.0.flow().presenter.push(&batch, away);
        pump(&self.0);
    }
}

impl Inner {
    fn flow(&self) -> MutexGuard<'_, Flow> {
        self.flow.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn window(&self, label: &str) -> Option<WebviewWindow> {
        self.app.get_webview_window(label)
    }
}

fn build_window(
    app: &AppHandle,
    paths: &Paths,
    label: &str,
    page: &str,
    (w, h): (f64, f64),
) -> tauri::Result<WebviewWindow> {
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title("rpg")
        .inner_size(w, h)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .shadow(false)
        .visible(false);
    // macOS transparency needs tauri's `macos-private-api` feature (#4).
    #[cfg(not(target_os = "macos"))]
    let builder = builder.transparent(true);
    let win = builder.build()?;
    if let Err(e) = platform::make_non_activating(&win) {
        log(paths, &format!("{label}: could not make non-activating: {e:#}"));
    }
    Ok(win)
}

/// Toast bottom-right and banner top-center of the primary monitor's work area.
fn place_windows(app: &AppHandle) {
    let Ok(Some(mon)) = app.primary_monitor() else { return };
    let s = mon.scale_factor();
    let area = mon.work_area();
    let (ax, ay) = (area.position.x as f64, area.position.y as f64);
    let (aw, ah) = (area.size.width as f64, area.size.height as f64);
    let at = |x: f64, y: f64| PhysicalPosition::new(x.round() as i32, y.round() as i32);
    if let Some(w) = app.get_webview_window(TOAST) {
        let x = ax + aw - (TOAST_SIZE.0 + MARGIN) * s;
        let y = ay + ah - (TOAST_SIZE.1 + MARGIN) * s;
        let _ = w.set_position(at(x, y));
    }
    if let Some(w) = app.get_webview_window(BANNER) {
        let x = ax + (aw - BANNER_SIZE.0 * s) / 2.0;
        let y = ay + 2.0 * MARGIN * s;
        let _ = w.set_position(at(x, y));
    }
}

/// Show whatever the presenter has ready.
fn pump(inner: &Arc<Inner>) {
    let mut f = inner.flow();
    if f.toast_ready {
        let toasts = f.presenter.next_toasts();
        if !toasts.is_empty() {
            show_toasts(inner, &mut f, toasts);
        }
    }
    if !f.banner_ready {
        return;
    }
    let Some(b) = f.presenter.next_banner() else { return };
    f.next_id += 1;
    let id = f.next_id;
    f.current = Some(Current { id, shown: None });
    drop(f);

    let i = inner.clone();
    let spawned = std::thread::Builder::new().name("rpg-banner".into()).spawn(move || {
        if let Err(p) = catch_unwind(AssertUnwindSafe(|| show_banner(&i, id, b))) {
            log(&i.state.paths, &format!("banner {id} panicked: {}", panic_message(&*p)));
            finish_banner(&i, id);
        }
    });
    if let Err(e) = spawned {
        log(&inner.state.paths, &format!("banner thread failed: {e}"));
        finish_banner(inner, id);
    }
}

/// Called with the flow lock held.
fn show_toasts(inner: &Arc<Inner>, f: &mut Flow, toasts: Vec<Toast>) {
    let Some(win) = inner.window(TOAST) else { return };
    let _ = win.show();
    let payload: Vec<ToastPayload> = toasts.into_iter().map(|t| ToastPayload { amount: t.amount, label: t.label, notice: t.notice }).collect();
    if let Err(e) = inner.app.emit_to(TOAST, "toast", payload) {
        log(&inner.state.paths, &format!("emitting toast failed: {e}"));
    }
    f.toast_gen += 1;
    let gen = f.toast_gen;
    let i = inner.clone();
    let _ = std::thread::Builder::new().name("rpg-toast".into()).spawn(move || {
        std::thread::sleep(TOAST_HIDE_AFTER);
        let f = i.flow();
        if f.toast_gen == gen {
            if let Some(w) = i.window(TOAST) {
                let _ = w.hide();
            }
        }
    });
}

/// Runs on its own thread: resolve the text (may wait on the LLM), then, if
/// DND did not come on meanwhile, play the sound and show the window. A
/// watchdog ends the banner if the page never reports `banner-done`.
fn show_banner(inner: &Arc<Inner>, id: u64, mut b: Banner) {
    if !b.text_ready {
        if let Some(src) = &b.flavor {
            b.text = resolve_flavor(&inner.state, &src.unlock, &src.event);
        }
        b.text_ready = true;
    }
    let sound_cfg = inner.state.cfg.read().unwrap_or_else(|p| p.into_inner()).sound.clone();
    {
        let mut f = inner.flow();
        if f.current_id() != Some(id) {
            return;
        }
        let Some(b) = f.presenter.confirm_show(b) else {
            // DND came on while resolving: requeued (with its text) for later.
            f.current = None;
            return;
        };
        let Some(win) = inner.window(BANNER) else {
            drop(f);
            finish_banner(inner, id);
            return;
        };
        let muted = inner.toggles.muted.load(Ordering::Relaxed);
        let paths = inner.state.paths.clone();
        sound::play(&b.sound_key, &sound_cfg, &paths.sounds(), muted, move |msg| log(&paths, &msg));
        let _ = win.show();
        if let Err(e) = inner.app.emit_to(BANNER, "banner", payload(id, &b)) {
            log(&inner.state.paths, &format!("emitting banner failed: {e}"));
        }
        f.current = Some(Current { id, shown: Some(b) });
    }
    std::thread::sleep(BANNER_WATCHDOG);
    finish_banner(inner, id);
}

fn payload(id: u64, b: &Banner) -> BannerPayload {
    BannerPayload {
        id,
        kind: match b.kind {
            BannerKind::Achievement => "achievement",
            BannerKind::LevelUp => "level_up",
            BannerKind::AwaySummary => "summary",
            BannerKind::Notice => "notice",
        },
        title: b.title.clone(),
        name: b.name.clone(),
        text: b.text.clone(),
        reward: b.reward.clone(),
        tier: b.tier.map(tier_name),
    }
}

/// End banner `id` (no-op if it already ended or was requeued) and show what's next.
fn finish_banner(inner: &Arc<Inner>, id: u64) {
    {
        let mut f = inner.flow();
        if f.current_id() != Some(id) {
            return;
        }
        f.current = None;
        f.presenter.banner_done();
        if let Some(w) = inner.window(BANNER) {
            let _ = w.hide();
        }
    }
    pump(inner);
}

fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Bronze => "Bronze",
        Tier::Silver => "Silver",
        Tier::Gold => "Gold",
    }
}

/// The announcement line for an unlock that is about to get a banner: LLM text
/// for `flavor = llm` rules (waits up to `llm.timeout_secs`), else the rule's
/// fallback. Stored with the unlock so it never changes. Call off the UI thread.
pub fn resolve_flavor(state: &AppState, unlock: &Unlock, ev: &Event) -> String {
    let rule = state
        .rules
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .list
        .iter()
        .find(|r| r.id == unlock.rule_id)
        .cloned();
    // The rule was removed since the unlock: nothing to say beyond its name.
    let Some(rule) = rule else { return String::new() };
    let llm = state.cfg.read().unwrap_or_else(|p| p.into_inner()).llm.clone();
    let tz = local_offset(ev.ts);
    let text = catch_unwind(AssertUnwindSafe(|| flavor::render(&rule, unlock, ev, &llm, state.gen.as_deref(), tz)))
        .unwrap_or_else(|p| {
            log(&state.paths, &format!("flavor for {} panicked: {}", rule.id, panic_message(&*p)));
            rule.fallback.clone()
        });
    if let Err(e) = state.store.set_unlock_flavor(unlock.unlock_id, &text) {
        log(&state.paths, &format!("storing flavor for unlock {} failed: {e:#}", unlock.unlock_id));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use rpg_core::config::Config;
    use rpg_core::engine;
    use rpg_core::event::{EventKind, Source};
    use rpg_core::flavor::TextGen;
    use rpg_core::rules::{parse_rule, Rules};
    use rpg_core::store::Store;
    use std::sync::RwLock;

    struct FakeGen;
    impl TextGen for FakeGen {
        fn generate(&self, _prompt: &str, _t: Duration) -> Result<String> {
            Ok("FAKE FLAVOR".into())
        }
    }

    fn state(flavor: &str, gen: Option<Arc<dyn TextGen>>) -> (tempfile::TempDir, AppState) {
        let rule = parse_rule(&format!(
            r#"
id = "first_push"
name = "First Push"
tier = "bronze"
trigger = ["push"]
detector = {{ type = "first", scope = "ever" }}
repeatable = false
fallback = "STATIC"
flavor = {flavor}
"#
        ))
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(Some(tmp.path().to_path_buf())).unwrap();
        let st = AppState {
            paths,
            store: Arc::new(Store::open_in_memory().unwrap()),
            rules: Arc::new(RwLock::new(Rules { list: vec![rule] })),
            cfg: Arc::new(RwLock::new(Config::default())),
            gen,
        };
        (tmp, st)
    }

    /// Run one push through the engine; return its event and unlock.
    fn unlock_once(st: &AppState) -> (Event, Unlock) {
        let ev = Event::new(EventKind::Push, Source::Git, Some("r".into()), serde_json::json!({}));
        let rules = st.rules.read().unwrap().clone();
        let cfg = st.cfg.read().unwrap().clone();
        let out = engine::process(&st.store, &rules, &cfg, &ev, &[], local_offset(ev.ts)).unwrap();
        let u = out.unlocks.into_iter().next().expect("unlocked");
        (ev, u)
    }

    #[test]
    fn llm_flavor_resolved_and_stored_for_banner() {
        let (_t, st) = state(r#"{ type = "llm", prompt = "Rewrite {fallback} {context}" }"#, Some(Arc::new(FakeGen)));
        let (ev, u) = unlock_once(&st);
        assert_eq!(st.store.list_unlocks().unwrap()[0].flavor_text, None, "not rendered at processing time");
        assert_eq!(resolve_flavor(&st, &u, &ev), "FAKE FLAVOR");
        assert_eq!(st.store.list_unlocks().unwrap()[0].flavor_text.as_deref(), Some("FAKE FLAVOR"));
    }

    #[test]
    fn static_flavor_uses_fallback() {
        let (_t, st) = state(r#"{ type = "static" }"#, Some(Arc::new(FakeGen)));
        let (ev, u) = unlock_once(&st);
        assert_eq!(resolve_flavor(&st, &u, &ev), "STATIC");
        assert_eq!(st.store.list_unlocks().unwrap()[0].flavor_text.as_deref(), Some("STATIC"));
    }
}
