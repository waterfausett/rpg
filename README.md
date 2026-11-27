# rpg

Your commits finally get the recognition they deserve. **rpg** is a small tray app that turns dev work into an RPG: XP and levels for commits, pushes, PRs, reviews and Claude Code sessions, plus *Dungeon Crawler Carl*-style achievements with a gold banner, a sound byte and a snarky announcement. It runs entirely on your machine, with a character-sheet page on localhost.

Design: [`docs/superpowers/specs/2026-11-23-xp-achievements-design.md`](docs/superpowers/specs/2026-11-23-xp-achievements-design.md). On-screen checks to do by hand: [`docs/manual-checklist.md`](docs/manual-checklist.md).

## Build

Requirements: Rust, `cargo install tauri-cli`, and (Windows) WebView2. Optional: `gh` (GitHub events) and `claude` (flavor text).

```
cargo test --workspace
cd crates/rpg-app && cargo tauri build
cargo build --release -p rpg-cli     # optional: the `rpg` terminal CLI
```

Output: `target/release/rpg-app.exe` plus installers in `target/release/bundle/{msi,nsis}/`. The app is the only binary you need: the hooks call it as `rpg-app emit …`, which records the event and exits without starting the tray. The separate `rpg` CLI does the same jobs from a terminal, with printed output.

## Install and first run

1. Download `rpg_<version>_x64-setup.exe` from the [latest release](../../releases/latest) (or build it yourself), install and launch it. A tray icon appears. The first run creates the data and config dirs, seeds starter achievements and runs a read-only GitHub backfill.
2. Install the hooks, either:
   - tray icon, **Install hooks** (a pop-up confirms; a failure shows a banner with the reason), or
   - `rpg install-hooks` (hooks then call `rpg.exe` instead of the app)

   This sets a global git `core.hooksPath` to rpg's shim folder and adds `SessionStart`/`SessionEnd` entries to `~/.claude/settings.json` (a backup is written next to it as `settings.json.rpg-backup`, and your other settings are kept). Every standard git hook except `reference-transaction` and `post-index-change` (see Known limitations) gets a pass-through shim, so a repo's own `.git/hooks/*` keep working.
3. Commit something. A `+10 exp · commit` pop-up appears bottom-right; unlocks show a gold banner top-center. To write your own achievements, see the [rule format reference](docs/rules.md).

Tray menu: Open character sheet, Settings…, Mute sounds, Do not disturb, Open rules folder, Install hooks, Uninstall hooks, Check for updates, Quit. Only one app instance runs at a time. Hooks never fail a git command; errors go to `rpg.log`.

## Updates

The app checks the latest GitHub release a minute after startup and then every `updates.check_hours` (default 6; `0` turns background checks off). When there's a newer version, a pop-up says so and the tray item becomes **Install update vX.Y.Z**. Nothing installs until you click it. The installer then runs with a progress bar and restarts the app. Your data, config and hooks are kept, because the installer always uses the same folder (`%LOCALAPPDATA%\rpg`). **Check for updates** checks right away. Updates are signed, and the app refuses any download whose signature doesn't match the key built into it.

Debug builds never check in the background.

### Releasing

1. Bump `version` under `[workspace.package]` in the root `Cargo.toml` and merge that to `main`.
2. Tag the merge commit and push the tag: `git tag v0.2.0 && git push origin v0.2.0`.

The `Release` workflow checks that the tag matches the version, runs the tests, builds the signed installers and publishes a release with generated notes and `latest.json`. Installed apps pick it up on their next check.

The updater signing key is the `TAURI_SIGNING_PRIVATE_KEY` repo secret (no password). The maintainer keeps a copy at `~/.tauri/rpg-updater.key`, and the matching public key is in `tauri.conf.json`. If the key is lost, installed apps can't verify new releases, and everyone has to reinstall by hand once.

## Uninstall

`rpg uninstall-hooks` (or tray **Uninstall hooks**) restores your previous `core.hooksPath` (or unsets it) and removes only rpg's entries from `settings.json`. Then delete the app. Your data stays until you delete the data dir. Always run `uninstall-hooks` **before** deleting the data dir: the global `core.hooksPath` points into it, and if that folder is gone git silently runs no hooks at all, in any repo.

## Where things live

| What | Windows | macOS (not yet supported) |
| --- | --- | --- |
| Data: `rpg.db`, `inbox/`, `git-hooks/`, `rpg.log` | `%APPDATA%\rpg\data` | `~/Library/Application Support/rpg` |
| Config: `config.toml`, `rules/`, `sounds/` | `%APPDATA%\rpg\config` | `~/Library/Application Support/rpg` |

(Resolved with `directories::ProjectDirs::from("", "", "rpg")`.) Set `RPG_HOME=<dir>` to put both under `<dir>/data` and `<dir>/config` instead (an empty `RPG_HOME` counts as unset).

