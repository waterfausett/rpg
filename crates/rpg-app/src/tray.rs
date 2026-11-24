//! Tray icon and menu.
use crate::overlay::OverlaySink;
use crate::runtime::{log, AppState};
use rpg_core::install;
use rpg_core::paths::Paths;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

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
const OPEN_RULES: &str = "open_rules";
const INSTALL: &str = "install_hooks";
const UNINSTALL: &str = "uninstall_hooks";
const QUIT: &str = "quit";

pub fn build(app: &AppHandle, state: &AppState, toggles: Arc<Toggles>) -> tauri::Result<()> {
    let mute = CheckMenuItem::with_id(app, MUTE, "Mute sounds", true, false, None::<&str>)?;
    let dnd = CheckMenuItem::with_id(app, DND, "Do not disturb", true, false, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, OPEN_SHEET, "Open character sheet", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &mute,
            &dnd,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, OPEN_RULES, "Open rules folder", true, None::<&str>)?,
            &MenuItem::with_id(app, INSTALL, "Install hooks", true, None::<&str>)?,
            &MenuItem::with_id(app, UNINSTALL, "Uninstall hooks", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
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
            OPEN_RULES => {
                if let Err(e) = tauri_plugin_opener::open_path(paths.rules(), None::<&str>) {
                    log(paths, &format!("open rules folder failed: {e}"));
                }
            }
            INSTALL => run_installer(paths.clone(), true),
            UNINSTALL => run_installer(paths.clone(), false),
            QUIT => app.exit(0),
            _ => {}
        }
    })
    .build(app)?;
    Ok(())
}

/// The `rpg` CLI next to this executable; hook shims call it.
fn sibling_rpg_bin() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let bin = exe.with_file_name(format!("rpg{}", std::env::consts::EXE_SUFFIX));
    anyhow::ensure!(bin.is_file(), "rpg CLI not found at {}", bin.display());
    Ok(bin)
}

/// Install or uninstall git + Claude Code hooks off the UI thread, logging each step
/// (same steps as `rpg install-hooks` / `rpg uninstall-hooks`).
fn run_installer(paths: Paths, install_hooks: bool) {
    std::thread::spawn(move || {
        let git = install::GitEnv::inherit();
        let steps: [(&str, anyhow::Result<()>); 2] = if install_hooks {
            match sibling_rpg_bin() {
                Ok(bin) => [
                    ("install git hooks", install::install_git_hooks(&paths, &bin, &git)),
                    (
                        "install claude hooks",
                        install::claude_settings_path().and_then(|s| install::install_claude_hooks(&s, &bin)),
                    ),
                ],
                Err(e) => {
                    log(&paths, &format!("install hooks: {e:#}"));
                    return;
                }
            }
        } else {
            [
                ("uninstall git hooks", install::uninstall_git_hooks(&paths, &git)),
                (
                    "uninstall claude hooks",
                    install::claude_settings_path().and_then(|s| install::uninstall_claude_hooks(&s)),
                ),
            ]
        };
        for (step, res) in steps {
            match res {
                Ok(()) => log(&paths, &format!("{step}: ok")),
                Err(e) => log(&paths, &format!("{step}: FAILED: {e:#}")),
            }
        }
    });
}

/// Build the tray around the shared toggles.
pub fn setup(app: &tauri::App, state: &AppState, toggles: Arc<Toggles>) -> tauri::Result<()> {
    build(app.handle(), state, toggles)
}
