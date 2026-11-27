// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(dead_code)]
mod config_store;
mod overlay;
mod platform;
mod presenter;
mod runtime;
mod site;
mod sound;
mod tray;
mod update;

use overlay::OverlaySink;
use rpg_core::config::Config;
use rpg_core::engine::Outcome;
use rpg_core::event::Event;
use rpg_core::flavor::{ClaudeCli, TextGen};
use rpg_core::paths::Paths;
use rpg_core::store::Store;
use runtime::{log, AppState, OutcomeSink};
use std::sync::{Arc, RwLock};
use tauri::Manager;
use tray::Toggles;

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
    let gen: Option<Arc<dyn TextGen>> = (cfg.llm.enabled && ClaudeCli::available()).then(|| {
        Arc::new(ClaudeCli { model: cfg.llm.model.clone(), workdir: paths.data.join("llm-sandbox") }) as Arc<dyn TextGen>
    });
    log(&paths, &format!("starting: {} rules, llm flavor {}", rules.list.len(), if gen.is_some() { "on" } else { "off" }));
    Ok(AppState {
        paths,
        store: Arc::new(store),
        rules: Arc::new(RwLock::new(rules)),
        cfg: Arc::new(RwLock::new(cfg)),
        gen,
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
        .setup(|app| {
            platform::configure_app(app);
            let state = init_state().map_err(|e| {
                if let Ok(p) = Paths::resolve() {
                    log(&p, &format!("startup failed: {e:#}"));
                }
                e
            })?;
            let toggles = Arc::new(Toggles::default());
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
            tray::setup(app, &state, toggles)?;
            runtime::spawn(state, sink);
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
