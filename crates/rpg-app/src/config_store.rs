//! The single runtime writer of `config.toml`. Saves are serialized, the live
//! `Config` is swapped in place, and listeners hear about every change.
//! Listeners must not call `set` (the save lock is held while they run).
use crate::runtime::{log, watch_file, DEBOUNCE};
use rpg_core::config::Config;
use rpg_core::config_edit::{apply_edit, write_atomic, EditError};
use rpg_core::paths::Paths;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};

type ChangeFn = Box<dyn Fn(&Config, &Config) + Send + Sync>;
type ExternalFn = Box<dyn Fn() + Send + Sync>;

pub struct ConfigStore {
    paths: Paths,
    cfg: Arc<RwLock<Config>>,
    /// Serializes `set` and `reload_from_disk`; holds the text we last wrote.
    save: Mutex<Option<String>>,
    on_change: Mutex<Vec<ChangeFn>>,
    on_external: Mutex<Vec<ExternalFn>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl ConfigStore {
    pub fn new(paths: Paths, cfg: Arc<RwLock<Config>>) -> Arc<Self> {
        Arc::new(Self {
            paths,
            cfg,
            save: Mutex::new(None),
            on_change: Mutex::new(vec![]),
            on_external: Mutex::new(vec![]),
        })
    }

    pub fn on_change(&self, f: impl Fn(&Config, &Config) + Send + Sync + 'static) {
        lock(&self.on_change).push(Box::new(f));
    }

    #[allow(dead_code)] // used by the tray (Task 5)
    pub fn on_external(&self, f: impl Fn() + Send + Sync + 'static) {
        lock(&self.on_external).push(Box::new(f));
    }

    fn swap(&self, new: Config) -> Config {
        std::mem::replace(&mut *self.cfg.write().unwrap_or_else(|p| p.into_inner()), new)
    }

    fn notify_change(&self, old: &Config, new: &Config) {
        for f in lock(&self.on_change).iter() {
            f(old, new);
        }
    }

    #[allow(dead_code)] // used by the settings commands (Task 7)
    pub fn set(&self, key: &str, value: &serde_json::Value) -> Result<(), EditError> {
        let mut last_written = lock(&self.save);
        let file = self.paths.config_file();
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(EditError::Io(e.to_string())),
        };
        let (new_text, new_cfg) = apply_edit(&text, key, value)?;
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        write_atomic(&file, &new_text).map_err(|e| EditError::Io(e.to_string()))?;
        *last_written = Some(new_text);
        let old = self.swap(new_cfg.clone());
        self.notify_change(&old, &new_cfg);
        Ok(())
    }

