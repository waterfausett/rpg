# Achievement rule format

Each achievement is one TOML file in the rules folder (tray menu, **Open rules folder**). Files load in filename order. A file that fails to parse or validate is skipped and reported; the others still load. If two files use the same `id`, the first one wins and the second is reported as a duplicate.

A rule says which events count (`trigger` and `filter`), what to do with them (`counter`), and what it unlocks (`tier`, or `tiers` for a series).

```toml
id = "test_pilot"
name = "Test Pilot"
trigger = ["commit"]
filter = { files_any = ["tests/**", "*_test.*", "*.test.*", "*.spec.*"] }
counter = { type = "lifetime" }
tiers = [
  { tier = "bronze", at = 10 },
  { tier = "silver", at = 100 },
  { tier = "gold", at = 500 },
]
hint = "Trust, but verify. Mostly verify."
fallback = "Another commit that touches the tests."
```

## Rule fields

Unknown fields are a load error, so a typo is reported rather than silently ignored.

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | string | yes | Unique id: letters, digits, `_` and `-` only (no `.`, which separates ladder steps; see [Step ids](#step-ids)). Anything else is a load error. It is the stored unlock key, so don't rename it after unlocking. |
| `name` | string | yes | Shown in banners, toasts and on the character sheet. |
| `fallback` | string | yes | The announcement text. Used as is, or as the base of the LLM text when `flavor` is `llm`. |
| `trigger` | list of event kinds | yes | Which events the rule looks at (see [Event kinds](#event-kinds)). |
| `tier` | `"bronze"` / `"silver"` / `"gold"` / `"platinum"` | one of `tier`, `tiers` | A single achievement. |
| `tiers` | list of steps | one of `tier`, `tiers` | A series (ladder). Threshold counters only. See [Tiers](#tiers). |
| `at` | integer | threshold counters with `tier` | The threshold for a single threshold achievement. Not allowed with moment counters or with `tiers`. |
| `xp` | integer | no | XP for a single achievement. Defaults to the tier's XP. Not allowed with `tiers` (set `xp` per step). |
| `hint` | string | no | Shown for locked achievements on the character sheet. |
| `filter` | table | no | Conditions on the event; all of them must match. Absent means every triggering event matches. See [Filters](#filters). |
| `counter` | table | no | What to do with matching events. Defaults to `{ type = "each" }`. See [Counters](#counters). |
| `repeatable` | bool | no, default `false` | A single moment achievement can unlock again. Ignored by threshold counters, not allowed with `tiers`. |
| `cooldown_hours` | integer | no | With `repeatable`: the minimum hours between unlocks. Ignored by threshold counters, not allowed with `tiers`. |
| `flavor` | table | no, default `{ type = "static" }` | `{ type = "static" }` shows `fallback`. `{ type = "llm", prompt = "..." }` asks the configured LLM to rewrite it; `{fallback}`, `{name}` and `{context}` in the prompt are replaced. Any LLM failure falls back to `fallback`. |
| `announce` | `"banner"` / `"toast"` | no | How the unlock is announced. Overrides the tier default. A step in `tiers` may set its own. (Banner/toast selection and the `[announce]` tier defaults in config are coming in this release.) |
| `hidden` | bool | no, default `false` | Not listed on the character sheet until earned (no "???" row). |
| `share_text` | bool | no, default `false` | Allows the commit message or PR title into the LLM flavor context. Otherwise they stay in the local database only. |
| `enabled` | bool | no, default `true` | `enabled = false` turns a rule off without deleting its file. The rule still loads and is validated, and the site lists it with an "off" badge, but the engine never evaluates it (meta and skill-level rules included). Turn it back on and a threshold ladder catches up on the next matching event. |

## Event kinds

| Kind | From | Useful data |
| --- | --- | --- |
| `commit` | git hook | `message`, `hash`, `branch`, `files` (`path`, `added`, `removed`), `lines_added`, `lines_removed`, `github_repo` |
| `push` | git hook | `branch`, `remote`, `github_repo` |
| `pr_opened` | GitHub | `number`, `title`, `url`, `additions`, `deletions`, `created_at` |
| `pr_merged` | GitHub | as `pr_opened`, plus `merged_at` |
| `review_submitted` | GitHub | `url` (of the PR) |
| `claude_session_start` | Claude Code hook | `session_id`, `open_sessions` |
| `claude_session_end` | Claude Code hook | `session_id`, `session_minutes` (only when the start is known) |

Older events may lack newer fields. A filter on a missing field never matches.

## Filters

All filters are optional, and every one given must match. Times are local. Regexes are case-insensitive. Globs use the same syntax as skills.

A filter that can never match one of the rule's `trigger` kinds is a load error (for example `message` on `review_submitted`). The "Kinds" column lists where each filter can match; "any" means every kind.

| Filter | Type | Kinds | Matches when |
| --- | --- | --- | --- |
| `message` | regex | `commit` | the commit message matches |
| `title` | regex | `pr_opened`, `pr_merged` | the PR title matches |
| `hash` | regex | `commit` | the full commit hash matches |
| `hash_palindrome` | integer N | `commit` | the first N hash characters read the same backwards |
| `branch` | list of globs | `commit`, `push` (never matches other kinds) | the branch matches one glob |
| `repo` | list of globs | any | the repo matches one glob (local path with `/` separators for git events, `owner/name` for GitHub events) |
| `files_any` | list of globs | `commit` | at least one changed file matches |
| `files_only` | list of globs | `commit` | there is at least one file and every one matches |
| `min_files` / `max_files` | integer | `commit` | the number of changed files is within the bound |
| `min_extensions` | integer | `commit` | the changed files have at least this many distinct extensions |
| `lines` | `{ min, max, min_delete_ratio }` | `commit`, `pr_opened`, `pr_merged` (never matches other kinds) | lines added plus removed is within `min`..`max`, and removed / total is at least `min_delete_ratio` (needs at least one line). Events without line counts never match. |
| `message_length` | `{ min, max }` | `commit` | the message's character count is within the range |
| `weekdays` | list of `"mon"`..`"sun"` | any | the local weekday is listed |
| `hours` | `{ start, end }` (0 to 24) | any | the local hour is in `start`..`end`, end exclusive. It may wrap midnight (`{ start = 22, end = 4 }`). |
| `time` | `"HH:MM"` | any | the local time is exactly that minute |
| `dates` | list of `"MM-DD"` | any | the local date is listed (`"02-29"` is allowed) |
| `pr_open_minutes` | `{ min, max }` | `pr_merged` | merged minus created, in minutes, is within the range |
| `session_minutes` | `{ min, max }` | `claude_session_end` | the session length is within the range (only for sessions with a known start) |
| `open_sessions` | `{ min }` | `claude_session_start` | at least this many Claude sessions are open, this one included |

The `files` filters (`files_any`, `files_only`, `min_files`, `max_files`, `min_extensions`) never match an event that has no file list.

## Counters

`counter = { type = "...", ... }`. There are two families.

**Moment counters** look at the current event (and its history) and unlock a single achievement. They take `tier`, not `tiers`, and never `at` at the top level. `repeatable` and `cooldown_hours` apply to them.

**Threshold counters** compute a running value and unlock each step whose `at` it has reached and that isn't unlocked yet. They take `tier` plus `at`, or `tiers`. They count all stored events of the rule's `trigger` kinds that match its filter, imported history (backfill) included. A step that is already reached when the rule is added unlocks on the next matching event. Each step unlocks once; `repeatable` and `cooldown_hours` don't apply.

### Moment counters

| Type | Parameters | Fires when |
| --- | --- | --- |
| `each` | | any matching event (the default) |
| `per_day` | `at` | the number of matching events today reaches exactly `at` (once per day) |
| `run` | `at` | the last `at` events of this kind in this repo all matched, and the one before them didn't (once per run) |
| `combo` | `kinds`, `within_minutes` | events of each kind in `kinds` happened in order within the window, in the same GitHub repo, ending with this event. The last kind must be one of the `trigger` kinds. |
| `gap_days` | `min`, `scope` (`"ever"` / `"repo"`) | a matching event at least `min` days after the previous matching event (in this repo, or anywhere). The very first one doesn't count. |
| `first` | `scope` (`"ever"` / `"repo"` / `"year"`) | the first event of this kind ever, in this repo, or in this calendar year |
| `personal_record` | `metric` (`"prs_merged_per_day"` / `"commits_per_day"` / `"pr_deletions"`) | this event beats the stored record. The first value only seeds the record. |
| `file_age` | `min_years` | the commit touches a file last changed at least `min_years` ago |
| `month_every_day` | | the first matching event on the last day of a month in which every day had a matching event |

```toml
# each
id = "night_shift"
name = "Night Shift"
tier = "bronze"
trigger = ["commit"]
filter = { hours = { start = 0, end = 4 } }
repeatable = true
cooldown_hours = 168
fallback = "A commit between midnight and four in the morning."
```

```toml
# per_day
id = "hat_trick"
name = "Hat Trick"
tier = "bronze"
trigger = ["pr_merged"]
counter = { type = "per_day", at = 3 }
repeatable = true
fallback = "Three pull requests merged in one day."
```

```toml
# run
id = "wip_wip_wip"
name = "WIP WIP WIP"
tier = "bronze"
trigger = ["commit"]
filter = { message = '\bwip\b' }
counter = { type = "run", at = 5 }
fallback = "Five work-in-progress commits in a row."
```

```toml
# combo
id = "ship_it"
name = "Ship It"
tier = "silver"
trigger = ["pr_opened"]
counter = { type = "combo", kinds = ["commit", "push", "pr_opened"], within_minutes = 15 }
fallback = "Commit, push and pull request in fifteen minutes."
```

```toml
# gap_days
id = "the_return"
name = "The Return"
tier = "bronze"
trigger = ["commit"]
counter = { type = "gap_days", min = 30, scope = "repo" }
fallback = "Back in a repo after a month away."
```

```toml
# first
id = "new_year"
name = "New Year, New Commits"
tier = "bronze"
trigger = ["commit"]
counter = { type = "first", scope = "year" }
repeatable = true
fallback = "The first commit of the year."
```

```toml
# personal_record
id = "personal_best"
name = "Personal Best"
tier = "gold"
trigger = ["pr_merged"]
counter = { type = "personal_record", metric = "prs_merged_per_day" }
repeatable = true
fallback = "A new record for pull requests merged in one day."
```

```toml
# file_age
id = "archaeologist"
name = "Archaeologist"
tier = "bronze"
xp = 500
trigger = ["commit"]
counter = { type = "file_age", min_years = 5 }
fallback = "You modified a file nobody has touched in over five years."
```

```toml
# month_every_day
id = "full_calendar"
name = "Full Calendar"
tier = "gold"
trigger = ["commit"]
counter = { type = "month_every_day" }
repeatable = true
fallback = "A commit on every day of the month."
```

### Threshold counters

| Type | Parameters | Value |
| --- | --- | --- |
| `lifetime` | optional `sum = "<data field>"` or `distinct = "repo"` (not both) | the number of matching events; with `sum`, the total of that numeric data field (missing counts as 0, rounded down); with `distinct = "repo"`, the number of distinct repos |
| `streak_days` | | consecutive local days with a matching event, ending today |
| `meta` | `of = "distinct_unlocks"` / `"same_unlock"` | counts of your own unlocks, leaving out this rule's own. `distinct_unlocks`: the number of different achievements unlocked, where a ladder (or a skill ladder) counts once whatever its steps. `same_unlock`: the most times any one single (non-ladder) achievement was unlocked. Meta rules run after the other rules for the same event, so they count that event's unlocks. |
| `skill_level` | | per skill: on every triggering event, each skill's current level is worked out from its stored skill exp (on the `[skill_levels]` curve), and every step with `at` at or below that level that isn't unlocked yet unlocks for that skill. So a skill already past a step (say, levelled while the rule was off) catches up on the next triggering event, whether or not that event levels anything. Each skill gets its own steps (see [Step ids](#step-ids)), named e.g. "Skill Master: Rust (Bronze)". A new skill file gets its steps automatically. It has no single progress value. |

```toml
# lifetime, with sum
id = "demolition_crew"
name = "Demolition Crew"
trigger = ["commit"]
counter = { type = "lifetime", sum = "lines_removed" }
tiers = [
  { tier = "bronze", at = 1000 },
  { tier = "silver", at = 10000 },
  { tier = "gold", at = 100000 },
]
fallback = "A lot of code, removed."
```

```toml
# streak_days, single tier
id = "week_streak"
name = "Seven Days"
tier = "silver"
at = 7
trigger = ["commit"]
counter = { type = "streak_days" }
fallback = "Seven days in a row with a commit."
```

```toml
# meta
id = "achievement_hunter"
name = "Achievement Hunter"
trigger = ["commit", "push", "pr_opened", "pr_merged", "review_submitted", "claude_session_end"]
counter = { type = "meta", of = "distinct_unlocks" }
tiers = [
  { tier = "silver", at = 25 },
  { tier = "gold", at = 50 },
]
fallback = "Many achievements, collected."
```

```toml
# skill_level
id = "skill_master"
name = "Skill Master"
trigger = ["commit", "pr_merged", "review_submitted", "claude_session_end"]
counter = { type = "skill_level" }
tiers = [
  { tier = "bronze", at = 5 },
  { tier = "silver", at = 10 },
  { tier = "gold", at = 20 },
]
fallback = "A skill, levelled."
```

A threshold rule only runs on its `trigger` kinds, so a `meta` rule should list every kind that can unlock achievements, and a `skill_level` rule every kind a skill earns exp on (the starter skills earn on `commit`, `pr_merged`, `review_submitted` and `claude_session_end`).

## Tiers

| Tier | Default XP |
| --- | --- |
| `bronze` | 250 |
| `silver` | 500 |
| `gold` | 1,000 |
| `platinum` | 2,000 |

A single achievement sets `tier` (and `xp` to override the default). A series sets `tiers`, a list of steps:

| Step field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `tier` | tier | yes | Each tier at most once per ladder, so a ladder has at most four steps. |
| `at` | integer | yes | The threshold. Must be strictly increasing from step to step. |
| `xp` | integer | no | Defaults to the tier's XP. |
| `announce` | `"banner"` / `"toast"` | no | Overrides the rule's `announce` for this step. |

Each step is its own unlock, shown as e.g. "Committed (Silver)". One event can cross more than one step; each crossed step unlocks.

### Step ids

The stored unlock key is:

- `<id>` for a single achievement,
- `<id>.<tier>` for a ladder step (e.g. `committed.silver`),
- `<id>.<skill_id>.<tier>` for a `skill_level` step (e.g. `skill_master.rust.bronze`), with or without `tiers`.

**Warning:** because the key comes from the tier, changing a step's tier makes it a new step, which unlocks again (and gives its XP again). Changing a step's `at` or `xp` is safe. Renaming a rule's `id` likewise makes all of its achievements new.
