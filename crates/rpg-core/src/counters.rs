//! Counters decide whether a rule fires for an event and which steps it unlocks.
//!
//! Moment counters look at the current event (and its history) and fire the rule's single
//! step. Threshold counters compute a running value (`progress`) and unlock each step whose
//! `at` it has reached. History scans are over events of `ev.kind` unless a counter says otherwise; the
//! current event is already in the store when counters run.
use crate::engine::SkillLevelUp;
use crate::enrich::FileAge;
use crate::event::{Event, EventKind};
use crate::records::{local_day_bounds, metric_key, RecordBroken};
use crate::rules::{Counter, FirstScope, Rule, Scope, Step};
use crate::store::Store;
use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, FixedOffset, Months, NaiveDate, TimeZone, Utc};
use std::collections::BTreeSet;
use serde_json::{json, Value};

pub struct EvalCtx<'a> {
    pub ev: &'a Event,
    pub store: &'a Store,
    pub broken: &'a [RecordBroken],
    pub file_ages: &'a [FileAge],
    pub tz: FixedOffset,
    pub skill_level_ups: &'a [SkillLevelUp],
}

/// One step a rule unlocked for this event.
#[derive(Debug, Clone, PartialEq)]
pub struct Fired {
    /// The stored `unlocks.rule_id`: the rule id, or `id.tier` for a ladder step.
    pub unlock_key: String,
    /// `rule.name`, or `"{name} ({Tier})"` for a ladder step.
    pub name: String,
    pub step: Step,
    /// Context for the flavor text.
    pub findings: Value,
}

fn fired(rule: &Rule, step: &Step, findings: Value) -> Fired {
    let name = if rule.ladder { format!("{} ({})", rule.name, step.tier.label()) } else { rule.name.clone() };
    Fired { unlock_key: rule.step_id(step.tier), name, step: step.clone(), findings }
}

/// The steps `rule` unlocks for `ctx.ev`. The caller has already checked `rule.filter`.
///
/// A threshold rule unlocks every step with `progress >= at` that isn't unlocked yet, in
/// step order, so one event can catch up on several steps. Repeatable/cooldown don't apply.
pub fn evaluate(rule: &Rule, ctx: &EvalCtx) -> Result<Vec<Fired>> {
    if !rule.counter.is_threshold() {
        return Ok(moment(rule, ctx)?.map(|f| vec![fired(rule, rule.top(), f)]).unwrap_or_default());
    }
    let value = match &rule.counter {
        Counter::StreakDays => Some(streak_days(rule, ctx.store, ctx.tz, Some(local_date(ctx.ev.ts, ctx.tz)))?),
        _ => progress(rule, ctx.store, ctx.tz)?,
    };
    let Some(value) = value else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for step in &rule.steps {
        let Some(at) = step.at else { continue };
        if value >= at && ctx.store.unlock_count(&rule.step_id(step.tier))? == 0 {
            out.push(fired(rule, step, json!({ "progress": value, "at": at })));
        }
    }
    Ok(out)
}

/// The current value of a threshold counter, from the stored events of the rule's `trigger`
/// kinds that match its filter (backfill included). `None` for moment counters and
/// `skill_level`. `streak_days` is the streak ending on the local day of the most recent
/// matching event (0 with none).
pub fn progress(rule: &Rule, store: &Store, tz: FixedOffset) -> Result<Option<u64>> {
    Ok(match &rule.counter {
        Counter::Lifetime { sum, distinct } => {
            let events = matching(rule, store, tz, None)?;
            Some(if let Some(field) = sum {
                // A missing or non-numeric field adds 0: the event still matched the filter.
                let total: f64 = events.iter().map(|e| e.data[field.as_str()].as_f64().unwrap_or(0.0)).sum();
                total.max(0.0).floor() as u64
            } else if distinct.is_some() {
                // `distinct` is validated to be "repo".
                events.iter().filter_map(|e| e.repo.as_deref()).collect::<BTreeSet<_>>().len() as u64
            } else {
                events.len() as u64
            })
        }
        Counter::StreakDays => Some(streak_days(rule, store, tz, None)?),
        // `meta` arrives in Task 8; `skill_level` and moment counters have no running value.
        _ => None,
    })
}

