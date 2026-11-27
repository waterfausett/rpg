//! Self-update from GitHub Releases via `tauri-plugin-updater`.
//!
//! CI publishes a signed installer plus `latest.json` for each `v*` tag (see
//! `.github/workflows/release.yml`). We check that manifest in the background and from the
//! tray. A found update turns the tray item into "Install update vX" and shows a notice; it
//! only installs when the user clicks it. On Windows the installer runs in passive mode,
//! this process exits, and the installer starts the new version.
use crate::overlay::OverlaySink;
use crate::runtime::{log, AppState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};

pub const MENU_ID: &str = "update";
pub const CHECK_LABEL: &str = "Check for updates";

/// Delay before the first background check, so startup stays quick.
const FIRST_CHECK: Duration = Duration::from_secs(60);

pub struct Updates {
    state: AppState,
    item: MenuItem<Wry>,
    pending: Mutex<Option<Update>>,
    busy: AtomicBool,
    /// Wakes the background loop when `updates.check_hours` changes.
    changed: tokio::sync::Notify,
}

/// Background check interval: `None` when off (0 hours) or in debug builds, which would
/// otherwise offer to replace the installed app with a release.
pub fn background_interval(hours: u64, debug: bool) -> Option<Duration> {
    (hours > 0 && !debug).then(|| Duration::from_secs(hours * 3600))
}

fn install_label(version: &str) -> String {
    format!("Install update v{version}")
}

/// Notify for a background find only when it's a version we haven't announced yet.
fn is_new_find(announced: Option<&str>, found: &str) -> bool {
    announced != Some(found)
}

impl Updates {
    pub fn new(state: AppState, item: MenuItem<Wry>) -> Arc<Self> {
        Arc::new(Updates { state, item, pending: Mutex::new(None), busy: AtomicBool::new(false), changed: tokio::sync::Notify::new() })
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Option<Update>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn notice(&self, app: &AppHandle, ok: bool, title: &str, detail: &str) {
        if let Some(overlay) = app.try_state::<Arc<OverlaySink>>() {
            overlay.notice(ok, title, detail);
        }
    }

    /// Tray click: install a found update, otherwise check now (and say what we found).
    pub fn on_click(self: &Arc<Self>, app: &AppHandle) {
        let found = self.pending().clone();
        let me = self.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            match found {
                Some(update) => me.install(&app, update).await,
                None => me.check(&app, true).await,
            }
        });
    }

    /// The settings changed: have the background loop re-read `updates.check_hours`.
    pub fn settings_changed(&self) {
        self.changed.notify_one();
    }

    /// Background checks every `updates.check_hours` (0 = off), re-read every cycle and
    /// whenever `settings_changed` fires.
    pub fn spawn_background(self: &Arc<Self>, app: &AppHandle) {
        let me = self.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut first = true;
            loop {
                let hours = me.state.cfg.read().unwrap_or_else(|p| p.into_inner()).updates.check_hours;
                let Some(every) = background_interval(hours, cfg!(debug_assertions)) else {
                    me.changed.notified().await;
                    first = true;
                    continue;
                };
                let wait = if first { FIRST_CHECK } else { every };
                tokio::select! {
                    _ = me.changed.notified() => {
                        first = true;
                        continue;
                    }
                    _ = tokio::time::sleep(wait) => {}
                }
                me.check(&app, false).await;
                first = false;
            }
        });
    }

    async fn check(&self, app: &AppHandle, manual: bool) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let result = match app.updater() {
            Ok(u) => u.check().await.map_err(anyhow::Error::from),
            Err(e) => Err(e.into()),
        };
        self.busy.store(false, Ordering::SeqCst);
        let paths = &self.state.paths;
        match result {
            Ok(Some(update)) => {
                let version = update.version.clone();
                let announced = self.pending().as_ref().map(|u| u.version.clone());
                *self.pending() = Some(update);
                if let Err(e) = self.item.set_text(install_label(&version)) {
                    log(paths, &format!("update menu label: {e}"));
                }
                if manual || is_new_find(announced.as_deref(), &version) {
                    log(paths, &format!("update available: v{version}"));
                    self.notice(app, true, &format!("Update available: rpg v{version}"), "Tray → Install update");
                }
            }
            Ok(None) => {
                if manual {
                    let current = app.package_info().version.to_string();
                    self.notice(app, true, "rpg is up to date", &format!("v{current}"));
                }
            }
            Err(e) => {
                log(paths, &format!("update check failed: {e:#}"));
                if manual {
                    self.notice(app, false, "Update check failed", "details in rpg.log");
                }
            }
        }
    }

    async fn install(&self, app: &AppHandle, update: Update) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let paths = &self.state.paths;
        log(paths, &format!("installing update v{}", update.version));
        let _ = self.item.set_enabled(false);
        let _ = self.item.set_text(format!("Installing v{}…", update.version));
        // On Windows this exits the process once the installer starts; the installer
        // relaunches the app. Elsewhere we restart ourselves.
        match update.download_and_install(|_, _| {}, || {}).await {
            Ok(()) => app.restart(),
            Err(e) => {
                log(paths, &format!("update install failed: {e:#}"));
                let _ = self.item.set_enabled(true);
                let _ = self.item.set_text(install_label(&update.version));
                self.busy.store(false, Ordering::SeqCst);
                self.notice(app, false, "Update failed", "details in rpg.log");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_interval_rules() {
        assert_eq!(background_interval(0, false), None);
        assert_eq!(background_interval(6, true), None);
        assert_eq!(background_interval(6, false), Some(Duration::from_secs(6 * 3600)));
    }

    #[test]
    fn labels_and_announcements() {
        assert_eq!(install_label("0.2.0"), "Install update v0.2.0");
        assert!(is_new_find(None, "0.2.0"));
        assert!(!is_new_find(Some("0.2.0"), "0.2.0"));
        assert!(is_new_find(Some("0.2.0"), "0.3.0"));
    }
}
