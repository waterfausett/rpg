//! Tray icon and menu.
use crate::overlay::OverlaySink;
use crate::runtime::{log, AppState};
use crate::update::{self, Updates};
use rpg_core::install;
use rpg_core::paths::Paths;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

/// User toggles from the tray (not persisted). The overlay reads `muted` when
/// it plays a sound; `dnd` is pushed to the overlay when toggled.
#[derive(Default)]
pub struct Toggles {
    pub muted: AtomicBool,
    pub dnd: AtomicBool,
}

const OPEN_SHEET: &str = "open_sheet";
const MUTE: &str = "mute";
const DND: &str = "dnd";
const AUTOSTART: &str = "autostart";
const OPEN_RULES: &str = "open_rules";
const INSTALL: &str = "install_hooks";
const UNINSTALL: &str = "uninstall_hooks";
const QUIT: &str = "quit";

pub fn build(app: &AppHandle, state: &AppState, toggles: Arc<Toggles>) -> tauri::Result<()> {
    let mute = CheckMenuItem::with_id(app, MUTE, "Mute sounds", true, false, None::<&str>)?;
    let dnd = CheckMenuItem::with_id(app, DND, "Do not disturb", true, false, None::<&str>)?;
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
            &PredefinedMenuItem::separator(app)?,
            &mute,
            &dnd,
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
            MUTE => toggles.muted.store(mute.is_checked().unwrap_or(false), Ordering::Relaxed),
            DND => {
                let on = dnd.is_checked().unwrap_or(false);
                toggles.dnd.store(on, Ordering::Relaxed);
                if let Some(overlay) = app.try_state::<Arc<OverlaySink>>() {
                    overlay.set_dnd(on);
                }
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
    use super::*;

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