`RPG_HOME` does **not** isolate the hooks. `install-hooks` always changes your real global git config (`core.hooksPath`, pointed at `<dir>/data/git-hooks`) and your real `~/.claude/settings.json`; the installed hooks pass `--home <dir>` so their events land in `<dir>`. Only one set of hooks is active at a time, and you must run `rpg uninstall-hooks` with the same `RPG_HOME` **before** deleting `<dir>`, or git silently runs no hooks for any repo.

Set `RPG_INTERNAL=1` to make `rpg emit` a no-op (the app does this for its own `claude` calls).

## Sounds

Drop `.wav`, `.mp3`, `.ogg` or `.flac` files into the `sounds/` folder. With no mapping, a random file plays on each banner. Subfolders split that bag by type; files directly in `sounds/` are shared by every pool:

```
sounds/
  boom.mp3                  # any banner
  achievements/             # achievement banners
    gold/                   # only gold achievements (also silver, bronze, ...)
    night_shift/            # only the rule with this id
  levels/                   # level-up banners
```

To choose a file instead, name it in `config.toml`; a pin beats the pool at its own level. Lookup order for an achievement is the `by_rule` pin, `achievements/<rule id>/`, the `by_tier` pin, `achievements/<tier>/`, the `achievement` pin, then a random file from `achievements/` plus `sounds/`. Level-ups go `level_up` pin, then `levels/` plus `sounds/`. A pinned name is looked up in its own folder, then the type's folder, then `sounds/`:

```toml
[sound]
volume = 0.8                       # 0.0 - 1.0
achievement = "fanfare.wav"        # default for achievements and summaries
level_up = "levelup.mp3"
by_tier = { bronze = "ding.wav", silver = "chime.wav", gold = "gong.wav" }
by_rule = { new_territory = "explore.ogg" }
```

Names that don't exist in `sounds/` are skipped. Tray **Mute sounds** silences everything.

You can also set sounds in the Settings window. **Choose…** on a sound row opens a file dialog, copies the file into the matching folder (`sounds/achievements/`, `sounds/levels/` or `sounds/achievements/<tier>/`; a name clash gets a ` (2)` suffix and nothing is overwritten) and pins it. ▶ previews it (even when muted), and ✕ stops using it (the file stays in `sounds/`).

## Adding an achievement

Rules are TOML files in `rules/` (hot-reloaded; a broken file is skipped with a line in `rpg.log` and the others keep working). One achievement per file, any filename ending in `.toml`:

```toml
id = "my_rule"                 # unique; first file wins on duplicates
name = "Midnight Oil"          # shown once unlocked ("???" before)
tier = "silver"                # bronze | silver | gold | platinum | legendary | celestial (default xp 250 / 500 / 1000 / 2000 / 4000 / 8000)
xp = 600                       # optional; overrides the tier default
fallback = "Static text shown when there is no LLM flavor."
hint = "Shown while still locked."          # optional
trigger = ["commit"]           # commit, push, pr_opened, pr_merged,
                               # review_submitted, claude_session_start, claude_session_end
filter = { hours = { start = 0, end = 4 } }   # optional; which events count
counter = { type = "each" }    # optional, default "each"; what to do with them
repeatable = true              # default false (unlock once ever)
cooldown_hours = 24            # optional; min hours between repeat unlocks
flavor = { type = "llm", prompt = "Rewrite \"{fallback}\" for the {name} achievement. Context: {context}" }
```

`flavor` is optional and defaults to `{ type = "static" }` (always show `fallback`). With `type = "llm"`, `{fallback}`, `{name}` and `{context}` in `prompt` are substituted. If the LLM is off, unavailable or slower than `llm.timeout_secs`, `fallback` is used.

Filters used by the starter rules: `hours = { start, end }` (local, end exclusive, may wrap midnight) and `lines = { min, max, min_delete_ratio }` (commit or PR size and deletion share).

Counters (set `type` plus the params):

| type | params | fires when |
| --- | --- | --- |
| `each` | | every matching event |
| `first` | `scope = "ever"` or `"repo"` | first event ever / first commit in this repo |
| `per_day` | `at` | the `at`-th event in the local day |
| `personal_record` | `metric = "prs_merged_per_day"`, `"commits_per_day"` or `"pr_deletions"` | a new personal best |
| `file_age` | `min_years` | a commit touches a file untouched for that long |

**Upgrading from 0.2:** the old `detector = {...}` field is gone, and a rule file that still has it fails to load. After upgrading to this version, empty your `rules/` folder, `skills/` included; the app reseeds it on the next start (move custom rules or skills aside first and copy them back after).

The starter rules in `rules/` (copied from `crates/rpg-core/assets/rules/` on first run; starters added by later versions are copied on upgrade, and one you delete stays deleted; a `rules/` folder with no rule files left in it is reseeded in full on the next start) are working examples.

## Skills

Skills show where your time goes: *Rust Lv 12*, *YAML Wrangling Lv 7*, *Code Review Lv 4*. Each event that earns character exp also gives skill exp to every skill it matches. Skill exp is kept apart from your character exp (nothing counts twice), and skills start from zero (history is not replayed). The character sheet lists them biggest first, and a level-up shows a small `▲ Rust Lv 12` pop-up.

