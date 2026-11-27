//! The Settings window: its window, the snapshot it renders, and the IPC commands it calls.
//! Every command is async and does its work in `spawn_blocking`, so none runs on the main
//! thread (config listeners may need it) or blocks the async runtime.
use crate::config_store::ConfigStore;
use crate::runtime::log;
use crate::sound::{collect_files, preview, resolve_sound};
use crate::sound_slots::{copy_into, fallback, pin_exists, Slot};
use rpg_core::config::{escape_folder, raw_paths, Config};
use rpg_core::config_edit::EditError;
use rpg_core::paths::Paths;
use rpg_core::rules::{Announce, Tier};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;

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

const TIERS: [Tier; 6] = [Tier::Bronze, Tier::Silver, Tier::Gold, Tier::Platinum, Tier::Legendary, Tier::Celestial];

fn announce(a: Announce) -> &'static str {
    match a {
        Announce::Banner => "banner",
        Announce::Toast => "toast",
    }
}

fn label(slot: Slot) -> &'static str {
    match slot {
        Slot::Achievement => "Achievements",
        Slot::LevelUp => "Level-ups",
        Slot::Tier(Tier::Bronze) => "Bronze",
        Slot::Tier(Tier::Silver) => "Silver",
        Slot::Tier(Tier::Gold) => "Gold",
        Slot::Tier(Tier::Platinum) => "Platinum",
        Slot::Tier(Tier::Legendary) => "Legendary",
        Slot::Tier(Tier::Celestial) => "Celestial",
    }
}

/// What the window renders, from the config file's text (`None` = missing, so defaults)
/// and every file under the sounds folder. A file that doesn't parse yields only `invalid`.
pub fn snapshot(text: Option<&str>, sounds_root: &Path, files: &[PathBuf]) -> Value {
    let cfg = match text.map(Config::parse) {
        None => Config::default(),
        Some(Ok(c)) => c,
        Some(Err(e)) => return json!({ "invalid": format!("{e:#}") }),
    };
    let (include, exclude) = text.map(raw_paths).unwrap_or_default();
    let a = &cfg.announce;
    let tiers: serde_json::Map<String, Value> =
        TIERS.iter().map(|t| (t.as_str().to_string(), json!(announce(a.for_tier(*t))))).collect();
    let slots: Vec<Value> = Slot::all()
        .into_iter()
        .map(|slot| {
            let pinned = slot.pinned(&cfg.sound);
            json!({
                "id": slot.id(),
                "label": label(slot),
                "pinned": pinned,
                "exists": pinned.is_some_and(|n| pin_exists(slot, n, sounds_root, files)),
                "fallback": fallback(slot, &cfg.sound, sounds_root, files),
            })
        })
        .collect();
    // f32 -> f64 without the float noise (0.8 stays 0.8).
    let volume = (f64::from(cfg.sound.volume) * 1e4).round() / 1e4;
    json!({
        "invalid": null,
        "notifications": {
            "dnd": a.dnd,
            "first_banner": a.first_banner,
            "all": a.all.map(announce),
            "tiers": tiers,
        },
        "sounds": { "muted": cfg.sound.muted, "volume": volume, "slots": slots },
        "integrations": {
            "github": { "enabled": cfg.github.enabled, "poll_minutes": cfg.github.poll_minutes },
            "llm": { "enabled": cfg.llm.enabled, "model": cfg.llm.model, "timeout_secs": cfg.llm.timeout_secs },
            "updates": { "check_hours": cfg.updates.check_hours },
        },
        "repos": { "include": include, "exclude": exclude },
    })
}

fn read_config_text(paths: &Paths) -> Result<Option<String>, String> {
    match std::fs::read_to_string(paths.config_file()) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(EditError::Io(e.to_string()).to_string()),
    }
}

fn sound_files(paths: &Paths) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(&paths.sounds(), 2, &mut files);
    files
}

