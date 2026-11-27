//! The overlay: a corner window for "+N exp" toasts and the quest timer pill, and a
//! top-center window for the Gold Banner. Drives the `Presenter` and resolves banner
//! flavor text lazily.
use crate::presenter::{Banner, BannerKind, Presenter, Toast};
use crate::runtime::{local_offset, log, panic_message, AppState, OutcomeSink, Runtime};
use crate::tray::Toggles;
use crate::{platform, sound};
use rpg_core::engine::{Outcome, Unlock};
use rpg_core::event::Event;
use rpg_core::flavor;
use rpg_core::paths::Paths;
use rpg_core::quests::state::{Offer, QuestStatus, QuestView};
use rpg_core::rules::Tier;
use serde::{Deserialize, Serialize};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Listener, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const TOAST: &str = "toast";
const BANNER: &str = "banner";
const TOAST_SIZE: (f64, f64) = (360.0, 400.0);
/// The banner frame is 540x200; the window adds BANNER_INSET on every side so the
/// tier glow (up to 32px, plus the flash) is not cut off at the window edge.
const BANNER_SIZE: (f64, f64) = (620.0, 280.0);
/// Must match the `.ach-b` `inset` in `ui/overlay/overlay.css`.
const BANNER_INSET: f64 = 40.0;
// Room for the strongest glow (32px) on every side of the frame.
const _: () = assert!(BANNER_INSET >= 32.0);
/// Where the banner frame's top sits below the top of the work area.
const BANNER_TOP: f64 = 42.0;
/// While a banner shows, how often to check whether the cursor is over its frame.
const CLICK_POLL: Duration = Duration::from_millis(40);
/// Margin from the screen edge, in logical pixels.
const MARGIN: f64 = 16.0;
/// Hide the toast window this long after the last toast (pills fade over 3s),
/// unless the quest pill is up.
const TOAST_HIDE_AFTER: Duration = Duration::from_millis(3600);
/// If the banner page never reports `banner-done` (it does after ~8s), move on anyway.
/// While the pointer rests on the banner the deadline keeps sliding forward.
const BANNER_WATCHDOG: Duration = Duration::from_secs(15);
const WATCHDOG_POLL: Duration = Duration::from_millis(250);
/// An offer's banner ends by its fuse (`offer_seconds`); the watchdog allows this much more.
const OFFER_WATCHDOG_SLACK: Duration = Duration::from_secs(5);
/// After Accept / Pass, the page's exit animation should end within this.
const ANSWERED_WATCHDOG: Duration = Duration::from_secs(2);

#[derive(Serialize, Clone)]
struct BannerPayload {
    id: u64,
    kind: &'static str,
    title: String,
    name: String,
    text: String,
    reward: String,
    tier: Option<&'static str>,
    quest: Option<QuestOfferPayload>,
}

/// What the offer banner shows beyond the common fields. The page runs the fuse.
#[derive(Serialize, Clone)]
struct QuestOfferPayload {
    offer_id: u64,
    tier_label: &'static str,
    /// The step labels, in order.
    goal: String,
    minutes: u32,
    xp: u64,
    multiplier: f64,
    next_multiplier: f64,
    reward: u64,
    /// Passes (or ignored offers) left before the chain breaks, counting this one.
    passes_left: u32,
    offer_seconds: u64,
}

#[derive(Deserialize)]
struct AnswerPayload {
    offer_id: u64,
    accept: bool,
}

#[derive(Serialize, Clone)]
struct ToastPayload {
    amount: u64,
    label: String,
    notice: bool,
    skill: bool,
    /// Tier label of an achievement toast ("Bronze"...).
    tier: Option<&'static str>,
}

/// The timer pill's state, sent to the toast page as `quest`. The page runs the countdown.
#[derive(Serialize, Clone)]
struct QuestPayload {
    /// Which quest: an `active` update for another row replaces the pill.
    row_id: i64,
    status: &'static str,
    name: String,
    tier_label: &'static str,
    deadline_ms: i64,
    steps: Vec<StepPayload>,
    reward: u64,
    multiplier: f64,
}

#[derive(Serialize, Clone)]
struct StepPayload {
    label: String,
    have: u32,
    need: u32,
}

