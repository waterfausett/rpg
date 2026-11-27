# Settings Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A native Settings window, opened from the tray, that edits the behavioral settings in `config.toml`
(keeping comments), applies them without a restart, persists Mute/DND, and copies picked sound files into `sounds/`.

**Architecture:** `rpg-core` gains a format-preserving config editor (`config_edit`) with an allow-list of
editable keys and validation. `rpg-app` gains a `ConfigStore` that serializes saves, writes atomically, swaps
`AppState.cfg`, notifies listeners and watches `config.toml` for hand edits. The listeners make the remaining
startup-only settings live. A new Tauri window (`ui/settings/`) talks to `settings.rs` commands over IPC. The
commands are restricted to that window by an app manifest plus a capability.

**Tech Stack:** Rust, Tauri 2, `toml_edit` 0.25 (`DocumentMut`), `tauri-plugin-dialog` 2, `notify` 8, `rodio`,
vanilla HTML/CSS/JS.

**Spec:** `docs/superpowers/specs/2026-11-26-settings-window-design.md`

## Global Constraints

- No new HTTP endpoint that writes. The character-sheet server stays read-only.
- Writable keys are only the allow-list in Task 2. `[xp]`, `[levels]`, `[skill_levels]`, `[site]`, `sound.by_rule`
  and anything else are rejected.
- Saving never loses comments or unrelated text in `config.toml`, never writes a file that fails to parse as
  `Config`, and never deletes a sound file.
- Window: label `settings`, title `rpg · Settings`, 720×560 logical, resizable, decorated, page
  `settings/index.html`. Tray item text: `Settings…`, placed right below `Open character sheet`.
- Status-line copy: `✓ Saved · <what>` on success, the error text on failure.
- Broken-file copy: `config.toml has an error, so settings can't be saved until it's fixed`. Buttons are
  `Open config file` and `Check again`.
- Tray save-failure pop-up: `overlay.notice(false, "Couldn't save", "config.toml has an error")`.
- Section names, in order: Notifications, Sounds, Integrations, Repos.
- Sound file types: `wav mp3 ogg flac`.
- Tier palette in `ui/settings/settings.css` must equal `ui/site/site.css` (test-enforced).
- Vanilla JS only. Use `window.__TAURI__.core.invoke` and `window.__TAURI__.event.listen` (`withGlobalTauri` is on).

## Review Focus

1. **A `config.toml` with CRLF line endings** (Notepad): editing one key keeps every other line byte-for-byte,
   and the result still parses. Test in Task 2.
2. **An inline-table `by_tier = { bronze = "ding.wav" }`**, as the README shows it: setting
   `sound.by_tier.gold` adds to that inline table instead of creating a conflicting `[sound.by_tier]` table.
   Test in Task 2.
3. **Rapid saves from dragging the volume slider:** the saves are serialized, the file ends at the last value,
   and the app's own writes are not mistaken for hand edits. Test in Task 3.
4. **A picked sound file with spaces or non-ASCII in its name, or a clash with an existing file:** the copy
   succeeds and never overwrites. Test in Task 6.
5. **`config.toml` missing entirely:** the first save creates it with only that key. Test in Task 2.

---

### Task 1: Config keys for Mute/DND and helpers for the editor

**Files:**
- Modify: `crates/rpg-core/src/config.rs`
- Modify: `crates/rpg-core/assets/config.default.toml`

**Interfaces:**
- Produces:
  - `SoundConfig.muted: bool` (default false)
  - `AnnounceConfig.dnd: bool` (default false)
  - `Config::parse(text: &str) -> anyhow::Result<Config>` (used by `Config::load`)
  - `config::validate_path_pattern(raw: &str) -> Result<(), String>` (same rules as `path_set`)
  - `config::raw_paths(text: &str) -> (Vec<String>, Vec<String>)`: the include and exclude strings as written;
    empty on parse failure

- [ ] **Step 1: Write failing tests** in `config.rs` `mod tests`:

