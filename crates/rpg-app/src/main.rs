// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod platform;
mod runtime;
mod tray;

use rpg_core::config::Config;
use rpg_core::engine::Outcome;
use rpg_core::event::Event;
use rpg_core::flavor::{ClaudeCli, TextGen};
use rpg_core::paths::Paths;
use rpg_core::store::Store;
use runtime::{log, AppState, OutcomeSink};
use std::sync::{Arc, RwLock};

/// Placeholder sink until the overlay (Task 10): records batches in the log.
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
    let app = tauri::Builder::default()
        // Must be the first plugin: a second launch just exits.
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .setup(|app| {
            platform::configure_app(app);
            let state = init_state().map_err(|e| {
                if let Ok(p) = Paths::resolve() {
                    log(&p, &format!("startup failed: {e:#}"));
                }
                e
            })?;
            tray::setup(app, &state)?;
            let sink: Arc<dyn OutcomeSink> = Arc::new(LogSink(state.paths.clone()));
            runtime::spawn(state, sink);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building tauri app");

    app.run(|_app, event| {
        // No windows: keep running until Quit (which exits with an explicit code).
        if let tauri::RunEvent::ExitRequested { code: None, api, .. } = event {
            api.prevent_exit();
        }
    });
}
