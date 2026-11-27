# Quests

Quests are timed challenges. Something you just did (a commit, a PR) can raise an **offer**: a crimson banner with **Accept** and **Pass**. If you accept, a timer pill sits in the bottom-right corner while you race to finish the steps. Finishing pays exp times a chain multiplier and grows the chain. Failing breaks the chain and goes on your record. There is no exp loss.

Quest files are TOML, like rules. This page is the format reference. For the event kinds and filters, see [rules.md](rules.md).

## How a quest runs

- An offer comes only from a **live** event: not backfill, not part of the startup "while you were away" batch, and not older than 5 minutes. Never while Do not disturb is on or quests are off.
- At most one offer or active quest exists at a time.
- For one event, the quests whose `trigger` and `filter` match, whose own `cooldown_hours` and the global `min_gap_minutes` have passed, roll their `chance` in filename order. The first that hits is offered.
- **Accept** starts the deadline at that moment and resets the pass count. **Pass**, or letting the offer run out (`offer_seconds`), counts as a pass. After `max_passes` passes in a row the chain resets.
- Steps are done in order. A step is credited by an event of its `kind` that matches its `filter`, with a time between accepting and the deadline, and only after the previous step is complete. It needs `count` such events. Deadlines go by **event time**: a PR merged at 0:58 counts even if the poll finds it at 1:03.
- **Completed:** the reward is `floor(exp * multiplier)`, then the chain goes up one. A `quest_completed` event is recorded.
- **Failed:** the deadline passed with steps left. The chain resets and a `quest_failed` event is recorded. A GitHub event that shows up later does not revive it.
- If a step still open at the deadline needs a GitHub event (`pr_opened`, `pr_merged`, `review_submitted`) and GitHub is enabled, the app polls GitHub right away. The pill reads "checking GitHub..." until that poll is processed (at most about 30 seconds), then the quest is resolved.
- Quests survive a restart. Hook events written while the app was closed are processed with their own timestamps, so they can still complete steps before the deadline. An offer that is showing when the app quits is dropped and is not a pass.
- Turning quests off (config or the tray **Quests** item) while one is active ends it quietly: no failure, no event, and the chain is kept.
- Do not disturb during an active quest: the pill and timer stay. It only stops new offers.

**Chain math:** the chain level `n` is the number of completions in a row (0 after a reset). The multiplier is `min(1 + chain_step * n, chain_max)`.

## Where quest files live

`<config>/rules/quests/`. The folder is seeded with six starter quests when it has no `*.toml` file, and reloaded when files change. Tray **Open rules folder** opens `rules/`, and `quests/` is inside it. As with rules: files load in filename order, unknown fields are errors, a bad file is skipped with a line in `rpg.log`, and on a duplicate `id` the first file wins.

## File format

