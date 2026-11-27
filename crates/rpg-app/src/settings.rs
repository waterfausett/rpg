//! The Settings window.
use crate::runtime::log;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Show the Settings window, creating it on first use.
pub fn open(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings/index.html".into()))
        .title("rpg · Settings")
        .inner_size(720.0, 560.0)
        .min_inner_size(600.0, 440.0)
        .resizable(true)
        .visible(true)
        .focused(true)
        .build();
    if let Err(e) = built {
        if let Ok(paths) = rpg_core::paths::Paths::resolve() {
            log(&paths, &format!("settings window failed: {e}"));
        }
    }
}
