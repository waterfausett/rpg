//! Personal-record tracking.
use crate::event::{Event, EventKind};
use crate::rules::Metric;
use crate::store::Store;
use anyhow::Result;
use chrono::{DateTime, Duration, FixedOffset, TimeZone, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct RecordBroken {
    pub metric: Metric,
    pub old: f64,
    pub new: f64,
}

/// Stable storage key for a metric.
pub fn metric_key(m: Metric) -> &'static str {
    match m {
        Metric::PrsMergedPerDay => "prs_merged_per_day",
        Metric::CommitsPerDay => "commits_per_day",
        Metric::PrDeletions => "pr_deletions",
    }
}

/// UTC bounds `[start, end)` of the local calendar day containing `ts`.
pub fn local_day_bounds(ts: DateTime<Utc>, tz: FixedOffset) -> (DateTime<Utc>, DateTime<Utc>) {
    let midnight = ts.with_timezone(&tz).date_naive().and_hms_opt(0, 0, 0).expect("valid midnight");
    let start = tz.from_local_datetime(&midnight).single().expect("fixed offset is unambiguous").with_timezone(&Utc);
    (start, start + Duration::days(1))
}

/// Number of events of `ev.kind` on `ev`'s local day, including `ev` itself.
pub fn count_today(store: &Store, ev: &Event, tz: FixedOffset) -> Result<u32> {
    let (from, to) = local_day_bounds(ev.ts, tz);
    Ok(store.count_events(ev.kind, from, to, &ev.id)? + 1)
}

fn metrics_for(store: &Store, ev: &Event, tz: FixedOffset) -> Result<Vec<(Metric, f64)>> {
    Ok(match ev.kind {
        EventKind::PrMerged => vec![
            (Metric::PrsMergedPerDay, count_today(store, ev, tz)? as f64),
            (Metric::PrDeletions, ev.data["deletions"].as_f64().unwrap_or(0.0)),
        ],
        EventKind::Commit => vec![(Metric::CommitsPerDay, count_today(store, ev, tz)? as f64)],
        _ => vec![],
    })
}

/// Update stored records for `ev`. A first-ever value seeds without counting as a break.
pub fn update(store: &Store, ev: &Event, tz: FixedOffset) -> Result<Vec<RecordBroken>> {
    let mut broken = Vec::new();
    for (metric, new) in metrics_for(store, ev, tz)? {
        let key = metric_key(metric);
        match store.record(key)? {
            None => store.set_record(key, new, ev.ts)?,
            Some(old) if new > old => {
                store.set_record(key, new, ev.ts)?;
                broken.push(RecordBroken { metric, old, new });
            }
            Some(_) => {}
        }
    }
    Ok(broken)
}
