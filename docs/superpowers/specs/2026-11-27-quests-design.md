# Quests

## Intent

Achievements reward what you already did. Quests give a reason to do something **right now**, with **a bit of risk**:
something you just did triggers an offer, you choose whether to take it, and then you race a timer to hit a target.
Finishing pays exp and grows a chain multiplier. Failing breaks the chain and goes on your record. Quests can be
turned off.

Reference prototype (open in a browser; style A, every tier, the full offer → pill → card → complete/fail flow):
[`2026-11-27-quests-prototype.html`](2026-11-27-quests-prototype.html).

## Goals

- A **trigger** (same `trigger` + `filter` as achievement rules) can raise a quest **offer**, subject to a chance
  roll and cooldowns.
- The offer is a **banner** with Accept and Pass and a short window to decide. Accepting starts the quest timer.
- While a quest is active, a **persistent timer pill** sits in the toast corner. Clicking it expands it into a
  card with the steps. Clicking anywhere else collapses it.
- **Goal format:** an ordered list of steps (`kind`, optional `filter`, `count`), optionally tied to the triggering
  event's repo, branch or PR.
- **Chain:** each completion raises the multiplier on the next quest's reward. A failure resets it.
  **3 passes in a row** (an ignored offer counts as a pass) also reset it.
- **Tiers:** quests have tiers (bronze…celestial). They drive the reward and the visual escalation.
- **Record:** completions and failures show on the character sheet with the current and best chain.
- Quest results are **events** (`quest_completed`, `quest_failed`) that achievement rules can count, including chain size.
- `[quests] enabled = false` in the config (and a tray toggle) turns the feature off.

## Non-goals

