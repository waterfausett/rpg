//! The engine: one event in, XP gains / unlocks / level-ups out.
use crate::config::Config;
use crate::detectors::{evaluate, EvalCtx};
use crate::enrich::FileAge;
use crate::event::{Event, EventKind};
use crate::records;
use crate::rules::{Rules, Tier};
use crate::store::{NewUnlock, Store};
use crate::xp::level_for;
use anyhow::Result;
use chrono::{DateTime, Duration, FixedOffset, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct XpGain {
    pub amount: u64,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Unlock {
    pub unlock_id: i64,
    pub rule_id: String,
    pub name: String,
    pub tier: Tier,
    pub xp: u64,
    pub findings: serde_json::Value,
    /// Includes this unlock.
    pub unlock_count: u32,
    pub last_unlocked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub xp_gains: Vec<XpGain>,
    pub unlocks: Vec<Unlock>,
    pub level_ups: Vec<u32>,
    pub total_xp: u64,
    pub level: u32,
}

/// Process one event: record it, update records, award XP, evaluate rules.
///
/// Runs in a single SQLite transaction (rolled back on any error). Assumes
/// single-threaded processing; see `Store::begin_tx`.
pub fn process(
    store: &Store,
    rules: &Rules,
    cfg: &Config,
    ev: &Event,
    file_ages: &[FileAge],
    tz: FixedOffset,
) -> Result<Outcome> {
    store.begin_tx()?;
    match process_inner(store, rules, cfg, ev, file_ages, tz) {
        Ok(out) => {
            if let Err(e) = store.commit_tx() {
                let _ = store.rollback_tx();
                return Err(e);
            }
            Ok(out)
        }
        Err(e) => {
            let _ = store.rollback_tx();
            Err(e)
        }
    }
}

fn base_xp(store: &Store, cfg: &Config, ev: &Event) -> Result<Option<(u64, &'static str)>> {
    Ok(match ev.kind {
        EventKind::Commit => Some((cfg.xp.commit, "commit")),
        EventKind::Push => Some((cfg.xp.push, "push")),
        EventKind::PrOpened => Some((cfg.xp.pr_opened, "PR opened")),
        EventKind::PrMerged => Some((cfg.xp.pr_merged, "PR merged")),
        EventKind::ReviewSubmitted => Some((cfg.xp.review, "review")),
        EventKind::ClaudeSessionStart => None,
        EventKind::ClaudeSessionEnd => {
            let has_start = match ev.data["session_id"].as_str() {
                Some(id) => store.find_session_start(id)?.is_some(),
                None => false,
            };
            has_start.then_some((cfg.xp.claude_session, "Claude session"))
        }
    })
}

fn process_inner(
    store: &Store,
    rules: &Rules,
    cfg: &Config,
    ev: &Event,
    file_ages: &[FileAge],
    tz: FixedOffset,
) -> Result<Outcome> {
    let mut out = Outcome::default();
    let before = store.total_xp()?;
    out.total_xp = before;
    out.level = level_for(before, &cfg.levels);

    if !store.insert_event(ev)? {
        return Ok(out);
    }
    let broken = records::update(store, ev, tz)?;
    if ev.backfill {
        return Ok(out);
    }

    if let Some((amount, label)) = base_xp(store, cfg, ev)? {
        if amount > 0 {
            store.add_xp(&ev.id, amount, label, ev.ts)?;
            out.xp_gains.push(XpGain { amount, label: label.to_string() });
        }
    }

    for rule in rules.for_kind(ev.kind) {
        let count = store.unlock_count(&rule.id)?;
        if !rule.repeatable && count > 0 {
            continue;
        }
        let last = store.last_unlock_at(&rule.id)?;
        if let (Some(hours), Some(last)) = (rule.cooldown_hours, last) {
            if ev.ts < last + Duration::hours(hours as i64) {
                continue;
            }
        }
        let ctx = EvalCtx { ev, store, broken: &broken, file_ages, tz };
        let Some(findings) = evaluate(&rule.detector, &ctx)? else { continue };
        let unlock_id = store.insert_unlock(&NewUnlock {
            rule_id: rule.id.clone(),
            name: rule.name.clone(),
            tier: tier_str(rule.tier).to_string(),
            xp: rule.xp,
            context: findings.clone(),
            ts: ev.ts,
        })?;
        if rule.xp > 0 {
            store.add_xp(&ev.id, rule.xp, &rule.name, ev.ts)?;
            out.xp_gains.push(XpGain { amount: rule.xp, label: rule.name.clone() });
        }
        out.unlocks.push(Unlock {
            unlock_id,
            rule_id: rule.id.clone(),
            name: rule.name.clone(),
            tier: rule.tier,
            xp: rule.xp,
            findings,
            unlock_count: count + 1,
            last_unlocked_at: last,
        });
    }

    out.total_xp = store.total_xp()?;
    out.level = level_for(out.total_xp, &cfg.levels);
    out.level_ups = (level_for(before, &cfg.levels) + 1..=out.level).collect();
    Ok(out)
}

fn tier_str(t: Tier) -> &'static str {
    match t {
        Tier::Bronze => "bronze",
        Tier::Silver => "silver",
        Tier::Gold => "gold",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use crate::rules::{Detector, Flavor, Metric, Window};

    fn tz() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn mk(kind: EventKind, ts: &str, data: serde_json::Value) -> Event {
        let mut e = Event::new(kind, Source::Git, Some("r".into()), data);
        e.ts = at(ts);
        e
    }

    fn rule(id: &str, kind: EventKind, det: Detector, xp: u64) -> crate::rules::Rule {
        crate::rules::Rule {
            id: id.into(),
            name: format!("Name {id}"),
            tier: Tier::Bronze,
            xp,
            fallback: "f".into(),
            hint: None,
            trigger: vec![kind],
            detector: det,
            repeatable: false,
            cooldown_hours: None,
            flavor: Flavor::Static,
        }
    }

    fn run(store: &Store, rules: &Rules, ev: &Event) -> Outcome {
        process(store, rules, &Config::default(), ev, &[], tz()).unwrap()
    }

    #[test]
    fn duplicate_event_yields_nothing() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        let e = mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({}));
        let first = run(&store, &rules, &e);
        assert_eq!(first.total_xp, 10);
        let again = run(&store, &rules, &e);
        assert!(again.xp_gains.is_empty() && again.unlocks.is_empty() && again.level_ups.is_empty());
        assert_eq!(store.total_xp().unwrap(), 10);
    }

    #[test]
    fn backfill_seeds_records_without_xp_or_unlocks() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules {
            list: vec![rule("pb", EventKind::PrMerged, Detector::PersonalRecord { metric: Metric::PrsMergedPerDay }, 7)],
        };
        for i in 0..2 {
            let mut e = mk(EventKind::PrMerged, &format!("2026-01-01T10:0{i}:00Z"), serde_json::json!({}));
            e.backfill = true;
            let o = run(&store, &rules, &e);
            assert!(o.xp_gains.is_empty() && o.unlocks.is_empty());
        }
        assert_eq!(store.total_xp().unwrap(), 0);
        let mut last = Outcome::default();
        for i in 0..3 {
            let e = mk(EventKind::PrMerged, &format!("2026-01-02T10:0{i}:00Z"), serde_json::json!({}));
            last = run(&store, &rules, &e);
        }
        assert_eq!(last.unlocks.len(), 1);
        let u = &last.unlocks[0];
        assert_eq!(u.rule_id, "pb");
        assert_eq!(u.findings["old"], 2.0);
        assert_eq!(u.unlock_count, 1);
        assert_eq!(u.tier, Tier::Bronze);
        // 3 live merges at 100 plus the 7 XP unlock; backfill gave nothing.
        assert_eq!(store.total_xp().unwrap(), 307);
    }

    #[test]
    fn non_repeatable_unlocks_once() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules { list: vec![rule("any", EventKind::Commit, Detector::TimeOfDay { start_hour: 0, end_hour: 24 }, 5)] };
        let a = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({})));
        let b = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T11:00:00Z", serde_json::json!({})));
        assert_eq!(a.unlocks.len(), 1);
        assert!(b.unlocks.is_empty());
        assert_eq!(store.unlock_count("any").unwrap(), 1);
    }

    #[test]
    fn cooldown_respected() {
        let store = Store::open_in_memory().unwrap();
        let mut r = rule("rep", EventKind::Commit, Detector::TimeOfDay { start_hour: 0, end_hour: 24 }, 5);
        r.repeatable = true;
        r.cooldown_hours = Some(2);
        let rules = Rules { list: vec![r] };
        let t0 = at("2026-01-01T10:00:00Z");
        let at_off = |m: i64| {
            let mut e = Event::new(EventKind::Commit, Source::Git, Some("r".into()), serde_json::json!({}));
            e.ts = t0 + Duration::minutes(m);
            e
        };
        assert_eq!(run(&store, &rules, &at_off(0)).unlocks.len(), 1);
        assert_eq!(run(&store, &rules, &at_off(60)).unlocks.len(), 0);
        let third = run(&store, &rules, &at_off(121));
        assert_eq!(third.unlocks.len(), 1);
        assert_eq!(third.unlocks[0].unlock_count, 2);
        assert!(third.unlocks[0].last_unlocked_at.is_some());
    }

    #[test]
    fn level_up_reported() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        let mut ups = vec![];
        let mut last = Outcome::default();
        for i in 0..10 {
            let e = mk(EventKind::PrMerged, &format!("2026-01-01T10:{i:02}:00Z"), serde_json::json!({}));
            last = run(&store, &rules, &e);
            ups.extend(last.level_ups.clone());
        }
        assert_eq!(ups, vec![2, 3, 4, 5]);
        assert_eq!((last.total_xp, last.level), (1000, 5));
        assert_eq!(last.xp_gains[0], XpGain { amount: 100, label: "PR merged".into() });
    }

    #[test]
    fn claude_session_end_without_start_gives_no_xp() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        let end = mk(EventKind::ClaudeSessionEnd, "2026-01-01T10:00:00Z", serde_json::json!({"session_id": "s1"}));
        assert!(run(&store, &rules, &end).xp_gains.is_empty());

        let start = mk(EventKind::ClaudeSessionStart, "2026-01-01T11:00:00Z", serde_json::json!({"session_id": "s2"}));
        assert!(run(&store, &rules, &start).xp_gains.is_empty());
        let end2 = mk(EventKind::ClaudeSessionEnd, "2026-01-01T12:00:00Z", serde_json::json!({"session_id": "s2"}));
        let o = run(&store, &rules, &end2);
        assert_eq!(o.xp_gains, vec![XpGain { amount: 15, label: "Claude session".into() }]);
    }

    #[test]
    fn failing_step_leaves_no_event_row() {
        let store = Store::open_in_memory().unwrap();
        store.exec_raw("DROP TABLE xp_ledger").unwrap();
        let e = mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({}));
        assert!(process(&store, &Rules::default(), &Config::default(), &e, &[], tz()).is_err());
        assert!(!store.has_prior_event(EventKind::Commit, None, "none").unwrap());
        // The connection is usable again (transaction was closed).
        store.begin_tx().unwrap();
        store.rollback_tx().unwrap();
    }

    #[test]
    fn count_rule_uses_window_detector() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules { list: vec![rule("c2", EventKind::Commit, Detector::CountInWindow { count: 2, window: Window::Day }, 0)] };
        let a = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({})));
        let b = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T11:00:00Z", serde_json::json!({})));
        assert!(a.unlocks.is_empty());
        assert_eq!(b.unlocks.len(), 1);
    }
}