/// Where the toast page drew the pill (CSS pixels from the window's top-left), from `quest-pill-rect`.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
struct PillRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Deserialize)]
struct CardPayload {
    open: bool,
}

#[derive(Deserialize)]
struct DonePayload {
    id: u64,
}

#[derive(Deserialize)]
struct HoverPayload {
    id: u64,
    hovering: bool,
}

#[derive(Deserialize)]
struct ReadyPayload {
    window: String,
}

/// The banner taken from the presenter: resolving its text (`shown: None`) or on screen.
struct Current {
    id: u64,
    shown: Option<Banner>,
    /// A quest offer banner: the runtime's offer id.
    offer: Option<u64>,
    /// The offer was accepted or passed (the page is playing its exit).
    answered: bool,
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
    /// When the last toast batch showed; the window hides `TOAST_HIDE_AFTER` later.
    last_toast: Option<Instant>,
    /// The latest quest update, from the first one until the page reports its pill gone
    /// after an ending (completed / failed / cancelled). While set, the toast window stays up.
    quest: Option<QuestPayload>,
    /// The pill's last reported rect (`None`: no pill on the page).
    pill_rect: Option<PillRect>,
    /// The thread that lets clicks reach the pill is running.
    pill_polling: bool,
    /// The pointer is on the current banner (it stays up while this is set).
    banner_hovered: bool,
}

impl Flow {
    fn current_id(&self) -> Option<u64> {
        self.current.as_ref().map(|c| c.id)
    }

    /// Whether the toast window may hide now: no quest pill and no recent toast.
    fn toasts_idle(&self, now: Instant) -> bool {
        match self.last_toast {
            Some(t) => should_hide_toasts(self.quest.is_some(), t, now),
            None => self.quest.is_none(),
        }
    }

