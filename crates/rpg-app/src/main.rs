// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config_store;
mod overlay;
mod platform;
mod presenter;
mod runtime;
mod settings;
mod site;
mod sound;
mod sound_slots;
mod tray;
mod update;

use config_store::ConfigStore;
use overlay::OverlaySink;
use rpg_core::config::Config;
use rpg_core::engine::Outcome;
use rpg_core::event::Event;
use rpg_core::flavor::TextGen;
use rpg_core::paths::Paths;
use rpg_core::store::Store;
use runtime::{log, AppState, OutcomeSink};
use std::sync::{Arc, Mutex, RwLock};
use tauri::Manager;
use tray::Toggles;

/// Keeps the config file watcher alive for the life of the app.
#[allow(dead_code)]
struct ConfigWatcher(Mutex<Option<notify::RecommendedWatcher>>);

/// Fallback sink when the overlay windows can't be created: records batches in the log.
struct LogSink(Paths);

impl OutcomeSink for LogSink {
    fn present(&self, batch: Vec<(Event, Outcome)>, away: bool) {
        let xp: u64 = batch.iter().flat_map(|(_, o)| &o.xp_gains).map(|g| g.amount).sum();
        let unlocks: Vec<&str> = batch.iter().flat_map(|(_, o)| &o.unlocks).map(|u| u.name.as_str()).collect();
        log(&self.0, &format!("presented {} events (away={away}): +{xp} exp, unlocks {unlocks:?}", batch.len()));
    }
}

fn init_state() -> anyhow::Result<AppState> {
    let paths = Paths::resolve()?;
    paths.ensure()?;
    let cfg = Config::load(&paths.config_file()).unwrap_or_else(|e| {
        log(&paths, &format!("config invalid, using defaults: {e:#}"));
        Config::default()
    });
    let store = Store::open(&paths.db())?;
    let rules = runtime::load_rules(&paths);
    let gen: Option<Arc<dyn TextGen>> = runtime::make_gen(&cfg.llm, &paths);
    log(&paths, &format!("starting: {} rules, llm flavor {}", rules.list.len(), if gen.is_some() { "on" } else { "off" }));
    Ok(AppState {
        paths,
        store: Arc::new(store),
        rules: Arc::new(RwLock::new(rules)),
        cfg: Arc::new(RwLock::new(cfg)),
        gen: Arc::new(RwLock::new(gen)),
    })
}

fn main() {
    // Installed hooks call this binary as `rpg-app emit …` (#12): record the event and
    // exit before any Tauri setup, so the single-instance plugin never sees these calls.
    if rpg_core::cli::is_emit(std::env::args_os()) {
        rpg_core::cli::run(std::env::args_os());
        return;
    }
    let app = tauri::Builder::default()
        // Must be the first plugin: a second launch just exits.
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            settings::settings_get,
            settings::settings_set,
            settings::settings_choose_sound,
            settings::settings_add_folder,
            settings::settings_preview_sound,
            settings::settings_open_sounds_folder,
            settings::settings_open_config_file,
        ])
        .setup(|app| {
            platform::configure_app(app);
            let state = init_state().map_err(|e| {
                if let Ok(p) = Paths::resolve() {
                    log(&p, &format!("startup failed: {e:#}"));
                }
                e
            })?;
            let toggles = Arc::new(Toggles::from_config(&state.cfg.read().unwrap_or_else(|p| p.into_inner())));
            // Without an overlay, XP still counts: fall back to logging batches.
            let sink: Arc<dyn OutcomeSink> = match OverlaySink::new(app.handle(), state.clone(), toggles.clone()) {
                Ok(overlay) => {
                    let overlay = Arc::new(overlay);
                    app.manage(overlay.clone());
                    overlay
                }
                Err(e) => {
                    log(&state.paths, &format!("overlay unavailable, logging outcomes only: {e:#}"));
                    Arc::new(LogSink(state.paths.clone()))
                }
            };
            // A busy port only costs us the site, never the app.
            let site = site::SiteState { store: state.store.clone(), rules: state.rules.clone(), cfg: state.cfg.clone(), port: 0 };
            let port = state.cfg.read().unwrap_or_else(|p| p.into_inner()).site.port;
            let site_paths = state.paths.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = site::serve(site, port).await {
                    log(&site_paths, &format!("character sheet unavailable on 127.0.0.1:{port}: {e}"));
                }
            });
            tray::setup(app, &state, toggles.clone())?;
            // Live settings: the tray and the settings commands fetch it with `app.state::<Arc<ConfigStore>>()`.
            let store = ConfigStore::new(state.paths.clone(), state.cfg.clone());
            app.manage(store.clone());
            app.manage(ConfigWatcher(Mutex::new(store.watch())));
            let gen_state = state.clone();
            let poll_wake = runtime::spawn(state, sink);
            let handle = app.handle().clone();
            // A hand edit (valid or not): an open settings window re-reads its fields.
            let external = handle.clone();
            store.on_external(move || tray::notify_settings_changed(&external));
            // Runs under the save lock of the store: only cheap, non-blocking work here.
            store.on_change(move |old, new| {
                if old.sound.muted != new.sound.muted || old.announce.dnd != new.announce.dnd {
                    tray::sync_toggles(&handle, &toggles, new);
                    tray::notify_settings_changed(&handle);
                }
                if old.llm.enabled != new.llm.enabled || old.llm.model != new.llm.model {
                    let g = runtime::make_gen(&new.llm, &gen_state.paths);
                    *gen_state.gen.write().unwrap_or_else(|p| p.into_inner()) = g;
                }
                if old.github.enabled != new.github.enabled || old.github.poll_minutes != new.github.poll_minutes {
                    let _ = poll_wake.send(());
                }
                if old.updates.check_hours != new.updates.check_hours {
                    if let Some(updates) = handle.try_state::<Arc<update::Updates>>() {
                        updates.settings_changed();
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building tauri app");

    app.run(|_app, event| {
        // Overlay windows only: keep running until Quit (which exits with an explicit code).
        if let tauri::RunEvent::ExitRequested { code: None, api, .. } = event {
            api.prevent_exit();
        }
    });
}