fn current_snapshot(store: &ConfigStore) -> Result<Value, String> {
    let paths = store.paths();
    let text = read_config_text(paths)?;
    Ok(snapshot(text.as_deref(), &paths.sounds(), &sound_files(paths)))
}

/// Refuse up front when the file doesn't parse, before any dialog or copy.
fn ensure_valid(store: &ConfigStore) -> Result<(), String> {
    match read_config_text(store.paths())?.as_deref().map(Config::parse) {
        Some(Err(e)) => Err(EditError::InvalidFile(format!("{e:#}")).to_string()),
        _ => Ok(()),
    }
}

fn parse_slot(slot: &str) -> Result<Slot, String> {
    Slot::parse(slot).ok_or_else(|| format!("unknown sound: {slot}"))
}

/// Run blocking work (dialogs, file copies, saves, audio) off the main thread and async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn settings_get(store: State<'_, Arc<ConfigStore>>) -> Result<Value, String> {
    let store = store.inner().clone();
    blocking(move || current_snapshot(&store)).await
}

#[tauri::command]
pub async fn settings_set(store: State<'_, Arc<ConfigStore>>, key: String, value: Value) -> Result<Value, String> {
    let store = store.inner().clone();
    blocking(move || {
        store.set(&key, &value).map_err(|e| e.to_string())?;
        current_snapshot(&store)
    })
    .await
}

#[tauri::command]
pub async fn settings_choose_sound(window: WebviewWindow, store: State<'_, Arc<ConfigStore>>, slot: String) -> Result<Value, String> {
    let slot = parse_slot(&slot)?;
    let store = store.inner().clone();
    blocking(move || {
        ensure_valid(&store)?;
        let picked = window
            .dialog()
            .file()
            .set_parent(&window)
            .add_filter("Audio", &["wav", "mp3", "ogg", "flac"])
            .blocking_pick_file();
        let Some(picked) = picked else { return current_snapshot(&store) };
        let src = picked.into_path().map_err(|e| e.to_string())?;
        let sounds = store.paths().sounds();
        let copied = copy_into(slot, &src, &sounds).map_err(|e| format!("could not copy the sound: {e}"))?;
        if let Err(e) = store.set(&slot.config_key(), &Value::String(copied.name.clone())) {
            // Don't leave a rejected copy behind to join the random pool. Only a file this call
            // wrote is removed; a reused file was already the user's.
            if copied.created {
                let _ = std::fs::remove_file(slot.folder(&sounds).join(&copied.name));
            }
            return Err(e.to_string());
        }
        current_snapshot(&store)
    })
    .await
}

#[tauri::command]
pub async fn settings_add_folder(window: WebviewWindow, store: State<'_, Arc<ConfigStore>>, list: String) -> Result<Value, String> {
    if list != "include" && list != "exclude" {
        return Err(format!("unknown list: {list}"));
    }
    let store = store.inner().clone();
    blocking(move || {
        ensure_valid(&store)?;
        let picked = window.dialog().file().set_parent(&window).blocking_pick_folder();
        let Some(picked) = picked else { return current_snapshot(&store) };
        // Escaped so a folder named like `[old]` is matched as itself, not read as a glob.
        let folder = escape_folder(&picked.into_path().map_err(|e| e.to_string())?.to_string_lossy());
        // Read the list after the dialog closes: the file may have changed while it was open.
        let text = read_config_text(store.paths())?;
        let (include, exclude) = text.as_deref().map(raw_paths).unwrap_or_default();
        let mut entries = if list == "include" { include } else { exclude };
        if !entries.contains(&folder) {
            entries.push(folder);
            store.set(&format!("paths.{list}"), &json!(entries)).map_err(|e| e.to_string())?;
        }
        current_snapshot(&store)
    })
    .await
}

