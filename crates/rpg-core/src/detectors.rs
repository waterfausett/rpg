//! Detectors decide whether a rule fires for an event. Interim adapter over `Counter`:
//! evaluates the counters v0 had; the rest never fire yet (Tasks 6-7 replace this file).
use crate::enrich::FileAge;
use crate::event::Event;
use crate::records::{count_today, RecordBroken};
use crate::rules::{Counter, FirstScope};
use crate::store::Store;
use anyhow::Result;
use chrono::FixedOffset;
use serde_json::{json, Value};

pub struct EvalCtx<'a> {
    pub ev: &'a Event,
    pub store: &'a Store,
    pub broken: &'a [RecordBroken],
    pub file_ages: &'a [FileAge],
    pub tz: FixedOffset,
}

/// `Some(findings)` means the rule fired; findings become the LLM context. The caller has
/// already checked the rule's filter.
pub fn evaluate(counter: &Counter, ctx: &EvalCtx) -> Result<Option<Value>> {
    let ev = ctx.ev;
    Ok(match counter {
        Counter::Each => Some(json!({})),
        Counter::PerDay { at } => {
            let n = count_today(ctx.store, ev, ctx.tz)?;
            (n == *at).then(|| json!({ "count": n }))
        }
        Counter::PersonalRecord { metric } => ctx
            .broken
            .iter()
            .find(|b| b.metric == *metric)
            .map(|b| json!({ "metric": crate::records::metric_key(b.metric), "old": b.old, "new": b.new })),
        Counter::FileAge { min_years } => ctx
            .file_ages
            .iter()
            .filter(|f| f.years >= *min_years)
            .max_by(|a, b| a.years.total_cmp(&b.years))
            .map(|f| json!({ "path": f.path, "years": f.years, "last_author": f.last_author })),
        Counter::First { scope: scope @ (FirstScope::Ever | FirstScope::Repo) } => {
            let repo = if *scope == FirstScope::Repo { ev.repo.as_deref() } else { None };
            (!ctx.store.has_prior_event(ev.kind, repo, &ev.id)?).then(|| json!({ "repo": ev.repo }))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, Source};
    use crate::records;
    use crate::rules::Metric;
    use chrono::{DateTime, Utc};

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

    fn eval(det: &Counter, store: &Store, ev: &Event, broken: &[RecordBroken], ages: &[FileAge]) -> Option<Value> {
        let ctx = EvalCtx { ev, store, broken, file_ages: ages, tz: tz() };
        evaluate(det, &ctx).unwrap()
    }

    #[test]
    fn count_fires_exactly_once_per_day() {
        let store = Store::open_in_memory().unwrap();
        let det = Counter::PerDay { at: 3 };
        let mut results = vec![];
        for h in 1..=4 {
            let e = mk(EventKind::PrMerged, &format!("2026-03-01T0{h}:00:00Z"), "r", json!({}));
            store.insert_event(&e).unwrap();
            results.push(eval(&det, &store, &e, &[], &[]));
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
    }

    #[test]
    fn record_seed_does_not_break_then_beat_does() {
        let store = Store::open_in_memory().unwrap();
        let det = Counter::PersonalRecord { metric: Metric::PrDeletions };
        let e1 = mk(EventKind::PrMerged, "2026-03-01T01:00:00Z", "r", json!({ "deletions": 100 }));
        store.insert_event(&e1).unwrap();
        let b1 = records::update(&store, &e1, tz()).unwrap();
        assert!(b1.is_empty());
        assert_eq!(eval(&det, &store, &e1, &b1, &[]), None);

        let e2 = mk(EventKind::PrMerged, "2026-03-02T01:00:00Z", "r", json!({ "deletions": 100 }));
        store.insert_event(&e2).unwrap();
        assert!(records::update(&store, &e2, tz()).unwrap().is_empty(), "equal is not a break");

        let e3 = mk(EventKind::PrMerged, "2026-03-03T01:00:00Z", "r", json!({ "deletions": 250 }));
        store.insert_event(&e3).unwrap();
        let b3 = records::update(&store, &e3, tz()).unwrap();
        assert_eq!(b3.iter().filter(|b| b.metric == Metric::PrDeletions).count(), 1);
        let f = eval(&det, &store, &e3, &b3, &[]).unwrap();
        assert_eq!(f, json!({ "metric": "pr_deletions", "old": 100.0, "new": 250.0 }));
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
        let det = Counter::FileAge { min_years: 5.0 };
        let f = eval(&det, &store, &e, &[], &ages).unwrap();
        assert_eq!(f["path"], "old.rs");
        assert_eq!(f["last_author"], "c");
        assert_eq!(eval(&Counter::FileAge { min_years: 10.0 }, &store, &e, &[], &ages), None);
    }

    #[test]
    fn surgical_strike_needs_at_least_one_line() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rules");
        let (rules, _) = crate::rules::load_dir(&dir);
        let rule = rules.list.iter().find(|r| r.id == "surgical_strike").unwrap();
        let store = Store::open_in_memory().unwrap();
        let fires = |a: u64, r: u64| {
            let e = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({ "lines_added": a, "lines_removed": r }));
            rule.filter.matches(&e, tz()) && eval(&rule.counter, &store, &e, &[], &[]).is_some()
        };
        assert!(!fires(0, 0), "binary-only/rename commit");
        assert!(fires(1, 0));
        assert!(fires(1, 1));
        assert!(!fires(2, 1));
    }

    #[test]
    fn unimplemented_counters_never_fire() {
        let store = Store::open_in_memory().unwrap();
        let e = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({}));
        store.insert_event(&e).unwrap();
        for c in [Counter::First { scope: FirstScope::Year }, Counter::StreakDays, Counter::Run { at: 1 }] {
            assert_eq!(eval(&c, &store, &e, &[], &[]), None, "{c:?}");
        }
    }

    #[test]
    fn first_repo_scope() {
        let store = Store::open_in_memory().unwrap();
        let a1 = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "A", json!({}));
        store.insert_event(&a1).unwrap();
        let repo = Counter::First { scope: FirstScope::Repo };
        let ever = Counter::First { scope: FirstScope::Ever };
        assert_eq!(eval(&repo, &store, &a1, &[], &[]), Some(json!({ "repo": "A" })));
        let a2 = mk(EventKind::Commit, "2026-03-01T02:00:00Z", "A", json!({}));
        store.insert_event(&a2).unwrap();
        assert_eq!(eval(&repo, &store, &a2, &[], &[]), None);
        let b1 = mk(EventKind::Commit, "2026-03-01T03:00:00Z", "B", json!({}));
        store.insert_event(&b1).unwrap();
        assert!(eval(&repo, &store, &b1, &[], &[]).is_some(), "new repo is first in repo scope");
        assert_eq!(eval(&ever, &store, &b1, &[], &[]), None, "not first ever");
    }
}
