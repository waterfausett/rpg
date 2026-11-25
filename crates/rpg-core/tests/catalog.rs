//! One firing case and one near miss for every starter rule.

use chrono::{DateTime, Datelike, Duration, FixedOffset, Utc, Weekday};
use rpg_core::config::Config;
use rpg_core::engine;
use rpg_core::enrich::FileAge;
use rpg_core::event::{Event, EventKind, Source};
use rpg_core::rules::{self, Rules};
use rpg_core::store::Store;
use serde_json::{json, Value};
use std::path::Path;

/// Every starter rule id that has a test below. Add the id here with the test.
const COVERED: &[&str] = &[
    // v0
    "archaeologist",
    "demolitions_expert",
    "first_blood",
    "hat_trick",
    "leviathan",
    "new_territory",
    "night_shift",
    "peer_pressure",
    "personal_best",
    "surgical_strike",
    // clock, calendar, daily volume
    "weekend_warrior",
    "dawn_patrol",
    "leet",
    "make_a_wish",
    "leap_of_faith",
    "read_only_friday",
    "holiday_spirit",
    "new_year_same_bugs",
    "speed_run",
    "rubber_stamp",
    "codependent",
];

fn starter() -> Rules {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rules");
    let (rules, errors) = rules::load_dir(&dir);
    assert!(errors.is_empty(), "starter rules failed to load: {errors:?}");
    rules
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// An event in repo "r" at a fixed UTC time.
fn ev(kind: EventKind, ts: &str, data: Value) -> Event {
    let source = match kind {
        EventKind::Commit | EventKind::Push => Source::Git,
        EventKind::PrOpened | EventKind::PrMerged | EventKind::ReviewSubmitted => Source::Github,
        _ => Source::Claude,
    };
    let mut e = Event::new(kind, source, Some("r".into()), data);
    e.ts = at(ts);
    e
}

fn commit(ts: &str) -> Event {
    ev(EventKind::Commit, ts, json!({}))
}

/// Runs `history` and then `event` through the engine (UTC, in-memory store, default config)
/// and returns whether `event` itself unlocked `expect`. History may unlock things; only the
/// final event's unlocks are checked.
fn fires(rules: &Rules, history: &[Event], event: Event, expect: &str) -> bool {
    fires_with_ages(rules, history, event, &[], expect)
}

/// As `fires`, with the file ages the enrich step would have found for `event`.
fn fires_with_ages(rules: &Rules, history: &[Event], event: Event, ages: &[FileAge], expect: &str) -> bool {
    let store = Store::open_in_memory().unwrap();
    let cfg = Config::default();
    let tz = FixedOffset::east_opt(0).unwrap();
    for h in history {
        engine::process(&store, rules, &cfg, h, &[], tz).unwrap();
    }
    let out = engine::process(&store, rules, &cfg, &event, ages, tz).unwrap();
    out.unlocks.iter().any(|u| u.rule_id == expect)
}

/// `n` events of one kind on 2026-11-24 (a Tuesday), one minute apart from 09:00.
fn same_day(kind: EventKind, n: usize) -> Vec<Event> {
    (0..n).map(|i| ev(kind, &format!("2026-11-24T09:{i:02}:00Z"), json!({}))).collect()
}

#[test]
fn every_starter_rule_has_a_test() {
    let rules = starter();
    let ids: Vec<&str> = rules.list.iter().map(|r| r.id.as_str()).collect();
    let missing: Vec<&&str> = ids.iter().filter(|id| !COVERED.contains(id)).collect();
    assert!(missing.is_empty(), "starter rules without a test: {missing:?}");
    let stale: Vec<&&str> = COVERED.iter().filter(|id| !ids.contains(id)).collect();
    assert!(stale.is_empty(), "COVERED names ids that are not starter rules: {stale:?}");
}

#[test]
fn dates_used_below_have_the_expected_weekday() {
    let wd = |s: &str| at(s).weekday();
    assert_eq!(wd("2026-11-28T10:00:00Z"), Weekday::Sat);
    assert_eq!(wd("2026-11-29T10:00:00Z"), Weekday::Sun);
    assert_eq!(wd("2026-11-27T10:00:00Z"), Weekday::Fri);
    assert_eq!(wd("2026-11-26T10:00:00Z"), Weekday::Thu);
    assert_eq!(wd("2026-11-24T10:00:00Z"), Weekday::Tue);
}

// ---- v0 rules ----

#[test]
fn archaeologist() {
    let r = starter();
    let old = [FileAge { path: "a.rs".into(), years: 6.0, last_author: "x".into() }];
    let young = [FileAge { path: "a.rs".into(), years: 1.0, last_author: "x".into() }];
    assert!(fires_with_ages(&r, &[], commit("2026-11-24T10:00:00Z"), &old, "archaeologist"));
    assert!(!fires_with_ages(&r, &[], commit("2026-11-24T10:00:00Z"), &young, "archaeologist"));
}

#[test]
fn demolitions_expert() {
    let r = starter();
    let pr = |added: u64, removed: u64| {
        ev(EventKind::PrMerged, "2026-11-24T10:00:00Z", json!({"additions": added, "deletions": removed}))
    };
    assert!(fires(&r, &[], pr(10, 200), "demolitions_expert"));
    assert!(!fires(&r, &[], pr(200, 10), "demolitions_expert"));
}

#[test]
fn first_blood() {
    let r = starter();
    assert!(fires(&r, &[], commit("2026-11-24T10:00:00Z"), "first_blood"));
    assert!(!fires(&r, &[commit("2026-11-23T10:00:00Z")], commit("2026-11-24T10:00:00Z"), "first_blood"));
}

#[test]
fn hat_trick() {
    let r = starter();
    let pr = |ts: &str| ev(EventKind::PrOpened, ts, json!({}));
    let h = [pr("2026-11-24T09:00:00Z"), pr("2026-11-24T09:01:00Z")];
    assert!(fires(&r, &h, pr("2026-11-24T09:02:00Z"), "hat_trick"));
    assert!(!fires(&r, &h[..1], pr("2026-11-24T09:02:00Z"), "hat_trick"));
}

#[test]
fn leviathan() {
    let r = starter();
    let pr = |n: u64| ev(EventKind::PrOpened, "2026-11-24T10:00:00Z", json!({"additions": n, "deletions": 0}));
    assert!(fires(&r, &[], pr(1500), "leviathan"));
    assert!(!fires(&r, &[], pr(900), "leviathan"));
}

#[test]
fn new_territory() {
    let r = starter();
    let mut other = commit("2026-11-23T10:00:00Z");
    other.repo = Some("elsewhere".into());
    assert!(fires(&r, &[other], commit("2026-11-24T10:00:00Z"), "new_territory"));
    assert!(!fires(&r, &[commit("2026-11-23T10:00:00Z")], commit("2026-11-24T10:00:00Z"), "new_territory"));
}

#[test]
fn night_shift() {
    let r = starter();
    assert!(fires(&r, &[], commit("2026-11-24T02:30:00Z"), "night_shift"));
    assert!(!fires(&r, &[], commit("2026-11-24T04:00:00Z"), "night_shift"));
}

#[test]
fn peer_pressure() {
    let r = starter();
    let h = same_day(EventKind::ReviewSubmitted, 4);
    let next = ev(EventKind::ReviewSubmitted, "2026-11-24T10:00:00Z", json!({}));
    assert!(fires(&r, &h, next.clone(), "peer_pressure"));
    assert!(!fires(&r, &h[..3], next, "peer_pressure"));
}

#[test]
fn personal_best() {
    let r = starter();
    let pr = |ts: &str| ev(EventKind::PrMerged, ts, json!({}));
    // Day one merges one PR (seeds the record); day two merges two (beats it).
    let h = [pr("2026-11-23T10:00:00Z"), pr("2026-11-24T09:00:00Z")];
    assert!(fires(&r, &h, pr("2026-11-24T09:05:00Z"), "personal_best"));
    assert!(!fires(&r, &h[..1], pr("2026-11-24T09:05:00Z"), "personal_best"));
}

#[test]
fn surgical_strike() {
    let r = starter();
    let c = |n: u64| ev(EventKind::Commit, "2026-11-24T10:00:00Z", json!({"lines_added": n, "lines_removed": 0}));
    assert!(fires(&r, &[], c(2), "surgical_strike"));
    assert!(!fires(&r, &[], c(3), "surgical_strike"));
}

// ---- clock, calendar, daily volume ----

#[test]
fn weekend_warrior() {
    let r = starter();
    assert!(fires(&r, &[], commit("2026-11-28T10:00:00Z"), "weekend_warrior"));
    assert!(fires(&r, &[], commit("2026-11-29T10:00:00Z"), "weekend_warrior"));
    assert!(!fires(&r, &[], commit("2026-11-27T10:00:00Z"), "weekend_warrior"));
}

#[test]
fn dawn_patrol() {
    let r = starter();
    assert!(fires(&r, &[], commit("2026-11-24T05:00:00Z"), "dawn_patrol"));
    assert!(fires(&r, &[], commit("2026-11-24T06:59:00Z"), "dawn_patrol"));
    assert!(!fires(&r, &[], commit("2026-11-24T07:00:00Z"), "dawn_patrol"));
    assert!(!fires(&r, &[], commit("2026-11-24T04:59:00Z"), "dawn_patrol"));
}

#[test]
fn leet() {
    let r = starter();
    assert!(fires(&r, &[], commit("2026-11-24T13:37:00Z"), "leet"));
    assert!(!fires(&r, &[], commit("2026-11-24T13:38:00Z"), "leet"));
}

#[test]
fn make_a_wish() {
    let r = starter();
    assert!(fires(&r, &[], commit("2026-11-24T11:11:00Z"), "make_a_wish"));
    assert!(!fires(&r, &[], commit("2026-11-24T11:12:00Z"), "make_a_wish"));
}

#[test]
fn leap_of_faith() {
    let r = starter();
    assert!(fires(&r, &[], commit("2028-02-29T10:00:00Z"), "leap_of_faith"));
    assert!(!fires(&r, &[], commit("2028-02-28T10:00:00Z"), "leap_of_faith"));
}

#[test]
fn read_only_friday() {
    let r = starter();
    let push = |ts: &str, branch: &str| ev(EventKind::Push, ts, json!({"branch": branch}));
    assert!(fires(&r, &[], push("2026-11-27T16:00:00Z", "main"), "read_only_friday"));
    assert!(fires(&r, &[], push("2026-11-27T23:59:00Z", "master"), "read_only_friday"));
    assert!(!fires(&r, &[], push("2026-11-27T15:59:00Z", "main"), "read_only_friday"));
    assert!(!fires(&r, &[], push("2026-11-27T17:00:00Z", "feature/x"), "read_only_friday"));
    assert!(!fires(&r, &[], push("2026-11-26T17:00:00Z", "main"), "read_only_friday"));
}

#[test]
fn holiday_spirit() {
    let r = starter();
    assert!(fires(&r, &[], commit("2026-12-25T10:00:00Z"), "holiday_spirit"));
    assert!(fires(&r, &[], commit("2027-01-01T10:00:00Z"), "holiday_spirit"));
    assert!(!fires(&r, &[], commit("2026-12-24T10:00:00Z"), "holiday_spirit"));
}

#[test]
fn new_year_same_bugs() {
    let r = starter();
    // The first commit of 2027 fires even though 2026 had commits.
    assert!(fires(&r, &[commit("2026-12-30T10:00:00Z")], commit("2027-01-04T10:00:00Z"), "new_year_same_bugs"));
    // A second commit in the same year does not.
    assert!(!fires(&r, &[commit("2027-01-04T10:00:00Z")], commit("2027-01-05T10:00:00Z"), "new_year_same_bugs"));
}

#[test]
fn speed_run() {
    let r = starter();
    let h = same_day(EventKind::Commit, 9);
    let tenth = commit("2026-11-24T10:00:00Z");
    assert!(fires(&r, &h, tenth.clone(), "speed_run"));
    assert!(!fires(&r, &h[..8], tenth, "speed_run"));
}

#[test]
fn rubber_stamp() {
    let r = starter();
    let h = same_day(EventKind::ReviewSubmitted, 9);
    let tenth = ev(EventKind::ReviewSubmitted, "2026-11-24T10:00:00Z", json!({}));
    assert!(fires(&r, &h, tenth.clone(), "rubber_stamp"));
    assert!(!fires(&r, &h[..8], tenth, "rubber_stamp"));
}

#[test]
fn codependent() {
    let r = starter();
    // Session i starts at 08:00 + 10i minutes and ends five minutes later.
    let session = |i: i64| {
        let sid = format!("s{i}");
        let mut start = ev(EventKind::ClaudeSessionStart, "2026-11-24T08:00:00Z", json!({"session_id": sid}));
        start.ts += Duration::minutes(10 * i);
        let mut end = ev(EventKind::ClaudeSessionEnd, "2026-11-24T08:00:00Z", json!({"session_id": sid}));
        end.ts = start.ts + Duration::minutes(5);
        (start, end)
    };
    let mut history = Vec::new();
    for i in 0..9 {
        let (s, e) = session(i);
        history.push(s);
        history.push(e);
    }
    // The tenth session ending today fires.
    let (s9, e9) = session(9);
    let mut full = history.clone();
    full.push(s9);
    assert!(fires(&r, &full, e9, "codependent"));
    // With only eight earlier sessions, the same ending is the ninth and does not.
    let (s8, e8) = session(8);
    let mut short = history[..16].to_vec();
    short.push(s8);
    assert!(!fires(&r, &short, e8, "codependent"));
}