```rust
#[test]
fn muted_and_dnd_default_off_and_parse() {
    let c = Config::default();
    assert!(!c.sound.muted && !c.announce.dnd);
    let c = Config::parse("[sound]\nmuted = true\n[announce]\ndnd = true\n").unwrap();
    assert!(c.sound.muted && c.announce.dnd);
}

#[test]
fn validate_path_pattern_matches_path_set_rules() {
    assert!(validate_path_pattern("C:/work").is_ok());
    assert!(validate_path_pattern("octo/*").is_ok());
    assert!(validate_path_pattern("[").is_err());
    assert!(validate_path_pattern("  ").is_err());
}

#[test]
fn raw_paths_returns_strings_as_written() {
    let (i, e) = raw_paths("[paths]\ninclude = ['C:\\work']\nexclude = ['**/tmp*']\n");
    assert_eq!(i, vec![r"C:\work".to_string()]);
    assert_eq!(e, vec!["**/tmp*".to_string()]);
    assert_eq!(raw_paths("not toml ["), (vec![], vec![]));
}
```

- [ ] **Step 2: Run** `cargo test -p rpg-core config::` and expect a compile failure (missing items).
- [ ] **Step 3: Implement.** Add the two fields with defaults. `Config::parse` is `toml::from_str` with the same
  context, and `load` calls it. `validate_path_pattern` calls `path_set("pattern", &[raw.to_string()])`. Make the
  empty-pattern check trim first, so `"  "` errors. In `config.default.toml`, add under `[sound]`
  `muted = false  # tray "Mute sounds"; also set from the Settings window` and under `[announce]`
  `dnd = false  # tray "Do not disturb"`.
- [ ] **Step 4: Run** `cargo test -p rpg-core config::` and expect a PASS, including `default_config_file_parses`.
- [ ] **Step 5: Commit** `git add -A && git commit -m "Config: sound.muted, announce.dnd, parse/validate helpers (#49)"`

---

### Task 2: Format-preserving config editor with an allow-list

**Files:**
- Create: `crates/rpg-core/src/config_edit.rs` (add `pub mod config_edit;` to `lib.rs`)
- Modify: `crates/rpg-core/Cargo.toml` (`toml_edit = "0.25"`)

**Interfaces:**
- Consumes: `Config::parse`, `validate_path_pattern` (Task 1).
- Produces:
  - `pub fn apply_edit(text: &str, key: &str, value: &serde_json::Value) -> Result<(String, Config), EditError>`:
    returns the new file text and its parsed `Config`. `value == Null` removes the key.
  - `pub enum EditError { UnknownKey(String), BadValue { key: String, msg: String }, InvalidFile(String) }`,
    which implements `Display`. `InvalidFile` means the existing text didn't parse as `Config` (checked before
    editing). Its message includes the TOML error, with line and column.
  - `pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()>`: writes a temp file in the same dir, then
    renames it over the target.

Allow-list (key → accepted JSON):

| keys | value |
|---|---|
| `sound.muted`, `announce.dnd`, `announce.first_banner`, `github.enabled`, `llm.enabled` | bool |
| `announce.{bronze,silver,gold,platinum,legendary,celestial}`, `announce.all` | `"banner"` or `"toast"` |
| `sound.volume` | number, 0.0–1.0 |
| `sound.achievement`, `sound.level_up`, `sound.by_tier.{six tiers}` | file name with no `/`, `\` or `..`, non-empty |
| `github.poll_minutes`, `llm.timeout_secs` | integer ≥ 1 |
| `updates.check_hours` | integer ≥ 0 |
| `llm.model` | non-empty string |
| `paths.include`, `paths.exclude` | array of strings; each passes `validate_path_pattern` (the error names the bad pattern) |

Any key may be `null` (removed, so the default applies).

- [ ] **Step 1: Write failing tests** in `config_edit.rs`:

```rust
const SAMPLE: &str = "# my config\n[sound]\nvolume = 0.8 # loud\nby_tier = { bronze = \"ding.wav\" }\n\n[xp]\ncommit = 99\n";

