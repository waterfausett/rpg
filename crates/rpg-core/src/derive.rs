//! Fields derived from stored history, attached to an event before it is stored.

use crate::event::{Event, EventKind};
use crate::store::Store;
use anyhow::Result;
use chrono::Duration;

/// A session start older than this with no end is considered abandoned, not concurrent.
pub const STALE_SESSION_HOURS: i64 = 12;

fn session_id(ev: &Event) -> Option<&str> {
    ev.data.get("session_id").and_then(|v| v.as_str())
}

pub fn derive(store: &Store, ev: &mut Event) -> Result<()> {
    match ev.kind {
        EventKind::ClaudeSessionEnd => {
            let Some(sid) = session_id(ev).map(str::to_string) else { return Ok(()) };
            if let Some(start) = store.find_session_start(&sid)? {
                let minutes = (ev.ts - start.ts).num_milliseconds() as f64 / 60_000.0;
                if let Some(obj) = ev.data.as_object_mut() {
                    obj.insert("session_minutes".into(), serde_json::json!(minutes));
                }
            }
        }
        EventKind::ClaudeSessionStart => {
            let since = ev.ts - Duration::hours(STALE_SESSION_HOURS);
            let events = store.events_of(
                &[EventKind::ClaudeSessionStart, EventKind::ClaudeSessionEnd],
                Some(since),
            )?;
            let ended: std::collections::HashSet<&str> = events
                .iter()
                .filter(|e| e.kind == EventKind::ClaudeSessionEnd && e.id != ev.id)
                .filter_map(session_id)
                .collect();
            let open = events
                .iter()
                .filter(|e| e.kind == EventKind::ClaudeSessionStart && e.id != ev.id && e.ts <= ev.ts)
                .filter(|e| session_id(e).map_or(true, |s| !ended.contains(s)))
                .count() as u64
                + 1;
            if let Some(obj) = ev.data.as_object_mut() {
                obj.insert("open_sessions".into(), serde_json::json!(open));
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use chrono::{TimeZone, Utc};

    fn mk(id: &str, kind: EventKind, h: u32, m: u32, sid: &str) -> Event {
        Event {
            id: id.into(),
            kind,
            ts: Utc.with_ymd_and_hms(2026, 3, 2, h, m, 0).unwrap(),
            source: Source::Claude,
            repo: None,
            backfill: false,
            data: serde_json::json!({"session_id": sid}),
        }
    }
    use EventKind::{ClaudeSessionEnd as End, ClaudeSessionStart as Start};

    #[test]
    fn end_gets_minutes() {
        let s = Store::open_in_memory().unwrap();
        s.insert_event(&mk("a", Start, 10, 0, "x")).unwrap();
        let mut e = mk("b", End, 13, 30, "x");
        derive(&s, &mut e).unwrap();
        assert_eq!(e.data["session_minutes"].as_f64(), Some(210.0));
    }

    #[test]
    fn end_without_start_has_no_minutes() {
        let s = Store::open_in_memory().unwrap();
        let mut e = mk("b", End, 13, 30, "x");
        derive(&s, &mut e).unwrap();
        assert!(e.data.get("session_minutes").is_none());
    }

    #[test]
    fn open_sessions_counts_concurrent() {
        let s = Store::open_in_memory().unwrap();
        s.insert_event(&mk("a", Start, 9, 0, "a")).unwrap();
        s.insert_event(&mk("b", Start, 9, 10, "b")).unwrap();
        let mut c = mk("c", Start, 9, 20, "c");
        derive(&s, &mut c).unwrap();
        assert_eq!(c.data["open_sessions"].as_u64(), Some(3));
        s.insert_event(&c).unwrap();
        s.insert_event(&mk("ea", End, 9, 30, "a")).unwrap();
        let mut d = mk("d", Start, 9, 40, "d");
        derive(&s, &mut d).unwrap();
        assert_eq!(d.data["open_sessions"].as_u64(), Some(3));
    }

    #[test]
    fn stale_start_not_open() {
        let s = Store::open_in_memory().unwrap();
        s.insert_event(&mk("a", Start, 1, 0, "a")).unwrap();
        let mut b = mk("b", Start, 14, 0, "b");
        derive(&s, &mut b).unwrap();
        assert_eq!(b.data["open_sessions"].as_u64(), Some(1));
    }
}
