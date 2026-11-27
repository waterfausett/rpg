use crate::event::{Event, EventKind, Source};
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA_VERSION: &str = "3";

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

/// One skill's lifetime XP (see `Store::skill_totals`).
#[derive(Debug, Clone, PartialEq)]
pub struct SkillTotal {
    pub skill_id: String,
    /// The name it was last credited under.
    pub name: String,
    pub xp: u64,
    pub last_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub event: Event,
    /// Total XP the event earned.
    pub xp: u64,
    /// The ledger entries behind `xp`, in the order they were awarded.
    pub gains: Vec<Gain>,
}

/// One ledger entry. `tier` is set when the entry is an achievement's reward: it comes from the
/// unlock the ledger row points at (`xp_ledger.unlock_id`).
#[derive(Debug, Clone, PartialEq)]
pub struct Gain {
    pub reason: String,
    pub amount: u64,
    pub tier: Option<String>,
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
        Source::Quest => "quest",
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
                amount INTEGER NOT NULL, reason TEXT NOT NULL, ts TEXT NOT NULL, unlock_id INTEGER);
             CREATE INDEX IF NOT EXISTS idx_ledger_event ON xp_ledger(event_id);
             CREATE TABLE IF NOT EXISTS unlocks(
                id INTEGER PRIMARY KEY AUTOINCREMENT, rule_id TEXT NOT NULL, name TEXT NOT NULL,
                tier TEXT NOT NULL, xp INTEGER NOT NULL, context TEXT NOT NULL,
                flavor_text TEXT, ts TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_unlocks_rule ON unlocks(rule_id);
             CREATE TABLE IF NOT EXISTS skill_xp(
                id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL, skill_id TEXT NOT NULL,
                name TEXT NOT NULL, amount INTEGER NOT NULL, ts TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_skill_xp_skill ON skill_xp(skill_id);
             CREATE TABLE IF NOT EXISTS records(metric TEXT PRIMARY KEY, value REAL NOT NULL, ts TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        conn.execute("INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', ?1)", params![SCHEMA_VERSION])?;
        Self::migrate(&conn)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    /// Bring an older database up to `SCHEMA_VERSION`.
    fn migrate(conn: &Connection) -> Result<()> {
        let version = || -> Result<String> {
            Ok(conn.query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))?)
        };
        if version()? == "1" {
            // v2: repos are normalized at ingest; rewrite the ones stored before that.
            let tx = conn.unchecked_transaction()?;
            let repos: Vec<String> = {
                let mut st = tx.prepare("SELECT DISTINCT repo FROM events WHERE repo IS NOT NULL")?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for r in repos {
                let n = crate::event::normalize_repo(&r);
                if n != r {
                    tx.execute("UPDATE events SET repo=?1 WHERE repo=?2", params![n, r])?;
                }
            }
            tx.execute("UPDATE meta SET value='2' WHERE key='schema_version'", [])?;
            tx.commit()?;
        }
        if version()? == "2" {
            // v3: an achievement's ledger row points at its unlock. Older rows are matched once by
            // name, timestamp and amount (the engine wrote both with the event's ts and the step's xp).
            let tx = conn.unchecked_transaction()?;
            let has_col: bool =
                tx.query_row("SELECT COUNT(*) FROM pragma_table_info('xp_ledger') WHERE name='unlock_id'", [], |r| r.get(0))?;
            if !has_col {
                tx.execute("ALTER TABLE xp_ledger ADD COLUMN unlock_id INTEGER", [])?;
            }
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_unlocks_name_ts ON unlocks(name, ts);
                 UPDATE xp_ledger SET unlock_id = (
                    SELECT u.id FROM unlocks u
                    WHERE u.name = xp_ledger.reason AND u.ts = xp_ledger.ts AND u.xp = xp_ledger.amount
                    ORDER BY u.id LIMIT 1);
                 DROP INDEX idx_unlocks_name_ts;
                 UPDATE meta SET value='3' WHERE key='schema_version';",
            )?;
            tx.commit()?;
        }
        Ok(())
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

    /// Events of the given kinds with `ts >= since`, oldest first (`ts ASC, id ASC`), backfill
    /// included. An empty `kinds` slice yields no events.
    pub fn events_of(&self, kinds: &[EventKind], since: Option<DateTime<Utc>>) -> Result<Vec<Event>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; kinds.len()].join(",");
        let mut args: Vec<String> = kinds.iter().map(|k| k.as_str().to_string()).collect();
        let mut sql = format!("SELECT {EVENT_COLS} FROM events e WHERE e.kind IN ({marks})");
        if let Some(t) = since {
            sql.push_str(" AND e.ts >= ?");
            args.push(ts_str(t));
        }
        sql.push_str(" ORDER BY e.ts ASC, e.id ASC");
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), event_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Up to `limit` events of `kind` (optionally only in `repo`) that sort strictly before
    /// `(ts, id)` in `events_of` order, newest first (`ts DESC, id DESC`). Pass the last one
    /// returned as the next `(ts, id)` to page further back.
    pub fn events_before(
        &self,
        kind: EventKind,
        repo: Option<&str>,
        ts: DateTime<Utc>,
        id: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let sql = format!(
            "SELECT {EVENT_COLS} FROM events e
             WHERE e.kind=?1 AND (?2 IS NULL OR e.repo=?2) AND (e.ts<?3 OR (e.ts=?3 AND e.id<?4))
             ORDER BY e.ts DESC, e.id DESC LIMIT ?5"
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![kind.as_str(), repo, ts_str(ts), id, limit as i64], event_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// `unlock_id` is set when the XP is an achievement's reward (see `insert_unlock`).
    pub fn add_xp(&self, event_id: &str, amount: u64, reason: &str, unlock_id: Option<i64>, ts: DateTime<Utc>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO xp_ledger(event_id, amount, reason, ts, unlock_id) VALUES(?1,?2,?3,?4,?5)",
            params![event_id, amount as i64, reason, ts_str(ts), unlock_id],
        )?;
        Ok(())
    }

    pub fn total_xp(&self) -> Result<u64> {
        let n: i64 = self.conn().query_row("SELECT COALESCE(SUM(amount),0) FROM xp_ledger", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// Credit `amount` skill XP to `skill_id` (`name` is kept so a removed skill still shows).
    pub fn add_skill_xp(&self, event_id: &str, skill_id: &str, name: &str, amount: u64, ts: DateTime<Utc>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO skill_xp(event_id, skill_id, name, amount, ts) VALUES(?1,?2,?3,?4,?5)",
            params![event_id, skill_id, name, amount as i64, ts_str(ts)],
        )?;
        Ok(())
    }

    pub fn skill_xp(&self, skill_id: &str) -> Result<u64> {
        let n: i64 = self.conn().query_row(
            "SELECT COALESCE(SUM(amount),0) FROM skill_xp WHERE skill_id=?1",
            params![skill_id],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// Every skill with XP, biggest first.
    pub fn skill_totals(&self) -> Result<Vec<SkillTotal>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT skill_id,
                    (SELECT name FROM skill_xp n WHERE n.skill_id=s.skill_id ORDER BY n.id DESC LIMIT 1),
                    SUM(amount), MAX(ts)
             FROM skill_xp s GROUP BY skill_id ORDER BY SUM(amount) DESC, skill_id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(SkillTotal {
                skill_id: r.get(0)?,
                name: r.get(1)?,
                xp: r.get::<_, i64>(2)?.max(0) as u64,
                last_at: parse_ts(&r.get::<_, String>(3)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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

    /// Whether rule `id` has any stored unlock: `<id>` itself or one of its steps
    /// (`<id>.<tier>`, `<id>.<skill_id>.<tier>`).
    pub fn rule_ever_unlocked(&self, id: &str) -> Result<bool> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM unlocks WHERE rule_id=?1 OR substr(rule_id, 1, length(?1) + 1) = ?1 || '.'",
            params![id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// How many times each stored `rule_id` was unlocked, ordered by `rule_id`.
    pub fn unlock_counts(&self) -> Result<Vec<(String, u32)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT rule_id, COUNT(*) FROM unlocks GROUP BY rule_id ORDER BY rule_id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
            Ok(EventRow { event: event_from_row(r)?, xp: r.get::<_, i64>(7)?.max(0) as u64, gains: vec![] })
        })?;
        let mut rows: Vec<EventRow> = rows.collect::<rusqlite::Result<_>>()?;
        let mut gains = conn.prepare(
            "SELECT l.reason, l.amount, u.tier
             FROM xp_ledger l LEFT JOIN unlocks u ON u.id = l.unlock_id
             WHERE l.event_id=?1 ORDER BY l.id",
        )?;
        for row in &mut rows {
            let g = gains.query_map(params![row.event.id], |r| {
                Ok(Gain { reason: r.get(0)?, amount: r.get::<_, i64>(1)?.max(0) as u64, tier: r.get(2)? })
            })?;
            row.gains = g.collect::<rusqlite::Result<_>>()?;
        }
        Ok(rows)
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
    fn v1_database_gets_repos_normalized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpg.db");
        {
            let s = Store::open(&path).unwrap();
            let mut a = ev("a", EventKind::Commit, Utc::now());
            a.repo = Some(r"c:\work\rpg".into());
            let mut b = ev("b", EventKind::Commit, Utc::now());
            b.repo = Some("octo/widgets".into());
            s.insert_event(&a).unwrap();
            s.insert_event(&b).unwrap();
            s.meta_set("schema_version", "1").unwrap();
        }
        let s = Store::open(&path).unwrap();
        let repos: Vec<_> = s.list_events(None, 10).unwrap().into_iter().map(|r| r.event.repo.unwrap()).collect();
        assert!(repos.contains(&"C:/work/rpg".to_string()), "{repos:?}");
        assert!(repos.contains(&"octo/widgets".to_string()), "{repos:?}");
        assert_eq!(s.meta_get("schema_version").unwrap().as_deref(), Some("3"));
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
    fn unlock_counts_per_rule_id() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.unlock_counts().unwrap().is_empty());
        for id in ["b", "a.silver", "b", "a.bronze"] {
            let u = NewUnlock { rule_id: id.into(), name: id.into(), tier: "bronze".into(), xp: 0, context: serde_json::json!({}), ts: Utc::now() };
            s.insert_unlock(&u).unwrap();
        }
        let want: Vec<(String, u32)> = vec![("a.bronze".into(), 1), ("a.silver".into(), 1), ("b".into(), 2)];
        assert_eq!(s.unlock_counts().unwrap(), want);
    }

    #[test]
    fn rule_ever_unlocked_matches_id_and_steps_only() {
        let s = Store::open_in_memory().unwrap();
        assert!(!s.rule_ever_unlocked("lad").unwrap());
        for id in ["lad_x", "ladder.bronze", "sm.rust.bronze"] {
            s.insert_unlock(&nu(id, Utc::now())).unwrap();
        }
        assert!(!s.rule_ever_unlocked("lad").unwrap(), "prefixes of other ids don't count");
        assert!(s.rule_ever_unlocked("ladder").unwrap());
        assert!(s.rule_ever_unlocked("sm").unwrap());
        assert!(s.rule_ever_unlocked("lad_x").unwrap());
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
    fn skill_xp_totals() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        assert_eq!(s.skill_xp("rust").unwrap(), 0);
        assert!(s.skill_totals().unwrap().is_empty());
        s.add_skill_xp("e1", "rust", "Rust", 10, now).unwrap();
        s.add_skill_xp("e2", "rust", "Rustacean", 10, now).unwrap();
        s.add_skill_xp("e2", "docs", "Scribe", 30, now).unwrap();
        assert_eq!(s.skill_xp("rust").unwrap(), 20);
        let t = s.skill_totals().unwrap();
        let t: Vec<_> = t.iter().map(|s| (s.skill_id.as_str(), s.name.as_str(), s.xp)).collect();
        assert_eq!(t, vec![("docs", "Scribe", 30), ("rust", "Rustacean", 20)]);
        assert_eq!(s.total_xp().unwrap(), 0, "skill xp is separate from character xp");
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
    fn events_before_pages_backwards() {
        let s = Store::open_in_memory().unwrap();
        let t0 = Utc::now() - Duration::days(1);
        let at = |m: i64| t0 + Duration::minutes(m);
        for (id, m, repo) in [("a", 0, "r"), ("b", 1, "x"), ("c", 2, "r"), ("d2", 3, "r"), ("d1", 3, "r"), ("e", 4, "r")] {
            let mut e = ev(id, EventKind::Commit, at(m));
            e.repo = Some(repo.into());
            s.insert_event(&e).unwrap();
        }
        s.insert_event(&ev("p", EventKind::Push, at(2))).unwrap();
        let ids = |v: Vec<Event>| v.into_iter().map(|e| e.id).collect::<Vec<_>>();
        // Strictly before (ts, id) = (3, "d2"), newest first, ties broken by id descending.
        assert_eq!(ids(s.events_before(EventKind::Commit, None, at(3), "d2", 10).unwrap()), ["d1", "c", "b", "a"]);
        assert_eq!(ids(s.events_before(EventKind::Commit, None, at(3), "d2", 2).unwrap()), ["d1", "c"], "limit");
        assert_eq!(ids(s.events_before(EventKind::Commit, Some("r"), at(3), "d2", 10).unwrap()), ["d1", "c", "a"]);
        assert_eq!(ids(s.events_before(EventKind::Commit, None, at(0), "a", 10).unwrap()), Vec::<String>::new());
    }

    #[test]
    fn events_of_filters_and_orders() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        s.insert_event(&ev("b", EventKind::Commit, now)).unwrap();
        s.insert_event(&ev("a", EventKind::Commit, now)).unwrap();
        s.insert_event(&ev("old", EventKind::Commit, now - chrono::Duration::days(2))).unwrap();
        s.insert_event(&ev("s", EventKind::ClaudeSessionStart, now)).unwrap();
        let all = s.events_of(&[EventKind::Commit], None).unwrap();
        assert_eq!(all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["old", "a", "b"]);
        let recent = s.events_of(&[EventKind::Commit], Some(now)).unwrap();
        assert_eq!(recent.len(), 2);
        assert!(s.events_of(&[], None).unwrap().is_empty());
    }

    #[test]
    fn xp_events_and_lookups() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        let mut st = ev("s1", EventKind::ClaudeSessionStart, now);
        st.data = serde_json::json!({"session_id": "abc"});
        s.insert_event(&st).unwrap();
        s.insert_event(&ev("c1", EventKind::Commit, now)).unwrap();
        s.add_xp("c1", 10, "commit", None, now).unwrap();
        s.add_xp("c1", 5, "bonus", None, now).unwrap();
        assert_eq!(s.total_xp().unwrap(), 15);
        assert_eq!(s.find_session_start("abc").unwrap().unwrap().id, "s1");
        assert!(s.find_session_start("zzz").unwrap().is_none());
        assert!(s.has_prior_event(EventKind::Commit, Some("r"), "other").unwrap());
        assert!(!s.has_prior_event(EventKind::Commit, Some("r"), "c1").unwrap());
        assert!(!s.has_prior_event(EventKind::Commit, Some("x"), "other").unwrap());
        let rows = s.list_events(Some(EventKind::Commit), 10).unwrap();
        assert_eq!((rows.len(), rows[0].xp), (1, 15));
        assert_eq!(rows[0].gains.iter().map(|g| g.amount).sum::<u64>(), 15);
        assert_eq!(s.list_events(None, 10).unwrap().len(), 2);
        s.meta_set("k", "v").unwrap();
        assert_eq!(s.meta_get("k").unwrap().as_deref(), Some("v"));
        assert_eq!(s.meta_get("schema_version").unwrap().as_deref(), Some("3"));
        let id = s.insert_unlock(&nu("r1", now)).unwrap();
        s.set_unlock_flavor(id, "hi").unwrap();
        let u = s.list_unlocks().unwrap();
        assert_eq!(u[0].flavor_text.as_deref(), Some("hi"));
    }

    fn unlock(s: &Store, name: &str, tier: &str, xp: u64, ts: DateTime<Utc>) -> i64 {
        s.insert_unlock(&NewUnlock { rule_id: name.into(), name: name.into(), tier: tier.into(), xp,
            context: serde_json::json!({}), ts }).unwrap()
    }

    #[test]
    fn gains_carry_unlock_tier() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        s.insert_event(&ev("c1", EventKind::Commit, now)).unwrap();
        s.add_xp("c1", 10, "commit", None, now).unwrap();
        let id = unlock(&s, "Hat Trick", "silver", 50, now);
        s.add_xp("c1", 50, "Hat Trick", Some(id), now).unwrap();
        let g = &s.list_events(None, 10).unwrap()[0].gains;
        assert_eq!((g[0].reason.as_str(), g[0].tier.as_deref()), ("commit", None));
        assert_eq!((g[1].reason.as_str(), g[1].amount, g[1].tier.as_deref()), ("Hat Trick", 50, Some("silver")));
    }

    #[test]
    fn same_named_unlocks_on_one_event_keep_their_own_tiers() {
        // Two rules sharing a name, both firing on one event: matching by name and time can't tell them apart.
        let s = Store::open_in_memory().unwrap();
        let now = Utc::now();
        s.insert_event(&ev("c1", EventKind::Commit, now)).unwrap();
        let a = unlock(&s, "Twin", "bronze", 50, now);
        let b = unlock(&s, "Twin", "gold", 50, now);
        s.add_xp("c1", 50, "Twin", Some(a), now).unwrap();
        s.add_xp("c1", 50, "Twin", Some(b), now).unwrap();
        let tiers: Vec<_> = s.list_events(None, 10).unwrap()[0].gains.iter().map(|g| g.tier.clone()).collect();
        assert_eq!(tiers, [Some("bronze".to_string()), Some("gold".to_string())]);
    }

    #[test]
    fn v2_database_gets_ledger_unlock_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpg.db");
        let now = Utc::now();
        let (t, old) = (ts_str(now), ts_str(now - Duration::days(1)));
        {
            // The v2 shape of the two tables involved: no xp_ledger.unlock_id yet.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE xp_ledger(id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL,
                    amount INTEGER NOT NULL, reason TEXT NOT NULL, ts TEXT NOT NULL);
                 CREATE TABLE unlocks(id INTEGER PRIMARY KEY AUTOINCREMENT, rule_id TEXT NOT NULL, name TEXT NOT NULL,
                    tier TEXT NOT NULL, xp INTEGER NOT NULL, context TEXT NOT NULL, flavor_text TEXT, ts TEXT NOT NULL);
                 CREATE INDEX idx_unlocks_name_ts ON unlocks(name, ts);
                 CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta VALUES('schema_version', '2');",
            )
            .unwrap();
            c.execute("INSERT INTO unlocks(rule_id, name, tier, xp, context, ts) VALUES('h','Hat Trick','silver',50,'{}',?1)", [&t]).unwrap();
            c.execute("INSERT INTO unlocks(rule_id, name, tier, xp, context, ts) VALUES('h','Hat Trick','gold',50,'{}',?1)", [&old]).unwrap();
            c.execute("INSERT INTO xp_ledger(event_id, amount, reason, ts) VALUES('c1',10,'commit',?1)", [&t]).unwrap();
            c.execute("INSERT INTO xp_ledger(event_id, amount, reason, ts) VALUES('c1',50,'Hat Trick',?1)", [&t]).unwrap();
        }
        let s = Store::open(&path).unwrap();
        s.insert_event(&ev("c1", EventKind::Commit, now)).unwrap();
        let g = &s.list_events(None, 10).unwrap()[0].gains;
        assert_eq!((g[0].tier.as_deref(), g[1].tier.as_deref()), (None, Some("silver")));
        assert_eq!(s.meta_get("schema_version").unwrap().as_deref(), Some("3"));
        let idx: i64 = s.conn().query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='idx_unlocks_name_ts'", [], |r| r.get(0)).unwrap();
        assert_eq!(idx, 0, "the name/ts index is only needed for the one-time backfill");
    }
}