#[test] fn edit_keeps_comments_and_other_keys() {
    let (t, c) = apply_edit(SAMPLE, "sound.volume", &json!(0.5)).unwrap();
    assert!(t.starts_with("# my config\n[sound]\nvolume = 0.5"));
    assert!(t.contains("[xp]\ncommit = 99\n"));
    assert_eq!(c.xp.commit, 99);
}
#[test] fn inline_table_gets_new_tier_entry() {
    let (t, c) = apply_edit(SAMPLE, "sound.by_tier.gold", &json!("gong.wav")).unwrap();
    assert!(!t.contains("[sound.by_tier]"));
    assert_eq!(c.sound.by_tier.get("gold").map(String::as_str), Some("gong.wav"));
    assert_eq!(c.sound.by_tier.get("bronze").map(String::as_str), Some("ding.wav"));
}
#[test] fn missing_file_and_tables_are_created() {
    let (t, c) = apply_edit("", "announce.gold", &json!("toast")).unwrap();
    assert_eq!(c.announce.gold, Announce::Toast);
    assert!(t.contains("[announce]"));
}
#[test] fn null_removes_key() {
    let (t, c) = apply_edit(SAMPLE, "sound.volume", &Value::Null).unwrap();
    assert!(!t.contains("volume"));
    assert_eq!(c.sound.volume, 0.8);
}
#[test] fn crlf_lines_survive() {
    let crlf = SAMPLE.replace('\n', "\r\n");
    let (t, _) = apply_edit(&crlf, "github.enabled", &json!(false)).unwrap();
    assert!(t.contains("[xp]\r\ncommit = 99\r\n"));
}
#[test] fn rejects_unknown_keys_and_bad_values() {
    for (k, v) in [("xp.commit", json!(5)), ("site.port", json!(1)), ("sound.by_rule.x", json!("a.wav"))] {
        assert!(matches!(apply_edit(SAMPLE, k, &v), Err(EditError::UnknownKey(_))), "{k}");
    }
    for (k, v) in [("sound.volume", json!(1.5)), ("github.poll_minutes", json!(0)), ("announce.gold", json!("loud")),
                   ("sound.achievement", json!("../x.wav")), ("paths.exclude", json!(["["])), ("llm.model", json!(""))] {
        assert!(matches!(apply_edit(SAMPLE, k, &v), Err(EditError::BadValue { .. })), "{k}");
    }
}
#[test] fn broken_file_is_refused_untouched() {
    let err = apply_edit("[sound\nvolume = 1", "sound.volume", &json!(0.5)).unwrap_err();
    assert!(matches!(err, EditError::InvalidFile(ref m) if m.contains("line")));
}
#[test] fn write_atomic_replaces_and_leaves_no_temp() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("config.toml");
    std::fs::write(&p, "old").unwrap();
    write_atomic(&p, "new").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
}
```

- [ ] **Step 2: Run** `cargo test -p rpg-core config_edit` and expect a FAIL (module missing).
- [ ] **Step 3: Implement.** Parse with `toml_edit::DocumentMut`. Walk the dotted path, creating
  `toml_edit::Table` for missing parents. Inside an existing inline table, insert into it instead. Set a
  `toml_edit::value(...)` or remove the key. Re-serialize with `doc.to_string()`, then run `Config::parse` on the
  result (a failure there is `BadValue`). Validate the allow-list and value type before touching the doc.
  `write_atomic` writes `config.toml.tmp` in the same dir with `std::fs::write`, then `std::fs::rename`. If `toml_edit` rewrites untouched line endings, detect `
` in the input and emit CRLF throughout so the CRLF test holds.
- [ ] **Step 4: Run** `cargo test -p rpg-core` and expect a PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "Core: format-preserving config editor with allow-list (#49)"`

---

### Task 3: ConfigStore: serialized saves, live swap, listeners and file watch

**Files:**
- Create: `crates/rpg-app/src/config_store.rs` (add `mod config_store;` in `main.rs`)
- Modify: `crates/rpg-app/src/runtime.rs` (add a `watch_file` helper next to `watch`)