- No XP loss. Risk is only the chain and the failed record.
- No idle detection. "App running and DND off" is the only presence check.
- No negative filters (`not`, tracked in #57) and no branching or "don't do X" goals. One "Trust Issues"-style quest needs `not`,
  so it's out of the starter set.
- No CLI quest commands (`rpg quests …`). The CLI only has to load and validate quest files where it already does that for rules.
- No retroactive reversal: once a quest is resolved as failed, a GitHub event that shows up later doesn't revive it.

## Anti-goals

- **Unfair losses.** You must never lose a chain to something you didn't see: no offers while DND is on, while the
  app is closed, or from backfilled or away events.
- **Spam.** Offers must feel occasional. A trigger like "any commit" must not produce an offer every commit.
- **Quest state that disagrees with exp.** Step progress, completion, exp and the chain change in the same
  transaction as the event that caused them, and survive restarts.
- **Looks like an achievement.** The crimson frame must read as "a choice", never as a reward.

## Approaches

1. **Quests in `rpg-core`, persisted in the store.** A `quests` module plus a table. The engine advances steps
   in the event's transaction. The app adds timers and UI. **Chosen.**
2. **Quests in `rpg-app`, in memory.** Faster to build, but an active quest is lost on restart or update, the logic
   lives where tests don't reach, and progress and exp are written separately.
3. *(Dropped)* A new achievement counter type. Offers, accept, fail and timers don't fit the unlock model.

The user chose 1: the chain is something you can lose, so it has to be trustworthy.

## Behavior

### Lifecycle

```
trigger event (live) ──chance/cooldown──▶ OFFER (banner, 60s) ──Accept──▶ ACTIVE (pill + timer)
                                             │ Pass / ignored                 │ all steps done ──▶ COMPLETED
                                             ▼                                 │ deadline passed ─▶ FAILED
                                          passes += 1 (3 → chain = ×1)        │ quests disabled ─▶ CANCELLED
```

- **At most one** offer or active quest at a time. Triggers fire nothing while one exists.
- An offer can only come from a **live** event: not `backfill`, not part of the startup "away" batch, and with `ts`
  within the last 5 minutes (*my call*). Never while DND is on or quests are off. Rule order: a quest whose trigger
  matches, whose own cooldown and `min_gap_minutes` have passed, then rolls `chance`. If several quests are eligible
  for one event, roll them in filename order and offer the first that hits.
- **Accept** resets the pass count to 0 and starts the deadline at the moment of accepting.
- **Pass** (button) or the offer window running out: passes += 1. At 3, the chain resets to ×1 and passes reset to 0.
- **Completed:** reward = quest exp × chain multiplier, then the chain goes up one step. A `quest_completed` event is recorded.
- **Failed:** the chain resets to ×1. A `quest_failed` event is recorded.
- **Cancelled** (quests turned off while one is active): the quest ends quietly. The chain and passes are kept, nothing goes on the record as a failure,
  and no event is recorded.

### Step matching

- A step is credited by an event of its `kind` that matches its `filter` (same filter semantics as rules), with
  `ts` in `[accepted_at, deadline]`, after the previous step is complete. Each step needs `count` matching events.
- `same = "repo"`: the event's repo must match the trigger's. Use `github_repo` when both have one, otherwise the
  local repo path, the same way `distinct = "repo"` resolves it. `"branch"`: same repo and the same `branch`.
  `"pr"`: the same PR (`github_repo` + `number`). For `review_submitted` the PR comes from its `url`.
- Deadlines go by **event time**, not arrival time. A PR merged at 0:58 counts even if the poll lands at 1:03,
  as long as it arrives before the quest is resolved.

### Deadline and GitHub

When the timer runs out with a step left that needs a GitHub event kind (`pr_opened`, `pr_merged`,
`review_submitted`) and GitHub is enabled, the app **wakes the GitHub poller right away**. The pill shows
"checking GitHub…" until the poll's events are processed, then the quest is resolved. If the poll fails or takes
longer than a short cap (~30s), the quest is resolved with what's known. Also poll once on accept for quests that have GitHub steps.

### App restarts

- Active quests are in the store, so they survive. Hook events written to the inbox while the app was closed are
  processed with their own timestamps on startup, so they can still complete steps that fall before the deadline.
  After the startup batch, any quest past its deadline is resolved (with the GitHub check above).
- Offers are **not** persisted. An offer that's showing when the app quits is dropped and doesn't count as a pass.

## Quest files

TOML files in a `quests/` folder next to `rules/` in the config dir, seeded with a starter set like rules. They're
loaded, validated and watched the same way: unknown fields are errors, a bad file is skipped and reported, and duplicate ids follow the same rule.

```toml
id = "ship_it_speedrun"
name = "Ship It (Speedrun)"
tier = "gold"
trigger = ["commit"]
filter = { }                 # optional, rule filter syntax
chance = 0.10                # probability a matching live event raises an offer
cooldown_hours = 24          # min hours between offers of this quest
minutes = 15                 # time limit after accepting
same = "repo"                # optional: "repo" | "branch" | "pr"
steps = [
  { kind = "push" },
  { kind = "pr_opened" },
]
# xp = 600                   # optional; defaults to the tier's quest exp
hint = "Commit, push, PR. Go."
fallback = "You committed. Bold. Now finish the job before your nerve runs out."
# flavor = { type = "llm", prompt = "..." }   # same as rules; offer clock starts once the text is resolved and shown
# enabled = false
```

Validation, in the spirit of rule validation: `steps` is non-empty. A step's `kind` is an event kind (not `quest_*`),
and its `filter` must be able to match that kind. `count` ≥ 1, default 1. `same = "pr"` requires the trigger kinds
to be PR kinds. `0 < chance ≤ 1`. `minutes` ≥ 1.

### Starter quests

| Quest | Tier | Trigger | Steps | Time | Same |
| --- | --- | --- | --- | --- | --- |
| Hot Streak | bronze | commit | commit ×4 | 30 min | — |
| Mop-Up Duty | silver | commit, `lines.min = 300` | commit with `lines.min_delete_ratio = 0.5` | 30 min | repo |
| Review Bounty | silver | pr_opened | review_submitted ×2 | 60 min | — |
| Ship It (Speedrun) | gold | commit | push → pr_opened | 15 min | repo |
| Land the Plane | gold | pr_opened | pr_merged | 2 h | pr |
| Friday Gambit | legendary | commit, Fri, hours 15–24 | push → pr_opened → pr_merged | 60 min | repo |

Chances and cooldowns are tuned so offers feel occasional (*builder's call*, roughly one offer every hour or two of
active work at most).

## Config

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

`enabled` is the setting. The tray also gets a **Quests** check item next to Mute and Do not disturb (*my call*). Like those,
it is **not persisted**: it starts from `[quests] enabled` at launch and turns quests off or on for the session. Turning
it off works exactly like `enabled = false`.

**Chain math:** the chain level `n` is the number of completions in a row (0 after a reset). The multiplier is
`min(1 + chain_step × n, chain_max)`. The reward is `floor(quest exp × multiplier)`.

**Quest exp by tier** (default when `xp` isn't set): half the achievement tier exp: 125 / 250 / 500 / 1,000 / 2,000 / 4,000.

## Events and achievements

The engine records two new event kinds when a quest resolves (source: internal) and runs rules on them in the same
transaction, so achievements can unlock from them:

- `quest_completed`: data `quest_id`, `tier`, `chain` (the chain level reached *after* this completion), `multiplier`, `xp`
- `quest_failed`: data `quest_id`, `tier`, `chain_lost` (the chain level that was lost)

They earn no base exp, and they never trigger quests. The quest reward goes into `xp_ledger` against the `quest_completed` event.

New rule filters, valid only on these kinds: `chain = { min, max }` (`quest_completed`) and `quest = [globs]` (both).
Starter achievements (*my call*): **Quest Giver's Pet** (complete 10 / 50 / 200, ladder), **Chain Gang** (reach a ×5 chain,
using the `chain` filter), **Fumble** (`run` of 3 `quest_failed`). These kinds also go into the `meta` rule's trigger list in
`achievement_hunter.toml`.

## Data

Schema **v4**:

- `quests` table: `id`, `quest_id`, `name`, `tier`, `status` (`active` | `completed` | `failed` | `cancelled`),
  `trigger_event_id`, `binding` (JSON: repo/branch/pr), `accepted_at`, `deadline`, `progress` (JSON: count per step),
  `chain_at_start`, `multiplier`, `xp`, `resolved_at`, `result_event_id`.
- `meta`: `quest_chain`, `quest_best_chain`, `quest_passes`, and the last-offer time per quest and overall, for cooldowns.

## Presentation

### Offer banner: "Blood Contract" (style A)

The achievement banner's frame, size, position (top-center) and entrance, recolored into the quest family:
crimson border (`#ff4d5e`), dark crimson radial background, ember accents (`#ff9a3c`). It shows:

- a header with tier gems: "◆ GOLD QUEST OFFERED ◆" in the tier color
- the name, the offer text (`fallback` or the LLM text), and the goal line: steps · time ·
  "exp × multiplier = total"
- **Accept** (filled crimson) and **Pass** (outline) buttons
- the stakes line: "Chain ×1.5 → ×2 on success · N more passes breaks it"
- a **burning fuse** along the bottom edge that shrinks over `offer_seconds`, with a spark at its tip and puffs of smoke

The offer's clock starts when the banner is actually on screen. An offer goes to the **front** of the banner queue.
Clicking the banner body does nothing; only the buttons act. Pressing Accept animates the banner shrinking into the
bottom-right corner, where the pill appears.

### Tier treatment

The crimson frame always means "quest", and the tier layers on top the way achievement banners escalate: the header
text and gems take the tier color, and the glow grows with the tier. Gold and up gets the bigger entrance and a flash, Platinum and up a light sweep,
Legendary and up a pulsing glow behind the banner, and Celestial the star field and the shifting aurora name. Reduced motion: fade
only, the same as banners today. The palette is the one already in `overlay.css`.

### Timer pill and card

- Bottom-right, above the transient exp pills, which keep coming and going around it. It has the quest crimson border, a tier gem,
  the name, the time left (tabular digits) and one dot per step (filled when done).
- In the last 2 minutes it pulses red.
- **Click → card:** it widens into a card listing the steps (✓ done, ▸ next), a draining bar, and the reward and chain at stake.
  **Clicking anywhere else** collapses it.
- The toast window passes clicks through today. It has to stop passing them through while the pointer is over the
  pill, using the same kind of pointer polling the banner uses for hover. Collapsing on "click anywhere else" can use window blur.
- **Completed:** the pill turns into an **achievement-toast-styled** pill in the quest's tier ("Quest complete! +1,200"),
  matching `.xp.ach[data-tier]`: tier color text, border and glow, a sweep for Platinum and up, and a gradient border for Legendary and up.
  Then it fades like a toast. Any level-up or achievement unlocks it caused announce as usual.
- **Failed:** grey, struck-through time, a short shake, "Quest failed · chain lost", then it fades.
- **Checking GitHub:** the time is replaced with "checking GitHub…" while the deadline poll runs.
- Do not disturb turned on **during** an active quest: the timer keeps running and the pill stays. DND only stops new offers and
  drops transient toasts.

### Character sheet

A **Quests** section shows the current chain and multiplier, the best chain, the number of completions and failures, and a log of
resolved quests (name, tier color, result, reward or chain lost, when), newest first, with the same tier colors as the event rows. A quest board
lists every loaded quest with its tier, hint, trigger chance and completion count. It's hidden when quests are disabled, and the
board marks off quests with an "off" badge like rules.

## Builder's calls

These are left to the builder. Settle them sensibly and note them in the plan:

- The exact chance and cooldown values for each starter quest, and the starter quests' text, in the existing snarky voice.
- Sounds for offer, complete and fail (the `sound` module and `[sound]` config already exist; default to none if unsure).
- How the deadline timer thread is structured, and how "poll now" reaches the GitHub thread (for example a wake channel into
  `poll_loop`'s sleep).
- How the engine returns quest outcomes (offer candidate, step progress, resolution) inside `Outcome`, and where the
  chance roll lives (injectable RNG for tests).
- Updates to `docs/rules.md` (new kinds and filters), a new `docs/quests.md` format reference, the README and
  `docs/manual-checklist.md` (each tier's offer banner, the pill expand/collapse, complete and fail states).
