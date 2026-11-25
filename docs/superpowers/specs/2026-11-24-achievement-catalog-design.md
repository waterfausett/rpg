# Expanded achievement catalog (design)

Issue: #2. Related: #26 (`enabled` flag and seeding). Builds on [`2026-11-23-xp-achievements-design.md`](2026-11-23-xp-achievements-design.md).

## Intent

v0 ships 10 achievements, which isn't enough to keep the game interesting. Grow the catalog to about 50: silly ones, rare ones, and tiered ladders (Bronze/Silver/Gold of the same feat) that make steady progress visible. Banners stay a moment: frequent unlocks show as small toasts, and how often things fire stays easy to tune without code.

## Goals

- About 50 starter achievements (the catalog below), shipped in the repo and delivered to existing installs on upgrade.
- A rule format where most achievements are pure TOML: a **filter** (which events count) plus a **counter** (what to do with them).
- Tiered series: one rule, several thresholds, each step its own unlock, with progress shown on the character sheet.
- A per-achievement **announce** setting (banner or toast), defaulted by tier in config.
- Hidden achievements that don't show on the character sheet until earned.
- Commit messages recorded locally, so message-based achievements work.

## Non-goals

- **Revert Revert** (reverting a revert) is on hold. It would need special code for one achievement.
- An expression language or scripting in rules.
- Changing XP per event, the level curve, or the skills system.

## Anti-goals

- **No banner spam.** If a normal workday produces more than one or two banners, the defaults are wrong.
- **Commit messages and PR titles never leave the machine by default.** They are kept out of the LLM flavor context unless a rule opts in.
- **Losing earned history.** Events, XP and unlocks in the database survive the upgrade.

