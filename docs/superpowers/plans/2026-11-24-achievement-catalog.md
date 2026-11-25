# Expanded Achievement Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Grow the starter catalog to about 50 achievements. To get there, replace the six fixed detectors with filters plus counters, and add tiered ladders (with a Platinum tier), banner-or-toast announcing, hidden achievements, an `enabled` flag, and seeding of new starter rules on upgrade.

**Architecture:** A rule has an optional `filter` (typed conditions on one event, all of which must match) and a `counter` (what to do with matching events). Counters come in two kinds:
- **Moment counters** (`each`, `per_day`, `run`, `combo`, `gap_days`, `first`, `personal_record`, `file_age`) decide from the current event.
- **Threshold counters** (`lifetime`, `streak_days`, `month_every_day`, `meta`, `skill_level`) compute a number over stored history and unlock every step whose threshold is met and that isn't unlocked yet.

Every rule normalizes to a list of `Step`s, with one step for a single achievement. The engine resolves each unlock's announce mode from the rule or the config, and the presenter sends toast unlocks to the toast window and banner unlocks to the banner queue.

**Tech Stack:** Rust (workspace: `rpg-core`, `rpg-app` with Tauri 2, `rpg-cli`), SQLite via rusqlite, serde/toml, globset, new dependency `regex = "1"`, plain JS/CSS in `ui/`.

**Spec:** `docs/superpowers/specs/2026-11-24-achievement-catalog-design.md`

## Global Constraints

