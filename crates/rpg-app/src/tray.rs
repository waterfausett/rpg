//! Tray icon and menu.
use crate::config_store::ConfigStore;
use crate::overlay::OverlaySink;
use crate::runtime::{log, AppState, Runtime};
use crate::update::{self, Updates};
use rpg_core::config::Config;
use rpg_core::config_edit::EditError;
use rpg_core::install;
use rpg_core::paths::Paths;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

/// User toggles from the tray, persisted as `sound.muted`, `announce.dnd` and
/// `quests.enabled` in the config. The overlay reads `muted` when it plays a sound;
/// `dnd` is pushed to the overlay and `quests` to the runtime when toggled.
#[derive(Default)]
pub struct Toggles {
    pub muted: AtomicBool,
    pub dnd: AtomicBool,
    pub quests: AtomicBool,
}

impl Toggles {
    pub fn from_config(cfg: &Config) -> Toggles {
        Toggles {
            muted: AtomicBool::new(cfg.sound.muted),
            dnd: AtomicBool::new(cfg.announce.dnd),
            quests: AtomicBool::new(cfg.quests.enabled),
        }
    }
}

/// Handles to the persisted check items, so config changes can update their check marks.
pub struct TrayChecks {
    mute: CheckMenuItem<tauri::Wry>,
    dnd: CheckMenuItem<tauri::Wry>,
    quests: CheckMenuItem<tauri::Wry>,
}

impl Toggles {
    /// True when the session toggles disagree with `cfg` (e.g. tray clicks made while the
    /// file was broken); the file's values win on reload, so the tray must be resynced.
    pub fn differ_from(&self, cfg: &Config) -> bool {
        self.muted.load(Ordering::Relaxed) != cfg.sound.muted
            || self.dnd.load(Ordering::Relaxed) != cfg.announce.dnd
            || self.quests.load(Ordering::Relaxed) != cfg.quests.enabled
    }
}

/// Push the config's Mute/DND/Quests state into the atomics, the menu check marks, the
/// overlay and the runtime. Safe to call under the config store's save lock: only the atomics
/// are set and the runtime messaged (a channel send) inline; the menu and overlay
/// (main-thread-bound, may block) are updated via `run_on_main_thread`.
pub fn sync_toggles(app: &AppHandle, toggles: &Toggles, cfg: &Config) {
    let (muted, dnd, quests) = (cfg.sound.muted, cfg.announce.dnd, cfg.quests.enabled);
    toggles.muted.store(muted, Ordering::Relaxed);
    toggles.dnd.store(dnd, Ordering::Relaxed);
    if toggles.quests.swap(quests, Ordering::Relaxed) != quests {
        if let Some(rt) = app.try_state::<Runtime>() {
            rt.set_quests_enabled(quests);
        }
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(checks) = handle.try_state::<TrayChecks>() {
            let _ = checks.mute.set_checked(muted);
            let _ = checks.dnd.set_checked(dnd);
            let _ = checks.quests.set_checked(quests);
        }
        if let Some(overlay) = handle.try_state::<Arc<OverlaySink>>() {
            overlay.set_dnd(dnd);
        }
    });
}

/// The notice to show when saving a toggle failed: `None` when it saved.
fn toggle_saved(res: Result<(), EditError>) -> Option<(String, String)> {
    res.err().map(|_| ("Couldn't save".to_string(), "config.toml has an error".to_string()))
}

/// Persist a toggle after its session effect was applied; report failure in the log and overlay.
fn save_toggle(app: &AppHandle, paths: &Paths, key: &str, on: bool) {
    let Some(store) = app.try_state::<Arc<ConfigStore>>() else { return };
    let res = store.set(key, &serde_json::Value::Bool(on));
    if let Err(e) = &res {
        log(paths, &format!("saving {key} failed: {e}"));
    }
    if let Some((title, detail)) = toggle_saved(res) {
        if let Some(overlay) = app.try_state::<Arc<OverlaySink>>() {
            overlay.notice(false, &title, &detail);
        }
    }
}

