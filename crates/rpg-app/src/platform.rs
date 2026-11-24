//! Platform-specific app tweaks (the only place for them besides `rpg_core::proc`).

/// Tray-only app: on macOS hide the Dock icon. Nothing to do elsewhere.
#[allow(unused_variables)]
pub fn configure_app(app: &mut tauri::App) {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

/// Make an overlay window never take focus: showing or clicking it leaves the
/// foreground window (e.g. the terminal) active. Windows: `WS_EX_NOACTIVATE`.
/// macOS: stub for now (#4).
#[allow(unused_variables)]
pub fn make_non_activating(window: &tauri::WebviewWindow) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        };
        let hwnd = window.hwnd()?.0 as windows_sys::Win32::Foundation::HWND;
        // SAFETY: `hwnd` is the live window handle Tauri just gave us; these calls only
        // read and update its extended style bits.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize);
        }
    }
    Ok(())
}