/// Stored events of the rule's `trigger` kinds since `since` that match its filter, oldest first.
fn matching(rule: &Rule, store: &Store, tz: FixedOffset, since: Option<DateTime<Utc>>) -> Result<Vec<Event>> {
    let mut events = store.events_of(&rule.trigger, since)?;
    events.retain(|e| rule.filter.matches(e, tz));
    Ok(events)
}

fn local_date(ts: DateTime<Utc>, tz: FixedOffset) -> NaiveDate {
    ts.with_timezone(&tz).date_naive()
}

/// Consecutive local days with a matching event, ending on `end` (default: the day of the
/// most recent matching event).
fn streak_days(rule: &Rule, store: &Store, tz: FixedOffset, end: Option<NaiveDate>) -> Result<u64> {
    let days: BTreeSet<NaiveDate> = matching(rule, store, tz, None)?.iter().map(|e| local_date(e.ts, tz)).collect();
    let Some(mut day) = end.or_else(|| days.last().copied()) else { return Ok(0) };
    let mut n = 0;
    while days.contains(&day) {
        n += 1;
        day = day.pred_opt().expect("date in range");
    }
    Ok(n)
}

/// `ev` is the first matching event on the last day of its local month, and every day of
/// that month has a matching event.
fn month_every_day(rule: &Rule, ctx: &EvalCtx) -> Result<Option<Value>> {
    let ev = ctx.ev;
    let today = local_date(ev.ts, ctx.tz);
    let first = today.with_day(1).expect("day 1 exists");
    let next = first.checked_add_months(Months::new(1)).expect("date in range");
    let last = next.pred_opt().expect("date in range");
    if today != last {
        return Ok(None);
    }
    let since = local_midnight(first, ctx.tz);
    let events = matching(rule, ctx.store, ctx.tz, Some(since))?;
    let events: Vec<&Event> = events.iter().filter(|e| local_date(e.ts, ctx.tz) <= last).collect();
    if events.iter().find(|e| local_date(e.ts, ctx.tz) == today).map(|e| e.id.as_str()) != Some(ev.id.as_str()) {
        return Ok(None);
    }
    let days: BTreeSet<u32> = events.iter().map(|e| local_date(e.ts, ctx.tz).day()).collect();
    let n = last.day();
    Ok((days.len() == n as usize).then(|| json!({ "month": today.format("%Y-%m").to_string(), "days": n })))
}

/// `Some(findings)` if a moment counter fires.
fn moment(rule: &Rule, ctx: &EvalCtx) -> Result<Option<Value>> {
    let ev = ctx.ev;
    Ok(match &rule.counter {
        Counter::Each => Some(json!({})),
        Counter::PerDay { at } => {
            let (from, to) = local_day_bounds(ev.ts, ctx.tz);
            let n = ctx
                .store
                .events_of(&[ev.kind], Some(from))?
                .iter()
                .filter(|e| e.ts < to && rule.filter.matches(e, ctx.tz))
                .count();
            (n == *at as usize).then(|| json!({ "count": n }))
        }
        Counter::Run { at } => {
            let history = history(ctx, |e| e.repo == ev.repo)?;
            let run = history.iter().rev().take_while(|e| rule.filter.matches(e, ctx.tz)).count();
            // Once per run: when it reaches exactly `at` (the event before it doesn't match).
            (run == *at as usize).then(|| json!({ "run": run }))
        }
        Counter::Combo { kinds, within_minutes } => combo(ctx, kinds, *within_minutes)?,
        Counter::GapDays { min, scope } => {
            let history = history(ctx, |e| *scope == Scope::Ever || e.repo == ev.repo)?;
            // `history` ends with `ev`; the one before it is the previous matching event.
            let prev = history.iter().rev().skip(1).find(|e| rule.filter.matches(e, ctx.tz));
            prev.and_then(|p| {
                let gap = ev.ts - p.ts;
                (gap >= Duration::days(*min as i64)).then(|| json!({ "gap_days": gap.num_days(), "repo": ev.repo }))
            })
        }
        Counter::First { scope: scope @ (FirstScope::Ever | FirstScope::Repo) } => {
            let repo = if *scope == FirstScope::Repo { ev.repo.as_deref() } else { None };
            (!ctx.store.has_prior_event(ev.kind, repo, &ev.id)?).then(|| json!({ "repo": ev.repo }))
        }
        Counter::First { scope: FirstScope::Year } => {
            let year = ev.ts.with_timezone(&ctx.tz).year();
            let start = year_start(year, ctx.tz);
            let end = year_start(year + 1, ctx.tz);
            let other = ctx.store.events_of(&[ev.kind], Some(start))?.iter().any(|e| e.ts < end && e.id != ev.id);
            (!other).then(|| json!({ "year": year }))
        }
        Counter::PersonalRecord { metric } => ctx
            .broken
            .iter()
            .find(|b| b.metric == *metric)
            .map(|b| json!({ "metric": metric_key(b.metric), "old": b.old, "new": b.new })),
        Counter::FileAge { min_years } => ctx
            .file_ages
            .iter()
            .filter(|f| f.years >= *min_years)
            .max_by(|a, b| a.years.total_cmp(&b.years))
            .map(|f| json!({ "path": f.path, "years": f.years, "last_author": f.last_author })),
        Counter::MonthEveryDay => month_every_day(rule, ctx)?,
        // Threshold counters are handled in `evaluate`.
        Counter::Lifetime { .. } | Counter::StreakDays | Counter::Meta { .. } | Counter::SkillLevel => None,
    })
}