**Interfaces:**
- Consumes: `apply_edit`, `write_atomic`, `EditError` (Task 2). `Config::parse` (Task 1).
- Produces:
  - `pub struct ConfigStore` built with `ConfigStore::new(paths: Paths, cfg: Arc<RwLock<Config>>) -> Arc<Self>`.
  - `pub fn set(&self, key: &str, value: &serde_json::Value) -> Result<(), EditError>`. Under a `Mutex<()>` save
    lock: read the file (missing = `""`), `apply_edit`, `write_atomic`, record the written text as
    `last_written`, swap `cfg`, then call every listener with `(&old, &new)`.
  - `pub fn reload_from_disk(&self)`. If the file text equals `last_written`, do nothing. Else parse it: on
    success, swap and notify; on error, log `config.toml invalid, keeping current settings: <err>` and keep
    `cfg`. Either way, call the `on_external` listeners (so an open window re-fetches).
  - `pub fn on_change(&self, f: impl Fn(&Config, &Config) + Send + Sync + 'static)`
  - `pub fn on_external(&self, f: impl Fn() + Send + Sync + 'static)`
  - `pub fn watch(self: &Arc<Self>) -> Option<notify::RecommendedWatcher>`: watches the config dir
    non-recursively and calls `reload_from_disk` (debounced by `DEBOUNCE`) for events whose paths include
    `config.toml`. The caller keeps the watcher alive.
  - `runtime::watch_file(paths: &Paths, file: &Path, on_change: impl Fn() + Send + 'static) -> Option<RecommendedWatcher>`

- [ ] **Step 1: Write failing tests** in `config_store.rs`. Use a `Paths::from_home(tempdir)` fixture and
  `Arc<RwLock<Config::default()>>`. Count listener calls with an `Arc<AtomicUsize>`.
  - `set_writes_file_swaps_cfg_and_notifies`: `set("sound.volume", 0.3)`. Then `cfg.sound.volume == 0.3`, the
    file contains `volume = 0.3`, and the change listener ran once with `old.sound.volume == 0.8`.
  - `own_write_is_not_an_external_change`: after `set`, `reload_from_disk()` calls neither listener.
  - `hand_edit_reloads_and_notifies`: write `[github]\nenabled = false\n` to the file, then call
    `reload_from_disk()`. Then `!cfg.github.enabled`, and each listener ran once.
  - `broken_hand_edit_keeps_current_config`: write `[sound\n`, then call `reload_from_disk()`. `cfg` is unchanged,
    the change listener didn't run, `on_external` ran, and `rpg.log` contains `config.toml invalid`.
  - `rapid_sets_end_at_last_value`: 20 threads each call `set("sound.volume", i/20)`. Afterwards the file parses
    and its volume equals `cfg.sound.volume`.
  - `set_on_broken_file_is_refused`: the file is `[sound\n`, and `set` returns `EditError::InvalidFile`. The file
    is unchanged.
- [ ] **Step 2: Run** `cargo test -p rpg-app config_store` and expect a FAIL.
- [ ] **Step 3: Implement** as specified in Interfaces. Listeners are `Mutex<Vec<Box<dyn Fn…>>>`. Call them after
  releasing the cfg write lock.