**Decided: no compatibility for v0 rule files.** There's one user, and they're fine with resetting their install. The `detector` field is removed and the 10 v0 starter rules are rewritten in the new format. After upgrading, the user empties their `rules/` folder (the README's upgrade note says so) and the app reseeds it.

## Constraints (from the code today)

- Every unlock becomes a banner (`presenter.rs`); there's no toast path for achievements.
- `tier` is a per-rule label for style and XP; nothing links rules into a series.
- Detectors (`detectors.rs`) are a closed enum of six; `count_in_window` only knows a local day.
- Commit events (`emit.rs commit_event`) carry hash, branch, files with line counts, but **no message**. PR events (`github.rs parse`) carry number, title, url, additions, deletions, but **not** created/merged timestamps in `data`. Push events know the current branch, not the pushed ref.
- Starter rules are seeded only when `rules/` is empty (`paths.rs seed`), so existing installs never get new ones.
- Backfilled events are stored (they feed records) but never evaluate rules or earn XP.

## Approaches considered

- **A. One new detector type per idea.** Simple and typed, but combinations multiply (lifetime count of commits touching tests, lifetime count of commits saying "typo"…), so most new ideas need Rust.
- **B. A small expression language** (`when = "commit.message ~ /wip/ && hour == 13"`). Maximum freedom, but a parser and evaluator to maintain, and worse error messages, for a one-person app.
- **C. Filters plus counters. Chosen.** A rule's filter says which events count, using optional typed conditions that must all match. Its counter says what to do with the matching events. Most of the catalog becomes TOML, and the Rust stays a small, typed, testable set.

## Design

### Rule format

```toml
id = "test_believer"
name = "Test Believer"
trigger = ["commit"]
filter = { files_any = ["tests/**", "*_test.*", "*.test.*", "*.spec.*"] }
counter = { type = "lifetime" }
tiers = [
  { tier = "bronze", at = 10 },
  { tier = "silver", at = 100 },
  { tier = "gold", at = 500 },
]
hint = "Trust, but verify. Mostly verify."
fallback = "..."
```

- A rule has either `tier` (single achievement) or `tiers` (series), not both.
- `filter` is optional (absent means every triggering event matches).
- `counter` defaults to `each`.
- The old `detector = {...}` field is gone. A file that still has it fails to load with a clear error.
- New optional fields: `announce` (`"banner"` / `"toast"`), `hidden` (bool), `share_text` (bool, see Privacy). `enabled` comes from #26.
- Existing fields (`repeatable`, `cooldown_hours`, `xp`, `flavor`, `hint`, `fallback`) keep their meaning.

### Filters

All optional; every one given must match. Conditions that don't apply to the event kind (e.g. `files_any` on a review) mean "no match", and a rule that combines a filter with a trigger it can never match is a load error, like skills' commit-only `globs`.

| Filter | Meaning |
| --- | --- |
| `message` | regex, case-insensitive, against the commit message |
| `title` | regex, case-insensitive, against the PR title |
| `branch` | glob list against the branch |
| `repo` | glob list against the repo |
| `files_any` | at least one file matches a glob (reuse the skills globset code) |
| `files_only` | every file matches a glob |
| `min_files` / `max_files` | file count |
| `min_extensions` | distinct file extensions |
| `lines` | `{ min, max, min_delete_ratio }`, same meaning as today's `diff_size` |
| `hash` | regex against the full commit hash |
| `hash_palindrome` | the first N hash characters read the same backwards |
| `weekdays` | e.g. `["sat", "sun"]`, local time |
| `hours` | `{ start, end }`, local, end exclusive, may wrap midnight (today's `time_of_day`) |
| `time` | exact local `"HH:MM"` |
| `dates` | local `"MM-DD"` list |
| `pr_open_minutes` | `{ min, max }`, merged minus created, on `pr_merged` |
| `session_minutes` | `{ min, max }`, on `claude_session_end` with a known start |
| `open_sessions` | `{ min }`, Claude sessions open at the same time, on `claude_session_start` |

### Counters

| Counter | Fires when |
| --- | --- |
| `each` | any matching event |
| `per_day` `{ at }` | the matching count for the local day reaches `at` (exactly once per day, like today's `count_in_window`) |
| `lifetime` `{ at }` or tiers, optional `sum = "<field>"` or `distinct = "repo"` | the running count (or sum, or distinct count) of matching events reaches a threshold |
| `streak_days` `{ at }` or tiers | consecutive local days with at least one matching event reach a threshold |
| `month_every_day` | a matching event on every day of a calendar month |
| `gap_days` `{ min, scope = "ever" \| "repo" }` | the first matching event after at least `min` days with none (in scope) |
| `run` `{ at }` | the last `at` events of the trigger kind all matched (consecutive) |
| `combo` `{ kinds, within_minutes }` | events of each listed kind happened in order within the window, ending with this one |
| `first` `{ scope = "ever" \| "repo" \| "year" }` | today's `first`, plus a calendar-year scope |
| `personal_record`, `file_age` | today's detectors, unchanged |
| `meta` `{ distinct_unlocks }` or `{ same_unlock }`, with tiers | counts of the player's own unlocks. Evaluated after the other rules for the same event. |

"Reaches" means count ≥ threshold for a step not yet unlocked (see Decisions). Threshold counters count all stored events, backfill included. Every counter returns structured findings for the LLM context (count, threshold, streak length, and so on), as detectors do today.

### Tiers

- **New tier: Platinum** (decided), above gold. Default 2,000 exp, announced with a banner, and its own color in the Gold Banner theme (banner, toast and character sheet). It ranks above gold in the storm collapse. It's usable by single achievements and ladders alike.
- A series is one rule with `tiers`. Each step is its own unlock with id `<rule_id>.<tier>` and that tier's XP default (or a per-step `xp`), shown as e.g. "Committed (Silver)".
- Each tier appears at most once per ladder, so a ladder has at most four steps. A repeated tier is a load error. If longer ladders are ever needed, add an optional per-step `key` that defaults to the tier name; existing unlock ids won't change.
- Because the step id comes from the tier, changing a step's tier makes it a new step, which unlocks again. Changing its `at` or `xp` is safe.
- One event can cross more than one step. Each crossed step unlocks.
- Steps are not repeatable. `repeatable` and `cooldown_hours` apply only to single-tier rules.
- The character sheet shows each series with its current step and progress ("Silver · 412 / 1,000"). Locked steps above the next one show as "???".

### Announcing: banner or toast

- New config section with defaults: `[announce] bronze = "toast"`, `silver = "banner"`, `gold = "banner"`, `platinum = "banner"`. A rule's `announce` overrides it, and a step in `tiers` may set its own.
- An achievement toast is a new toast style, distinct from XP toasts. It shows tier color, name and XP, plays no sound, and has no flavor text; the flavor text still appears on the character sheet. It uses the static fallback, so no LLM call.
- The existing storm and while-you-were-away collapsing counts only banners. Toasts join the existing toast flow and obey DND the same way.

### Hidden achievements

`hidden = true`: not listed on the character sheet until earned (no "???" row). The page may show "and N hidden achievements" as a count.

### Event data

- **Commit and push:** add `github_repo` (`owner/name` parsed from the `origin` remote, if it's GitHub). Commits also get `message` (the full message), captured in `commit_event`.
- **PRs:** add `created_at` and `merged_at` to `data` on both PR events.
- **Claude sessions:** at processing time, `session_end` with a known start gets the duration, and `session_start` gets the number of sessions open at the time. A start with no end within 12 hours (crashed sessions) stops counting as open. This is computed in the engine, not in the hook.
- Old events lack these fields. Filters treat a missing field as "no match".

### Privacy

Commit messages and PR titles are stored only in the local SQLite database. They are left out of the LLM flavor context unless the rule sets `share_text = true`. No starter rule sets it.

### Shipping and seeding (with #26)

- All catalog rules ship in `crates/rpg-core/assets/rules/`. The existing test that parses every starter rule covers them.
- On startup, seed any starter rule whose id has **never been seeded before**, tracked in a small manifest in the data dir (seeded ids). A user's edited files are never overwritten. A file the user deleted stays deleted. `enabled = false` (#26) is the non-destructive way to turn one off.
- Existing installs: on first start after upgrading, the manifest is created from the files currently in `rules/`. Starter ids not present are seeded. A user who already deleted a v0 starter will get it back once; that's acceptable.
- Update #26 to match this seeding approach (it currently says deletion would stop sticking).

### Build order (one PR each)

1. Event data: commit message, PR timestamps, Claude session duration and concurrency.
2. Filters and counters. The 10 v0 rules are rewritten in the new format and keep passing their tests.
3. Tiers, progress on the character sheet, `announce` and the achievement toast, `hidden`.
4. Seeding manifest and `enabled` (#26).
5. The catalog, in batches.

## Catalog

Tier shown for single achievements; ladders list their steps. Copy and exact thresholds are the builder's call, in the voice of the v0 fallbacks (snarky, specific, *Dungeon Crawler Carl*). "Rare" and "hidden" are as marked.

**Existing 10 (kept):** First Blood, Hat Trick, Personal Best, Archaeologist, Night Shift, The Leviathan, Surgical Strike, Demolitions Expert, New Territory, Peer Pressure.

**Clock and calendar**
- Weekend Warrior (bronze): commit on Sat or Sun.
- Dawn Patrol (bronze): commit 5–7am.
- 1337 (silver, rare): commit at 13:37.
- Make a Wish (bronze): commit at 11:11.
- Leap of Faith (gold, rare): commit on Feb 29.
- Read-Only Friday Violator (bronze): push while on `main`/`master`, Friday after 4pm.
- Holiday Spirit (silver): commit on Dec 25 or Jan 1 (dates editable).
- New Year, Same Bugs (bronze): first commit of the calendar year.

**Volume in a day**
- Speed Run (silver): 10 commits in a day.
- Rubber Stamp (silver): 10 reviews in a day.
- Codependent (silver): 10 Claude sessions in a day.

**Ladders**
- Committed: 100 / 1,000 / 5,000 commits.
- Merge Conflict of Interest: 10 / 100 / 500 PRs merged.
- Professional Critic: 25 / 250 / 1,000 reviews.
- Lumberjack: 1k / 10k / 100k lines deleted (sum).
- Frequent Flyer: commits in 5 / 25 / 100 distinct repos.
- Claude's Favorite: 50 / 500 / 2,000 Claude sessions.
- Spellcheck Paladin: 5 / 25 / 100 commits mentioning "typo".
- Ticket Machine: 10 / 100 / 500 commits referencing `#123`-style issues.
- Test Believer: 10 / 100 / 500 commits touching test files.
- On a Roll: 5 / 15 / 30-day commit streak.
- Achievement Hunter: 10 / 25 / 50 distinct achievements unlocked.
- Skill ladders: any skill reaches level 5 / 10 / 20. Needs a `skill_level` counter reading the skills system; the builder may fold it into `meta`.

**Streaks and rhythm**
- Touch Grass Denied (gold, rare): a commit every day of a calendar month.
- The Return (silver): first commit after 14+ days off.
- Necromancer (silver): commit in a repo untouched for a year.
- Full Combo (silver): commit, push and PR opened within 15 minutes.
- LGTM Speedrun (bronze): PR merged within 10 minutes of opening.
- The Thousand-Year Branch (silver): PR merged 30+ days after opening.
- Groundhog Day (silver): the same repeatable achievement earned 10 times.

**Claude sessions**
- Marathon (silver): a session over 3 hours.
- Speed Date (bronze): a session under 30 seconds.
- Pair Programming (silver): 3 sessions open at once.

**Commit messages and PR titles**
- WIP WIP WIP (bronze): three commits in a row with "wip".
- Famous Last Words (silver): "final", "fix for real", "should work" and similar.
- It's Always DNS (rare): a PR title mentioning DNS.
- Profanity Filter Disabled (bronze, hidden): swearing in a commit message.
- Eloquent (bronze): a one-word message.
- The Novelist (silver): a message over 500 characters. Needs a `message_length` filter; the builder adds it.
- Emotional Support Emoji (bronze): an emoji in a message.

**Files, branches, hashes**
- Polyglot (silver): one commit touching 4+ file types.
- Tiny Violence (bronze): a merged PR under 5 lines.
- Find and Replace All (silver): one commit touching 100+ files.
- Documentation Gremlin (bronze): a commit touching only Markdown.
- Lawyered (bronze, rare): editing a LICENSE file.
- Dependency Hell (bronze): a commit touching only lockfiles.
- Naming Things Is Hard (bronze, hidden): committing on a branch named `test`, `temp`, `asdf`, `fix2`.
- Self-Inflicted (bronze): pushing while on `main`/`master`.
- Palindrome (gold, rare): first 7 hash characters are a palindrome.
- Hash Brown (silver, rare): hash starts with `dead`, `beef`, `cafe` or `f00d`.
- Lucky Sevens (bronze, rare): `777` in a commit hash.

## Decisions

- **History counts, with no catch-up pass (decided).** Threshold counters (`lifetime`, `streak_days`, `meta`, skill ladders) use "count ≥ `at` and this step isn't unlocked yet", checked when the next matching event arrives. They don't require that event to cross the threshold. So stored history counts, including backfilled GitHub PRs and reviews, and a new, edited or re-enabled rule catches up the next time a matching event happens. One event may unlock several steps, and the normal storm collapse handles that. There's no load-time pass and no stored "counts from" date. Cost: one count query per ladder per matching event. History is small: hooks record only from install, and the GitHub import is capped at 100 PRs and 100 reviews. Moment counters (`each`, `per_day`, `run`, `combo`, `gap_days`, `first`) look only at the current event and never fire for history.

## My calls (correct any of these)

- **Skill ladders are per skill.** Each skill has its own 5 / 10 / 20 steps, with step ids `skill_master.<skill_id>.<tier>`. The character sheet groups them under one heading. A new skill file gets its ladder automatically.
- **Repeatability.** Moment achievements are repeatable. Ones tied to routine habits get `cooldown_hours = 168`: Weekend Warrior, Dawn Patrol, Make a Wish, Read-Only Friday Violator, Self-Inflicted, Eloquent, Emotional Support Emoji, Documentation Gremlin, Dependency Hell, Naming Things Is Hard and Profanity Filter Disabled. Per-day ones are naturally once a day, and calendar ones naturally once a year. Rare ones have no cooldown, because they're rare by nature. Tier XP defaults are unchanged; cooldowns are the throttle, and they're tunable.
- **"Rare" is not a field.** A rare achievement sets `announce = "banner"` in its TOML, so a rare bronze still gets the full moment. Catalog entries listed without a tier (It's Always DNS) are silver.
- **Full Combo requires one repo.** Commit and push events also record the `origin` remote as `owner/name` (`github_repo` in `data`), which is compared with the PR's repo. The combo is judged by event timestamps, not processing order, because PRs arrive by polling. It fires when the PR is fetched.

## Left to the builder

Exact TOML field names beyond those above, the storage for counters (query on demand vs. running tallies), the toast's look within the Gold Banner theme, copy and thresholds, regex flavor and length limits, how the site lays out series and progress, the manifest format, and test strategy.