    pub fn reload_from_disk(&self) {
        let mut last_written = lock(&self.save);
        let text = match std::fs::read_to_string(self.paths.config_file()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                log(&self.paths, &format!("cannot read config.toml, keeping current settings: {e}"));
                return;
            }
        };
        if last_written.as_deref() == Some(text.as_str()) {
            return;
        }
        match Config::parse(&text) {
            Ok(new) => {
                *last_written = Some(text);
                let old = self.swap(new.clone());
                self.notify_change(&old, &new);
            }
            Err(e) => log(&self.paths, &format!("config.toml invalid, keeping current settings: {e:#}")),
        }
        for f in lock(&self.on_external).iter() {
            f();
        }
    }

    /// Watch `config.toml` for hand edits. The caller keeps the watcher alive.
    pub fn watch(self: &Arc<Self>) -> Option<notify::RecommendedWatcher> {
        let (tx, rx) = mpsc::channel::<()>();
        let store = Arc::clone(self);
        std::thread::Builder::new()
            .name("rpg-config".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    std::thread::sleep(DEBOUNCE);
                    while rx.try_recv().is_ok() {}
                    store.reload_from_disk();
                }
            })
            .ok()?;
        watch_file(&self.paths, &self.paths.config_file(), move || {
            let _ = tx.send(());
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fx {
        _tmp: tempfile::TempDir,
        paths: Paths,
        cfg: Arc<RwLock<Config>>,
        store: Arc<ConfigStore>,
        changes: Arc<AtomicUsize>,
        externals: Arc<AtomicUsize>,
        old_volume: Arc<Mutex<Option<f64>>>,
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(Some(tmp.path().to_path_buf())).unwrap();
        std::fs::create_dir_all(&paths.config).unwrap();
        let cfg = Arc::new(RwLock::new(Config::default()));
        let store = ConfigStore::new(paths.clone(), cfg.clone());
        let (changes, externals) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let old_volume = Arc::new(Mutex::new(None));
        let (c, ov) = (changes.clone(), old_volume.clone());
        store.on_change(move |old, _| {
            c.fetch_add(1, Ordering::SeqCst);
            *ov.lock().unwrap() = Some(old.sound.volume as f64);
        });
        let e = externals.clone();
        store.on_external(move || {
            e.fetch_add(1, Ordering::SeqCst);
        });
        Fx { _tmp: tmp, paths, cfg, store, changes, externals, old_volume }
    }

    #[test]
    fn set_writes_file_swaps_cfg_and_notifies() {
        let f = fx();
        f.store.set("sound.volume", &json!(0.3)).unwrap();
        assert!((f.cfg.read().unwrap().sound.volume as f64 - 0.3).abs() < 1e-6);
        let text = std::fs::read_to_string(f.paths.config_file()).unwrap();
        assert!(text.contains("volume = 0.3"), "{text}");
        assert_eq!(f.changes.load(Ordering::SeqCst), 1);
        assert!((f.old_volume.lock().unwrap().unwrap() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn own_write_is_not_an_external_change() {
        let f = fx();
        f.store.set("sound.volume", &json!(0.3)).unwrap();
        f.store.reload_from_disk();
        assert_eq!(f.changes.load(Ordering::SeqCst), 1);
        assert_eq!(f.externals.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn hand_edit_reloads_and_notifies() {
        let f = fx();
        std::fs::write(f.paths.config_file(), "[github]\nenabled = false\n").unwrap();
        f.store.reload_from_disk();
        assert!(!f.cfg.read().unwrap().github.enabled);
        assert_eq!(f.changes.load(Ordering::SeqCst), 1);
        assert_eq!(f.externals.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn broken_hand_edit_keeps_current_config() {
        let f = fx();
        let before = f.cfg.read().unwrap().sound.volume;
        std::fs::write(f.paths.config_file(), "[sound\n").unwrap();
        f.store.reload_from_disk();
        assert_eq!(f.cfg.read().unwrap().sound.volume, before);
        assert!(f.cfg.read().unwrap().github.enabled);
        assert_eq!(f.changes.load(Ordering::SeqCst), 0);
        assert_eq!(f.externals.load(Ordering::SeqCst), 1);
        let log = std::fs::read_to_string(f.paths.log()).unwrap();
        assert!(log.contains("config.toml invalid"), "{log}");
    }

    #[test]
    fn unreadable_config_keeps_current_config() {
        let f = fx();
        std::fs::create_dir(f.paths.config_file()).unwrap();
        f.store.reload_from_disk();
        assert!((f.cfg.read().unwrap().sound.volume as f64 - 0.8).abs() < 1e-6);
        assert_eq!(f.changes.load(Ordering::SeqCst), 0);
        let log = std::fs::read_to_string(f.paths.log()).unwrap();
        assert!(log.contains("cannot read config.toml"), "{log}");
    }

    #[test]
    fn rapid_sets_end_at_last_value() {
        let f = fx();
        let handles: Vec<_> = (0..20)
            .map(|i| {
                let s = f.store.clone();
                std::thread::spawn(move || s.set("sound.volume", &json!(i as f64 / 20.0)).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(f.paths.config_file()).unwrap();
        let parsed = Config::parse(&text).unwrap();
        assert_eq!(parsed.sound.volume, f.cfg.read().unwrap().sound.volume);
    }

    #[test]
    fn set_on_broken_file_is_refused() {
        let f = fx();
        std::fs::write(f.paths.config_file(), "[sound\n").unwrap();
        let r = f.store.set("sound.volume", &json!(0.5));
        assert!(matches!(r, Err(EditError::InvalidFile(_))), "{r:?}");
        assert_eq!(std::fs::read_to_string(f.paths.config_file()).unwrap(), "[sound\n");
    }
}