- [ ] **Step 4: Run** `cargo test -p rpg-app config_store` and expect a PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "App: ConfigStore with atomic saves, live swap and file watch (#49)"`

---

### Task 4: Make the startup-only settings live

**Files:**
- Modify: `crates/rpg-app/src/runtime.rs` (`AppState.gen`, poller), `crates/rpg-app/src/overlay.rs`
  (`resolve_flavor`), `crates/rpg-app/src/update.rs`, `crates/rpg-app/src/main.rs`,
  `crates/rpg-app/Cargo.toml` (tokio feature `sync`)

**Interfaces:**
- Consumes: `ConfigStore::on_change` (Task 3).
- Produces:
  - `AppState.gen: Arc<RwLock<Option<Arc<dyn TextGen>>>>`. Readers clone it at use.
  - `runtime::make_gen(llm: &LlmConfig, paths: &Paths) -> Option<Arc<dyn TextGen>>`, moved from `init_state`.
  - `runtime::spawn(state, sink) -> Sender<()>`: returns the GitHub poller's wake sender. The poller is always
    spawned.
  - `fn poll_loop_with(state: &AppState, wake: &Receiver<()>, poll: impl FnMut() -> Result<usize>)`. Each pass
    reads `github.enabled`. When off, it blocks on `wake.recv_timeout(60s)` without polling. When on, it polls,
    then blocks on `wake.recv_timeout(poll_minutes)`. It returns when `wake` is disconnected. `poll_loop` wraps it
    with `poll_github`.
  - `update::background_interval(hours: u64, debug: bool) -> Option<Duration>` (`None` when 0 or debug).
  - `Updates::settings_changed(&self)`: wakes the background loop via a `tokio::sync::Notify`. The loop recomputes
    `background_interval` every cycle. When `None`, it waits only on the notify.
  - In `main.rs`, register an `on_change` listener. It rebuilds `gen` when `llm.enabled` or `llm.model` changed,
    sends on the poller wake when `github` changed, and calls `updates.settings_changed()` when
    `updates.check_hours` changed.

- [ ] **Step 1: Write failing tests:**
  - In `runtime.rs`, `poller_idles_while_disabled_and_resumes_on_wake`: start with `github.enabled = false`, run
    `poll_loop_with` on a thread with a counting `poll`, send a wake, and check the count is 0. Then set
    `enabled = true`, send a wake, and wait until the count is ≥ 1 (bounded 2s). Drop the sender; the thread
    joins.
  - In `update.rs`, `background_interval_rules`: `(0,false) → None`, `(6,true) → None`,
    `(6,false) → Some(6h)`.
  - Update the runtime and overlay test fixtures for the new `gen` type.
- [ ] **Step 2: Run** `cargo test -p rpg-app` and expect a FAIL.
- [ ] **Step 3: Implement** as specified in Interfaces.
- [ ] **Step 4: Run** `cargo test -p rpg-app` and expect a PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "App: flavor, GitHub polling and update checks follow live config (#49)"`

---

### Task 5: Persisted Mute/DND and the tray

**Files:**
- Modify: `crates/rpg-app/src/tray.rs`, `crates/rpg-app/src/main.rs`

**Interfaces:**
- Consumes: `ConfigStore::{set, on_change}` (Task 3); `sound.muted` and `announce.dnd` (Task 1).
  `settings::open(app: &AppHandle)` (Task 7). Add the menu item here, and stub `open` until Task 7 if you build
  in order.
- Produces:
  - `Toggles::from_config(cfg: &Config) -> Toggles`
  - `tray::sync_toggles(app: &AppHandle, toggles: &Toggles, cfg: &Config)`: sets the atomics, the two
    `CheckMenuItem` check states (keep the handles in a managed `TrayChecks` struct) and `overlay.set_dnd`.
  - `fn toggle_saved(res: Result<(), EditError>) -> Option<(String, String)>`: `None` on Ok, or
    `Some(("Couldn't save", "config.toml has an error"))` for `InvalidFile` and other errors (the error is logged
    by the caller).
  - The tray menu adds `Settings…` (id `settings`) below `Open character sheet`.
  - Mute or DND click: set the atomic and the overlay first (session effect), then `store.set(...)`. On error, log
    `saving <key> failed: <err>` and show the notice.
  - Startup: `Toggles::from_config`. The check items are created with those states, and `set_dnd` is applied after
    the overlay exists.
  - Register `on_change`: when `sound.muted` or `announce.dnd` differs, call `sync_toggles` and emit
    `settings-changed` to the `settings` window.

- [ ] **Step 1: Write failing tests** in `tray.rs`:
  - `toggles_start_from_config`: from `Config::parse("[sound]\nmuted=true\n")`, `muted` is set and `dnd` isn't.
  - `toggle_save_failure_notice`: an `InvalidFile` error maps to the exact copy above, and `Ok` maps to `None`.
- [ ] **Step 2: Run** `cargo test -p rpg-app tray` and expect a FAIL.
- [ ] **Step 3: Implement** as specified in Interfaces. Update the `Toggles` doc comment, since the toggles are
  now persisted.
