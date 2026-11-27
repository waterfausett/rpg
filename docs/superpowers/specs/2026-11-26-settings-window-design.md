# Settings window (#49)

## Intent

Every setting lives in `config.toml` and the `sounds/` folder today, and the app reads the config only at startup.
Changing anything means finding the config folder, editing TOML by hand and restarting. Issue #49 asks for a way to
change settings and sounds from the app itself.

The user's picture: a screen where you can tweak all the *behavioral* settings, and, for sounds, a file picker where
you choose a file and the app puts it in the right place. The UI doesn't need to cover every sound permutation.

## Goals

- A **Settings window** opened from a new tray item, with a sidebar of sections (layout A from the mockups):
  Notifications, Sounds, Integrations, Repos.
- Every change saves as you make it (no Save button), and a status line at the bottom confirms it
  ("✓ Saved · Gold sound set to gong.wav") or shows the error.
- Changes take effect without restarting the app.
- Saving rewrites `config.toml` **in place, keeping its comments and layout**, and touches only the keys that changed.
- Sound files picked through the system's Open dialog are copied into `sounds/` and pinned in the config.
- Mute sounds and Do not disturb are remembered across restarts. The tray checkboxes and the window stay in sync.

## Non-goals

- **Progression stays file-only:** `[xp]`, `[levels]` and `[skill_levels]` are not in the window (the user's decision).
- `[site] port` is not in the window (plumbing; changing it breaks bookmarks).
- Per-achievement sounds (`by_rule`) and the folder-pool sounds (`achievements/<tier>/`, `levels/`, etc.) stay
  file-only. The window shows when a pool is in effect ("random from folder") but doesn't manage folders.
- No editing of rules or skills.
- The character sheet's localhost server stays read-only. Nothing in this work adds an HTTP endpoint that writes.
- No macOS-specific work beyond what Tauri gives for free (macOS isn't supported yet).

## Anti-goals

- Losing the user's hand edits or comments in `config.toml`.
- A broken or half-written `config.toml` (crash mid-save, bad value) that makes the app fall back to defaults on the
  next start.
- The window and the file disagreeing: if the user hand-edits `config.toml` while the app runs, the window must not
  silently overwrite those edits with stale values.
- Deleting the user's sound files. Clearing a sound only removes the pin.

## Constraints (from the code today)

- `AppState.cfg` is already an `Arc<RwLock<Config>>` shared by the engine, overlay, site and poller, so swapping in a
  new `Config` reaches most readers immediately.
- `Config` only derives `Deserialize`. It isn't round-tripped; writing has to go through a format-preserving editor
  (`toml_edit`, which the `toml` crate is already built on).
- `PathFilter` deserializes into compiled glob sets, so the window needs the raw include/exclude strings from the
  file, not from `Config`.
- Some settings are captured once rather than read live:
  - The flavor generator (`AppState.gen`: `llm.enabled`, `llm.model`) is built in `init_state`.
  - The GitHub poller checks `github.enabled` once at spawn; `poll_minutes` is re-read each loop.
  - The update checker reads `updates.check_hours` once in `spawn_background`.
  - Overlay sound and announce settings, `llm.timeout_secs` and the path filter are already read per use.
- Tray Mute and DND are in-memory `AtomicBool`s (`tray::Toggles`). DND is pushed to the presenter with `set_dnd`.
- Overlay windows are created from Rust with their own capability (`capabilities/overlay.json`) and pages under
  `ui/overlay/`, served via `frontendDist = ../../ui`. The CSP allows only `'self'` and IPC.
- The site palette (Refined Gold, tier colors) is in `ui/site/site.css`. A Rust test already keeps the tier colors
  in sync between the site and overlay stylesheets.

## Approaches considered

- **A. A Settings tab on the character sheet.** Reuses the page and styles, and the file picker would be a browser
  upload. It needs the localhost server to accept writes, which then have to be protected against other websites
  sending requests to `127.0.0.1`.
- **B. A separate native Settings window.** Opened from the tray and built like the overlay windows. It talks to the
  app over Tauri IPC and uses the system's Open dialog. Nothing new is exposed over HTTP. **Chosen by the user.**

Layout: **A, sections in a sidebar** (one section at a time) was chosen over one long scrolling page.

## Design

### Window

- A new tray item **Settings…** sits right below **Open character sheet**. It opens the window, or focuses it if it's
  already open. A normal decorated window, about 720×560, resizable, titled "rpg · Settings", created from Rust like
  the overlays (`WebviewWindowBuilder`, label `settings`). Closing it destroys it; the app keeps running.
- Page: `ui/settings/index.html` + `settings.css` + `settings.js`, vanilla like the rest of `ui/`. It uses the
  Refined Gold palette and tier colors. Add `ui/settings/settings.css` to the existing tier-palette sync test.
- A new capability `capabilities/settings.json` for window `settings`. It allows invoking only the settings commands
  below, plus the dialog plugin's open permission. The overlay capability is unchanged.
- The sidebar remembers the last section only for the life of the window (no persistence needed).

### Sections and fields

Each field shows its current value from the file (or the default when the key is missing).

**Notifications**
- Do not disturb (switch), with a note: "hold banners and drop pop-ups until you turn it off" (matches the presenter).
- First unlock is always a banner (`announce.first_banner`, switch).
- Same for every tier (`announce.all`): Off / Banner / Pop-up. When set, the per-tier rows are disabled, with a note
  that this overrides them.
- Per tier, six rows with tier dots: Banner / Pop-up (`announce.bronze` … `announce.celestial`).

**Sounds**
- Mute all sounds (switch).
- Volume (`sound.volume`, slider 0-100%, stored 0.0-1.0), with a **▶ Test** button that plays the current
  achievement sound at that volume.
- Banner sounds: **Achievements** (`sound.achievement`) and **Level-ups** (`sound.level_up`).
- By tier (overrides Achievements): six rows (`sound.by_tier.<tier>`).
- Each sound row shows the pinned file name, or what plays without a pin ("random from folder" when a pool has files,
  "use default" for a tier with nothing more specific, "none" when nothing would play). Buttons: ▶ (play it, ignoring
  Mute), **Choose…**, ✕ (clear the pin; shown only when pinned). A pin whose file is missing shows "missing" with
  the name, and its ▶ is disabled.
- `by_rule` pins aren't shown. A one-line note under the section says per-achievement sounds and folder pools are set
  up in the `sounds/` folder, with an **Open sounds folder** button.

**Integrations**
- GitHub: on/off (`github.enabled`), check every N minutes (`github.poll_minutes`, number, min 1).
  Note: needs the `gh` CLI, logged in.
- Claude flavor text: on/off (`llm.enabled`), model (`llm.model`, text with suggestions haiku/sonnet/opus),
  timeout seconds (`llm.timeout_secs`, number, min 1). Note: needs the `claude` CLI.
- Updates: check every N hours (`updates.check_hours`, number, 0 = only from the tray).

**Repos**
- Two lists, **Only count these** (`paths.include`, empty = everywhere) and **Never count these** (`paths.exclude`).
  Each entry is a row with ✕. **Add folder…** opens the system folder picker; **Add pattern** adds an inline text row
  for globs or `owner/name`. Each entry is validated with the same rules as `path_set`, so a bad glob shows the
  error inline and isn't saved.

### Saving

- The page calls a single IPC command, `settings_set(key, value)`, per change. `key` is a dotted path
  (`announce.gold`, `sound.by_tier.gold`, `paths.exclude`); `null` removes the key. Text and number fields save on
  change/blur, not on every keystroke.
- Rust side, under one save lock:
  1. Read `config.toml` (missing = empty document) and parse it with `toml_edit::DocumentMut`.
  2. Set or remove the key, creating its table if needed and keeping all other text, comments and ordering.
  3. Parse the resulting text as `Config` (the same `toml::from_str` path as startup). If it fails, reject the
     change with the error and leave the file untouched.
  4. Write atomically: a temp file in the same folder, then rename over `config.toml`.
  5. Replace `state.cfg` with the new `Config` and run the apply step (below).
- The command returns the fresh settings snapshot or an error string. The status line shows "✓ Saved · <what>", or the
  error in the failure style, and the field reverts to its saved value.
- Only keys the window knows are writable. `settings_set` rejects anything else, so the window can never touch
  `[xp]`, `[levels]`, `[site]` and so on.
- **Out-of-band edits:** the app watches `config.toml` (the same `notify` helper the rules folder uses). When the file
  changes and the app didn't write it, the app reloads `Config` (a parse error is logged and keeps the current
  config), applies it, and tells an open window to re-read its fields. Because every save re-reads the file first,
  the window never writes stale values back over hand edits. Side effect: hand-editing `config.toml` now applies
  without a restart too. Update the README's "restart the app after editing" line (the site port still needs one).

### When `config.toml` doesn't parse

The app already runs on defaults (or, after a bad hand edit while running, on the last good config) when the file
has an error. The file is the user's, so the window never "fixes" it by writing over it:

- `settings_get` returns an `invalid` state carrying the parse error, including the line. Instead of the
  fields, the window shows a panel: "config.toml has an error, so settings can't be saved until it's fixed",
  the error text, and **Open config file** and **Check again** buttons. The file watcher also re-checks
  automatically when the file changes.
- `settings_set` refuses the change with the same error (step 3 already does this) and writes nothing.
- Tray Mute and Do not disturb still change the setting for this session, so the toggles keep working. Saving is
  attempted and, on failure, logged to `rpg.log` once and shown as a single failure pop-up ("Couldn't save: config.toml
  has an error"). When the file becomes valid again, the in-memory toggles are not written back automatically;
  the file's values win, as with any reload.

### Applying without a restart

After every save or reload, compare old and new config:

- `llm.enabled` / `llm.model` changed: rebuild the flavor generator. `AppState.gen` becomes swappable
  (e.g. `Arc<RwLock<Option<Arc<dyn TextGen>>>>`), and readers take a clone at use.
- `github.enabled` changed: the poller loop re-reads `enabled` each tick, idling while off, instead of deciding once at
  spawn. Turning it on starts polling within one tick, not by spawning a second poller.
- `updates.check_hours` changed: the background checker re-reads the setting each cycle and is woken when it changes,
  so 0 stops it and a non-zero value restarts the schedule.
- `muted` / `dnd` changed: update the tray `Toggles`, the tray checkbox state and `presenter.set_dnd`.
- Everything else is already read live.

### Persisting Mute and Do not disturb

- New keys: `sound.muted` (default false) and `announce.dnd` (default false), added to `Config` and to
  `config.default.toml` with a comment.
- At startup, `Toggles` and the tray checkboxes start from these values, and DND is applied to the presenter.
- Toggling either in the tray now saves through the same path as the window, so the two stay in sync and a restart
  keeps the setting.

### Choosing a sound file

- **Choose…** opens the system Open dialog filtered to `.wav .mp3 .ogg .flac`. Use `tauri-plugin-dialog`, called from
  the Rust command so the page never sees arbitrary paths.
- The chosen file is copied into the folder for its slot:
  - Achievements → `sounds/achievements/`
  - Level-ups → `sounds/levels/`
  - Tier `t` → `sounds/achievements/<t>/`

  These are folders the resolver already searches for that pin. Then the pin is set to the file name.
- **Name clash:** if a file with that name already exists there and has identical bytes, reuse it. Otherwise add a
  suffix (`gong (2).wav`). A clash never overwrites anything.
- Caveat to make visible: a file copied into `achievements/<tier>/` also joins that tier's random pool. That has no
  effect while the tier is pinned, but clearing the pin leaves the file in the pool. The ✕ tooltip says "Stop using
  this sound (the file stays in sounds/)". The window doesn't offer to delete files.
- **▶ preview** plays through the existing `sound::play_file` path at the current volume, on the sound thread,
  ignoring Mute. A decode error shows in the status line.

### IPC surface (all in a new `crates/rpg-app/src/settings.rs`)

- `settings_get()` → a snapshot built from the file plus defaults: every field above, the raw path lists, and for each
  sound slot `{ pinned, exists, fallback: "pool" | "default" | "none" }`.
- `settings_set(key, value)` → snapshot or error.
- `settings_choose_sound(slot)` → snapshot or error (cancel = unchanged snapshot).
- `settings_preview_sound(slot)` → ok or error.
- `settings_add_folder(list)` → snapshot (folder picker, then append to `paths.include` or `paths.exclude`).
- `settings_open_sounds_folder()`.
- Event `settings-changed` → the window re-fetches (sent after out-of-band reloads and tray toggles).

Keep the pure parts free of Tauri so they can be unit-tested in `rpg-core` or a plain module:
- the `toml_edit` set/remove-by-key with validation
- the allowed-key list
- slot → folder mapping and name-clash naming
- the snapshot's fallback computation, reusing `resolve_sound`'s lookup

## Testing

- Unit:
  - set/remove keeps comments and unrelated keys byte-for-byte
  - creates missing tables
  - rejects unknown keys and invalid values (bad glob, volume out of range) without writing
  - atomic write leaves the old file on failure
  - clash naming
  - fallback labels per slot
  - `muted`/`dnd` defaults and parsing
  - `config.default.toml` still parses
  - tier palette sync includes `settings.css`
- Behavior: the poller and update checker react to changed settings (with the existing runtime test fixtures),
  and the generator swap is honored by the next flavor call.
- `docs/manual-checklist.md`: open the window from the tray, edit each section, check that the change applies
  without a restart, that tray and window stay in sync, that a hand edit while the window is open shows up, and that
  choosing, previewing and clearing a sound works.

## Calls made by Claude (not discussed with the user)

- No Save button: changes save on change, with a confirming status line (shown in the mockups; not objected to).
- Mute/DND persisted in `config.toml` as `sound.muted` / `announce.dnd`.
- Picked sounds go into the type/tier folders and are pinned by name; clearing never deletes files.
- `config.toml` is watched so hand edits apply live and the window can't overwrite them.
- Claude-flavor and GitHub on/off, model and update cadence are made live rather than flagged "restart required".
- Repos section has a folder picker plus free-text patterns, validated inline.
- A broken `config.toml` puts the window into a read-only error panel. The tray toggles still work for the session,
  and nothing overwrites the file (from the reviewer pass).
- ▶ on an unpinned slot plays what an unlock would play: a random pick from its pool, or nothing if "none".
- Range checks that parsing doesn't enforce (volume 0-1, minutes ≥ 1, timeout ≥ 1, check_hours ≥ 0) are validated in
  `settings_set`.

## Left to the builder

Exact window size and spacing, field widgets beyond what's specified, status-line wording, how the snapshot is
shaped in JSON, and whether the pure helpers live in `rpg-core` or `rpg-app`.
