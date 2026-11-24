//! Detectors decide whether a rule fires for an event.
use crate::enrich::FileAge;
use crate::event::{Event, EventKind};
use crate::records::{count_today, RecordBroken};
use crate::rules::{Detector, FirstScope, Window};
use crate::store::Store;
use anyhow::Result;
use chrono::{FixedOffset, Timelike};
use serde_json::{json, Value};

pub struct EvalCtx<'a> {
    pub ev: &'a Event,
    pub store: &'a Store,
    pub broken: &'a [RecordBroken],
    pub file_ages: &'a [FileAge],
    pub tz: FixedOffset,
}

/// `Some(findings)` means the rule fired; findings become the LLM context.
pub fn evaluate(det: &Detector, ctx: &EvalCtx) -> Result<Option<Value>> {
    let ev = ctx.ev;
    Ok(match det {
        Detector::CountInWindow { count, window } => {
            let Window::Day = window;
            let n = count_today(ctx.store, ev, ctx.tz)?;
            (n == *count).then(|| json!({ "count": n }))
        }
        Detector::PersonalRecord { metric } => ctx
            .broken
            .iter()
            .find(|b| b.metric == *metric)
            .map(|b| json!({ "metric": crate::records::metric_key(b.metric), "old": b.old, "new": b.new })),
        Detector::FileAge { min_years } => ctx
            .file_ages
            .iter()
            .filter(|f| f.years >= *min_years)
            .max_by(|a, b| a.years.total_cmp(&b.years))
            .map(|f| json!({ "path": f.path, "years": f.years, "last_author": f.last_author })),
        Detector::TimeOfDay { start_hour, end_hour } => {
            let hour = ev.ts.with_timezone(&ctx.tz).hour();
            let inside = if start_hour <= end_hour {
                hour >= *start_hour && hour < *end_hour
            } else {
                hour >= *start_hour || hour < *end_hour // window wraps midnight
            };
            inside.then(|| json!({ "hour": hour }))
        }
        Detector::DiffSize { min_lines, max_lines, min_delete_ratio } => {
            let (a_key, r_key) = match ev.kind {
                EventKind::Commit => ("lines_added", "lines_removed"),
                EventKind::PrOpened | EventKind::PrMerged => ("additions", "deletions"),
                _ => return Ok(None),
            };
            let added = ev.data[a_key].as_u64().unwrap_or(0);
            let removed = ev.data[r_key].as_u64().unwrap_or(0);
            let total = added + removed;
            let ok = min_lines.is_none_or(|m| total >= m)
                && max_lines.is_none_or(|m| total <= m)
                && min_delete_ratio.is_none_or(|m| total > 0 && removed as f64 / total as f64 >= m);
            ok.then(|| json!({ "added": added, "removed": removed }))
        }
        Detector::First { scope } => {
            let repo = match scope {
                FirstScope::Repo => ev.repo.as_deref(),
                FirstScope::Ever => None,
            };
            (!ctx.store.has_prior_event(ev.kind, repo, &ev.id)?).then(|| json!({ "repo": ev.repo }))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
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

    fn eval(det: &Detector, store: &Store, ev: &Event, broken: &[RecordBroken], ages: &[FileAge]) -> Option<Value> {
        let ctx = EvalCtx { ev, store, broken, file_ages: ages, tz: tz() };
        evaluate(det, &ctx).unwrap()
    }

    #[test]
    fn count_fires_exactly_once_per_day() {
        let store = Store::open_in_memory().unwrap();
        let det = Detector::CountInWindow { count: 3, window: Window::Day };
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
        let det = Detector::PersonalRecord { metric: Metric::PrDeletions };
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
        let det = Detector::FileAge { min_years: 5.0 };
        let f = eval(&det, &store, &e, &[], &ages).unwrap();
        assert_eq!(f["path"], "old.rs");
        assert_eq!(f["last_author"], "c");
        assert_eq!(eval(&Detector::FileAge { min_years: 10.0 }, &store, &e, &[], &ages), None);
    }

    #[test]
    fn time_of_day_end_exclusive() {
        let store = Store::open_in_memory().unwrap();
        let det = Detector::TimeOfDay { start_hour: 0, end_hour: 4 };
        let a = mk(EventKind::Commit, "2026-03-01T03:59:00Z", "r", json!({}));
        let b = mk(EventKind::Commit, "2026-03-01T04:00:00Z", "r", json!({}));
        assert_eq!(eval(&det, &store, &a, &[], &[]), Some(json!({ "hour": 3 })));
        assert_eq!(eval(&det, &store, &b, &[], &[]), None);
    }

    #[test]
    fn diff_size_delete_ratio() {
        let store = Store::open_in_memory().unwrap();
        let det = Detector::DiffSize { min_lines: Some(100), max_lines: None, min_delete_ratio: Some(0.8) };
        let pr = |a: u64, d: u64| mk(EventKind::PrMerged, "2026-03-01T01:00:00Z", "r", json!({ "additions": a, "deletions": d }));
        assert_eq!(eval(&det, &store, &pr(20, 400), &[], &[]), Some(json!({ "added": 20, "removed": 400 })));
        assert_eq!(eval(&det, &store, &pr(300, 300), &[], &[]), None);
        assert_eq!(eval(&det, &store, &pr(1, 20), &[], &[]), None);
        let commit = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({ "lines_added": 0, "lines_removed": 500 }));
        assert!(eval(&det, &store, &commit, &[], &[]).is_some());
        let max = Detector::DiffSize { min_lines: None, max_lines: Some(5), min_delete_ratio: None };
        let tiny = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({ "lines_added": 2, "lines_removed": 1 }));
        assert!(eval(&max, &store, &tiny, &[], &[]).is_some());
        assert_eq!(eval(&max, &store, &commit, &[], &[]), None);
    }

    #[test]
    fn surgical_strike_needs_at_least_one_line() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rules");
        let (rules, _) = crate::rules::load_dir(&dir);
        let rule = rules.list.iter().find(|r| r.id == "surgical_strike").unwrap();
        let store = Store::open_in_memory().unwrap();
        let commit = |a: u64, r: u64| mk(EventKind::Commit, "2026-03-01T01:00:00Z", "r", json!({ "lines_added": a, "lines_removed": r }));
        assert_eq!(eval(&rule.detector, &store, &commit(0, 0), &[], &[]), None, "binary-only/rename commit");
        assert!(eval(&rule.detector, &store, &commit(1, 0), &[], &[]).is_some());
        assert!(eval(&rule.detector, &store, &commit(1, 1), &[], &[]).is_some());
        assert_eq!(eval(&rule.detector, &store, &commit(2, 1), &[], &[]), None);
    }

    #[test]
    fn first_repo_scope() {
        let store = Store::open_in_memory().unwrap();
        let a1 = mk(EventKind::Commit, "2026-03-01T01:00:00Z", "A", json!({}));
        store.insert_event(&a1).unwrap();
        let repo = Detector::First { scope: FirstScope::Repo };
        let ever = Detector::First { scope: FirstScope::Ever };
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
