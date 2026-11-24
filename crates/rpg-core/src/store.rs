use crate::event::{Event, EventKind, Source};
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA_VERSION: &str = "1";

/// SQLite-backed store. Timestamps are stored as fixed-width RFC3339 UTC
/// strings with millisecond precision, which sort lexicographically.
pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct NewUnlock {
    pub rule_id: String,
    pub name: String,
    pub tier: String,
    pub xp: u64,
    pub context: serde_json::Value,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UnlockRow {
    pub id: i64,
    pub rule_id: String,
    pub name: String,
    pub tier: String,
    pub xp: u64,
    pub context: serde_json::Value,
    pub flavor_text: Option<String>,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub event: Event,
    pub xp: u64,
}

fn conv_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

fn ts_str(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc)).map_err(conv_err)
}

fn enum_from<T: serde::de::DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(conv_err)
}

fn source_str(s: Source) -> &'static str {
    match s {
        Source::Git => "git",
        Source::Github => "github",
        Source::Claude => "claude",
    }
}

fn json_from(s: &str) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(s).map_err(conv_err)
}

const EVENT_COLS: &str = "e.id, e.kind, e.ts, e.source, e.repo, e.backfill, e.data";

fn event_from_row(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        id: r.get(0)?,
        kind: enum_from(&r.get::<_, String>(1)?)?,
        ts: parse_ts(&r.get::<_, String>(2)?)?,
        source: enum_from(&r.get::<_, String>(3)?)?,
        repo: r.get(4)?,
        backfill: r.get::<_, i64>(5)? != 0,
        data: json_from(&r.get::<_, String>(6)?)?,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events(
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, ts TEXT NOT NULL, source TEXT NOT NULL,
                repo TEXT, backfill INTEGER NOT NULL DEFAULT 0, data TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_events_kind_ts ON events(kind, ts);
             CREATE TABLE IF NOT EXISTS xp_ledger(
                id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL,
                amount INTEGER NOT NULL, reason TEXT NOT NULL, ts TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_ledger_event ON xp_ledger(event_id);
             CREATE TABLE IF NOT EXISTS unlocks(
                id INTEGER PRIMARY KEY AUTOINCREMENT, rule_id TEXT NOT NULL, name TEXT NOT NULL,
                tier TEXT NOT NULL, xp INTEGER NOT NULL, context TEXT NOT NULL,
                flavor_text TEXT, ts TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_unlocks_rule ON unlocks(rule_id);
             CREATE TABLE IF NOT EXISTS records(metric TEXT PRIMARY KEY, value REAL NOT NULL, ts TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        conn.execute("INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', ?1)", params![SCHEMA_VERSION])?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Begin an immediate transaction on the shared connection. Every Store
    /// method uses the same connection, so statements issued until
    /// `commit_tx`/`rollback_tx` are part of this transaction. This assumes
    /// single-threaded processing: concurrent callers would interleave into it.
    pub fn begin_tx(&self) -> Result<()> {
        self.conn().execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    /// Commit the transaction opened by `begin_tx` (same single-thread assumption).
    pub fn commit_tx(&self) -> Result<()> {
        self.conn().execute_batch("COMMIT")?;
        Ok(())
    }

    /// Roll back the transaction opened by `begin_tx` (same single-thread assumption).
    pub fn rollback_tx(&self) -> Result<()> {
        self.conn().execute_batch("ROLLBACK")?;
        Ok(())
    }

    /// Run raw SQL; lets tests inject failures.
    #[cfg(test)]
    pub(crate) fn exec_raw(&self, sql: &str) -> Result<()> {
        self.conn().execute_batch(sql)?;
        Ok(())
    }

    /// Returns false if an event with this id already exists.
    pub fn insert_event(&self, e: &Event) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO events(id, kind, ts, source, repo, backfill, data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![e.id, e.kind.as_str(), ts_str(e.ts), source_str(e.source), e.repo, e.backfill as i64, e.data.to_string()],
        )?;
        Ok(n > 0)
    }

    /// Count events of `kind` with `from <= ts < to`, excluding `exclude_id`.
    pub fn count_events(&self, kind: EventKind, from: DateTime<Utc>, to: DateTime<Utc>, exclude_id: &str) -> Result<u32> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM events WHERE kind=?1 AND ts>=?2 AND ts<?3 AND id<>?4",
            params![kind.as_str(), ts_str(from), ts_str(to), exclude_id],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Count of non-backfill events of `kind` (backfilled history is not "yours").
    pub fn count_live_events(&self, kind: EventKind) -> Result<u32> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM events WHERE kind=?1 AND backfill=0",
            params![kind.as_str()],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Whether another event of `kind` (optionally in `repo`) exists besides `exclude_id`.
    pub fn has_prior_event(&self, kind: EventKind, repo: Option<&str>, exclude_id: &str) -> Result<bool> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM events WHERE kind=?1 AND id<>?2 AND (?3 IS NULL OR repo=?3)",
            params![kind.as_str(), exclude_id, repo],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Find the session-start event whose `data.session_id` matches.
    pub fn find_session_start(&self, session_id: &str) -> Result<Option<Event>> {
        let sql = format!(
            "SELECT {EVENT_COLS} FROM events e WHERE e.kind=?1 AND json_extract(e.data, '$.session_id')=?2 ORDER BY e.ts DESC LIMIT 1"
        );
        Ok(self
            .conn()
            .query_row(&sql, params![EventKind::ClaudeSessionStart.as_str(), session_id], event_from_row)
            .optional()?)
    }

    pub fn add_xp(&self, event_id: &str, amount: u64, reason: &str, ts: DateTime<Utc>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO xp_ledger(event_id, amount, reason, ts) VALUES(?1,?2,?3,?4)",
            params![event_id, amount as i64, reason, ts_str(ts)],
        )?;
        Ok(())
    }

    pub fn total_xp(&self) -> Result<u64> {
        let n: i64 = self.conn().query_row("SELECT COALESCE(SUM(amount),0) FROM xp_ledger", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    pub fn record(&self, metric: &str) -> Result<Option<f64>> {
        Ok(self.conn().query_row("SELECT value FROM records WHERE metric=?1", params![metric], |r| r.get(0)).optional()?)
    }

    pub fn set_record(&self, metric: &str, value: f64, ts: DateTime<Utc>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO records(metric, value, ts) VALUES(?1,?2,?3)
             ON CONFLICT(metric) DO UPDATE SET value=excluded.value, ts=excluded.ts",
            params![metric, value, ts_str(ts)],
        )?;
        Ok(())
    }

    pub fn insert_unlock(&self, u: &NewUnlock) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO unlocks(rule_id, name, tier, xp, context, ts) VALUES(?1,?2,?3,?4,?5,?6)",
            params![u.rule_id, u.name, u.tier, u.xp as i64, u.context.to_string(), ts_str(u.ts)],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_unlock_flavor(&self, id: i64, text: &str) -> Result<()> {
        self.conn().execute("UPDATE unlocks SET flavor_text=?1 WHERE id=?2", params![text, id])?;
        Ok(())
    }

    pub fn unlock_count(&self, rule_id: &str) -> Result<u32> {
        let n: i64 =
            self.conn().query_row("SELECT COUNT(*) FROM unlocks WHERE rule_id=?1", params![rule_id], |r| r.get(0))?;
        Ok(n as u32)
    }

    pub fn last_unlock_at(&self, rule_id: &str) -> Result<Option<DateTime<Utc>>> {
        let s: Option<String> =
            self.conn().query_row("SELECT MAX(ts) FROM unlocks WHERE rule_id=?1", params![rule_id], |r| r.get(0))?;
        Ok(s.map(|s| parse_ts(&s)).transpose()?)
    }

    /// All unlocks, newest first.
    pub fn list_unlocks(&self) -> Result<Vec<UnlockRow>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, rule_id, name, tier, xp, context, flavor_text, ts FROM unlocks ORDER BY ts DESC, id DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(UnlockRow {
                id: r.get(0)?,
                rule_id: r.get(1)?,
                name: r.get(2)?,
                tier: r.get(3)?,
                xp: r.get::<_, i64>(4)?.max(0) as u64,
                context: json_from(&r.get::<_, String>(5)?)?,
                flavor_text: r.get(6)?,
                ts: parse_ts(&r.get::<_, String>(7)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Newest-first events with the XP each earned.
    pub fn list_events(&self, kind: Option<EventKind>, limit: u32) -> Result<Vec<EventRow>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {EVENT_COLS}, COALESCE((SELECT SUM(amount) FROM xp_ledger WHERE event_id=e.id),0)
             FROM events e WHERE (?1 IS NULL OR e.kind=?1) ORDER BY e.ts DESC, e.id DESC LIMIT ?2"
        );
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map(params![kind.map(|k| k.as_str()), limit as i64], |r| {
            Ok(EventRow { event: event_from_row(r)?, xp: r.get::<_, i64>(7)?.max(0) as u64 })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn().query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO meta(key, value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn ev(id: &str, kind: EventKind, ts: DateTime<Utc>) -> Event {
        Event { id: id.into(), kind, ts, source: Source::Git, repo: Some("r".into()), backfill: false, data: serde_json::json!({}) }
    }

    #[test]
    fn count_live_events_skips_backfill_and_other_kinds() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        s.insert_event(&ev("a", EventKind::PrMerged, now)).unwrap();
        s.insert_event(&ev("b", EventKind::PrMerged, now)).unwrap();
        let mut old = ev("c", EventKind::PrMerged, now);
        old.backfill = true;
        s.insert_event(&old).unwrap();
        s.insert_event(&ev("d", EventKind::Commit, now)).unwrap();
        assert_eq!(s.count_live_events(EventKind::PrMerged).unwrap(), 2);
        assert_eq!(s.count_live_events(EventKind::ReviewSubmitted).unwrap(), 0);
    }

    #[test]
    fn duplicate_event_rejected() {
        let s = Store::open_in_memory().unwrap();
        let e = ev("a", EventKind::Commit, Utc::now());
        assert!(s.insert_event(&e).unwrap());
        assert!(!s.insert_event(&e).unwrap());
    }

    #[test]
    fn count_events_excludes_self_and_respects_window() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        for (i, off) in [0i64, 10, 20, 300].iter().enumerate() {
            s.insert_event(&ev(&format!("e{i}"), EventKind::Commit, now - Duration::minutes(*off))).unwrap();
        }
        s.insert_event(&ev("p", EventKind::Push, now)).unwrap();
        let n = s.count_events(EventKind::Commit, now - Duration::minutes(60), now + Duration::seconds(1), "e0").unwrap();
        assert_eq!(n, 2);
    }

    fn nu(rule: &str, ts: DateTime<Utc>) -> NewUnlock {
        NewUnlock { rule_id: rule.into(), name: "N".into(), tier: "gold".into(), xp: 5, context: serde_json::json!({"a":1}), ts }
    }

    #[test]
    fn unlock_count_and_last_at() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.unlock_count("r1").unwrap(), 0);
        assert!(s.last_unlock_at("r1").unwrap().is_none());
        let t1 = Utc::now() - Duration::hours(2);
        let t2 = Utc::now();
        s.insert_unlock(&nu("r1", t1)).unwrap();
        s.insert_unlock(&nu("r1", t2)).unwrap();
        assert_eq!(s.unlock_count("r1").unwrap(), 2);
        assert_eq!(s.last_unlock_at("r1").unwrap().unwrap().timestamp_millis(), t2.timestamp_millis());
    }

    #[test]
    fn records_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.record("m").unwrap(), None);
        s.set_record("m", 3.5, Utc::now()).unwrap();
        s.set_record("m", 7.0, Utc::now()).unwrap();
        assert_eq!(s.record("m").unwrap(), Some(7.0));
    }

    #[test]
    fn xp_events_and_lookups() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        let mut st = ev("s1", EventKind::ClaudeSessionStart, now);
        st.data = serde_json::json!({"session_id": "abc"});
        s.insert_event(&st).unwrap();
        s.insert_event(&ev("c1", EventKind::Commit, now)).unwrap();
        s.add_xp("c1", 10, "commit", now).unwrap();
        s.add_xp("c1", 5, "bonus", now).unwrap();
        assert_eq!(s.total_xp().unwrap(), 15);
        assert_eq!(s.find_session_start("abc").unwrap().unwrap().id, "s1");
        assert!(s.find_session_start("zzz").unwrap().is_none());
        assert!(s.has_prior_event(EventKind::Commit, Some("r"), "other").unwrap());
        assert!(!s.has_prior_event(EventKind::Commit, Some("r"), "c1").unwrap());
        assert!(!s.has_prior_event(EventKind::Commit, Some("x"), "other").unwrap());
        let rows = s.list_events(Some(EventKind::Commit), 10).unwrap();
        assert_eq!((rows.len(), rows[0].xp), (1, 15));
        assert_eq!(s.list_events(None, 10).unwrap().len(), 2);
        s.meta_set("k", "v").unwrap();
        assert_eq!(s.meta_get("k").unwrap().as_deref(), Some("v"));
        assert_eq!(s.meta_get("schema_version").unwrap().as_deref(), Some("1"));
        let id = s.insert_unlock(&nu("r1", now)).unwrap();
        s.set_unlock_flavor(id, "hi").unwrap();
        let u = s.list_unlocks().unwrap();
        assert_eq!(u[0].flavor_text.as_deref(), Some("hi"));
    }
}