/// Events of `ev.kind` that `keep` accepts, oldest first, up to and including `ev`.
fn history(ctx: &EvalCtx, keep: impl Fn(&Event) -> bool) -> Result<Vec<Event>> {
    let mut all = ctx.store.events_of(&[ctx.ev.kind], None)?;
    if let Some(i) = all.iter().position(|e| e.id == ctx.ev.id) {
        all.truncate(i + 1);
    } else {
        // Not stored (shouldn't happen in the engine): treat it as the newest event.
        all.retain(|e| e.ts <= ctx.ev.ts);
        all.push(ctx.ev.clone());
    }
    all.retain(|e| keep(e));
    Ok(all)
}

/// UTC instant of local midnight on 1 January of `year`.
fn year_start(year: i32, tz: FixedOffset) -> DateTime<Utc> {
    local_midnight(NaiveDate::from_ymd_opt(year, 1, 1).expect("valid year"), tz)
}

/// UTC instant of local midnight at the start of `day`.
fn local_midnight(day: NaiveDate, tz: FixedOffset) -> DateTime<Utc> {
    let midnight = day.and_hms_opt(0, 0, 0).expect("valid midnight");
    tz.from_local_datetime(&midnight).single().expect("fixed offset is unambiguous").with_timezone(&Utc)
}

/// The GitHub repo (`owner/name`) an event belongs to: `data.github_repo` for git events,
/// `repo` for GitHub events, none otherwise.
fn github_key(e: &Event) -> Option<&str> {
    match e.kind {
        EventKind::Commit | EventKind::Push => e.data["github_repo"].as_str(),
        EventKind::PrOpened | EventKind::PrMerged | EventKind::ReviewSubmitted => e.repo.as_deref(),
        EventKind::ClaudeSessionStart | EventKind::ClaudeSessionEnd => None,
    }
    .filter(|k| !k.is_empty())
}

