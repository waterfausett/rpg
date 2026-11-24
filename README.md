# rpg

Your commits finally get the recognition they deserve. **rpg** is a small tray app that turns dev work into an RPG: XP and levels for commits, pushes, PRs, reviews and Claude Code sessions, plus *Dungeon Crawler Carl*-style achievements with a gold banner, a sound byte and a snarky announcement. It runs entirely on your machine, with a character-sheet page on localhost.

Design: [`docs/superpowers/specs/2026-11-23-xp-achievements-design.md`](docs/superpowers/specs/2026-11-23-xp-achievements-design.md). On-screen checks to do by hand: [`docs/manual-checklist.md`](docs/manual-checklist.md).

## Build

Requirements: Rust, `cargo install tauri-cli`, and (Windows) WebView2. Optional: `gh` (GitHub events) and `claude` (flavor text).

```
cargo test --workspace
cargo build --release -p rpg-cli     # the `rpg` CLI (target/release/rpg.exe)
cd crates/rpg-app && cargo tauri build
```

Output: `target/release/rpg-app.exe` plus installers in `target/release/bundle/{msi,nsis}/`.

The tray's "Install hooks" looks for `rpg.exe` **next to `rpg-app.exe`** (the hook shims call it). The Tauri bundle does not include the CLI yet (see Known limitations), so keep both binaries in one folder, or use the CLI directly.

## Install and first run

1. Put `rpg-app.exe` and `rpg.exe` in the same folder and launch `rpg-app.exe`. A tray icon appears. The first run creates the data and config dirs, seeds starter achievements and runs a read-only GitHub backfill.
2. Install the hooks, either:
   - tray icon, **Install hooks**, or
   - `rpg install-hooks`

   This sets a global git `core.hooksPath` to rpg's shim folder and adds `SessionStart`/`SessionEnd` entries to `~/.claude/settings.json` (a backup is written next to it as `settings.json.rpg-backup`, and your other settings are kept). Every git hook name gets a pass-through shim, so a repo's own `.git/hooks/*` keep working.
3. Commit something. A `+10 exp · commit` pop-up appears bottom-right; unlocks show a gold banner top-center.

Tray menu: Open character sheet, Mute sounds, Do not disturb, Open rules folder, Install hooks, Uninstall hooks, Quit. Only one app instance runs at a time. Hooks never fail a git command; errors go to `rpg.log`.

## Uninstall

`rpg uninstall-hooks` (or tray **Uninstall hooks**) restores your previous `core.hooksPath` (or unsets it) and removes only rpg's entries from `settings.json`. Then delete the app. Your data stays until you delete the data dir.

## Where things live

| What | Windows | macOS (not yet supported) |
| --- | --- | --- |
| Data: `rpg.db`, `inbox/`, `git-hooks/`, `rpg.log` | `%APPDATA%\rpg\data` | `~/Library/Application Support/rpg` |
| Config: `config.toml`, `rules/`, `sounds/` | `%APPDATA%\rpg\config` | `~/Library/Application Support/rpg` |

(Resolved with `directories::ProjectDirs::from("", "", "rpg")`.) Set `RPG_HOME=<dir>` to put both under `<dir>/data` and `<dir>/config` instead. That is handy for trying things out without touching your real character.

Set `RPG_INTERNAL=1` to make `rpg emit` a no-op (the app does this for its own `claude` calls).

## Sounds

Drop `.wav`, `.mp3`, `.ogg` or `.flac` files into the `sounds/` folder. With no mapping, a random file plays on each banner. To choose, name files in `config.toml`. Lookup order is `by_rule`, then `by_tier`, then `achievement` / `level_up`, then a random file:

```toml
[sound]
volume = 0.8                       # 0.0 - 1.0
achievement = "fanfare.wav"        # default for achievements and summaries
level_up = "levelup.mp3"
by_tier = { bronze = "ding.wav", silver = "chime.wav", gold = "gong.wav" }
by_rule = { new_territory = "explore.ogg" }
```