/// Play what the slot would play, at the current volume, even while muted.
#[tauri::command]
pub async fn settings_preview_sound(store: State<'_, Arc<ConfigStore>>, slot: String) -> Result<Value, String> {
    let slot = parse_slot(&slot)?;
    let store = store.inner().clone();
    blocking(move || {
        let cfg = store.current().sound;
        let files = sound_files(store.paths());
        let file = resolve_sound(&slot.sound_key(), &cfg, &store.paths().sounds(), &files)
            .ok_or_else(|| "Nothing to play for this sound".to_string())?;
        preview(&file, cfg.volume).map_err(|e| format!("{e:#}"))?;
        Ok(Value::Null)
    })
    .await
}

#[tauri::command]
pub async fn settings_open_sounds_folder(store: State<'_, Arc<ConfigStore>>) -> Result<Value, String> {
    let dir = store.paths().sounds();
    blocking(move || {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(|e| e.to_string())?;
        Ok(Value::Null)
    })
    .await
}

#[tauri::command]
pub async fn settings_open_config_file(store: State<'_, Arc<ConfigStore>>) -> Result<Value, String> {
    let paths = store.paths().clone();
    blocking(move || {
        // A missing file is recreated from the commented default, as at startup.
        if !paths.config_file().exists() {
            paths.ensure().map_err(|e| format!("{e:#}"))?;
        }
        tauri_plugin_opener::open_path(paths.config_file(), None::<&str>).map_err(|e| e.to_string())?;
        Ok(Value::Null)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const COMMANDS: [&str; 7] = [
        "settings_get",
        "settings_set",
        "settings_choose_sound",
        "settings_add_folder",
        "settings_preview_sound",
        "settings_open_sounds_folder",
        "settings_open_config_file",
    ];

    fn slot<'a>(s: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
        s["sounds"]["slots"].as_array().unwrap().iter().find(|x| x["id"] == id).unwrap()
    }

    #[test]
    fn snapshot_of_missing_file_is_defaults() {
        let s = snapshot(None, Path::new("/s"), &[]);
        assert!(s["invalid"].is_null(), "{s}");
        assert_eq!(s["sounds"]["volume"], 0.8);
        assert_eq!(s["sounds"]["slots"].as_array().unwrap().len(), 8);
        assert_eq!(slot(&s, "tier:silver")["fallback"], "none");
        assert_eq!(slot(&s, "tier:silver")["label"], "Silver");
        assert_eq!(s["notifications"]["tiers"]["bronze"], "toast");
    }

    #[test]
    fn snapshot_of_broken_file_reports_error() {
        let s = snapshot(Some("[sound\n"), Path::new("/s"), &[]);
        assert!(s["invalid"].as_str().unwrap().contains("line"), "{s}");
    }

    #[test]
    fn snapshot_shows_raw_paths_and_missing_pins() {
        let text = "[paths]\nexclude = ['C:\\tmp']\n[sound]\nachievement = \"gone.wav\"\n";
        let s = snapshot(Some(text), Path::new("/s"), &[]);
        assert_eq!(s["repos"]["exclude"], serde_json::json!([r"C:\tmp"]));
        let a = slot(&s, "achievement");
        assert_eq!(a["pinned"], "gone.wav");
        assert_eq!(a["exists"], false);
    }

    #[test]
    fn capability_lists_exactly_the_manifest_commands() {
        let cap = include_str!("../capabilities/settings.json");
        let build = include_str!("../build.rs");
        let overlay = include_str!("../capabilities/overlay.json");
        for c in COMMANDS {
            assert!(build.contains(&format!("\"{c}\"")), "{c} missing from build.rs");
            let perm = format!("\"allow-{}\"", c.replace('_', "-"));
            assert!(cap.contains(&perm), "{perm} missing from settings.json");
            assert!(!overlay.contains(&c.replace('_', "-")), "{c} granted to overlays");
        }
        assert_eq!(cap.matches("\"allow-settings-").count(), COMMANDS.len());
    }
}