/// `kinds[..n-1]` happened in order (non-decreasing `ts`) within `within_minutes` before
/// `ev`, all in `ev`'s GitHub repo. `kinds`' last element is `ev.kind` (checked at load).
fn combo(ctx: &EvalCtx, kinds: &[EventKind], within_minutes: u32) -> Result<Option<Value>> {
    let ev = ctx.ev;
    let (Some(key), Some((last, before))) = (github_key(ev), kinds.split_last()) else { return Ok(None) };
    if *last != ev.kind {
        return Ok(None);
    }
    let since = ev.ts - Duration::minutes(within_minutes as i64);
    let candidates: Vec<Event> = ctx
        .store
        .events_of(before, Some(since))?
        .into_iter()
        .filter(|e| e.ts <= ev.ts && e.id != ev.id && github_key(e) == Some(key))
        .collect();
    // Greedy: the earliest event of each kind at or after the previous one's position.
    let mut pos = 0;
    let mut first_ts = None;
    for kind in before {
        let Some(i) = candidates[pos..].iter().position(|e| e.kind == *kind) else { return Ok(None) };
        first_ts.get_or_insert(candidates[pos + i].ts);
        pos += i + 1;
    }
    let minutes = first_ts.map_or(0, |t| (ev.ts - t).num_minutes());
    Ok(Some(json!({ "github_repo": key, "minutes": minutes })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrich::FileAge;
    use crate::event::{Event, EventKind, Source};
    use crate::records::{self, RecordBroken};
    use crate::rules::{parse_rule, Rule, Tier};
    use crate::store::Store;
    use chrono::{DateTime, FixedOffset, Utc};
    use serde_json::{json, Value};

    fn tz() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn mk(kind: EventKind, ts: &str, repo: &str, data: Value) -> Event {
        let mut e = Event::new(kind, Source::Git, Some(repo.into()), data);
        e.ts = at(ts);
        e
    }

    /// A bronze rule; `extra` holds the counter/filter TOML lines.
    fn rule(trigger: &str, extra: &str) -> Rule {
        parse_rule(&format!(
            "id=\"r\"\nname=\"R\"\ntier=\"bronze\"\nxp=7\nfallback=\"f\"\ntrigger=[{trigger}]\n{extra}\n"
        ))
        .unwrap()
    }

    fn eval_with(rule: &Rule, store: &Store, ev: &Event, broken: &[RecordBroken], ages: &[FileAge]) -> Vec<Fired> {
        let ctx = EvalCtx { ev, store, broken, file_ages: ages, tz: tz(), skill_level_ups: &[] };
        evaluate(rule, &ctx).unwrap()
    }

    /// Findings if the rule fired (as the engine would: filter first).
    fn eval(rule: &Rule, store: &Store, ev: &Event) -> Option<Value> {
        if !rule.filter.matches(ev, tz()) {
            return None;
        }
        let mut f = eval_with(rule, store, ev, &[], &[]);
        assert!(f.len() <= 1);
        f.pop().map(|f| f.findings)
    }

    /// Insert `ev` (as the engine does before rules run) and evaluate.
    fn add(rule: &Rule, store: &Store, ev: &Event) -> Option<Value> {
        store.insert_event(ev).unwrap();
        eval(rule, store, ev)
    }

    #[test]
    fn fired_carries_key_name_and_step() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "");
        let e = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({}));
        store.insert_event(&e).unwrap();
        let f = eval_with(&r, &store, &e, &[], &[]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].unlock_key, "r");
        assert_eq!(f[0].name, "R");
        assert_eq!((f[0].step.tier, f[0].step.xp), (Tier::Bronze, 7));
        assert_eq!(f[0].findings, json!({}));
    }

    #[test]
    fn ladder_step_key_and_name() {
        let r = parse_rule(
            "id=\"c\"\nname=\"Committed\"\nfallback=\"f\"\ntrigger=[\"commit\"]\ncounter={type=\"lifetime\"}\ntiers=[{tier=\"bronze\",at=1},{tier=\"silver\",at=5}]",
        )
        .unwrap();
        let f = fired(&r, &r.steps[1], json!({}));
        assert_eq!((f.unlock_key.as_str(), f.name.as_str()), ("c.silver", "Committed (Silver)"));
        assert_eq!(f.step.at, Some(5));
    }

    #[test]
    fn count_fires_exactly_once_per_day() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"pr_merged\"", "counter = { type = \"per_day\", at = 3 }");
        let mut results = vec![];
        for h in 1..=4 {
            let e = mk(EventKind::PrMerged, &format!("2026-03-01T0{h}:00:00Z"), "r", json!({}));
            results.push(add(&r, &store, &e));
        }
        assert_eq!(results[0], None);
        assert_eq!(results[1], None);
        assert_eq!(results[2], Some(json!({ "count": 3 })));
        assert_eq!(results[3], None);
    }

    #[test]
    fn count_uses_local_day() {
        let store = Store::open_in_memory().unwrap();
        let plus2 = FixedOffset::east_opt(2 * 3600).unwrap();
        // 23:30Z and 00:30Z are the same local day at +02:00.
        let a = mk(EventKind::PrMerged, "2026-03-01T23:30:00Z", "r", json!({}));
        let b = mk(EventKind::PrMerged, "2026-03-02T00:30:00Z", "r", json!({}));
        store.insert_event(&a).unwrap();
        store.insert_event(&b).unwrap();
        assert_eq!(records::count_today(&store, &b, plus2).unwrap(), 2);
        assert_eq!(records::count_today(&store, &b, tz()).unwrap(), 1);
        let r = rule("\"pr_merged\"", "counter = { type = \"per_day\", at = 2 }");
        let ctx = |tz| EvalCtx { ev: &b, store: &store, broken: &[], file_ages: &[], tz, skill_level_ups: &[] };
        assert_eq!(evaluate(&r, &ctx(plus2)).unwrap().len(), 1);
        assert!(evaluate(&r, &ctx(tz())).unwrap().is_empty());
    }

    #[test]
    fn record_seed_does_not_break_then_beat_does() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"pr_merged\"", "counter = { type = \"personal_record\", metric = \"pr_deletions\" }");
        let e1 = mk(EventKind::PrMerged, "2026-03-01T01:00:00Z", "r", json!({ "deletions": 100 }));
        store.insert_event(&e1).unwrap();
        let b1 = records::update(&store, &e1, tz()).unwrap();
        assert!(b1.is_empty());
        assert!(eval_with(&r, &store, &e1, &b1, &[]).is_empty());

        let e2 = mk(EventKind::PrMerged, "2026-03-02T01:00:00Z", "r", json!({ "deletions": 100 }));
        store.insert_event(&e2).unwrap();
        assert!(records::update(&store, &e2, tz()).unwrap().is_empty(), "equal is not a break");

        let e3 = mk(EventKind::PrMerged, "2026-03-03T01:00:00Z", "r", json!({ "deletions": 250 }));
        store.insert_event(&e3).unwrap();
        let b3 = records::update(&store, &e3, tz()).unwrap();
        let f = eval_with(&r, &store, &e3, &b3, &[]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].findings, json!({ "metric": "pr_deletions", "old": 100.0, "new": 250.0 }));
        assert_eq!(store.record("pr_deletions").unwrap(), Some(250.0));
    }

    #[test]
    fn file_age_picks_oldest_qualifying() {
        let store = Store::open_in_memory().unwrap();
        let e = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({}));
        let ages = vec![
            FileAge { path: "young.rs".into(), years: 1.0, last_author: "a".into() },
            FileAge { path: "mid.rs".into(), years: 6.0, last_author: "b".into() },
            FileAge { path: "old.rs".into(), years: 9.5, last_author: "c".into() },
        ];
        let five = rule("\"commit\"", "counter = { type = \"file_age\", min_years = 5 }");
        let f = eval_with(&five, &store, &e, &[], &ages);
        assert_eq!(f[0].findings["path"], "old.rs");
        assert_eq!(f[0].findings["last_author"], "c");
        let ten = rule("\"commit\"", "counter = { type = \"file_age\", min_years = 10 }");
        assert!(eval_with(&ten, &store, &e, &[], &ages).is_empty());
    }

    #[test]
    fn surgical_strike_needs_at_least_one_line() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rules");
        let (rules, _) = crate::rules::load_dir(&dir);
        let rule = rules.list.iter().find(|r| r.id == "surgical_strike").unwrap();
        let store = Store::open_in_memory().unwrap();
        let fires = |a: u64, r: u64| {
            let e = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({ "lines_added": a, "lines_removed": r }));
            eval(rule, &store, &e).is_some()
        };
        assert!(!fires(0, 0), "binary-only/rename commit");
        assert!(fires(1, 0));
        assert!(fires(1, 1));
        assert!(!fires(2, 1));
    }

    /// Task 8 implements these; until then they must not fire.
    #[test]
    fn meta_and_skill_level_counters_do_not_fire_yet() {
        let store = Store::open_in_memory().unwrap();
        let e = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({}));
        store.insert_event(&e).unwrap();
        for extra in [
            "at = 1
counter = { type = \"meta\", of = \"distinct_unlocks\" }",
            "at = 1
counter = { type = \"skill_level\" }",
        ] {
            assert_eq!(eval(&rule("\"commit\"", extra), &store, &e), None, "{extra}");
        }
    }

    #[test]
    fn lifetime_sum_lines_removed() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "at = 100
counter = { type = \"lifetime\", sum = \"lines_removed\" }");
        // 40 + 0 (missing) + 59.5 = 99.5, floored to 99: not yet.
        for (i, data) in [json!({ "lines_removed": 40 }), json!({}), json!({ "lines_removed": 59.5 })].into_iter().enumerate() {
            let e = mk(EventKind::Commit, &format!("2026-03-01T0{}:00:00Z", i + 1), "r", data);
            assert_eq!(add(&r, &store, &e), None, "{i}");
        }
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(99));
        let e = mk(EventKind::Commit, "2026-03-01T05:00:00Z", "r", json!({ "lines_removed": 1 }));
        assert_eq!(add(&r, &store, &e), Some(json!({ "progress": 100, "at": 100 })));
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(100));
    }

    #[test]
    fn lifetime_distinct_repo() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "at = 3