Names that don't exist in `sounds/` are skipped. Tray **Mute sounds** silences everything.

## Adding an achievement

Rules are TOML files in `rules/` (hot-reloaded; a broken file is skipped with a line in `rpg.log` and the others keep working). One achievement per file, any filename ending in `.toml`:

```toml
id = "my_rule"                 # unique; first file wins on duplicates
name = "Midnight Oil"          # shown once unlocked ("???" before)
tier = "silver"                # bronze | silver | gold (default xp 250 / 500 / 1000)
xp = 600                       # optional; overrides the tier default
fallback = "Static text shown when there is no LLM flavor."
hint = "Shown while still locked."          # optional
trigger = ["commit"]           # commit, push, pr_opened, pr_merged,
                               # review_submitted, claude_session_start, claude_session_end
detector = { type = "time_of_day", start_hour = 0, end_hour = 4 }
repeatable = true              # default false (unlock once ever)
cooldown_hours = 24            # optional; min hours between repeat unlocks
flavor = { type = "llm", prompt = "Rewrite \"{fallback}\" for the {name} achievement. Context: {context}" }
```

`flavor` is optional and defaults to `{ type = "static" }` (always show `fallback`). With `type = "llm"`, `{fallback}`, `{name}` and `{context}` in `prompt` are substituted. If the LLM is off, unavailable or slower than `llm.timeout_secs`, `fallback` is used.

Detectors (set `type` plus the params):

| type | params | fires when |
| --- | --- | --- |
| `first` | `scope = "ever"` or `"repo"` | first event ever / first commit in this repo |
| `count_in_window` | `count`, `window = "day"` | N matching events in the local day |
| `personal_record` | `metric = "prs_merged_per_day"`, `"commits_per_day"` or `"pr_deletions"` | a new personal best |
| `file_age` | `min_years` | a commit touches a file untouched for that long |
| `time_of_day` | `start_hour`, `end_hour` (local, end exclusive) | event falls in that window |
| `diff_size` | any of `min_lines`, `max_lines`, `min_delete_ratio` | commit size / deletion share matches |

The starter rules in `rules/` (copied from `crates/rpg-core/assets/rules/` on first run) are working examples.

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

[site]
port = 47474        # character sheet at http://127.0.0.1:47474
```

Config is read at startup, so restart the app after editing it.

## Privacy

- Everything is stored locally. The character sheet listens on `127.0.0.1` only and rejects requests with any other `Host` header.
- The only thing that leaves your machine is the optional flavor call (`claude -p --model haiku`), and only for rules with `flavor = { type = "llm" }`. It sends your rule's prompt plus: event type, repo path, branch, local time, the detector's findings (e.g. file path, file age, line counts) and unlock counts. Never code or diffs. It runs in an empty directory with no tools, MCP servers or settings. Set `llm.enabled = false` to turn it off.
- GitHub data comes from your own logged-in `gh` CLI (read-only queries). Backfilled history seeds records only: it grants no XP or achievements.

## Known limitations

- Repos with their own local `core.hooksPath` (e.g. husky) override the global one, so rpg does not see their commits ([#7](../../issues/7)).
- Repo-local `reference-transaction` and `post-index-change` hooks do not run while rpg is installed (no shim for those two names, to keep git fast) ([#7](../../issues/7)).
- A push is counted when `pre-push` runs, before the push is known to have succeeded.
- macOS is not supported yet ([#4](../../issues/4)).
- The Tauri bundle does not contain `rpg.exe`, so the tray's "Install hooks" only works when `rpg.exe` sits beside `rpg-app.exe` (e.g. run from `target/release/`). Use `rpg install-hooks` from the CLI otherwise.
- Do not disturb and Mute are not remembered across restarts. No autostart at login yet ([#10](../../issues/10)).