Skills are TOML files in `rules/skills/`, one per file, hot-reloaded like achievements. A starter set (languages, YAML, config, docs, tests, reviews, Claude sessions, merged PRs) is copied from `crates/rpg-core/assets/skills/` whenever that folder is empty, so deleting a single file sticks.

```toml
id = "typescript"              # unique; first file wins on duplicates
name = "TypeScript"
globs = ["*.ts", "*.tsx"]      # optional; at least one file in the commit must match
repos = ["**/my-app"]          # optional; the repo (path or owner/name) must match
events = ["commit"]            # optional, default ["commit"]; any event kind from `trigger` above
xp = 10                        # optional; skill exp per matching event (default 10)
```

Globs are case-insensitive and `*` crosses folders, so `*.ts` matches `src/ui/app.ts`; `**/tests/**` matches any file under a `tests` folder. Only commits carry file lists, so `globs` require `events = ["commit"]`. A skill with no `globs` or `repos` counts every event of its kinds (e.g. `events = ["review_submitted"]`). Languages are matched by file extension. New data sources (e.g. chat or calendar, [#11](../../issues/11)) bring their own event kinds, so skills can name those too.

## config.toml reference

Everything is optional; missing keys use these defaults.

```toml
[xp]
commit = 10
push = 25
pr_opened = 50
pr_merged = 100
review = 40
claude_session = 15

[levels]            # cumulative XP to reach level n = round(base * (n-1)^exponent)
base = 1000.0
exponent = 1.5

[skill_levels]      # same formula as [levels], for skills
base = 100.0
exponent = 1.5

[github]
enabled = true      # needs the `gh` CLI, logged in
poll_minutes = 10

[llm]
enabled = true      # flavor text via `claude -p`
timeout_secs = 10
model = "haiku"

[sound]             # see "Sounds"
volume = 0.8
muted = false       # tray Mute sounds; remembered across restarts

[announce]          # see the tray's Do not disturb
dnd = false         # remembered across restarts

[site]
port = 47474        # character sheet at http://127.0.0.1:47474

[paths]             # which repos count; see "Including and excluding paths"
include = []        # empty = everywhere
exclude = []

[updates]
check_hours = 6     # background update checks; 0 = only from the tray
```

Changes you make in the Settings window (tray **Settings…**) are saved to `config.toml` as you make them, with your comments kept, and apply right away. A hand edit of the file applies right away too, even while the window is open. The one exception is `[site] port`, which needs a restart. `[xp]`, `[levels]`, `[skill_levels]` and `[site]` are only edited in the file.

### Including and excluding paths

Events from an excluded folder count for nothing: no exp, skills, records or achievements, and they are not stored (so they stay off the character sheet too).

```toml
[paths]
exclude = ['C:\Users\me\AppData\Local\Temp']   # scratch repos, tool sandboxes
include = ['C:/work', 'octo/*']                # optional: only these count
```

- Patterns are matched against the event's repo: the local folder for commits, pushes and Claude sessions, `owner/name` for GitHub events.
- A plain folder (no `*`, `?`, `[` or `{`) covers itself and everything under it. Anything else is a glob, as in skills: case-insensitive, `*` crosses folders.
- `\` or `/`, drive-letter case and a trailing slash don't matter. Use single quotes in TOML so backslashes stay literal.
- `exclude` wins over `include`. A non-empty `include` drops everything that doesn't match it. Events with no repo always count.
- Only new events are filtered; past ones keep their exp. A bad pattern makes the whole config invalid (the app logs it to `rpg.log` and uses defaults).

## Privacy

- Everything is stored locally. The character sheet listens on `127.0.0.1` only and rejects requests with any other `Host` header.
- Update checks download `latest.json` (and, when you install, the installer) from this repo's GitHub releases. They send nothing about you beyond the request itself. Set `updates.check_hours = 0` to stop background checks.
- The only data that leaves your machine is the optional flavor call (`claude -p --model haiku`), and only for rules with `flavor = { type = "llm" }`. It sends your rule's prompt plus: event type, repo name (never the local path), branch, local time, the counter's findings (e.g. file path, file age, line counts) and unlock counts. Never code or diffs. It runs in an empty directory with no tools, MCP servers or settings. Set `llm.enabled = false` to turn it off.
- GitHub data comes from your own logged-in `gh` CLI (read-only queries). Backfilled history seeds records only: it grants no XP or achievements.

## Known limitations

- Repos with their own local `core.hooksPath` (e.g. husky) override the global one, so rpg does not see their commits ([#7](../../issues/7)).
- Repo-local `reference-transaction` and `post-index-change` hooks do not run while rpg is installed (no shim for those two names, to keep git fast) ([#7](../../issues/7)).
- A push is counted when `pre-push` runs, before the push is known to have succeeded.
- macOS is not supported yet ([#4](../../issues/4)).
- The installed hooks point at the exact `rpg-app.exe` (or `rpg.exe`) path they were installed from. If you move or reinstall the app somewhere else, run **Install hooks** again.
- No autostart at login yet ([#10](../../issues/10)).
