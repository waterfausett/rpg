# RPG: Dev XP & Achievements (v0 design)

## Intent

A whimsical, personal RPG layer over day-to-day dev work. You earn XP and levels for real activity (commits, pushes, PRs, reviews, Claude Code sessions), and you get *Dungeon Crawler Carl*-style achievements: snarky, specific, sometimes absurd, announced with a sound byte and a big banner that interrupts you. The interruption is the point. It's for one person only, runs locally, and is built for fun.

There will eventually be two machines: a Windows 11 desktop (where v0 is built) and a work MacBook Pro. Cross-platform is the target end state.

## Goals (v0)

- Capture git, GitHub and Claude Code activity into one local event log.
- Turn events into XP, levels and achievements through rules defined as data.
- Show a small, ephemeral "+N exp" pop-up in the screen corner, plus a large Gold Banner for achievements that plays a user-supplied sound.
- Serve a local stats site (a "character sheet") for browsing in detail.
- Use an LLM (Claude Haiku 5.5) only for announcer flavor text, and only where it helps.

## Non-goals (v0)

These are tracked as GitHub issues: LLM-judged achievements, skills, loot boxes, a leaderboard of other people's activity, macOS packaging, syncing between machines.

## Anti-goals (failures even if it "works")

- **Never break or slow down git.** Hooks must be fast, must never fail a git operation, and must pass through to the repo's own hooks (husky, pre-commit, etc.). This is critical on the work machine.
- **Never steal focus** or intercept keystrokes. Pop-ups are click-through, except that the banner can be clicked to dismiss.
- **No achievement spam.** Banners are rare and queued one at a time; frequent events only give small XP pop-ups.
- **Nothing leaves the machine** except the optional LLM flavor-text call, which sends minimal metadata (event type, repo name, achievement name, a few stats) and can be turned off in config.
- **No team visibility or surveillance vibes.** It's personal.

## Approaches considered

1. **Electron:** one TypeScript codebase, easy see-through overlays, fastest to iterate. It weighs ~150 MB and a few hundred MB of RAM.
2. **Tauri v2 (chosen):** a Rust core with web UI using the OS webview, ~10 MB, light on memory. It requires Rust, and see-through windows on macOS need the private-API flag.
3. **OS-native notifications:** close to zero effort, but they can't do the Gold Banner or custom sounds. Rejected because they kill the charm.

**Picked: Tauri.** The user chose it because the work Mac already runs many resident apps, so the memory savings matter.

## Architecture

```
git hooks ─┐
Claude Code hooks ─┼──> emitter ──> inbox (append-only event files) ──┐
GitHub poller (in app) ────────────────────────────────────────────────┤
                                                                       v
                                              Tauri tray app (Rust core)
                                              ├─ ingest + dedupe -> SQLite
                                              ├─ rules engine (TOML rules + built-in detectors)
                                              ├─ XP / levels / records
                                              ├─ flavor text (static, or `claude -p --model haiku`)
                                              ├─ overlay windows (XP pop-ups, Gold Banner + sound)
                                              └─ localhost stats site (character sheet)
```

- **The emitter is thin and offline-safe.** Hooks only record an event; they never talk to the app. When the app isn't running, events wait in the inbox and get processed on next start. Achievements earned while it was closed are announced on startup with a "while you were away…" framing; only the top few get banners, and the rest are summarized.
- The emitter is a subcommand of the app binary (`rpg emit <type> --json ...`), which avoids fragile JSON escaping in shell. Each event gets a unique ID. Writes must be safe when several processes write at once (e.g. one file per event, or atomic appends). The exact mechanism is the builder's call.
- **Storage:** SQLite in the OS app-data dir holds processed events, XP, unlocked achievements and personal records. The inbox lives next to it. User-editable files (rules, sounds, config) live in a user config dir that the tray menu can open.

## Event sources

### Git hooks (global)
- Installed through global `core.hooksPath` pointing at our hook dir. The app's tray menu or `rpg install-hooks` sets it up, and `rpg uninstall-hooks` restores the previous value.
- `post-commit` records commit metadata (repo, branch, hash, timestamp, files changed and lines added/removed, plus the last-modified date of each touched file for the "ancient file" detector, computed cheaply or deferred to the app).
- `pre-push` records the push. It must forward stdin and preserve the chained hook's exit code.
- **Chaining:** after recording, each hook runs the repo's own `.git/hooks/<name>` if present and returns its exit code. Recording failures are swallowed and logged.
- **Known limitation:** repos that set a *local* `core.hooksPath` (e.g. husky) override the global one, so our hooks won't fire there. That's accepted for v0; the GitHub poller still catches PR activity. We record this in an issue.
- Hook scripts are POSIX `sh`. Git for Windows runs these and so does macOS.

### Claude Code hooks
- Installed into user-level `~/.claude/settings.json` by `rpg install-hooks`. The change merges into existing settings and never clobbers them.
- v0 events: `SessionStart` and `Stop`/`SessionEnd` (session duration). The builder may add a cheap one or two, like a Bash command that runs tests and passes.

### GitHub poller
- Runs inside the app every ~10 minutes (configurable) and uses the user's existing `gh` login (`gh api`), so there's no separate token to manage.
- v0 events: PRs you opened, PRs you authored that got merged, reviews you submitted. Deduped by GitHub IDs.
- **First run / backfill (decided: middle ground):** import past GitHub activity only to seed records and baselines, so `personal_record` and `first` detectors are meaningful from day one. Backfilled history grants **no XP and no achievements**; the character starts at level 1.

## Rules as data

Achievements are TOML files in the rules dir and get hot-reloaded. Each one has:

- `id`, `name`, `tier` (bronze/silver/gold for styling and XP), `xp`
- `fallback`: static flavor text. It shows whenever LLM text isn't used, and it's the voice/tone reference for the LLM.
- `trigger`: the event type(s) that cause it to be checked
- `detector`: one of a small set of built-in Rust detectors, with params
- `repeatable`: whether it can be earned more than once, with an optional cooldown
- `flavor`: `static` (default), or `llm` with a prompt template. LLM is opt-in per achievement. It earns its keep on repeatable achievements and on unlocks with interesting context. LLM failure or timeout falls back to `fallback`.

**Context passed to the LLM** (available to the prompt template as `{context}`, plus `{fallback}`, `{name}`):
- what the detector found, in structured form (e.g. `file_age` → file path, years untouched, last author; `personal_record` → old best, new best; `count_in_window` → count, window)
- the triggering event's basic metadata (event type, repo, branch, time of day)
- **unlock count**: how many times this achievement has been earned, including this one, and when it was last earned. The prompt can use it for escalating or weary-announcer callbacks ("again?"). How it's used is left open for now.

The prompt always tells the LLM to keep the same voice as `{fallback}`, to write 1–2 sentences, and to return text only.

v0 built-in detectors:
- `count_in_window`: N events of type X within a window (e.g. 3 PRs in a calendar day)
- `personal_record`: a metric exceeds your previous best (e.g. PRs merged in a day, lines deleted in one PR)
- `file_age`: a touched file was last modified more than N years ago
- `time_of_day`: an event between given hours
- `diff_size`: total changed lines above or below thresholds
- `first`: the first event of a type ever, or in a new repo

Example:
```toml
id = "archaeologist"
name = "Archaeologist"
tier = "bronze"
xp = 500
trigger = ["commit"]
detector = { type = "file_age", min_years = 5 }
repeatable = false
fallback = "You modified a file nobody has touched in over five years. You are now legally responsible for it."
flavor = { type = "llm", prompt = "Rewrite this achievement announcement for this specific unlock, in the same voice: \"{fallback}\". Use the concrete details. Context: {context}" }
```

### Starter achievements (~10)
1. **First Blood:** first commit tracked by the system
2. **Hat Trick:** 3 PRs opened in one day
3. **Personal Best:** most PRs merged in a day, ever (repeatable when beaten)
4. **Archaeologist:** touched a file last modified 5+ years ago
5. **Night Shift:** committed between midnight and 4am
6. **The Leviathan:** a PR with 1,000+ changed lines
7. **Surgical Strike:** a commit that changes exactly one character or line
8. **Demolitions Expert:** a PR that deletes far more than it adds
9. **New Territory:** first commit in a repo you've never committed to
10. **Peer Pressure:** submitted 5 reviews in one day

Exact thresholds and text are the builder's call; they're tunable data anyway.

## XP and levels

- Fixed XP per event type, set in config. Defaults (my call): commit 10, push 25, PR opened 50, PR merged 100, review 40, Claude session completed 15.
- Achievement XP comes on top, by tier.
- Level curve (my call): XP to reach level *n* = `round(100 * n^1.5)` cumulative. Tunable in config.
- Level-up is a moment: it uses a banner style variant and plays a level-up sound if one is supplied.

## Overlay (look approved in mockups)

- **XP pop-up:** a small gold pill in the bottom-right of the primary display, e.g. "+50 exp · PR opened". It lasts ~3s and fades. Multiple pop-ups stack upward. Click-through, silent.
- **Gold Banner (style B):** a radial dark-gold gradient with a gold border and glowing serif title. It reads "✦ NEW ACHIEVEMENT ✦", then the name, then italic flavor text, then the reward line. It shows top-center for ~8s or until clicked, and plays a sound. Only one at a time; others queue.
- Windows are transparent, borderless, always on top, never take focus, and are excluded from the taskbar or Dock.
- **Sounds:** the user drops audio files in `sounds/`. Config maps achievements, tiers or level-ups to files; a random file is the fallback. Volume setting.
- **Tray menu:** open character sheet, mute sounds, **do not disturb** (pause pop-ups and queue them, for screen-sharing on the work machine), open rules folder, install/uninstall hooks, quit.

## Stats site

Served by the app on a localhost port and opened from the tray. v0 pages:
- **Character sheet:** level, XP bar, totals, recent activity
- **Achievements:** unlocked ones with date and flavor text; locked ones shown as "???" with a vague hint
- **Event log:** a filterable list of events and the XP each earned

Styling should match the Gold Banner theme. Layout is the builder's call, following the approved wireframe.

## LLM flavor text

- Generated by `claude -p --model haiku "<prompt>"` with a timeout (~10s). This uses the existing Claude Code login, so there's no API key.
- Used only for achievements with `flavor = llm`. The banner waits for the text up to the timeout, then falls back to `fallback`. Generated text is stored with the unlock so it doesn't change.
- Config switch: `llm.enabled`. If `claude` isn't on PATH, the app quietly uses static text.

## Platform

- v0 is built and verified on Windows 11. The code stays portable (no Windows-only APIs outside one small platform module) so macOS is a packaging and testing task, not a rewrite.
- macOS needs Tauri's `macOSPrivateApi` for transparent windows. That's tracked in an issue.
- Autostart at login is nice to have in v0 if it's cheap with Tauri's autostart plugin; otherwise it goes to an issue.

## Left to the builder

The emitter's file format and how concurrent writes stay safe, the SQLite schema, the Tauri project layout, the site framework (keep it light), the exact rule TOML schema beyond the fields above, achievement thresholds and copy, the overlay animation details, the port choice, and test strategy.