/// Tell the settings window (if open) that the config changed.
pub fn notify_settings_changed(app: &AppHandle) {
    let _ = app.emit_to("settings", "settings-changed", ());
}

const OPEN_SHEET: &str = "open_sheet";
const MUTE: &str = "mute";
const DND: &str = "dnd";
const QUESTS: &str = "quests";
const SETTINGS: &str = "settings";
const AUTOSTART: &str = "autostart";
const OPEN_RULES: &str = "open_rules";
const INSTALL: &str = "install_hooks";
const UNINSTALL: &str = "uninstall_hooks";
const QUIT: &str = "quit";

pub fn build(app: &AppHandle, state: &AppState, toggles: Arc<Toggles>) -> tauri::Result<()> {
    let muted = toggles.muted.load(Ordering::Relaxed);
    let dnd_on = toggles.dnd.load(Ordering::Relaxed);
    let quests_on = toggles.quests.load(Ordering::Relaxed);
    let mute = CheckMenuItem::with_id(app, MUTE, "Mute sounds", true, muted, None::<&str>)?;
    let dnd = CheckMenuItem::with_id(app, DND, "Do not disturb", true, dnd_on, None::<&str>)?;
    let quests = CheckMenuItem::with_id(app, QUESTS, "Quests", true, quests_on, None::<&str>)?;
    app.manage(TrayChecks { mute: mute.clone(), dnd: dnd.clone(), quests: quests.clone() });
    // The overlay exists by now: apply the persisted DND.
    if let Some(overlay) = app.try_state::<Arc<OverlaySink>>() {
        overlay.set_dnd(dnd_on);
    }
    // Unlike mute and DND, this lives in the OS (login items), so read it back from there.
    let autostart_on = app.autolaunch().is_enabled().unwrap_or_else(|e| {
        log(&state.paths, &format!("autostart: can't read login item: {e}"));
        false
    });
    let autostart = CheckMenuItem::with_id(app, AUTOSTART, "Start at login", true, autostart_on, None::<&str>)?;
    let update_item = MenuItem::with_id(app, update::MENU_ID, update::CHECK_LABEL, true, None::<&str>)?;
    let updates = Updates::new(state.clone(), update_item.clone());
    app.manage(updates.clone());
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, OPEN_SHEET, "Open character sheet", true, None::<&str>)?,
            &MenuItem::with_id(app, SETTINGS, "Settings…", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &mute,
            &dnd,
            &quests,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, OPEN_RULES, "Open rules folder", true, None::<&str>)?,
            &MenuItem::with_id(app, INSTALL, "Install hooks", true, None::<&str>)?,
            &MenuItem::with_id(app, UNINSTALL, "Uninstall hooks", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &update_item,
            &MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?,
        ],
    )?;

    let state = state.clone();
    let mut tray = TrayIconBuilder::with_id("rpg").tooltip("rpg").menu(&menu).show_menu_on_left_click(true);
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.on_menu_event(move |app, event| {
        let paths = &state.paths;
        match event.id.as_ref() {
            OPEN_SHEET => {
                let port = state.cfg.read().unwrap_or_else(|p| p.into_inner()).site.port;
                if let Err(e) = tauri_plugin_opener::open_url(format!("http://127.0.0.1:{port}"), None::<&str>) {
                    log(paths, &format!("open character sheet failed: {e}"));
                }
            }
            SETTINGS => crate::settings::open(app),
            MUTE => {
                let on = mute.is_checked().unwrap_or(false);
                toggles.muted.store(on, Ordering::Relaxed);
                save_toggle(app, paths, "sound.muted", on);
            }
            DND => {
                let on = dnd.is_checked().unwrap_or(false);
                toggles.dnd.store(on, Ordering::Relaxed);
                if let Some(overlay) = app.try_state::<Arc<OverlaySink>>() {
                    overlay.set_dnd(on);
                }
                save_toggle(app, paths, "announce.dnd", on);
            }
            QUESTS => {
                let on = quests.is_checked().unwrap_or(false);
                toggles.quests.store(on, Ordering::Relaxed);
                if let Some(rt) = app.try_state::<Runtime>() {
                    rt.set_quests_enabled(on);
                }
                save_toggle(app, paths, "quests.enabled", on);
            }
            AUTOSTART => set_autostart(app, paths, &autostart),
            OPEN_RULES => {
                if let Err(e) = tauri_plugin_opener::open_path(paths.rules(), None::<&str>) {
                    log(paths, &format!("open rules folder failed: {e}"));
                }
            }
            INSTALL => run_installer(app.clone(), paths.clone(), true),
            UNINSTALL => run_installer(app.clone(), paths.clone(), false),
            update::MENU_ID => updates.on_click(app),
            QUIT => app.exit(0),
            _ => {}
        }
    })
    .build(app)?;
    if let Some(updates) = app.try_state::<std::sync::Arc<Updates>>() {
        updates.spawn_background(app);
    }
    Ok(())
}