- [ ] **Step 4: Run** `cargo test -p rpg-app` and expect a PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "Tray: Mute/DND persist in config; Settings… item (#49)"`

---

### Task 6: Sound slots: folders, pins, fallbacks, copy and preview

**Files:**
- Create: `crates/rpg-app/src/sound_slots.rs`
- Modify: `crates/rpg-app/src/sound.rs` (make `collect_files` and `is_audio` `pub(crate)`, add `preview`)

**Interfaces:**
- Consumes: `sound::resolve_sound`, `SoundKey`, `Tier`.
- Produces:
  - `pub enum Slot { Achievement, LevelUp, Tier(Tier) }`. `Slot::parse(s: &str) -> Option<Slot>` accepts
    `"achievement"`, `"level_up"` and `"tier:<lowercase tier>"`. Also `Slot::id(&self) -> String` (same format),
    `Slot::config_key(&self) -> String` (`sound.achievement`, `sound.level_up`, `sound.by_tier.<tier>`) and
    `Slot::all() -> Vec<Slot>` (8 slots in UI order).
  - `Slot::folder(&self, root: &Path) -> PathBuf`: `achievements/`, `levels/` or `achievements/<tier>/`.
  - `Slot::sound_key(&self) -> SoundKey`: `Summary`, `LevelUp`, or `Achievement { rule_id: String::new(), tier }`.
  - `Slot::pinned<'a>(&self, cfg: &'a SoundConfig) -> Option<&'a String>`
  - `pin_exists(slot, name, root, files) -> bool`. It searches the slot's own folder, then the type folder, then
    the root, the same way `resolve_sound` finds pins.
  - `fallback(slot, cfg, root, files) -> &'static str` for the unpinned case, returning `"pool"` | `"default"` |
    `"none"`. Achievement and LevelUp give `pool` when their pool (type folder + root) has audio, else `none`.
    Tier gives `pool` when `achievements/<tier>/` has audio. Otherwise it gives `default` when
    `resolve_sound(Summary)` would play something, else `none`.
  - `copy_into(slot, src: &Path, root: &Path) -> std::io::Result<String>`. It creates the folder and returns the
    file name used. Identical bytes reuse the existing name. A clash gets `stem (2).ext`, `stem (3).ext` and so on.
  - `sound::preview(file: &Path, volume: f32) -> anyhow::Result<()>`. It opens and decodes synchronously (the error
    is returned), then plays on a `rpg-sound` thread. `play_file` stays the playback path.

- [ ] **Step 1: Write failing tests** in `sound_slots.rs`, using real temp dirs for `copy_into`:
  - `slot_ids_round_trip_and_keys`: every slot in `Slot::all()` passes `parse(id())`.
    `Tier(Gold).config_key() == "sound.by_tier.gold"`.
  - `fallback_labels`: with no files, Achievement is `none` and Tier(Gold) is `none`. With
    `achievements/a.mp3`, Achievement is `pool` and Tier(Gold) is `default`. With `achievements/gold/g.mp3`,
    Tier(Gold) is `pool`. With only `levels/l.mp3`, LevelUp is `pool` and Achievement is `none`.
  - `pin_exists_searches_like_resolver`: `gong.wav` in the root counts for Tier(Gold). The same file in `levels/`
    doesn't.
  - `copy_into_never_overwrites`: copy `Fan Fare é.mp3` → its name. The same bytes again → the same name. Other
    bytes with the same name → `Fan Fare é (2).mp3`. The original's bytes are unchanged.
- [ ] **Step 2: Run** `cargo test -p rpg-app sound_slots` and expect a FAIL.
- [ ] **Step 3: Implement** as specified in Interfaces.
- [ ] **Step 4: Run** `cargo test -p rpg-app sound` and expect a PASS (the existing resolver tests still pass).
- [ ] **Step 5: Commit** `git add -A && git commit -m "Sounds: slot folders, pin lookup, fallbacks, safe copy, preview (#49)"`

---

### Task 7: Settings commands, window and capability

**Files:**
- Create: `crates/rpg-app/src/settings.rs`, `crates/rpg-app/capabilities/settings.json`
- Modify: `crates/rpg-app/build.rs`, `crates/rpg-app/tauri.conf.json` (`capabilities: ["overlay","settings"]`),
  `crates/rpg-app/Cargo.toml` (`tauri-plugin-dialog = "2"`), `crates/rpg-app/src/main.rs` (plugin,
  `invoke_handler`, manage `Arc<ConfigStore>`)