- The old `detector = {...}` rule format keeps loading unchanged. Having both `detector` and `filter`/`counter` in one file is a load error.
- Tier defaults: bronze 250, silver 500, gold 1000, platinum 2000 exp. Platinum ranks above gold.
- `[announce]` defaults: bronze `toast`; silver, gold and platinum `banner`.
- Step unlock id is `<rule_id>.<tier>` (e.g. `committed.silver`); skill ladders use `<rule_id>.<skill_id>.<tier>`. Each tier appears at most once per ladder; a repeat is a load error.
- Threshold counters fire when **count ≥ `at`** for a step that isn't unlocked yet. They count all stored events, backfill included. Moment counters never fire for history.
- A missing event field means "no match" for any filter on it.
- Commit messages and PR titles stay out of the LLM flavor context unless the rule sets `share_text = true`. No starter rule sets it.
- Achievement toasts play no sound, show no flavor text and make no LLM call.
- Hooks must never fail or slow a git command. New capture in `emit.rs` must not error when `origin` is missing.
- One PR per phase (Phases 1–5), each with `cargo test --workspace` green.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Upgrading an existing install:** user rule files in the old format, a DB with months of events, and no seeding manifest. Expected: everything loads, and the first matching events afterwards unlock ladder steps that history already earned, collapsing as a storm when there are many. Pinned in Task 7 (`ladder_catches_up_on_next_event`) and Task 13 (`manifest_created_from_existing_files`).
2. **Events stored before this change** lack `message`, `github_repo`, `created_at` and session fields. Expected: no panic, and the filter doesn't match. Pinned in Task 4 (`missing_field_never_matches`).
3. **A bad regex or glob in a user's rule file.** Expected: that file is reported in `RuleError` and the others still load, as today. Pinned in Task 5 (`bad_regex_is_a_rule_error`).
4. **One event crossing several ladder steps, or many ladders at once.** Expected: every crossed step unlocks once, never twice. Pinned in Task 7 (`one_event_unlocks_all_crossed_steps_once`).
5. **A Claude session with no end (crashed), or an end with no start.** Expected: a start older than 12h doesn't count as open, and an end without a start gets no `session_minutes`. Pinned in Task 3 (`stale_start_not_open`, `end_without_start_has_no_minutes`).

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/rpg-core/src/emit.rs` (modify) | capture `message`, `github_repo` |
| `crates/rpg-core/src/github.rs` (modify) | `created_at`, `merged_at` in PR data |
| `crates/rpg-core/src/derive.rs` (create) | session fields computed at processing and persisted into `ev.data` |
| `crates/rpg-core/src/filters.rs` (create) | `RawFilter`, `Filter`, parsing, `Filter::matches` |
| `crates/rpg-core/src/counters.rs` (create, replaces `detectors.rs`) | `EvalCtx`, `Fired`, `evaluate`, `progress` |
| `crates/rpg-core/src/rules.rs` (modify) | `Tier` (+Platinum), `Announce`, `Step`, `Counter`, `Rule`, legacy mapping, validation, `enabled` |
| `crates/rpg-core/src/store.rs` (modify) | `events_of`, `unlock_counts` |
| `crates/rpg-core/src/engine.rs` (modify) | derive, filter, counters, steps, announce, meta ordering |
| `crates/rpg-core/src/config.rs` + `assets/config.default.toml` (modify) | `[announce]` |
| `crates/rpg-core/src/flavor.rs` (modify) | strip text unless `share_text` |
| `crates/rpg-core/src/paths.rs` (modify) | seeding manifest |
| `crates/rpg-app/src/presenter.rs`, `overlay.rs`, `ui/overlay/*` (modify) | achievement toasts, platinum |
| `crates/rpg-app/src/site.rs`, `ui/site/*` (modify) | steps, progress, hidden, disabled, platinum |
| `crates/rpg-core/assets/rules/*.toml` (create ~45) | the catalog |
| `crates/rpg-core/tests/catalog.rs` (create) | every starter rule fires on a crafted event |
| `docs/rules.md` (create) | rule format reference for users and #27 |

---

## Phase 1: Event data (PR 1)

### Task 1: Commit message and GitHub repo on git events

**Files:**
- Modify: `crates/rpg-core/src/emit.rs` (`commit_event`, `push_event`)
- Test: `crates/rpg-core/tests/emit.rs`

**Interfaces:**
- Produces:
  - commit `data.message: String` (full message, `git show -s --format=%B <hash>`, trailing newline trimmed)
  - commit and push `data.github_repo: Option<String>` (`owner/name`, or JSON null)
  - `pub fn github_repo_from_url(url: &str) -> Option<String>` in `emit.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn commit_event_has_message() {
    let d = temp_repo(); // existing helper; its commit message is whatever temp_repo uses
    let e = commit_event(d.path(), "HEAD").unwrap();
    assert!(!e.data["message"].as_str().unwrap().is_empty());
    assert!(e.data["github_repo"].is_null()); // no origin
}

#[test]
fn github_repo_parsing() {
    use rpg_core::emit::github_repo_from_url as p;
    assert_eq!(p("https://github.com/acme/app.git").as_deref(), Some("acme/app"));
    assert_eq!(p("git@github.com:acme/app.git").as_deref(), Some("acme/app"));
    assert_eq!(p("https://github.com/acme/app").as_deref(), Some("acme/app"));
    assert_eq!(p("https://gitlab.com/acme/app.git"), None);
}
```

Add a third test in which `temp_repo` sets `origin` to `https://github.com/acme/app.git` (`git remote add`), and assert that both the commit and the push events have `github_repo == "acme/app"`.

- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core --test emit`. Expected: compile error (no `github_repo_from_url`).
- [ ] **Step 3: Implement.** Read the origin with `git remote get-url origin`. If that fails, use `None` rather than erroring.
- [ ] **Step 4: Run them and check they pass.** Run `cargo test -p rpg-core --test emit`. Expected: PASS.
- [ ] **Step 5: Commit** `Events: record commit message and GitHub repo on git events (#2)`

### Task 2: PR timestamps in PR data

**Files:**
- Modify: `crates/rpg-core/src/github.rs:58` (the `data` json in `parse`)
- Test: `crates/rpg-core/src/github.rs` tests (`parse_fixture`)

**Interfaces:**
- Produces: both PR events' `data.created_at` and `data.merged_at`, as RFC3339 strings or null. The query already fetches `createdAt` and `mergedAt`.

- [ ] **Step 1: Write the failing test.** Extend `parse_fixture` to assert that the merged PR's `data["created_at"]` and `data["merged_at"]` equal the fixture's `createdAt` and `mergedAt` strings, and that an unmerged PR has `merged_at` null.
- [ ] **Step 2: Run it and check it fails.** Run `cargo test -p rpg-core github::`. Expected: FAIL.
- [ ] **Step 3: Implement.** Add the two fields to `data`.
- [ ] **Step 4: Run it and check it passes.** Same command. Expected: PASS.
- [ ] **Step 5: Commit** `GitHub: keep PR created/merged times in event data (#2)`

### Task 3: Claude session duration and concurrency

**Files:**
- Create: `crates/rpg-core/src/derive.rs` (and register it in `lib.rs`)
- Modify: `crates/rpg-core/src/engine.rs` (`process_inner`: derive on a mutable clone **before** `store.insert_event`)
- Modify: `crates/rpg-core/src/store.rs` (add `events_of`)
- Test: unit tests in `derive.rs`

**Interfaces:**
- Produces:
  - `pub fn events_of(&self, kinds: &[EventKind], since: Option<DateTime<Utc>>) -> Result<Vec<Event>>` in `store.rs`: events of those kinds with `ts >= since`, ordered `ts ASC, id ASC`, backfill included. Later tasks use it for all history scans.
  - `pub const STALE_SESSION_HOURS: i64 = 12;` in `derive.rs`
  - `pub fn derive(store: &Store, ev: &mut Event) -> Result<()>` in `derive.rs`, which sets:
    - on `claude_session_end` with a known start (`store.find_session_start`): `data.session_minutes: f64`
    - on `claude_session_start`: `data.open_sessions: u64`. That's the number of starts within the last 12h that have no matching end (by `session_id`) and aren't this event, plus 1 for this event.

- [ ] **Step 1: Write the failing tests** `end_gets_minutes` (start at 10:00, end at 13:30 gives `session_minutes == 210.0`), `end_without_start_has_no_minutes`, `open_sessions_counts_concurrent` (two open starts, then a third start gives 3; after one of them ends, a new start gives 3), and `stale_start_not_open` (a start 13h earlier with no end isn't counted). Insert the earlier events with `store.insert_event`.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core derive::`. Expected: compile error.
- [ ] **Step 3: Implement `events_of` and `derive`, and call `derive` in `process_inner` before `insert_event`,** so the derived fields are persisted and history scans see them.
- [ ] **Step 4: Run all of `rpg-core`'s tests and check they pass.** Run `cargo test -p rpg-core`. Expected: PASS.
- [ ] **Step 5: Commit** `Engine: derive Claude session length and concurrency (#2)`. Open PR 1.

---

## Phase 2: Filters and counters (PR 2)

### Task 4: Filters

**Files:**
- Create: `crates/rpg-core/src/filters.rs`
- Modify: `crates/rpg-core/Cargo.toml` (`regex = "1"`), `crates/rpg-core/src/skills.rs` (make `glob_set` `pub(crate)` so filters reuse it)

**Interfaces:**
- Produces:
  - `#[derive(Deserialize, Default)] #[serde(deny_unknown_fields)] pub struct RawFilter`, with the TOML fields:
    - `message`, `title`, `hash`: `Option<String>` (regexes)
    - `branch`, `repo`, `files_any`, `files_only`: `Option<Vec<String>>` (globs)
    - `min_files`, `max_files`: `Option<u64>`
    - `min_extensions`: `Option<u32>`
    - `lines`: `Option<{min, max, min_delete_ratio}>`
    - `hash_palindrome`: `Option<usize>`
    - `weekdays`: `Option<Vec<String>>` (`"mon"`..`"sun"`)
    - `hours`: `Option<{start: u32, end: u32}>`, with `end <= 24`
    - `time`: `Option<String>` (`"HH:MM"`)
    - `dates`: `Option<Vec<String>>` (`"MM-DD"`)
    - `pr_open_minutes`, `session_minutes`, `message_length`: `Option<{min: Option<f64>, max: Option<f64>}>`
    - `open_sessions`: `Option<{min: u64}>`
  - `pub struct Filter` (compiled form; `Default` matches everything).
  - `impl TryFrom<RawFilter> for Filter { type Error = String; }`
  - `impl Filter { pub fn matches(&self, ev: &Event, tz: FixedOffset) -> bool; pub fn kinds_ok(&self, kinds: &[EventKind]) -> Result<(), String>; }`. `kinds_ok` rejects these filters unless every trigger kind supports them:
    - commit-only: `message`, `files_*`, `min/max_files`, `min_extensions`, `hash*`, `message_length`
    - `title`: PR kinds only
    - `pr_open_minutes`: `pr_merged` only
    - `session_minutes`: `claude_session_end` only
    - `open_sessions`: `claude_session_start` only
- Rules: regexes are case-insensitive (`RegexBuilder::case_insensitive(true)`). The time conditions (`weekdays`, `hours`, `time`, `dates`) use `ev.ts` in `tz`. `hours` keeps today's `time_of_day` semantics (end exclusive, wraps midnight). `lines` keeps today's `diff_size` semantics, including the commit vs. PR field names. `files_only` needs at least one file. `min_extensions` counts distinct lowercased extensions, and files without one don't count. `hash_palindrome = n` compares `hash[..n]` with its reverse.

- [ ] **Step 1: Write the failing tests** in `filters.rs`, building filters from TOML through `toml::from_str::<RawFilter>` and then `Filter::try_from`:
  - `message_regex_case_insensitive`: `message = "\\bwip\\b"` matches "WIP: stuff" and not "wiper"
  - `files_only_needs_all`: `["*.md"]` matches `[README.md, docs/a.md]`, not `[README.md, a.rs]`, and not `[]`
  - `min_extensions_counts_distinct`: `a.rs, b.rs, c.ts, d.md, e.toml` has 4
  - `hash_palindrome`: `"abccbax…"` matches with n=6
  - `time_exact`: 13:37:59 local matches `"13:37"`; 13:38 doesn't
  - `weekdays_local`: Saturday 23:30 at UTC−5 is Sunday 04:30 UTC, and matches `["sat"]` in tz −5
  - `dates_mm_dd`: `["02-29"]` matches 2028-02-29
  - `pr_open_minutes_range`: created 10:00, merged 10:09 matches `{max = 10}`
  - `missing_field_never_matches`: an event without `message` fails `message = "."`; one without `session_minutes` fails `session_minutes = {min = 0}`
  - `kinds_ok_rejects_message_on_review`
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core filters::`. Expected: compile error.
- [ ] **Step 3: Implement** `filters.rs`.
- [ ] **Step 4: Run them and check they pass.** Same command. Expected: PASS.
- [ ] **Step 5: Commit** `Rules: event filters (#2)`

### Task 5: Rule schema: counters, steps, Platinum, legacy detectors

**Files:**
- Modify: `crates/rpg-core/src/rules.rs`; every compile fallout of the changed `Rule`/`Tier` (`engine.rs`, `flavor.rs` tests, `site.rs`, `overlay.rs`, `presenter.rs`)
- Test: `crates/rpg-core/src/rules.rs` tests

**Interfaces:**
- Produces, in `rules.rs`:
  - `pub enum Tier { Bronze, Silver, Gold, Platinum }`, with:
    - `default_xp()`: 250 / 500 / 1000 / 2000
    - `as_str()`: `"bronze"`…`"platinum"`
    - `label()`: `"Bronze"`…`"Platinum"`
    - `rank()`: 0..=3
    
    These replace the local `tier_str`, `tier_name` and `tier_rank` copies in `engine.rs`, `site.rs`, `overlay.rs` and `presenter.rs`.
  - `#[serde(rename_all = "snake_case")] pub enum Announce { Banner, Toast }`
  - `pub struct Step { pub tier: Tier, pub at: Option<u64>, pub xp: u64, pub announce: Option<Announce> }`
  - `pub enum FirstScope { Ever, Repo, Year }`, `pub enum Scope { Ever, Repo }`, `pub enum MetaOf { DistinctUnlocks, SameUnlock }`
  - `#[serde(tag = "type", rename_all = "snake_case")] pub enum Counter`, with variants:
    - moment counters:
      - `Each`
      - `PerDay { at: u32 }`
      - `Run { at: u32 }`
      - `Combo { kinds: Vec<EventKind>, within_minutes: u32 }`
      - `GapDays { min: u32, scope: Scope }`
      - `First { scope: FirstScope }`
      - `PersonalRecord { metric: Metric }`
      - `FileAge { min_years: f64 }`
      - `MonthEveryDay`
    - threshold counters:
      - `Lifetime { sum: Option<String>, distinct: Option<String> }` (`distinct` accepts only `"repo"`)
      - `StreakDays`
      - `Meta { of: MetaOf }`
      - `SkillLevel`
  - `impl Counter { pub fn is_threshold(&self) -> bool }`. True for `Lifetime`, `StreakDays`, `Meta` and `SkillLevel`. `MonthEveryDay` is a moment counter: it fires on the event that completes the month.
  - `pub struct Rule`, with fields:
    - `id`, `name`, `fallback`, `hint`
    - `trigger: Vec<EventKind>`
    - `filter: Filter`
    - `counter: Counter`
    - `steps: Vec<Step>` (≥1)
    - `ladder: bool`
    - `repeatable`, `cooldown_hours`
    - `flavor: Flavor`
    - `announce: Option<Announce>`
    - `hidden: bool`
    - `share_text: bool`
    
    `tier` and `xp` are removed as fields. Add `pub fn top(&self) -> &Step` (last step) and `pub fn step_id(&self, tier: Tier) -> String`, which returns `id` for a single-step non-ladder rule and `format!("{id}.{}", tier.as_str())` for a ladder. Drop `PartialEq` from `Rule` if `Filter` can't derive it.
  - TOML (`RawRule`, `deny_unknown_fields`) accepts `tier` | `tiers = [{tier, at, xp?, announce?}]`, an optional top-level `at`, `xp`, `announce`, `hidden`, `share_text`, `filter`, `counter` (default `each`), or legacy `detector`.
- Validation, each failure a load error with a message naming the field:
  - exactly one of `tier`/`tiers`
  - threshold counters need `at` (single) or `tiers` with `at` on every step, strictly increasing
  - moment counters accept neither `tiers` nor `at`
  - no repeated tier in `tiers`
  - `repeatable`/`cooldown_hours` are not allowed with `tiers`
  - `detector` cannot be combined with `filter`/`counter`
  - `filter.kinds_ok(&trigger)`
- Legacy mapping for `detector`:
  - `count_in_window{count}` → `PerDay{at: count}`
  - `personal_record` → `PersonalRecord`
  - `file_age` → `FileAge`
  - `time_of_day{start_hour, end_hour}` → `filter.hours` + `Each`
  - `diff_size{..}` → `filter.lines` + `Each`
  - `first{scope}` → `First{scope}`

- [ ] **Step 1: Write the failing tests:**
  - `legacy_detector_still_parses` (all 10 files in `assets/rules` load with zero errors; `night_shift` has `Counter::Each` and an `hours` filter)
  - `ladder_parses` (a 3-step `lifetime` ladder: `steps.len() == 3`, `ladder`, step xp defaults by tier)
  - `platinum_default_xp_is_2000`
  - `step_ids`: `committed.silver`, and a single rule's id unchanged
  - `bad_regex_is_a_rule_error`: `load_dir` with one bad file and one good file gives 1 rule and 1 error
  - `repeated_tier_rejected`
  - `threshold_counter_needs_at`
  - `moment_counter_rejects_tiers`
  - `detector_and_counter_rejected`
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core rules::`. Expected: compile errors.
- [ ] **Step 3: Implement** `rules.rs`. Fix the compile fallout everywhere with `Tier::as_str/label/rank`, `rule.top().tier` and `rule.top().xp`. In tests that build `Rule` literals (`engine.rs`, `flavor.rs`), build rules with `parse_rule(toml)` instead.
- [ ] **Step 4: Run the whole workspace and check it compiles.** Run `cargo test --workspace`. Expected: rules tests PASS. Engine tests may fail until Task 6 replaces `detectors.rs`; leave them failing until then.
- [ ] **Step 5: Commit** `Rules: counters, ladder steps, Platinum, legacy detector mapping (#2)`

### Task 6: Moment counters (replace detectors.rs)

**Files:**
- Create: `crates/rpg-core/src/counters.rs`. Move the existing `detectors.rs` tests into it and delete `detectors.rs`.
- Modify: `crates/rpg-core/src/engine.rs` (`process_inner` rule loop), `crates/rpg-core/src/lib.rs`

**Interfaces:**
- Consumes: `Rule`, `Step`, `Counter`, `Filter::matches` (Tasks 4–5), `Store::events_of` (Task 3)
- Produces:
  - `pub struct EvalCtx<'a> { pub ev: &'a Event, pub store: &'a Store, pub broken: &'a [RecordBroken], pub file_ages: &'a [FileAge], pub tz: FixedOffset, pub skill_level_ups: &'a [SkillLevelUp] }`
  - `pub struct Fired { pub unlock_key: String, pub name: String, pub step: Step, pub findings: Value }`. `unlock_key` is the stored `unlocks.rule_id`; `name` is `rule.name` for single rules and `"{name} ({Tier label})"` for ladders.
  - `pub fn evaluate(rule: &Rule, ctx: &EvalCtx) -> Result<Vec<Fired>>`. The caller has already checked `rule.filter.matches(ev)`.
- Moment semantics:
  - `per_day`: fires when the count of today's **matching** events (scan with `events_of` from the local day start, applying the filter) `== at`
  - `run`: the last `at` events of `ev.kind` in the same `repo` (including `ev`) all match the filter
  - `combo`: for `kinds = [k1..kn]` with `kn == ev.kind`, there are events of k1..k(n−1) in non-decreasing `ts` order within `within_minutes` before `ev.ts`, all with the same GitHub key. The GitHub key is `data.github_repo` for git events and `repo` for GitHub events, and events without one never match.
  - `gap_days{min, scope}`: the previous matching event (in scope) is at least `min` days before `ev`. Fires only if a previous one exists.
  - `first{year}`: no other event of the kind in `ev`'s local calendar year
  - `personal_record` and `file_age`: unchanged
  - findings: as today; for the new counters, the count/gap/run length
- Engine changes:
  - skip rules whose filter doesn't match
  - keep the existing repeatable/cooldown checks keyed on `rule.id` for non-ladder rules
  - insert one unlock per `Fired`, with `rule_id = unlock_key`, `tier = step.tier.as_str()`, `xp = step.xp`
  - `Unlock.tier = step.tier`, `Unlock.xp = step.xp`, `Unlock.rule_id = unlock_key`
  - add `Unlock.base_rule_id: String` (= `rule.id`)

- [ ] **Step 1: Write the failing tests** in `counters.rs`:
  - the moved `detectors.rs` tests, rewritten to use `parse_rule`
  - `per_day_counts_only_matching` (10 commits, 3 with "wip": a `per_day{at=3}` rule with a "wip" filter fires on the third wip commit only)
  - `run_requires_consecutive` (wip, wip, fix, wip, wip, wip fires on the last)
  - `combo_same_repo_in_order` (a commit at 10:00 and a push at 10:05 with github_repo `a/b`, then a pr_opened at 10:12 with repo `a/b`, fires; the same with a PR in `c/d` doesn't; with the commit at 09:50 it doesn't)
  - `gap_days_scope_repo`
  - `first_year`
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core counters::`. Expected: compile errors.
- [ ] **Step 3: Implement** `counters.rs` and the engine changes.
- [ ] **Step 4: Run the whole workspace and check it passes.** Run `cargo test --workspace`. Expected: PASS, including the old engine tests.
- [ ] **Step 5: Commit** `Engine: moment counters replace detectors (#2)`

### Task 7: Threshold counters and ladders

**Files:**
- Modify: `crates/rpg-core/src/counters.rs`, `crates/rpg-core/src/engine.rs`
- Test: `counters.rs` and `engine.rs` tests

**Interfaces:**
- Produces:
  - `pub fn progress(rule: &Rule, store: &Store, tz: FixedOffset) -> Result<Option<u64>>`. It returns the current value for `Lifetime`, `StreakDays` and `Meta`, and `None` for moment counters and `SkillLevel`. The site uses it (Task 11), and `evaluate` uses it for threshold steps.
- Semantics:
  - `lifetime`: the number of stored events of the trigger kinds that match the filter. With `sum = "lines_removed"`, the sum of that numeric data field; with `distinct = "repo"`, the count of distinct `repo`.
  - `streak_days`: consecutive local days, ending on `ev`'s day, with ≥1 matching event
  - `month_every_day` (moment): `ev`'s local month has a matching event on every day of the month, and `ev` is the first matching event on its day
- Firing: for a threshold rule, every step with `progress >= at` whose `unlock_key` has `store.unlock_count == 0` fires, in step order.

- [ ] **Step 1: Write the failing tests:**
  - `lifetime_counts_filtered_history_including_backfill` (40 stored commits, 10 of them backfill, then a live commit with `at = 41`: fires)
  - `ladder_catches_up_on_next_event` (store 150 commits without running rules, then process one more commit through `engine::process` with a 100/1000 ladder: exactly `committed.bronze` unlocks)
  - `one_event_unlocks_all_crossed_steps_once` (1,200 stored commits, then two more commits processed: the first unlocks bronze and silver, the second unlocks nothing)
  - `lifetime_sum_lines_removed`
  - `lifetime_distinct_repo`
  - `streak_days_breaks_on_gap`
  - `month_every_day_fires_once` (February 2027: fires on the first commit of Feb 28 only)
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and check they pass.** Run `cargo test -p rpg-core`. Expected: PASS.
- [ ] **Step 5: Commit** `Engine: lifetime, streak and month counters with ladder steps (#2)`

### Task 8: Meta and skill-level counters, plus the rule-format reference

**Files:**
- Modify: `crates/rpg-core/src/counters.rs`, `crates/rpg-core/src/engine.rs`, `crates/rpg-core/src/store.rs`
- Create: `docs/rules.md`
- Modify: `README.md` (one link to `docs/rules.md` under Install and first run, step 3)

**Interfaces:**
- Produces:
  - `pub fn unlock_counts(&self) -> Result<Vec<(String, u32)>>` in `store.rs` (per stored `rule_id`)
  - `Meta{DistinctUnlocks}`: the number of distinct base rule ids among the unlocks, where the base id is the part before the first `.`. The meta rule's own id is excluded.
  - `Meta{SameUnlock}`: the highest unlock count of any single non-ladder `rule_id`. The meta rule's own id is excluded.
  - `SkillLevel`: for each `SkillLevelUp` in `ctx.skill_level_ups`, every step with `level >= at` not yet unlocked fires, with `unlock_key = "{rule.id}.{skill_id}.{tier}"` and name `"{rule.name}: {skill name} ({Tier label})"`.
- Engine: evaluate all non-`Meta` rules first, then `Meta` rules, so meta counts include this event's unlocks. `EvalCtx.skill_level_ups` is this event's `out.skill_level_ups`, which `award_skills` has already filled.

- [ ] **Step 1: Write the failing tests:**
  - `distinct_unlocks_counts_base_ids` (unlocks `a`, `a`, `b.bronze` and `b.silver` count as 2)
  - `meta_sees_same_event_unlocks`
  - `same_unlock_ignores_ladders`
  - `skill_level_per_skill` (Rust reaching level 5 unlocks `skill_master.rust.bronze`; Python reaching level 5 later unlocks `skill_master.python.bronze`)
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core`. Expected: FAIL.
- [ ] **Step 3: Implement. Write `docs/rules.md`:** every rule field, every filter (with type and the event kinds it applies to), every counter (moment vs. threshold), tiers, step ids and the "changing a step's tier re-unlocks it" warning, `announce`, `hidden`, `share_text`, `enabled`, and one example for each counter.
- [ ] **Step 4: Run the whole workspace and check it passes.** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** `Engine: meta and skill-level counters; rule format reference (#2)`. Open PR 2.

---

## Phase 3: Presentation (PR 3)

### Task 9: Announce: banner or toast

**Files:**
- Modify: `crates/rpg-core/src/config.rs`, `crates/rpg-core/assets/config.default.toml`, `crates/rpg-core/src/engine.rs`
- Modify: `crates/rpg-app/src/presenter.rs`, `crates/rpg-app/src/overlay.rs` (`ToastPayload`, banner tier), `ui/overlay/overlay.js`, `ui/overlay/overlay.css`

**Interfaces:**
- Produces:
  - `pub struct AnnounceConfig { pub bronze: Announce, pub silver: Announce, pub gold: Announce, pub platinum: Announce }` with `fn for_tier(&self, Tier) -> Announce`, as `Config.announce` (`[announce]`). Add the section to `config.default.toml`.
  - `Unlock.announce: Announce`, resolved in the engine as `step.announce.or(rule.announce).unwrap_or(cfg.announce.for_tier(step.tier))`.
  - `XpGain.achievement: Option<(Tier, Announce)>`, set on the gain that the engine pushes for an unlock.
  - presenter `Toast.tier: Option<Tier>`, which is `Some` for an achievement toast
  - overlay `ToastPayload.tier: Option<String>` (`Tier::label`)
- Presenter rules:
  - in `push_xp`, a gain with `achievement == Some((t, Toast))` becomes `Toast { amount, label, tier: Some(t), .. }`
  - only `announce == Banner` unlocks become banners
  - `STORM_CAP` and `collapse` count banner unlocks only
  - away batches keep summing all XP into one toast, as today
  - DND drops achievement toasts like other toasts
- Overlay:
  - an achievement toast is a pill with class `xp ach` and `data-tier`, showing `"✦ " + label + "  +N exp"`, with a tier-colored border and glow, and no sound
  - bronze `#cd7f32`, silver `#c0c0c0`, gold `#ffc83d`, platinum `#cfe8ff`
  - add `.ach-b[data-tier="Platinum"]` with a `#cfe8ff` glow to the banner

- [ ] **Step 1: Write the failing tests** in `presenter.rs`:
  - `toast_announced_unlock_is_a_toast_not_a_banner`
  - `storm_counts_only_banner_unlocks` (5 bronze toasts and 2 silver banners give 2 banners, no storm summary, and 5 achievement toasts)
  - `platinum_ranks_first_in_collapse`
  
  In `engine.rs`:
  - `announce_resolution_order` (step over rule over config)
  
  In `config.rs`:
  - `announce_defaults` (bronze toast, the others banner)
- [ ] **Step 2: Run them and check they fail.** Run `cargo test --workspace`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and check they pass.** Run `cargo test --workspace`. Expected: PASS. Then run the app (`cargo tauri dev` in `crates/rpg-app`), make a commit on a Saturday-filter test rule, and check by eye that the achievement toast and a platinum banner render. Add both checks to `docs/manual-checklist.md`.
- [ ] **Step 5: Commit** `Overlay: achievement toasts and the banner-or-toast setting (#2)`

### Task 10: Keep commit text out of the LLM context

**Files:**
- Modify: `crates/rpg-core/src/flavor.rs` (`build_context`)

**Interfaces:**
- Consumes: `Rule.share_text` (Task 5)
- Behavior: unless `rule.share_text` is set, remove the keys `message` and `title` from `findings`, recursively. Counters may include them in findings, so the site and debugging can show them.

- [ ] **Step 1: Write the failing tests** `message_stripped_by_default` and `message_kept_with_share_text`.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core flavor::`. Expected: FAIL.
- [ ] **Step 3: Implement.** The currently unused `_rule` parameter becomes `rule`.
- [ ] **Step 4: Run them and check they pass.** Same command. Expected: PASS.
- [ ] **Step 5: Commit** `Flavor: keep commit messages and PR titles out of the LLM context (#2)`

### Task 11: Character sheet: ladders, progress, hidden, Platinum

**Files:**
- Modify: `crates/rpg-app/src/site.rs` (`achievements`), `ui/site/site.js`, `ui/site/site.css`

**Interfaces:**
- Consumes: `counters::progress`, `Rule::step_id`, `Rule.hidden`, `Rule.steps`
- Produces: `GET /api/achievements` returns `{ "achievements": [...], "hidden_remaining": n }`. That's a breaking change to the array shape; update `site.js` in the same commit. Each entry keeps today's fields and adds:
  - `"ladder": bool`
  - `"steps": [{ "tier", "at", "unlocked", "last_at" }]`, ladders only
  - `"progress": u64 | null`
  - `"enabled": bool`, true until Task 12
- Rules:
  - a hidden rule with no unlocks is omitted and counted in `hidden_remaining`
  - an unlock belongs to a rule when `rule_id == r.id` or `rule_id.starts_with(&format!("{}.", r.id))`, so step unlocks aren't treated as orphans
  - skill ladders get one entry per skill that has any skill XP, with id `"{rule.id}.{skill_id}"` and progress equal to the skill level
  - locked steps above the next one show tier and `"???"` for `at`
- UI: a ladder card shows the current tier and a progress bar toward the next step ("Silver · 412 / 1,000"), and "N hidden achievements remain" when n > 0. Add `--platinum: #cfe8ff` and `.ach .tier.platinum`.

- [ ] **Step 1: Write the failing tests** in the `site.rs` tests:
  - `ladder_entry_has_steps_and_progress`
  - `step_unlocks_are_not_orphans`
  - `hidden_locked_rule_omitted_and_counted`
  - `skill_ladder_one_entry_per_skill`
  
  Update the existing tests to the new response shape.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-app site::`. Expected: FAIL.
- [ ] **Step 3: Implement** the API and the UI.
- [ ] **Step 4: Run them and check they pass.** Run `cargo test --workspace`. Expected: PASS. Then open the character sheet and check a ladder, a hidden count and a platinum card by eye. Add those checks to `docs/manual-checklist.md`.
- [ ] **Step 5: Commit** `Site: ladders with progress, hidden achievements, Platinum (#2)`. Open PR 3.

---

## Phase 4: Seeding and `enabled` (PR 4, closes #26)

### Task 12: `enabled` flag

**Files:**
- Modify: `crates/rpg-core/src/rules.rs` (`RawRule.enabled`, default true; `Rule.enabled`; `Rules::for_kind` skips disabled), `crates/rpg-app/src/site.rs` (`"enabled"` from the rule), `ui/site/site.js` (a dimmed "off" badge)

- [ ] **Step 1: Write the failing tests** `disabled_rule_loads_but_never_fires` (in `engine.rs`: `enabled = false` gives no unlock, the rule is still in `rules.list`, and no load error), `reenabled_ladder_catches_up` (disabled while 150 commits arrive, then re-enabled: the next commit unlocks bronze), and the site test `disabled_rule_shown_off`.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test --workspace`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and check they pass.** Same command. Expected: PASS.
- [ ] **Step 5: Commit** `Rules: enabled flag (#26)`

### Task 13: Seed new starter rules on upgrade

**Files:**
- Modify: `crates/rpg-core/src/paths.rs` (`ensure`, `seed`), plus its tests

**Interfaces:**
- Produces:
  - `pub fn seeded_manifest(&self) -> PathBuf` = `data/seeded-rules.txt`, one starter file name per line
  - Algorithm in `ensure`:
    1. Read the manifest.
    2. If it's missing, start it from the starter file names that already exist in `rules/`. An empty `rules/` gives an empty manifest.
    3. For each embedded starter file whose name isn't in the manifest: write it if no file of that name exists, and add it to the manifest.
    4. Write the manifest.
  - Skills seeding is unchanged.

- [ ] **Step 1: Write the failing tests:**
  - `fresh_install_seeds_all_and_writes_manifest`
  - `manifest_created_from_existing_files` (`rules/` holds 3 of the starters, one of them edited. After `ensure`, every starter exists and the edited file is unchanged.)
  - `deleted_starter_stays_deleted` (`ensure`, delete `night_shift.toml`, `ensure` again: still absent)
  - `new_starter_added_on_upgrade` (the manifest lacks one name: that file appears)
  - Keep `ensure_seeds_rules_and_config` passing.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core paths::`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and check they pass.** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** `Paths: seed new starter rules on upgrade, remember deletions (#26)`. Open PR 4 with "Closes #26".

---

## Phase 5: The catalog (PR 5, closes #2)

Every file is in `crates/rpg-core/assets/rules/<id>.toml`. Copy (`fallback`, `hint`) is in the voice of the existing ten: snarky, specific, *Dungeon Crawler Carl*, 1–2 sentences. `flavor = llm` goes on the repeatable silver+ ones, as in v0. Cooldown is 168h for the habit rules listed in the spec's "My calls". Rare rules set `announce = "banner"`. Ladders are not repeatable.

### Task 14: Catalog harness, clock and calendar, volume in a day

**Files:**
- Create: `crates/rpg-core/tests/catalog.rs`
- Create rules: `weekend_warrior`, `dawn_patrol`, `leet` (name "1337"), `make_a_wish`, `leap_of_faith`, `read_only_friday`, `holiday_spirit`, `new_year_same_bugs`, `speed_run`, `rubber_stamp`, `codependent`

**Interfaces:**
- Produces, in `tests/catalog.rs`:
  - `fn starter() -> Rules` (`load_dir` on `assets/rules`; asserts zero errors)
  - `fn fires(rules: &Rules, history: &[Event], ev: Event, expect: &str) -> bool`, which runs `history` and then `ev` through `engine::process` on an in-memory store with tz UTC and returns whether an unlock whose `rule_id == expect` came out of `ev`
  - one `#[test]` per rule with one firing case and one near-miss case
  - `fn every_starter_rule_has_a_test()`, which compares `starter().list` ids against a `const COVERED: &[&str]` list maintained in the file. It fails if a rule has no test.

Rule settings (copy is the builder's):

| id | trigger | filter | counter | tier, other |
| --- | --- | --- | --- | --- |
| weekend_warrior | commit | `weekdays = ["sat","sun"]` | each | bronze, cooldown 168 |
| dawn_patrol | commit | `hours = {start=5,end=7}` | each | bronze, cooldown 168 |
| leet | commit | `time = "13:37"` | each | silver, banner |
| make_a_wish | commit | `time = "11:11"` | each | bronze, cooldown 168 |
| leap_of_faith | commit | `dates = ["02-29"]` | each | gold, banner, cooldown 24 |
| read_only_friday | push | `branch = ["main","master"]`, `weekdays = ["fri"]`, `hours = {start=16,end=24}` | each | bronze, cooldown 168 |
| holiday_spirit | commit | `dates = ["12-25","01-01"]` | each | silver, cooldown 24 |
| new_year_same_bugs | commit | — | first scope=year | bronze |
| speed_run | commit | — | per_day at=10 | silver |
| rubber_stamp | review_submitted | — | per_day at=10 | silver |
| codependent | claude_session_end | `session_minutes = {min=0}` | per_day at=10 | silver |

- [ ] **Step 1: Write the harness and the tests for these 11 rules,** plus the 10 v0 rules, so `COVERED` starts complete for the existing rules.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core --test catalog`. Expected: the new rules' tests FAIL (rules missing).
- [ ] **Step 3: Write the 11 TOML files.**
- [ ] **Step 4: Run them and check they pass.** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** `Catalog: clock, calendar and daily-volume achievements (#2)`

### Task 15: Ladders

**Files:** create rules `committed`, `merge_conflict_of_interest`, `professional_critic`, `lumberjack`, `frequent_flyer`, `claudes_favorite`, `spellcheck_paladin`, `ticket_machine`, `test_believer`, `on_a_roll`, `achievement_hunter`, `skill_master`; add their tests to `tests/catalog.rs`. Each test seeds history just under bronze and then fires one event that reaches it.

| id | trigger | filter | counter | tiers (at) |
| --- | --- | --- | --- | --- |
| committed | commit | — | lifetime | bronze 100, silver 1000, gold 5000 |
| merge_conflict_of_interest | pr_merged | — | lifetime | 10, 100, 500 |
| professional_critic | review_submitted | — | lifetime | 25, 250, 1000 |
| lumberjack | commit | — | lifetime sum=`lines_removed` | 1000, 10000, 100000 |
| frequent_flyer | commit | — | lifetime distinct=`repo` | 5, 25, 100 |
| claudes_favorite | claude_session_end | `session_minutes = {min=0}` | lifetime | 50, 500, 2000 |
| spellcheck_paladin | commit | `message = "\\btypos?\\b"` | lifetime | 5, 25, 100 |
| ticket_machine | commit | `message = "#\\d+"` | lifetime | 10, 100, 500 |
| test_believer | commit | `files_any = ["tests/**","test/**","**/tests/**","*_test.*","*.test.*","*.spec.*","test_*"]` | lifetime | 10, 100, 500 |
| on_a_roll | commit | — | streak_days | 5, 15, 30 |
| achievement_hunter | commit, push, pr_opened, pr_merged, review_submitted, claude_session_end | — | meta of=distinct_unlocks | 10, 25, 50 |
| skill_master | commit, review_submitted, claude_session_end | — | skill_level | 5, 10, 20 |

- [ ] **Step 1: Write the tests.** One per ladder, firing bronze.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core --test catalog`. Expected: FAIL.
- [ ] **Step 3: Write the 12 TOML files.**
- [ ] **Step 4: Run them and check they pass.** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** `Catalog: ladders (#2)`

### Task 16: Streaks, rhythm, Claude sessions

**Files:** create rules `touch_grass_denied`, `the_return`, `necromancer`, `full_combo`, `lgtm_speedrun`, `thousand_year_branch`, `groundhog_day`, `marathon`, `speed_date`, `pair_programming`, plus their tests.

| id | trigger | filter | counter | tier, other |
| --- | --- | --- | --- | --- |
| touch_grass_denied | commit | — | month_every_day | gold, banner |
| the_return | commit | — | gap_days min=14 scope=ever | silver |
| necromancer | commit | — | gap_days min=365 scope=repo | silver |
| full_combo | pr_opened | — | combo kinds=[commit,push,pr_opened] within=15 | silver |
| lgtm_speedrun | pr_merged | `pr_open_minutes = {max=10}` | each | bronze |
| thousand_year_branch | pr_merged | `pr_open_minutes = {min=43200}` | each | silver |
| groundhog_day | commit, push, pr_opened, pr_merged, review_submitted, claude_session_end | — | meta of=same_unlock, `at = 10` | silver (not repeatable) |
| marathon | claude_session_end | `session_minutes = {min=180}` | each | silver |
| speed_date | claude_session_end | `session_minutes = {max=0.5}` | each | bronze |
| pair_programming | claude_session_start | `open_sessions = {min=3}` | each | silver, cooldown 24 |

- [ ] **Step 1: Write the tests.**
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core --test catalog`. Expected: FAIL.
- [ ] **Step 3: Write the 10 TOML files.**
- [ ] **Step 4: Run them and check they pass.** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** `Catalog: streaks, rhythm and Claude-session achievements (#2)`

### Task 17: Messages, files, branches, hashes

**Files:** create rules `wip_wip_wip`, `famous_last_words`, `always_dns`, `profanity_filter_disabled`, `eloquent`, `the_novelist`, `emotional_support_emoji`, `polyglot`, `tiny_violence`, `find_and_replace_all`, `documentation_gremlin`, `lawyered`, `dependency_hell`, `naming_things_is_hard`, `self_inflicted`, `palindrome`, `hash_brown`, `lucky_sevens`, plus their tests. Then add `every_starter_rule_has_a_test` to the required checks, so it passes with `COVERED` complete.

| id | trigger | filter | counter | tier, other |
| --- | --- | --- | --- | --- |
| wip_wip_wip | commit | `message = "\\bwip\\b"` | run at=3 | bronze |
| famous_last_words | commit | `message = "\\b(final( final)?\|fix(ed)? for real\|should work( now)?\|last fix\|definitely works)\\b"` | each | silver, cooldown 72 |
| always_dns | pr_opened | `title = "\\bdns\\b"` | each | silver, banner |
| profanity_filter_disabled | commit | `message =` a word-boundary list of common English swears (builder's list) | each | bronze, hidden, cooldown 168 |
| eloquent | commit | `message = "^\\s*\\S+\\s*$"` | each | bronze, cooldown 168 |
| the_novelist | commit | `message_length = {min=500}` | each | silver |
| emotional_support_emoji | commit | `message = "\\p{Extended_Pictographic}"` | each | bronze, cooldown 168 |
| polyglot | commit | `min_extensions = 4` | each | silver, cooldown 72 |
| tiny_violence | pr_merged | `lines = {min=1, max=4}` | each | bronze |
| find_and_replace_all | commit | `min_files = 100` | each | silver |
| documentation_gremlin | commit | `files_only = ["*.md","*.mdx"]` | each | bronze, cooldown 168 |
| lawyered | commit | `files_any = ["LICENSE*","**/LICENSE*","COPYING*","**/COPYING*"]` | each | bronze, banner |
| dependency_hell | commit | `files_only = ["Cargo.lock","package-lock.json","yarn.lock","pnpm-lock.yaml","poetry.lock","Gemfile.lock","go.sum","composer.lock","*.lock"]` | each | bronze, cooldown 168 |
| naming_things_is_hard | commit | `branch = ["test","temp","tmp","asdf","fix2","new-branch","wip"]` | each | bronze, hidden, cooldown 168 |
| self_inflicted | push | `branch = ["main","master"]` | each | bronze, cooldown 168 |
| palindrome | commit | `hash_palindrome = 7` | each | gold, banner |
| hash_brown | commit | `hash = "^(dead\|beef\|cafe\|f00d)"` | each | silver, banner |
| lucky_sevens | commit | `hash = "777"` | each | bronze, banner |

- [ ] **Step 1: Write the tests.** The profanity test uses a word from the builder's list, and its near-miss case uses a word that contains a swear inside it ("Scunthorpe"-style), which must not fire.
- [ ] **Step 2: Run them and check they fail.** Run `cargo test -p rpg-core --test catalog`. Expected: FAIL.
- [ ] **Step 3: Write the 18 TOML files.**
- [ ] **Step 4: Run the whole workspace and check it passes.** Run `cargo test --workspace`. Expected: PASS, including `every_starter_rule_has_a_test`.
- [ ] **Step 5: Commit** `Catalog: message, file, branch and hash achievements (#2)`. Open PR 5 with "Closes #2".