/// Add or remove the login item to match the check mark (the menu has already
/// flipped it). On failure, put the check mark back and say why.
fn set_autostart(app: &AppHandle, paths: &Paths, item: &CheckMenuItem<tauri::Wry>) {
    let on = item.is_checked().unwrap_or(false);
    let manager = app.autolaunch();
    let res = if on { manager.enable() } else { manager.disable() };
    let verb = if on { "enable" } else { "disable" };
    match res {
        Ok(()) => log(paths, &format!("autostart {verb}: ok")),
        Err(e) => {
            log(paths, &format!("autostart {verb}: FAILED: {e}"));
            let _ = item.set_checked(!on);
            if let Some(overlay) = app.try_state::<Arc<OverlaySink>>() {
                overlay.notice(false, &format!("Couldn't {verb} start at login"), &format!("{e} (details in rpg.log)"));
            }
        }
    }
}

/// Install or uninstall git + Claude Code hooks off the UI thread (same steps as
/// `rpg install-hooks` / `rpg uninstall-hooks`). The hooks call this executable
/// (`rpg-app emit …`, see `main`), so installers need no separate CLI (#12).
/// Each step is logged; the outcome is shown in the overlay.
fn run_installer(app: AppHandle, paths: Paths, install_hooks: bool) {
    std::thread::spawn(move || {
        let git = install::GitEnv::inherit();
        let steps: Vec<(&str, anyhow::Result<bool>)> = if install_hooks {
            match std::env::current_exe() {
                Ok(bin) => vec![
                    ("git hooks", install::install_git_hooks(&paths, &bin, &git).map(|()| true)),
                    (
                        "Claude Code hooks",
                        install::claude_settings_path()
                            .and_then(|s| install::install_claude_hooks(&s, &bin, paths.home.as_deref()))
                            .map(|()| true),
                    ),
                ],
                Err(e) => vec![("hooks", Err(anyhow::Error::from(e).context("cannot locate rpg-app")))],
            }
        } else {
            vec![
                ("git hooks", install::uninstall_git_hooks(&paths, &git)),
                (
                    "Claude Code hooks",
                    install::claude_settings_path().and_then(|s| install::uninstall_claude_hooks(&s)),
                ),
            ]
        };
        let verb = if install_hooks { "install" } else { "uninstall" };
        for (step, res) in &steps {
            match res {
                Ok(true) => log(&paths, &format!("{verb} {step}: ok")),
                Ok(false) => log(&paths, &format!("{verb} {step}: nothing to do")),
                Err(e) => log(&paths, &format!("{verb} {step}: FAILED: {e:#}")),
            }
        }
        let (ok, title, detail) = installer_notice(install_hooks, &steps);
        if let Some(overlay) = app.try_state::<Arc<OverlaySink>>() {
            overlay.notice(ok, &title, &detail);
        }
    });
}