counter = { type = \"lifetime\", distinct = \"repo\" }");
        let mut fired = vec![];
        for (i, repo) in ["A", "B", "A", "B", "C"].iter().enumerate() {
            let e = mk(EventKind::Commit, &format!("2026-03-01T0{}:00:00Z", i + 1), repo, json!({}));
            if add(&r, &store, &e).is_some() {
                fired.push(i);
            }
        }
        assert_eq!(fired, vec![4], "the third distinct repo");
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(3));
    }

    #[test]
    fn lifetime_counts_all_trigger_kinds() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\", \"push\"", "at = 3
counter = { type = \"lifetime\" }");
        store.insert_event(&mk(EventKind::Push, "2026-03-01T01:00:00Z", "r", json!({}))).unwrap();
        store.insert_event(&mk(EventKind::PrMerged, "2026-03-01T02:00:00Z", "r", json!({}))).unwrap();
        let c = mk(EventKind::Commit, "2026-03-01T03:00:00Z", "r", json!({}));
        assert_eq!(add(&r, &store, &c), None);
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(2));
        let c = mk(EventKind::Commit, "2026-03-01T04:00:00Z", "r", json!({}));
        assert!(add(&r, &store, &c).is_some());
    }

    #[test]
    fn streak_days_breaks_on_gap() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "filter = { message = 'wip' }
