//! Event filters: typed conditions on one event, all of which must match.
use crate::event::{Event, EventKind};
use crate::skills::glob_set;
use chrono::{DateTime, Datelike, FixedOffset, Timelike, Weekday};
use globset::GlobSet;
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawLines {
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub min_delete_ratio: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawHours {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawMin {
    pub min: u64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawFilter {
    pub message: Option<String>,
    pub title: Option<String>,
    pub hash: Option<String>,
    pub branch: Option<Vec<String>>,
    pub repo: Option<Vec<String>>,
    pub files_any: Option<Vec<String>>,
    pub files_only: Option<Vec<String>>,
    pub min_files: Option<u64>,
    pub max_files: Option<u64>,
    pub min_extensions: Option<u32>,
    pub lines: Option<RawLines>,
    pub hash_palindrome: Option<usize>,
    pub weekdays: Option<Vec<String>>,
    pub hours: Option<RawHours>,
    pub time: Option<String>,
    pub dates: Option<Vec<String>>,
    pub pr_open_minutes: Option<RawRange>,
    pub session_minutes: Option<RawRange>,
    pub message_length: Option<RawRange>,
    pub open_sessions: Option<RawMin>,
    pub chain: Option<RawRange>,
    pub quest: Option<Vec<String>>,
}

/// Compiled filter. The default matches everything.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    message: Option<Regex>,
    title: Option<Regex>,
    hash: Option<Regex>,
    branch: Option<GlobSet>,
    repo: Option<GlobSet>,
    files_any: Option<GlobSet>,
    files_only: Option<GlobSet>,
    min_files: Option<u64>,
    max_files: Option<u64>,
    min_extensions: Option<u32>,
    lines: Option<RawLines>,
    hash_palindrome: Option<usize>,
    weekdays: Option<Vec<Weekday>>,
    hours: Option<(u32, u32)>,
    time: Option<(u32, u32)>,
    dates: Option<Vec<(u32, u32)>>,
    pr_open_minutes: Option<RawRange>,
    session_minutes: Option<RawRange>,
    message_length: Option<RawRange>,
    open_sessions: Option<u64>,
    chain: Option<RawRange>,
    quest: Option<GlobSet>,
}

fn regex(field: &str, p: Option<String>) -> Result<Option<Regex>, String> {
    let Some(p) = p else { return Ok(None) };
    RegexBuilder::new(&p)
        .case_insensitive(true)
        .build()
        .map(Some)
        .map_err(|e| format!("bad {field} regex `{p}`: {e}"))
}

fn weekday(s: &str) -> Result<Weekday, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => return Err(format!("bad weekdays entry `{s}` (use mon..sun)")),
    })
}