    /// End the current banner without requeueing it (it is off screen or about to be hidden).
    fn end_current(&mut self) -> Option<Current> {
        self.banner_hovered = false;
        let c = self.current.take()?;
        self.presenter.banner_done();
        Some(c)
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
            if p.window == TOAST {
                // A quest update that came before the page could listen (the startup re-show).
                let f = i.flow();
                if let Some(q) = f.quest.clone() {
                    emit_quest(&i, q);
                }
            }
            pump(&i);
        });
        let i = inner.clone();
        app.listen_any("banner-done", move |e| {
            if let Ok(p) = serde_json::from_str::<DonePayload>(e.payload()) {
                finish_banner(&i, p.id);
            }
        });

        let i = inner.clone();
        app.listen_any("banner-hover", move |e| {
            if let Ok(p) = serde_json::from_str::<HoverPayload>(e.payload()) {
                let mut f = i.flow();
                if f.current_id() == Some(p.id) {
                    f.banner_hovered = p.hovering;
                }
            }
        });

        // Accept / Pass / the fuse ran out. The page then plays its exit and sends `banner-done`.
        let i = inner.clone();
        app.listen_any("quest-answer", move |e| {
            let Ok(p) = serde_json::from_str::<AnswerPayload>(e.payload()) else { return };
            let fresh = match i.flow().current.as_mut() {
                Some(c) if c.offer == Some(p.offer_id) && c.shown.is_some() && !c.answered => {
                    c.answered = true;
                    true
                }
                _ => false,
            };
            if fresh {
                if let Some(rt) = runtime(&i) {
                    rt.answer_offer(p.offer_id, p.accept);
                }
            }
        });

        // The toast page reports the pill's rect after every layout change, `null` once it is gone.
        let i = inner.clone();
        app.listen_any("quest-pill-rect", move |e| {
            let Ok(rect) = serde_json::from_str::<Option<PillRect>>(e.payload()) else { return };
            let mut f = i.flow();
            f.pill_rect = rect;
            // Gone after its ending: the window may hide. (Gone while the latest update is
            // live means a pill is being replaced; the new one reports in a moment.)
            if rect.is_none() && f.quest.as_ref().is_some_and(|q| !is_live(q.status)) {
                f.quest = None;
                if f.toasts_idle(Instant::now()) {
                    if let Some(w) = i.window(TOAST) {
                        let _ = w.hide();
                    }
                }
            }
        });

        // The card opened: take focus so that a click anywhere else blurs the page, which
        // collapses it. The window is non-activating otherwise, and goes back to that on close.
        let i = inner.clone();
        app.listen_any("quest-card-open", move |e| {
            let Ok(p) = serde_json::from_str::<CardPayload>(e.payload()) else { return };
            let Some(w) = i.window(TOAST) else { return };
            let _ = w.set_focusable(p.open);
            if p.open {
                if let Err(e) = w.set_focus() {
                    log(&i.state.paths, &format!("focusing the quest card failed: {e}"));
                }
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
    /// Quest offers, queued or on screen, are dropped instead (no pass).
    pub fn set_dnd(&self, on: bool) {
        let dropped = {
            let mut f = self.0.flow();
            let mut dropped = f.presenter.set_dnd(on);
            if on && f.current.as_ref().is_some_and(|c| c.shown.is_some()) {
                if f.current.as_ref().is_some_and(|c| c.offer.is_some()) {
                    let c = f.end_current().expect("checked above");
                    cancel_on_page(&self.0, c.id);
                    dropped.extend(c.offer.filter(|_| !c.answered));
                } else {
                    let shown = f.current.take().and_then(|c| c.shown).expect("checked above");
                    f.presenter.requeue_front(shown);
                }
                if let Some(w) = self.0.window(BANNER) {
                    let _ = w.hide();
                }
            }
            dropped
        };
        for id in dropped {
            drop_offer(&self.0, id);
        }
        // Even under DND a failure notice may be next.
        pump(&self.0);
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

    fn offers_allowed(&self) -> bool {
        let t = &self.0.toggles;
        !t.dnd.load(Ordering::Relaxed) && t.quests.load(Ordering::Relaxed)
    }

    /// Jumps the banner queue. Under DND (it raced the toggle) it is dropped at once.
    fn quest_offer(&self, id: u64, offer: Offer) {
        let queued = {
            let mut f = self.0.flow();
            let dnd = self.0.toggles.dnd.load(Ordering::Relaxed);
            if !dnd {
                f.presenter.offer(id, offer);
            }
            !dnd
        };
        if queued {
            pump(&self.0);
        } else {
            drop_offer(&self.0, id);
        }
    }

    /// Show the timer pill (any status). Not held by Do not disturb: it only drops toasts.
    fn quest_update(&self, view: QuestView) {
        log(&self.0.state.paths, &format!("quest {} is {:?}", view.quest_id, view.status));
        let q = quest_payload(&view);
        let start_poll = {
            let mut f = self.0.flow();
            f.quest = Some(q.clone());
            if f.toast_ready {
                emit_quest(&self.0, q);
            }
            let start = !f.pill_polling;
            f.pill_polling = true;
            start
        };
        if start_poll {
            let i = self.0.clone();
            let spawned = std::thread::Builder::new().name("rpg-pill-clicks".into()).spawn(move || pass_clicks_outside_pill(&i));
            if let Err(e) = spawned {
                log(&self.0.state.paths, &format!("pill click thread failed: {e}"));
                self.0.flow().pill_polling = false;
            }
        }
    }

    /// The runtime no longer wants an answer: unqueue or hide it, without calling back.
    fn quest_offer_withdrawn(&self, id: u64) {
        {
            let mut f = self.0.flow();
            if f.presenter.withdraw_offer(id) || f.current.as_ref().and_then(|c| c.offer) != Some(id) {
                return;
            }
            // Still resolving its text: its worker sees it is no longer current and stops.
            let c = f.end_current().expect("checked above");
            if c.shown.is_some() {
                cancel_on_page(&self.0, c.id);
                if let Some(w) = self.0.window(BANNER) {
                    let _ = w.hide();
                }
            }
        }
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
        let y = ay + (BANNER_TOP - BANNER_INSET) * s;
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
    let offer = b.offer.as_ref().map(|(oid, _)| *oid);
    f.current = Some(Current { id, shown: None, offer, answered: false });
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
    let payload: Vec<ToastPayload> = toasts.into_iter().map(|t| ToastPayload { amount: t.amount, label: t.label, notice: t.notice, skill: t.skill, tier: t.tier.map(|t| t.label()) }).collect();
    if let Err(e) = inner.app.emit_to(TOAST, "toast", payload) {
        log(&inner.state.paths, &format!("emitting toast failed: {e}"));
    }
    f.last_toast = Some(Instant::now());
    let i = inner.clone();
    let _ = std::thread::Builder::new().name("rpg-toast".into()).spawn(move || {
        std::thread::sleep(TOAST_HIDE_AFTER);
        let f = i.flow();
        // A later batch has its own timer; a quest pill hides the window itself once gone.
        if f.toasts_idle(Instant::now()) {
            if let Some(w) = i.window(TOAST) {
                let _ = w.hide();
            }
        }
    });
}

/// Whether the toast window should hide at `now`, given the last toast batch at
/// `last_toast`: never while the quest pill is up, else once the toasts have faded.
fn should_hide_toasts(quest_shown: bool, last_toast: Instant, now: Instant) -> bool {
    !quest_shown && now.saturating_duration_since(last_toast) >= TOAST_HIDE_AFTER
}

fn is_live(status: &str) -> bool {
    matches!(status, "active" | "checking")
}

fn quest_payload(v: &QuestView) -> QuestPayload {
    QuestPayload {
        row_id: v.row_id,
        status: match v.status {
            QuestStatus::Active => "active",
            QuestStatus::Checking => "checking",
            QuestStatus::Completed => "completed",
            QuestStatus::Failed => "failed",
            QuestStatus::Cancelled => "cancelled",
        },
        name: v.name.clone(),
        tier_label: v.tier.label(),
        deadline_ms: v.deadline.timestamp_millis(),
        steps: v.steps.iter().map(|s| StepPayload { label: s.label.clone(), have: s.have, need: s.need }).collect(),
        reward: v.reward,
        multiplier: v.multiplier,
    }
}

/// Show the toast window and send it the pill's state. Call with the flow lock held.
fn emit_quest(inner: &Inner, q: QuestPayload) {
    if let Some(w) = inner.window(TOAST) {
        let _ = w.show();
    }
    if let Err(e) = inner.app.emit_to(TOAST, "quest", q) {
        log(&inner.state.paths, &format!("emitting quest failed: {e}"));
    }
}

/// Whether `cursor` is over the pill drawn at `rect` (CSS pixels) in a toast window at
/// `win` (cursor and window in physical desktop pixels) on a display with `scale`.
fn over_pill(cursor: (f64, f64), win: (f64, f64), scale: f64, rect: &PillRect) -> bool {
    let (x, y) = ((cursor.0 - win.0) / scale, (cursor.1 - win.1) / scale);
    (rect.x..rect.x + rect.w).contains(&x) && (rect.y..rect.y + rect.h).contains(&y)
}

/// While there is a quest pill, make the toast window catch the mouse only while the
/// cursor is over the pill (exp pills and the empty rest keep passing clicks through).
/// When the pill is gone, restore full click-through and stop.
fn pass_clicks_outside_pill(inner: &Arc<Inner>) {
    let Some(win) = inner.window(TOAST) else {
        inner.flow().pill_polling = false;
        return;
    };
    let mut ignoring = None;
    loop {
        let rect = {
            let mut f = inner.flow();
            if f.quest.is_none() {
                // Under the lock, so a new quest update either sees this thread running or starts another.
                f.pill_polling = false;
                let _ = win.set_ignore_cursor_events(true);
                return;
            }
            f.pill_rect
        };
        let over = rect.is_some_and(|r| match (inner.app.cursor_position(), win.outer_position(), win.scale_factor()) {
            (Ok(c), Ok(p), Ok(s)) => over_pill((c.x, c.y), (p.x as f64, p.y as f64), s, &r),
            _ => true, // can't tell: keep the pill clickable
        });
        if ignoring != Some(!over) {
            let _ = win.set_ignore_cursor_events(!over);
            ignoring = Some(!over);
        }
        std::thread::sleep(CLICK_POLL);
    }
}

/// Runs on its own thread: resolve the text (may wait on the LLM), then, if
/// DND did not come on meanwhile, play the sound and show the window. A
/// watchdog ends the banner if the page never reports `banner-done`.
fn show_banner(inner: &Arc<Inner>, id: u64, mut b: Banner) {
    if !b.text_ready {
        if let Some(src) = &b.flavor {
            b.text = resolve_flavor(&inner.state, &src.unlock, &src.event);
        } else if let Some((_, offer)) = &b.offer {
            b.text = resolve_offer_flavor(&inner.state, offer);
        }
        b.text_ready = true;
    }
    let (sound_cfg, offer_seconds) = {
        let cfg = inner.state.cfg.read().unwrap_or_else(|p| p.into_inner());
        (cfg.sound.clone(), cfg.quests.offer_seconds)
    };
    let offer_id = b.offer.as_ref().map(|(oid, _)| *oid);
    {
        let mut f = inner.flow();
        if f.current_id() != Some(id) {
            return;
        }
        let Some(b) = f.presenter.confirm_show(b) else {
            // DND came on while resolving: requeued (with its text) for later,
            // except an offer, which is dropped.
            f.current = None;
            drop(f);
            if let Some(oid) = offer_id {
                drop_offer(inner, oid);
                pump(inner);
            }
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
        if let Err(e) = inner.app.emit_to(BANNER, "banner", payload(id, &b, offer_seconds)) {
            log(&inner.state.paths, &format!("emitting banner failed: {e}"));
        }
        f.banner_hovered = false;
        f.current = Some(Current { id, shown: Some(b), offer: offer_id, answered: false });
    }
    let i = inner.clone();
    let _ = std::thread::Builder::new().name("rpg-banner-clicks".into()).spawn(move || pass_clicks_outside_banner(&i, id));
    // An offer ends by its fuse (or an answer), not by time on screen or hover.
    let limit = if offer_id.is_some() { Duration::from_secs(offer_seconds) + OFFER_WATCHDOG_SLACK } else { BANNER_WATCHDOG };
    let mut deadline = Instant::now() + limit;
    let mut answered = false;
    while Instant::now() < deadline {
        std::thread::sleep(WATCHDOG_POLL);
        let f = inner.flow();
        let Some(c) = f.current.as_ref().filter(|c| c.id == id) else { return };
        if c.answered && !answered {
            answered = true;
            deadline = deadline.min(Instant::now() + ANSWERED_WATCHDOG);
        }
        if f.banner_hovered && offer_id.is_none() {
            deadline = Instant::now() + BANNER_WATCHDOG;
        }
    }
    let unanswered = inner.flow().current.as_ref().is_some_and(|c| c.id == id && !c.answered);
    if let Some(oid) = offer_id.filter(|_| unanswered) {
        // The page never answered (its fuse should have): not the user's pass.
        log(&inner.state.paths, &format!("offer {oid} banner timed out unanswered"));
        drop_offer(inner, oid);
    }
    finish_banner(inner, id);
}

/// Whether `cursor` is over the banner frame of a banner window at `win` (both in
/// physical desktop pixels) on a display with `scale`. The glow margin around the frame isn't.
fn over_banner(cursor: (f64, f64), win: (f64, f64), scale: f64) -> bool {
    let (x, y) = ((cursor.0 - win.0) / scale, (cursor.1 - win.1) / scale);
    (BANNER_INSET..BANNER_SIZE.0 - BANNER_INSET).contains(&x) && (BANNER_INSET..BANNER_SIZE.1 - BANNER_INSET).contains(&y)
}

/// While banner `id` shows, let clicks on its transparent glow margin through to the
/// window underneath and keep the frame itself clickable. A window only ignores the
/// mouse as a whole, so this follows the cursor and switches as it crosses the frame.
fn pass_clicks_outside_banner(inner: &Arc<Inner>, id: u64) {
    let Some(win) = inner.window(BANNER) else { return };
    let mut ignoring = None;
    while inner.flow().current_id() == Some(id) {
        let over = match (inner.app.cursor_position(), win.outer_position(), win.scale_factor()) {
            (Ok(c), Ok(p), Ok(s)) => over_banner((c.x, c.y), (p.x as f64, p.y as f64), s),
            _ => true, // can't tell: keep the banner clickable
        };
        if ignoring != Some(!over) {
            let _ = win.set_ignore_cursor_events(!over);
            ignoring = Some(!over);
        }
        std::thread::sleep(CLICK_POLL);
    }
}

fn payload(id: u64, b: &Banner, offer_seconds: u64) -> BannerPayload {
    BannerPayload {
        id,
        kind: match b.kind {
            BannerKind::Achievement => "achievement",
            BannerKind::LevelUp => "level_up",
            BannerKind::AwaySummary => "summary",
            BannerKind::Notice => "notice",
            BannerKind::QuestOffer => "quest_offer",
        },
        title: b.title.clone(),
        name: b.name.clone(),
        text: b.text.clone(),
        reward: b.reward.clone(),
        tier: b.tier.map(Tier::label),
        quest: b.offer.as_ref().map(|(offer_id, o)| QuestOfferPayload {
            offer_id: *offer_id,
            tier_label: o.tier.label(),
            goal: o.step_labels.join(" → "),
            minutes: o.minutes,
            xp: o.xp,
            multiplier: o.multiplier,
            next_multiplier: o.next_multiplier,
            reward: o.reward,
            passes_left: o.max_passes.saturating_sub(o.passes),
            offer_seconds,
        }),
    }
}

/// Tell the page banner `id` is gone (DND, withdrawn): reset, with no exit and no `banner-done`.
fn cancel_on_page(inner: &Inner, id: u64) {
    if let Err(e) = inner.app.emit_to(BANNER, "banner-cancel", id) {
        log(&inner.state.paths, &format!("emitting banner-cancel failed: {e}"));
    }
}

/// The runtime, looked up when needed (it is managed after the overlay is built).
fn runtime(inner: &Inner) -> Option<Runtime> {
    let rt = inner.app.try_state::<Runtime>().map(|s| s.inner().clone());
    if rt.is_none() {
        log(&inner.state.paths, "quest offer: runtime not available");
    }
    rt
}

/// Forget offer `id` without counting a pass.
fn drop_offer(inner: &Inner, id: u64) {
    if let Some(rt) = runtime(inner) {
        rt.drop_offer(id);
    }
}

/// A quest offer's text: LLM text when its `flavor` opts in (waits up to
/// `llm.timeout_secs`), else its fallback. Not stored. Call off the UI thread.
fn resolve_offer_flavor(state: &AppState, offer: &Offer) -> String {
    let llm = state.cfg.read().unwrap_or_else(|p| p.into_inner()).llm.clone();
    let tz = local_offset(offer.trigger.ts);
    // Cloned out of the lock, so a live LLM swap never waits on this call.
    let gen = state.gen.read().unwrap_or_else(|p| p.into_inner()).clone();
    catch_unwind(AssertUnwindSafe(|| {
        flavor::render_offer(&offer.name, &offer.fallback, &offer.flavor, &offer.trigger, &llm, gen.as_deref(), tz)
    }))
    .unwrap_or_else(|p| {
        log(&state.paths, &format!("flavor for quest {} panicked: {}", offer.quest_id, panic_message(&*p)));
        offer.fallback.clone()
    })
}

/// End banner `id` (no-op if it already ended or was requeued) and show what's next.
fn finish_banner(inner: &Arc<Inner>, id: u64) {
    {
        let mut f = inner.flow();
        if f.current_id() != Some(id) {
            return;
        }
        f.current = None;
        f.banner_hovered = false;
        f.presenter.banner_done();
        if let Some(w) = inner.window(BANNER) {
            let _ = w.hide();
        }
    }
    pump(inner);
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
        .find(|r| r.id == unlock.base_rule_id) // `rule_id` is `id.tier` for ladder steps
        .cloned();
    // The rule was removed since the unlock: nothing to say beyond its name.
    let Some(rule) = rule else { return String::new() };
    let llm = state.cfg.read().unwrap_or_else(|p| p.into_inner()).llm.clone();
    let gen = state.gen.read().unwrap_or_else(|p| p.into_inner()).clone();
    let tz = local_offset(ev.ts);
    let text = catch_unwind(AssertUnwindSafe(|| flavor::render(&rule, unlock, ev, &llm, gen.as_deref(), tz)))
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
counter = {{ type = "first", scope = "ever" }}
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
            rules: Arc::new(RwLock::new(Rules { list: vec![rule], ..Default::default() })),
            cfg: Arc::new(RwLock::new(Config::default())),
            gen: Arc::new(RwLock::new(gen)),
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

    #[test]
    fn ladder_step_flavor_finds_its_rule() {
        let (_t, st) = state(r#"{ type = "static" }"#, None);
        let ladder = parse_rule(
            r#"
id = "pusher"
name = "Pusher"
trigger = ["push"]
counter = { type = "lifetime" }
tiers = [{ tier = "bronze", at = 1 }, { tier = "silver", at = 5 }]
fallback = "LADDER"
"#,
        )
        .unwrap();
        st.rules.write().unwrap().list = vec![ladder];
        let (ev, u) = unlock_once(&st);
        assert_eq!(u.rule_id, "pusher.bronze", "unlock key is the step id");
        assert_eq!(resolve_flavor(&st, &u, &ev), "LADDER");
        assert_eq!(st.store.list_unlocks().unwrap()[0].flavor_text.as_deref(), Some("LADDER"));
    }

    #[test]
    fn offer_payload_carries_the_quest_fields() {
        let ev = Event::new(EventKind::Commit, Source::Git, Some("r".into()), serde_json::json!({}));
        let offer = Offer {
            quest_id: "ship_it".into(),
            name: "Ship It".into(),
            tier: Tier::Gold,
            fallback: "Go.".into(),
            flavor: rpg_core::rules::Flavor::Static,
            minutes: 15,
            step_labels: vec!["push".into(), "pr_opened".into()],
            xp: 400,
            multiplier: 1.5,
            next_multiplier: 2.0,
            reward: 600,
            passes: 1,
            max_passes: 3,
            has_github_step: true,
            trigger: ev,
            binding: Default::default(),
            steps: vec![],
        };
        let mut p = Presenter::new();
        p.offer(9, offer);
        let b = p.next_banner().unwrap();
        let v = serde_json::to_value(payload(3, &b, 60)).unwrap();
        assert_eq!(v["kind"], "quest_offer");
        assert_eq!(v["tier"], "Gold");
        assert_eq!(
            v["quest"],
            serde_json::json!({
                "offer_id": 9, "tier_label": "Gold", "goal": "push → pr_opened", "minutes": 15, "xp": 400,
                "multiplier": 1.5, "next_multiplier": 2.0, "reward": 600, "passes_left": 2, "offer_seconds": 60
            })
        );
        let mut p = Presenter::new();
        p.notice(false, "x", "y");
        let v = serde_json::to_value(payload(4, &p.next_banner().unwrap(), 60)).unwrap();
        assert!(v["quest"].is_null(), "other banners carry no quest");
    }

    #[test]
    fn banner_window_leaves_room_for_the_glow() {
        let css = include_str!("../../../ui/overlay/overlay.css");
        assert!(css.contains(&format!("inset: {BANNER_INSET}px")), "overlay.css .ach-b inset != BANNER_INSET");
        // The banner frame itself stays 540x200, with room for the strongest glow (32px) around it.
        assert_eq!((BANNER_SIZE.0 - 2.0 * BANNER_INSET, BANNER_SIZE.1 - 2.0 * BANNER_INSET), (540.0, 200.0));
    }

    #[test]
    fn cursor_is_over_banner_only_inside_its_frame() {
        // Window at (100, 50) on a 150% display: the frame spans x 160..970, y 110..410.
        let over = |x, y| over_banner((x, y), (100.0, 50.0), 1.5);
        assert!(over(160.0, 110.0) && over(969.0, 409.0) && over(500.0, 250.0));
        assert!(!over(159.0, 200.0), "left glow margin");
        assert!(!over(500.0, 105.0), "top glow margin");
        assert!(!over(970.0, 200.0), "right glow margin");
        assert!(!over(500.0, 410.0), "bottom glow margin");
    }

    #[test]
    fn toast_window_stays_up_while_quest_active() {
        let t0 = Instant::now();
        let later = |d: Duration| t0 + d;
        assert!(!should_hide_toasts(true, t0, later(Duration::ZERO)));
        assert!(!should_hide_toasts(true, t0, later(TOAST_HIDE_AFTER)));
        assert!(!should_hide_toasts(true, t0, later(Duration::from_secs(3600))), "a quest pill keeps it up at any age");
        assert!(!should_hide_toasts(false, t0, later(TOAST_HIDE_AFTER - Duration::from_millis(1))));
        assert!(should_hide_toasts(false, t0, later(TOAST_HIDE_AFTER)));
        assert!(should_hide_toasts(false, t0, later(Duration::from_secs(60))));
        // A toast newer than `now` (clock skew between threads) never hides.
        assert!(!should_hide_toasts(false, later(Duration::from_secs(1)), t0));
    }

    #[test]
    fn cursor_over_pill() {
        // Window at (100, 50) on a 150% display; the pill at CSS (60, 350) 270x34
        // spans physical x 190..595, y 575..626.
        let rect = PillRect { x: 60.0, y: 350.0, w: 270.0, h: 34.0 };
        let over = |x, y| over_pill((x, y), (100.0, 50.0), 1.5, &rect);
        assert!(over(190.0, 575.0) && over(594.0, 625.0) && over(400.0, 600.0));
        assert!(!over(189.0, 600.0), "left of the pill");
        assert!(!over(595.0, 600.0), "right of the pill");
        assert!(!over(400.0, 574.0), "above the pill (the exp pills)");
        assert!(!over(400.0, 626.0), "below the pill");
    }

    #[test]
    fn quest_payload_carries_the_view() {
        use rpg_core::quests::state::{QuestStatus, StepView};
        let deadline = chrono::DateTime::from_timestamp_millis(1_800_000_000_123).unwrap();
        let view = QuestView {
            row_id: 7,
            quest_id: "ship_it".into(),
            name: "Ship It".into(),
            tier: Tier::Platinum,
            deadline,
            steps: vec![StepView { label: "Push".into(), have: 1, need: 1 }, StepView { label: "Open a PR".into(), have: 0, need: 1 }],
            reward: 1200,
            multiplier: 1.5,
            status: QuestStatus::Checking,
            has_github_step: true,
        };
        let v = serde_json::to_value(quest_payload(&view)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "row_id": 7, "status": "checking", "name": "Ship It", "tier_label": "Platinum",
                "deadline_ms": 1_800_000_000_123_i64, "reward": 1200, "multiplier": 1.5,
                "steps": [{ "label": "Push", "have": 1, "need": 1 }, { "label": "Open a PR", "have": 0, "need": 1 }]
            })
        );
        for (s, want) in [
            (QuestStatus::Active, "active"),
            (QuestStatus::Completed, "completed"),
            (QuestStatus::Failed, "failed"),
            (QuestStatus::Cancelled, "cancelled"),
        ] {
            assert_eq!(quest_payload(&QuestView { status: s, ..view.clone() }).status, want);
        }
    }

    #[test]
    fn reduced_motion_stops_the_completed_pill_sweep() {
        // The override must match the base rule's specificity (`:is()` counts its argument).
        let css = include_str!("../../../ui/overlay/overlay.css");
        let reduced = &css[css.find("prefers-reduced-motion").unwrap()..];
        let sel = r#".qp.won:is([data-tier="Platinum"], [data-tier="Legendary"], [data-tier="Celestial"])::after"#;
        assert!(css[..css.len() - reduced.len()].contains(sel), "base sweep rule moved");
        assert!(reduced.contains(&format!("{sel} {{ animation: none; }}")), "reduced motion leaves the completed pill sweeping");
    }

    #[test]
    fn pill_rect_report_may_be_null() {
        assert_eq!(serde_json::from_str::<Option<PillRect>>("null").unwrap(), None);
        let r = serde_json::from_str::<Option<PillRect>>(r#"{"x":1,"y":2.5,"w":3,"h":4}"#).unwrap();
        assert_eq!(r, Some(PillRect { x: 1.0, y: 2.5, w: 3.0, h: 4.0 }));
    }
}