```toml
id = "ship_it_speedrun"
name = "Ship It (Speedrun)"
tier = "gold"
trigger = ["commit"]
filter = { }                 # optional, rule filter syntax
chance = 0.10
cooldown_hours = 24
minutes = 15
same = "repo"
steps = [
  { kind = "push" },
  { kind = "pr_opened" },
]
hint = "Commit, push, PR. Go."
fallback = "You committed. Bold. Now finish the job before your nerve runs out."
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | string | yes | Unique. Letters, digits, `_` and `-` only. |
| `name` | string | yes | Shown on the offer, the pill and the sheet. |
| `tier` | string | yes | `bronze`, `silver`, `gold`, `platinum`, `legendary` or `celestial`. Drives the default exp and the look of the banner. |
| `trigger` | list of kinds | yes | Event kinds that can raise the offer. Not empty, and never `quest_completed` or `quest_failed`. |
| `filter` | table | no | Which trigger events count. Same filters as rules; it must be able to match every `trigger` kind. |
| `chance` | number | yes | Probability that a matching live event raises the offer. Above 0, at most 1. |
| `cooldown_hours` | integer | no, default 0 | Minimum hours between offers of this quest. |
| `minutes` | integer | yes | Time limit after accepting. At least 1. |
| `same` | string | no | Ties the steps to the triggering event. See below. |
| `steps` | list of tables | yes | The goal. At least one step. |
| `xp` | integer | no | Reward before the multiplier. Defaults to half the tier's achievement exp: 125 / 250 / 500 / 1,000 / 2,000 / 4,000. |
| `hint` | string | no | Shown on the character sheet's quest board. |
| `fallback` | string | yes | The offer text. |
| `flavor` | table | no | Same as rules: `{ type = "llm", prompt = "..." }`, with `{fallback}`, `{name}` and `{context}` substituted. The default is static. The offer clock starts once the text is resolved and shown. |
| `enabled` | bool | no, default `true` | `false` keeps the file but never offers it. |

### Steps

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `kind` | string | yes | Event kind to count. Any kind except `quest_completed` and `quest_failed`. |
| `filter` | table | no | Which events of that kind count. It must be able to match `kind` (for example `message` is rejected on `push`). |
| `count` | integer | no, default 1 | How many matching events the step needs. At least 1. |
| `label` | string | no | Shown on the pill's card. Defaults by kind: Commit, Push, Open a PR, Merge a PR, Review a PR, Start a Claude session, Finish a Claude session. When `count` is above 1, ` ×<count>` is appended. |

Unknown fields in a step are errors too.

### `same`

Binds the steps to the event that triggered the offer:

- `"repo"`: the step event must be in the same repo (the GitHub repo when both have one, otherwise the local repo path).
- `"branch"`: the same repo and the same branch.
- `"pr"`: the same pull request, matched on the PR url (`github_repo` plus number). It needs a `trigger` made only of `pr_opened`, `pr_merged` and `review_submitted`.

## Examples

A plain count:

```toml
id = "hot_streak"
name = "Hot Streak"
tier = "bronze"
trigger = ["commit"]
chance = 0.10
cooldown_hours = 12
minutes = 30
steps = [{ kind = "commit", count = 4 }]
hint = "Four commits in half an hour."
fallback = "Momentum. Keep it going: four more commits in half an hour."
```

Tied to a PR, with a custom label:

```toml
id = "land_the_plane"
name = "Land the Plane"
tier = "gold"
trigger = ["pr_opened"]
chance = 0.10
cooldown_hours = 24
minutes = 120
same = "pr"
steps = [{ kind = "pr_merged", label = "Get it merged" }]
fallback = "A PR sits open, circling the airfield. Merge it within two hours."
```

The six starters (in `crates/rpg-core/assets/quests/`) are Hot Streak (bronze), Mop-Up Duty (silver), Review Bounty (silver), Ship It (Speedrun) (gold), Land the Plane (gold) and Friday Gambit (legendary).

## `[quests]` config

```toml
# Timed quests, offered after things you do. false turns them off (an active quest ends quietly).
[quests]
enabled = true
offer_seconds = 60       # how long an offer waits for Accept
max_passes = 3           # passes (or ignored offers) in a row that break the chain
chain_step = 0.5         # multiplier gained per completion: x1, x1.5, x2, ...
chain_max = 5.0          # multiplier cap
min_gap_minutes = 30     # min time between any two offers
```

Config is read at startup. The tray **Quests** check item turns quests off or on for the session only. It is not saved and starts from `enabled` at launch.

## Events and achievements

Resolving a quest records `quest_completed` (`quest_id`, `tier`, `chain`, `multiplier`, `xp`) or `quest_failed` (`quest_id`, `tier`, `chain_lost`), and rules run on them in the same transaction. They earn no base exp and never trigger quests. Rules can use the `chain = { min, max }` filter (`quest_completed`) and `quest = [globs]` (both). The starter achievements are Quest Giver's Pet (10 / 50 / 200 completions), Chain Gang (a chain of 5) and Fumble (three failures in one day).

The character sheet's Achievements tab has a Quests section: current and best chain, completions and failures, a log of resolved quests and a board of every loaded quest.