/// Parse `A<sep>B` as two numbers with `A <= a_max` and `B <= b_max`.
fn pair(field: &str, s: &str, sep: char, a_max: u32, b_max: u32) -> Result<(u32, u32), String> {
    let bad = || format!("bad {field} value `{s}`");
    let (a, b) = s.split_once(sep).ok_or_else(bad)?;
    let (a, b): (u32, u32) = (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
    if a > a_max || b > b_max {
        return Err(bad());
    }
    Ok((a, b))
}

impl TryFrom<RawFilter> for Filter {
    type Error = String;

    fn try_from(r: RawFilter) -> Result<Filter, String> {
        let weekdays = match r.weekdays {
            None => None,
            Some(v) if v.is_empty() => return Err("`weekdays` is empty".into()),
            Some(v) => Some(v.iter().map(|s| weekday(s)).collect::<Result<Vec<_>, _>>()?),
        };
        if let Some(h) = &r.hours {
            if h.start > 24 || h.end > 24 {
                return Err(format!("bad hours {}..{} (must be 0..=24)", h.start, h.end));
            }
        }
        let dates = match r.dates {
            None => None,
            Some(v) if v.is_empty() => return Err("`dates` is empty".into()),
            Some(v) => Some(
                v.iter()
                    .map(|s| {
                        let (m, d) = pair("dates", s, '-', 12, 31)?;
                        // 2024 is a leap year, so 02-29 is accepted.
                        if chrono::NaiveDate::from_ymd_opt(2024, m, d).is_none() {
                            return Err(format!("bad dates value `{s}`"));
                        }
                        Ok((m, d))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            ),
        };
        Ok(Filter {
            message: regex("message", r.message)?,
            title: regex("title", r.title)?,
            hash: regex("hash", r.hash)?,
            branch: glob_set("branch", r.branch)?,
            repo: glob_set("repo", r.repo)?,
            files_any: glob_set("files_any", r.files_any)?,
            files_only: glob_set("files_only", r.files_only)?,
            min_files: r.min_files,
            max_files: r.max_files,
            min_extensions: r.min_extensions,
            lines: r.lines,
            hash_palindrome: r.hash_palindrome,
            weekdays,
            hours: r.hours.map(|h| (h.start, h.end)),
            time: r.time.map(|s| pair("time", &s, ':', 23, 59)).transpose()?,
            dates,
            pr_open_minutes: r.pr_open_minutes,
            session_minutes: r.session_minutes,
            message_length: r.message_length,
            open_sessions: r.open_sessions.map(|o| o.min),
            chain: r.chain,
            quest: glob_set("quest", r.quest)?,
        })
    }
}

fn in_range(r: &RawRange, v: f64) -> bool {
    r.min.is_none_or(|m| v >= m) && r.max.is_none_or(|m| v <= m)
}

fn file_paths(ev: &Event) -> Vec<&str> {
    ev.data["files"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["path"].as_str()).collect())
        .unwrap_or_default()
}

fn extension(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_lowercase())
}

fn data_time(ev: &Event, key: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(ev.data[key].as_str()?).ok()
}

impl Filter {
    pub fn matches(&self, ev: &Event, tz: FixedOffset) -> bool {
        let data = &ev.data;
        let text = |k: &str| data[k].as_str();

        for (re, key) in [(&self.message, "message"), (&self.title, "title"), (&self.hash, "hash")] {
            if let Some(re) = re {
                if !text(key).is_some_and(|m| re.is_match(m)) {
                    return false;
                }
            }
        }
        if let Some(g) = &self.branch {
            let ok = matches!(ev.kind, EventKind::Commit | EventKind::Push) && text("branch").is_some_and(|b| g.is_match(b));
            if !ok {
                return false;
            }
        }
        if let Some(g) = &self.repo {
            if !ev.repo.as_deref().is_some_and(|r| g.is_match(r)) {
                return false;
            }
        }
        let wants_files = self.files_any.is_some()
            || self.files_only.is_some()
            || self.min_files.is_some()
            || self.max_files.is_some()
            || self.min_extensions.is_some();
        if wants_files {
            if !data["files"].is_array() {
                return false;
            }
            let paths = file_paths(ev);
            if let Some(g) = &self.files_any {
                if !paths.iter().any(|p| g.is_match(p)) {
                    return false;
                }
            }
            if let Some(g) = &self.files_only {
                if paths.is_empty() || !paths.iter().all(|p| g.is_match(p)) {
                    return false;
                }
            }
            let n = paths.len() as u64;
            if self.min_files.is_some_and(|m| n < m) || self.max_files.is_some_and(|m| n > m) {
                return false;
            }
            if let Some(m) = self.min_extensions {
                let exts: HashSet<String> = paths.iter().filter_map(|p| extension(p)).collect();
                if (exts.len() as u32) < m {
                    return false;
                }
            }
        }
        if let Some(l) = &self.lines {
            let (a_key, r_key) = match ev.kind {
                EventKind::Commit => ("lines_added", "lines_removed"),
                EventKind::PrOpened | EventKind::PrMerged => ("additions", "deletions"),
                _ => return false,
            };
            let (Some(added), Some(removed)) = (data[a_key].as_u64(), data[r_key].as_u64()) else {
                return false;
            };
            let total = added + removed;
            let ok = l.min.is_none_or(|m| total >= m)
                && l.max.is_none_or(|m| total <= m)
                && l.min_delete_ratio.is_none_or(|m| total > 0 && removed as f64 / total as f64 >= m);
            if !ok {
                return false;
            }
        }
        if let Some(n) = self.hash_palindrome {
            let ok = n > 0 && text("hash").and_then(|h| h.get(..n)).is_some_and(|p| p.chars().eq(p.chars().rev()));
            if !ok {
                return false;
            }
        }
        if let Some(r) = &self.message_length {
            if !text("message").is_some_and(|m| in_range(r, m.chars().count() as f64)) {
                return false;
            }
        }
        if let Some(r) = &self.session_minutes {
            if !data["session_minutes"].as_f64().is_some_and(|v| in_range(r, v)) {
                return false;
            }
        }
        if let Some(r) = &self.chain {
            if !data["chain"].as_f64().is_some_and(|v| in_range(r, v)) {
                return false;
            }
        }
        if let Some(g) = &self.quest {
            if !text("quest_id").is_some_and(|q| g.is_match(q)) {
                return false;
            }
        }
        if let Some(min) = self.open_sessions {
            if !data["open_sessions"].as_u64().is_some_and(|v| v >= min) {
                return false;
            }
        }
        if let Some(r) = &self.pr_open_minutes {
            let ok = match (data_time(ev, "created_at"), data_time(ev, "merged_at")) {
                (Some(c), Some(m)) => in_range(r, (m - c).num_seconds() as f64 / 60.0),
                _ => false,
            };
            if !ok {
                return false;
            }
        }

        let local = ev.ts.with_timezone(&tz);
        if let Some(w) = &self.weekdays {
            if !w.contains(&local.weekday()) {
                return false;
            }
        }
        if let Some((start, end)) = self.hours {
            let h = local.hour();
            let inside = if start <= end { h >= start && h < end } else { h >= start || h < end };
            if !inside {
                return false;
            }
        }
        if let Some(t) = self.time {
            if (local.hour(), local.minute()) != t {
                return false;
            }
        }
        if let Some(d) = &self.dates {
            if !d.contains(&(local.month(), local.day())) {
                return false;
            }
        }
        true
    }

    /// Reject filters that can never match one of the rule's trigger kinds.
    pub fn kinds_ok(&self, kinds: &[EventKind]) -> Result<(), String> {
        use EventKind::*;
        let commit_only = self.message.is_some()
            || self.files_any.is_some()
            || self.files_only.is_some()
            || self.min_files.is_some()
            || self.max_files.is_some()
            || self.min_extensions.is_some()
            || self.hash.is_some()
            || self.hash_palindrome.is_some()
            || self.message_length.is_some();
        let checks: [(&str, bool, &[EventKind]); 7] = [
            ("message/files/hash/message_length", commit_only, &[Commit]),
            ("title", self.title.is_some(), &[PrOpened, PrMerged]),
            ("pr_open_minutes", self.pr_open_minutes.is_some(), &[PrMerged]),
            ("session_minutes", self.session_minutes.is_some(), &[ClaudeSessionEnd]),
            ("open_sessions", self.open_sessions.is_some(), &[ClaudeSessionStart]),
            ("chain", self.chain.is_some(), &[QuestCompleted]),
            ("quest", self.quest.is_some(), &[QuestCompleted, QuestFailed]),
        ];
        for (name, used, ok) in checks {
            if used {
                if let Some(k) = kinds.iter().find(|k| !ok.contains(k)) {
                    return Err(format!("filter `{name}` does not apply to `{}` events", k.as_str()));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use chrono::{DateTime, FixedOffset, Utc};
    use serde_json::{json, Value};

    fn filt(toml_src: &str) -> Filter {
        let raw: RawFilter = toml::from_str(toml_src).unwrap();
        Filter::try_from(raw).unwrap()
    }

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn ev(kind: EventKind, ts: &str, data: Value) -> Event {
        let mut e = Event::new(kind, Source::Git, Some("octo/w".into()), data);
        e.ts = DateTime::parse_from_rfc3339(ts).unwrap().with_timezone(&Utc);
        e
    }

    fn commit(data: Value) -> Event {
        ev(EventKind::Commit, "2026-01-05T12:00:00Z", data)
    }

    #[test]
    fn message_regex_case_insensitive() {
        let f = filt(r"message = '\bwip\b'");
        assert!(f.matches(&commit(json!({"message": "WIP: stuff"})), utc()));
        assert!(!f.matches(&commit(json!({"message": "wiper"})), utc()));
    }

    #[test]
    fn files_only_needs_all() {
        let f = filt("files_only = ['*.md']");
        let files = |ps: &[&str]| commit(json!({"files": ps.iter().map(|p| json!({"path": p, "added": 1, "removed": 0})).collect::<Vec<_>>()}));
        assert!(f.matches(&files(&["README.md", "docs/a.md"]), utc()));
        assert!(!f.matches(&files(&["README.md", "a.rs"]), utc()));
        assert!(!f.matches(&files(&[]), utc()));
    }

    #[test]
    fn min_extensions_counts_distinct() {
        let files = |ps: &[&str]| commit(json!({"files": ps.iter().map(|p| json!({"path": p})).collect::<Vec<_>>()}));
        let e = files(&["a.rs", "b.rs", "c.ts", "d.md", "e.toml", "Makefile"]);
        assert!(filt("min_extensions = 4").matches(&e, utc()));
        assert!(!filt("min_extensions = 5").matches(&e, utc()));
    }

    #[test]
    fn hash_palindrome() {
        let f = filt("hash_palindrome = 6");
        assert!(f.matches(&commit(json!({"hash": "abccbaxyz"})), utc()));
        assert!(!f.matches(&commit(json!({"hash": "abcdefxyz"})), utc()));
    }

    #[test]
    fn time_exact() {
        let f = filt("time = '13:37'");
        assert!(f.matches(&ev(EventKind::Commit, "2026-01-05T13:37:59Z", json!({})), utc()));
        assert!(!f.matches(&ev(EventKind::Commit, "2026-01-05T13:38:00Z", json!({})), utc()));
    }

    #[test]
    fn weekdays_local() {
        // 2026-01-04 is a Sunday; 04:30 UTC is Saturday 23:30 at UTC-5.
        let e = ev(EventKind::Commit, "2026-01-04T04:30:00Z", json!({}));
        let f = filt("weekdays = ['sat']");
        assert!(f.matches(&e, FixedOffset::west_opt(5 * 3600).unwrap()));
        assert!(!f.matches(&e, utc()));
    }

    #[test]
    fn dates_mm_dd() {
        let f = filt("dates = ['02-29']");
        assert!(f.matches(&ev(EventKind::Commit, "2028-02-29T10:00:00Z", json!({})), utc()));
        assert!(!f.matches(&ev(EventKind::Commit, "2028-03-01T10:00:00Z", json!({})), utc()));
    }

    #[test]
    fn pr_open_minutes_range() {
        let e = ev(
            EventKind::PrMerged,
            "2026-01-05T10:09:00Z",
            json!({"created_at": "2026-01-05T10:00:00Z", "merged_at": "2026-01-05T10:09:00Z"}),
        );
        assert!(filt("pr_open_minutes = {max = 10}").matches(&e, utc()));
        assert!(!filt("pr_open_minutes = {min = 10}").matches(&e, utc()));
    }

    #[test]
    fn missing_field_never_matches() {
        assert!(!filt("message = '.'").matches(&commit(json!({})), utc()));
        let e = ev(EventKind::ClaudeSessionEnd, "2026-01-05T10:00:00Z", json!({}));
        assert!(!filt("session_minutes = {min = 0}").matches(&e, utc()));
    }

    #[test]
    fn lines_branch_repo_and_length() {
        let e = commit(json!({"lines_added": 10, "lines_removed": 90, "branch": "feat/x", "message": "héllo"}));
        assert!(filt("lines = {min = 100, min_delete_ratio = 0.8}").matches(&e, utc()));
        assert!(!filt("lines = {max = 50}").matches(&e, utc()));
        assert!(filt("branch = ['feat/*']").matches(&e, utc()));
        assert!(!filt("branch = ['main']").matches(&e, utc()));
        assert!(filt("repo = ['octo/*']").matches(&e, utc()));
        assert!(filt("message_length = {min = 5, max = 5}").matches(&e, utc()));
    }

    #[test]
    fn files_conditions_need_files_field() {
        let none = commit(json!({}));
        for src in ["max_files = 5", "min_files = 0", "min_extensions = 0"] {
            assert!(!filt(src).matches(&none, utc()), "{src}");
        }
        assert!(filt("max_files = 5").matches(&commit(json!({"files": []})), utc()));
    }

    #[test]
    fn lines_needs_count_fields() {
        let f = filt("lines = {max = 100}");
        assert!(!f.matches(&commit(json!({})), utc()));
        assert!(!f.matches(&commit(json!({"lines_added": 3})), utc()));
        assert!(!f.matches(&ev(EventKind::PrOpened, "2026-01-05T12:00:00Z", json!({"lines_added": 3, "lines_removed": 1})), utc()));
        assert!(f.matches(&ev(EventKind::PrOpened, "2026-01-05T12:00:00Z", json!({"additions": 3, "deletions": 1})), utc()));
    }

    #[test]
    fn hours_wrap_and_open_sessions() {
        let night = ev(EventKind::ClaudeSessionStart, "2026-01-05T23:00:00Z", json!({"open_sessions": 3}));
        assert!(filt("hours = {start = 22, end = 4}").matches(&night, utc()));
        assert!(filt("open_sessions = {min = 3}").matches(&night, utc()));
        assert!(!filt("open_sessions = {min = 4}").matches(&night, utc()));
    }

    #[test]
    fn malformed_values_are_errors() {
        for src in [
            "message = '('",
            "time = '25:00'",
            "dates = ['13-40']",
            "weekdays = ['funday']",
            "hours = {start = 1, end = 25}",
            "files_any = ['[']",
        ] {
            let raw: RawFilter = toml::from_str(src).unwrap();
            assert!(Filter::try_from(raw).is_err(), "{src}");
        }
    }

    #[test]
    fn kinds_ok_rejects_message_on_review() {
        let f = filt("message = 'x'");
        assert!(f.kinds_ok(&[EventKind::Commit]).is_ok());
        assert!(f.kinds_ok(&[EventKind::Commit, EventKind::ReviewSubmitted]).is_err());
        assert!(filt("pr_open_minutes = {max = 5}").kinds_ok(&[EventKind::PrOpened]).is_err());
        assert!(filt("title = 'x'").kinds_ok(&[EventKind::PrOpened, EventKind::PrMerged]).is_ok());
        assert!(Filter::default().kinds_ok(&[EventKind::ReviewSubmitted]).is_ok());
    }

    #[test]
    fn chain_and_quest_filters() {
        let f = filt("chain = { min = 5 }
quest = [\"ship_*\"]");
        let done = |chain, id| ev(EventKind::QuestCompleted, "2026-01-05T12:00:00Z", json!({"chain": chain, "quest_id": id}));
        assert!(f.matches(&done(5, "ship_it_speedrun"), utc()));
        assert!(!f.matches(&done(4, "ship_it_speedrun"), utc()));
        assert!(!f.matches(&done(9, "hot_streak"), utc()));
        assert!(filt("quest = [\"*\"]").kinds_ok(&[EventKind::QuestFailed]).is_ok());
        assert!(filt("chain = { min = 2 }").kinds_ok(&[EventKind::QuestFailed]).is_err());
        assert!(filt("chain = { min = 2 }").kinds_ok(&[EventKind::Commit]).is_err());
    }
}