**Interfaces:**
- Consumes: `ConfigStore` (Task 3), `raw_paths` and `Config::parse` (Task 1), `Slot`, `fallback`, `pin_exists`,
  `copy_into` and `preview` (Task 6).
- Produces:
  - `pub fn snapshot(text: Option<&str>, sounds_root: &Path, files: &[PathBuf]) -> serde_json::Value`. `None`
    means the file is missing (defaults). On parse failure it returns `{ "invalid": "<error with line>" }`.
    Otherwise it returns `{ "invalid": null, "notifications": {dnd, first_banner, all, tiers: {bronze..}},
    "sounds": {muted, volume, slots: [{id, label, pinned, exists, fallback}]}, "integrations": {github: {enabled,
    poll_minutes}, llm: {enabled, model, timeout_secs}, updates: {check_hours}}, "repos": {include, exclude} }`.
  - Commands, all returning `Result<Value, String>` (a snapshot, or the error's `Display`):
    - `settings_get()`
    - `settings_set(key: String, value: Value)`
    - `async settings_choose_sound(slot: String)`: Open dialog, filter `wav mp3 ogg flac`, run via
      `spawn_blocking`. Cancel returns the snapshot unchanged. Otherwise `copy_into`, then
      `set(slot.config_key(), name)`.
    - `async settings_add_folder(list: String)`: `list` is `include` or `exclude`. Folder picker, then append to
      that array and save.
    - `settings_preview_sound(slot: String)`: resolve with `resolve_sound(slot.sound_key())` and call `preview` at
      the current volume, ignoring Mute. "Nothing to play" is an error.
    - `settings_open_sounds_folder()`
    - `settings_open_config_file()`: `tauri_plugin_opener::open_path`.
  - `pub fn open(app: &AppHandle)`: if window `settings` exists, unminimize, show and set focus. Otherwise build it
    per Global Constraints, with min size 600×440.
  - `ConfigStore::on_external` and the tray sync emit `settings-changed` to the `settings` window.
  - `build.rs`: `tauri_build::try_build(Attributes::new().app_manifest(AppManifest::new().commands(&[the 7 names]))).unwrap()`.
    `capabilities/settings.json`: windows `["settings"]`, permissions `core:event:allow-listen`,
    `core:event:allow-unlisten`, and `allow-<command>` for each of the 7 (dashes, e.g. `allow-settings-get`).

- [ ] **Step 1: Write failing tests** in `settings.rs`:
  - `snapshot_of_missing_file_is_defaults`: `invalid` is null, `sounds.volume` is `0.8`, there are 8 slots, the
    Silver slot's `fallback` is `"none"` with no files, and `notifications.tiers.bronze` is `"toast"`.
  - `snapshot_of_broken_file_reports_error`: `snapshot(Some("[sound\n"), …)["invalid"]` is a string containing
    `line`.
  - `snapshot_shows_raw_paths_and_missing_pins`: with `[paths] exclude=['C:\\tmp']` and `[sound] achievement =
    "gone.wav"` and no files, `repos.exclude == ["C:\\tmp"]` and the achievement slot has
    `{pinned:"gone.wav", exists:false}`.
  - `capability_lists_exactly_the_manifest_commands`: read `capabilities/settings.json` and `build.rs` with
    `include_str!`. Each of the 7 command names appears in `build.rs`, and its `allow-` form appears in the
    capability. `capabilities/overlay.json` contains none of them.
- [ ] **Step 2: Run** `cargo test -p rpg-app settings` and expect a FAIL.
- [ ] **Step 3: Implement** as specified in Interfaces. Build `ConfigStore` in `main.rs` setup, `app.manage` it,
  keep its watcher alive in managed state, and register the listeners from Tasks 4 and 5 there.
- [ ] **Step 4: Run** `cargo test -p rpg-app` and `cargo build -p rpg-app`, and expect a PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "Settings: IPC commands, window and capability (#49)"`

---

### Task 8: Settings page UI

**Files:**
- Create: `ui/settings/index.html`, `ui/settings/settings.css`, `ui/settings/settings.js`
- Modify: `crates/rpg-app/src/site.rs` tests (palette sync includes settings.css)

**Interfaces:**
- Consumes: the commands, snapshot shape and `settings-changed` event from Task 7.
- The layout follows mockup A in `.superpowers/brainstorm/*/content/layout.html`: a 150px sidebar with the four
  sections, the active one gold with a left bar, and a status line pinned at the bottom.
- Field behavior per the spec's "Sections and fields":
  - Switches, segmented Banner/Pop-up controls and `all` (Off/Banner/Pop-up; Off sends `null`) save on change.
  - The volume slider saves on `change` (release), not on `input`.
  - Number and text fields save on `change`.
  - The tier rows are disabled while `all` is set.
  - A sound row shows its file name, or `random from folder` / `use default` / `none` from `fallback`. A pinned
    file that's missing shows `missing: <name>` with ▶ disabled.
  - Sound row buttons: ▶ for preview, `Choose…`, and ✕ (only when pinned; sends `null`; tooltip
    `Stop using this sound (the file stays in sounds/)`).
  - A note under Sounds says per-achievement sounds and folder pools are set up in the `sounds/` folder, next to an
    `Open sounds folder` button.
  - The Repos section has two lists, with `Add folder…`, `Add pattern` (an inline input; a bad pattern shows its
    error under the row and isn't kept) and ✕ per entry.
  - When `invalid` is set, replace every section with the broken-file panel (copy in Global Constraints).
    `Check again` calls `settings_get`.
- After each command, re-render from the returned snapshot. On error, show it in the status line and re-render
  from `settings_get`, so the field reverts. Listen for `settings-changed` and re-fetch.

- [ ] **Step 1: Write failing tests** in `site.rs` tests:
  - Extend `tier_palette_matches_between_overlay_and_site` to also compare
    `include_str!("../../../ui/settings/settings.css")`.
  - `settings_page_has_four_sections_and_invokes_known_commands`: `index.html` contains `data-section=` for
    `notifications`, `sounds`, `integrations` and `repos`. `settings.js` contains each of the 7 command names and
    `settings-changed`.
- [ ] **Step 2: Run** `cargo test -p rpg-app site` and expect a FAIL.
- [ ] **Step 3: Implement the page.** Copy the `:root` tokens and tier palette from `site.css`.
- [ ] **Step 4: Run** `cargo test -p rpg-app` and expect a PASS. Then `cd crates/rpg-app && cargo tauri dev`:
  open `Settings…`, change one field in each section, and confirm `config.toml` changes with its comments intact.
- [ ] **Step 5: Commit** `git add -A && git commit -m "Settings page: sidebar sections, sound rows, repos lists (#49)"`

---

### Task 9: Docs

**Files:**
- Modify: `README.md`, `docs/manual-checklist.md`

- [ ] **Step 1: Update the README.**
  - Tray menu list: add `Settings…`.
  - "Config is read at startup…": change it to say that changes from the Settings window or a hand edit apply
    right away, except `[site] port`, which needs a restart.
  - The config reference gains `muted` and `dnd`.
  - The Sounds section mentions `Choose…` in the Settings window.
- [ ] **Step 2: Add a "Settings window" section to the manual checklist:**
  - It opens from the tray. A second click focuses the same window.
  - Each section's change applies without a restart: a tier set to Pop-up, the volume, GitHub off then on (a poll
    appears in `rpg.log`), and Claude flavor off.
  - The tray Mute/DND check marks and the window agree both ways, and they survive a restart.
  - A hand edit of `config.toml` while the window is open shows up in the window.
  - Breaking `config.toml` shows the error panel. The tray Mute still works and shows the "Couldn't save" pop-up.
    Fixing the file and pressing `Check again` restores the sections.
  - `Choose…`: a file with spaces in its name lands in the right folder and plays on the next matching banner.
    ▶ previews it even while muted. ✕ unpins it, and the file stays.
  - Repos: `Add folder…` adds a path. A bad pattern shows an error and isn't saved.
- [ ] **Step 3: Run** `cargo test --workspace` and expect a PASS.
- [ ] **Step 4: Commit** `git add -A && git commit -m "Docs: settings window in README and manual checklist (#49)"`