at = 3
counter = { type = \"streak_days\" }");
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(0), "no events");
        let day = |d: u32, h: u32, msg: &str| mk(EventKind::Commit, &format!("2026-03-{d:02}T{h:02}:00:00Z"), "r", json!({ "message": msg }));
        assert_eq!(add(&r, &store, &day(1, 10, "wip")), None);
        assert_eq!(add(&r, &store, &day(2, 23, "wip")), None);
        assert_eq!(add(&r, &store, &day(3, 0, "wip")), Some(json!({ "progress": 3, "at": 3 })));
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(3));
        // Day 4 has only a non-matching commit: the streak breaks.
        assert_eq!(add(&r, &store, &day(4, 10, "fix")), None);
        assert_eq!(add(&r, &store, &day(5, 10, "wip")), None);
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(1));
        assert_eq!(add(&r, &store, &day(6, 10, "wip")), None);
        assert_eq!(add(&r, &store, &day(7, 10, "wip")), Some(json!({ "progress": 3, "at": 3 })));
        // Local days: at -02:00, 2026-03-08T01:00Z is still the 7th.
        let minus2 = FixedOffset::west_opt(2 * 3600).unwrap();
        let late = day(8, 1, "wip");
        store.insert_event(&late).unwrap();
        assert_eq!(progress(&r, &store, minus2).unwrap(), Some(3));
        assert_eq!(progress(&r, &store, tz()).unwrap(), Some(4));
    }

    #[test]
    fn month_every_day_fires_once() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "counter = { type = \"month_every_day\" }");
        let c = |d: &str| mk(EventKind::Commit, d, "r", json!({}));
        // January 2027 misses the 15th, so Jan 31 doesn't fire.
        for d in (1..=31).filter(|d| *d != 15) {
            assert_eq!(add(&r, &store, &c(&format!("2027-01-{d:02}T12:00:00Z"))), None, "jan {d}");
        }
        let mut fired = vec![];
        for d in 1..=28 {
            for h in [9, 18] {
                let e = c(&format!("2027-02-{d:02}T{h:02}:00:00Z"));
                if let Some(f) = add(&r, &store, &e) {
                    fired.push((d, h, f));
                }
            }
        }
        assert_eq!(fired, vec![(28, 9, json!({ "month": "2027-02", "days": 28 }))]);
        assert_eq!(add(&r, &store, &c("2027-03-01T09:00:00Z")), None);
        assert_eq!(progress(&r, &store, tz()).unwrap(), None, "a moment counter");
    }

    #[test]
    fn first_repo_scope() {
        let store = Store::open_in_memory().unwrap();
        let repo = rule("\"commit\"", "counter = { type = \"first\", scope = \"repo\" }");
        let ever = rule("\"commit\"", "counter = { type = \"first\", scope = \"ever\" }");
        let a1 = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "A", json!({}));
        assert_eq!(add(&repo, &store, &a1), Some(json!({ "repo": "A" })));
        let a2 = mk(EventKind::Commit, "2026-03-01T02:00:00Z", "A", json!({}));
        assert_eq!(add(&repo, &store, &a2), None);
        let b1 = mk(EventKind::Commit, "2026-03-01T03:00:00Z", "B", json!({}));
        assert!(add(&repo, &store, &b1).is_some(), "new repo is first in repo scope");
        assert_eq!(eval(&ever, &store, &b1), None, "not first ever");
    }

    #[test]
    fn per_day_counts_only_matching() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "filter = { message = 'wip' }\ncounter = { type = \"per_day\", at = 3 }");
        let wip = [1, 4, 8];
        let mut fired = vec![];
        for i in 0..10 {
            let msg = if wip.contains(&i) { "wip stuff" } else { "fix bug" };
            let e = mk(EventKind::Commit, &format!("2026-03-01T{:02}:00:00Z", i + 1), "r", json!({ "message": msg }));
            if add(&r, &store, &e).is_some() {
                fired.push(i);
            }
        }
        assert_eq!(fired, vec![8], "third wip commit only");
    }

    #[test]
    fn run_requires_consecutive() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "filter = { message = 'wip' }\ncounter = { type = \"run\", at = 3 }");
        let msgs = ["wip", "wip", "fix", "wip", "wip", "wip"];
        let mut fired = vec![];
        for (i, m) in msgs.iter().enumerate() {
            let e = mk(EventKind::Commit, &format!("2026-03-01T{:02}:00:00Z", i + 1), "r", json!({ "message": m }));
            if let Some(f) = add(&r, &store, &e) {
                fired.push((i, f));
            }
        }
        assert_eq!(fired, vec![(5, json!({ "run": 3 }))]);
        // A commit in another repo doesn't break (or extend) this repo's run.
        let other = mk(EventKind::Commit, "2026-03-01T07:00:00Z", "other", json!({ "message": "wip" }));
        assert_eq!(add(&r, &store, &other), None);
        let next = mk(EventKind::Commit, "2026-03-01T08:00:00Z", "r", json!({ "message": "wip" }));
        assert_eq!(add(&r, &store, &next), None, "run of 4 already fired at 3");
    }

    #[test]
    fn run_fires_once_per_run() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "filter = { message = 'wip' }
