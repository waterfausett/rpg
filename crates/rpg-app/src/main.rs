// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod overlay;
mod platform;
mod presenter;
mod runtime;
mod sound;
mod tray;

use overlay::OverlaySink;
use rpg_core::config::Config;
use rpg_core::flavor::{ClaudeCli, TextGen};
use rpg_core::paths::Paths;
use rpg_core::store::Store;
use runtime::{log, AppState, OutcomeSink};
use std::sync::{Arc, RwLock};
use tauri::Manager;
use tray::Toggles;

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
            let toggles = Arc::new(Toggles::default());
            let overlay = Arc::new(OverlaySink::new(app.handle(), state.clone(), toggles.clone())?);
            app.manage(overlay.clone());
            tray::setup(app, &state, toggles)?;
            runtime::spawn(state, overlay as Arc<dyn OutcomeSink>);
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