/// What to tell the user about an install/uninstall: `(ok, title, detail)`. Each step is
/// `Ok(changed)`; an uninstall where nothing changed means the hooks weren't installed.
fn installer_notice(install_hooks: bool, steps: &[(&str, anyhow::Result<bool>)]) -> (bool, String, String) {
    let failed: Vec<String> = steps
        .iter()
        .filter_map(|(step, r)| r.as_ref().err().map(|e| format!("{step}: {e:#}")))
        .collect();
    let done = if install_hooks { "installed" } else { "removed" };
    if failed.is_empty() {
        let changed = steps.iter().any(|(_, r)| matches!(r, Ok(true)));
        let title = if changed { format!("Hooks {done}") } else { "Hooks weren't installed".into() };
        return (true, title, String::new());
    }
    let title = if failed.len() == steps.len() { format!("Hooks not {done}") } else { format!("Hooks partly {done}") };
    (false, title, format!("{} (details in rpg.log)", failed.join(" · ")))
}

/// Build the tray around the shared toggles.
pub fn setup(app: &tauri::App, state: &AppState, toggles: Arc<Toggles>) -> tauri::Result<()> {
    build(app.handle(), state, toggles)
}

#[cfg(test)]
mod tests {
    #[test]
    fn toggles_differ_even_when_config_did_not_change() {
        let cfg = Config::default();
        let t = Toggles::from_config(&cfg);
        assert!(!t.differ_from(&cfg));
        t.muted.store(true, Ordering::Relaxed);
        assert!(t.differ_from(&cfg));
        t.muted.store(false, Ordering::Relaxed);
        t.dnd.store(true, Ordering::Relaxed);
        assert!(t.differ_from(&cfg));
        t.dnd.store(false, Ordering::Relaxed);
        t.quests.store(!cfg.quests.enabled, Ordering::Relaxed);
        assert!(t.differ_from(&cfg));
    }

    use super::*;
    use rpg_core::config::Config;
    use rpg_core::config_edit::EditError;

    #[test]
    fn toggles_start_from_config() {
        let t = Toggles::from_config(&Config::parse("[sound]
muted=true
").unwrap());
        assert!(t.muted.load(Ordering::Relaxed));
        assert!(!t.dnd.load(Ordering::Relaxed));
        assert!(t.quests.load(Ordering::Relaxed), "quests on by default");
        let off = Toggles::from_config(&Config::parse("[quests]
enabled=false
").unwrap());
        assert!(!off.quests.load(Ordering::Relaxed));
    }

    #[test]
    fn toggle_save_failure_notice() {
        assert_eq!(toggle_saved(Ok(())), None);
        assert_eq!(
            toggle_saved(Err(EditError::InvalidFile("bad".into()))),
            Some(("Couldn't save".into(), "config.toml has an error".into()))
        );
    }

    #[test]
    fn notice_for_full_success() {
        let steps = [("git hooks", Ok(true)), ("Claude Code hooks", Ok(true))];
        assert_eq!(installer_notice(true, &steps), (true, "Hooks installed".into(), String::new()));
        assert_eq!(installer_notice(false, &steps).1, "Hooks removed");
        let partial = [("git hooks", Ok(false)), ("Claude Code hooks", Ok(true))];
        assert_eq!(installer_notice(false, &partial).1, "Hooks removed");
    }

    #[test]
    fn uninstall_with_nothing_installed_says_so() {
        let steps = [("git hooks", Ok(false)), ("Claude Code hooks", Ok(false))];
        assert_eq!(installer_notice(false, &steps), (true, "Hooks weren't installed".into(), String::new()));
    }

    #[test]
    fn notice_names_each_failed_step() {
        let steps = [("git hooks", Err(anyhow::anyhow!("not installed: includeIf"))), ("Claude Code hooks", Ok(true))];
        let (ok, title, detail) = installer_notice(true, &steps);
        assert!(!ok);
        assert_eq!(title, "Hooks partly installed");
        assert_eq!(detail, "git hooks: not installed: includeIf (details in rpg.log)");
        let all = [("git hooks", Err(anyhow::anyhow!("a"))), ("Claude Code hooks", Err(anyhow::anyhow!("b")))];
        let (_, title, detail) = installer_notice(false, &all);
        assert_eq!(title, "Hooks not removed");
        assert_eq!(detail, "git hooks: a · Claude Code hooks: b (details in rpg.log)");
    }
}
