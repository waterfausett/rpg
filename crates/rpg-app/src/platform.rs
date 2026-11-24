//! Platform-specific app tweaks (the only place for them besides `rpg_core::proc`).

/// Tray-only app: on macOS hide the Dock icon. Nothing to do elsewhere.
#[allow(unused_variables)]
pub fn configure_app(app: &mut tauri::App) {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}