counter = { type = \"run\", at = 3 }");
        let msgs = ["wip", "wip", "wip", "wip", "wip", "fix", "wip", "wip", "wip"];
        let mut fired = vec![];
        for (i, m) in msgs.iter().enumerate() {
            let e = mk(EventKind::Commit, &format!("2026-03-01T{:02}:00:00Z", i + 1), "r", json!({ "message": m }));
            if add(&r, &store, &e).is_some() {
                fired.push(i);
            }
        }
        assert_eq!(fired, vec![2, 8]);
    }

    fn gh(kind: EventKind, ts: &str, repo: &str) -> Event {
        let mut e = Event::new(kind, Source::Github, Some(repo.into()), json!({}));
        e.ts = at(ts);
        e
    }

    #[test]
    fn combo_same_repo_in_order() {
        let r = rule(
            "\"pr_opened\"",
            "counter = { type = \"combo\", kinds = [\"commit\", \"push\", \"pr_opened\"], within_minutes = 15 }",
        );
        let setup = |commit_ts: &str| {
            let store = Store::open_in_memory().unwrap();
            for (kind, ts) in [(EventKind::Commit, commit_ts), (EventKind::Push, "2026-03-01T10:05:00Z")] {
                let e = mk(kind, ts, "C:/work/b", json!({ "github_repo": "a/b" }));
                store.insert_event(&e).unwrap();
            }
            store
        };
        let store = setup("2026-03-01T10:00:00Z");
        let pr = gh(EventKind::PrOpened, "2026-03-01T10:12:00Z", "a/b");
        assert!(add(&r, &store, &pr).is_some());

        let store = setup("2026-03-01T10:00:00Z");
        let other = gh(EventKind::PrOpened, "2026-03-01T10:12:00Z", "c/d");
        assert_eq!(add(&r, &store, &other), None, "different repo");

        let store = setup("2026-03-01T09:50:00Z");
        let pr = gh(EventKind::PrOpened, "2026-03-01T10:12:00Z", "a/b");
        assert_eq!(add(&r, &store, &pr), None, "commit outside the window");

        // Out of order: the push before the commit.
        let store = Store::open_in_memory().unwrap();
        for (kind, ts) in [(EventKind::Push, "2026-03-01T10:00:00Z"), (EventKind::Commit, "2026-03-01T10:05:00Z")] {
            store.insert_event(&mk(kind, ts, "C:/work/b", json!({ "github_repo": "a/b" }))).unwrap();
        }
        let pr = gh(EventKind::PrOpened, "2026-03-01T10:12:00Z", "a/b");
        assert_eq!(add(&r, &store, &pr), None, "out of order");

        // Git events with no github_repo never match.
        let store = Store::open_in_memory().unwrap();
        for (kind, ts) in [(EventKind::Commit, "2026-03-01T10:00:00Z"), (EventKind::Push, "2026-03-01T10:05:00Z")] {
            store.insert_event(&mk(kind, ts, "C:/work/b", json!({}))).unwrap();
        }
        let pr = gh(EventKind::PrOpened, "2026-03-01T10:12:00Z", "a/b");
        assert_eq!(add(&r, &store, &pr), None, "no github key");
    }

    #[test]
    fn gap_days_scope_repo() {
        let store = Store::open_in_memory().unwrap();
        let repo = rule("\"commit\"", "counter = { type = \"gap_days\", min = 30, scope = \"repo\" }");
        let ever = rule("\"commit\"", "counter = { type = \"gap_days\", min = 30, scope = \"ever\" }");
        let a1 = mk(EventKind::Commit, "2026-01-01T10:00:00Z", "A", json!({}));
        assert_eq!(add(&repo, &store, &a1), None, "no previous event");
        let b1 = mk(EventKind::Commit, "2026-01-20T10:00:00Z", "B", json!({}));
        assert_eq!(add(&repo, &store, &b1), None, "first in B: no previous in scope");
        let a2 = mk(EventKind::Commit, "2026-02-05T10:00:00Z", "A", json!({}));
        assert_eq!(add(&repo, &store, &a2), Some(json!({ "gap_days": 35, "repo": "A" })));
        assert_eq!(eval(&ever, &store, &a2), None, "B was 16 days before");
        let a3 = mk(EventKind::Commit, "2026-03-07T09:00:00Z", "A", json!({}));
        assert_eq!(add(&repo, &store, &a3), None, "29.96 days is under 30");
    }

    #[test]
    fn first_year() {
        let store = Store::open_in_memory().unwrap();
        let r = rule("\"commit\"", "counter = { type = \"first\", scope = \"year\" }");
        let a = mk(EventKind::Commit, "2025-12-31T23:30:00Z", "r", json!({}));
        assert_eq!(add(&r, &store, &a), Some(json!({ "year": 2025 })));
        let b = mk(EventKind::Commit, "2026-01-01T00:30:00Z", "r", json!({}));
        assert_eq!(add(&r, &store, &b), Some(json!({ "year": 2026 })));
        let c = mk(EventKind::Commit, "2026-06-01T00:00:00Z", "r", json!({}));
        assert_eq!(add(&r, &store, &c), None);
        // A push doesn't count against commits.
        let p = mk(EventKind::Push, "2027-01-01T00:00:00Z", "r", json!({}));
        store.insert_event(&p).unwrap();
        let d = mk(EventKind::Commit, "2027-02-01T00:00:00Z", "r", json!({}));
        assert!(add(&r, &store, &d).is_some());
        // The local year: at +02:00, 2025-12-31T23:30Z is already 2026.
        let plus2 = FixedOffset::east_opt(2 * 3600).unwrap();
        let ctx = EvalCtx { ev: &b, store: &store, broken: &[], file_ages: &[], tz: plus2, skill_level_ups: &[] };
        assert!(evaluate(&r, &ctx).unwrap().is_empty());
    }
}
