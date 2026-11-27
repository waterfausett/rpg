//! One firing case and one near miss for every starter rule.

use chrono::{DateTime, Datelike, Duration, FixedOffset, Utc, Weekday};
use rpg_core::config::Config;
use rpg_core::engine;
use rpg_core::enrich::FileAge;
use rpg_core::event::{Event, EventKind, Source};
use rpg_core::rules::{self, Rules};
use rpg_core::store::{NewUnlock, Store};
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
    // ladders
    "committed",
    "merge_conflict_of_interest",
    "professional_critic",
    "lumberjack",
    "frequent_flyer",
    "claudes_favorite",
    "spellcheck_paladin",
    "ticket_machine",
    "test_believer",
    "on_a_roll",
    "achievement_hunter",
    "skill_master",
    // streaks, rhythm, Claude sessions
    "touch_grass_denied",
    "the_return",
    "necromancer",
    "full_combo",
    "lgtm_speedrun",
    "thousand_year_branch",
    "groundhog_day",
    "marathon",
    "speed_date",
    "pair_programming",
    // messages, files, branches, hashes
    "wip_wip_wip",
    "famous_last_words",
    "always_dns",
    "profanity_filter_disabled",
    "eloquent",
    "the_novelist",
    "emotional_support_emoji",
    "polyglot",
    "tiny_violence",
    "find_and_replace_all",
    "documentation_gremlin",
    "lawyered",
    "dependency_hell",
    "naming_things_is_hard",
    "self_inflicted",
    "palindrome",
    "hash_brown",
    "lucky_sevens",
    // quests
    "quest_givers_pet",
    "chain_gang",
    "fumble",
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

/// As `fires`, but `history` is stored directly (no rule evaluation) and `unlocks` are
/// pre-existing unlocks with those rule ids. Threshold counters count stored events, so this is
/// enough for large ladder histories.
fn fires_seeded(rules: &Rules, history: &[Event], unlocks: &[String], event: Event, expect: &str) -> bool {
    let store = Store::open_in_memory().unwrap();
    for h in history {
        store.insert_event(h).unwrap();
    }
    for id in unlocks {
        store
            .insert_unlock(&NewUnlock {
                rule_id: id.clone(),
                name: id.clone(),
                tier: "bronze".into(),
                xp: 0,
                context: json!({}),
                ts: at("2026-11-01T00:00:00Z"),
            })
            .unwrap();
    }
    let tz = FixedOffset::east_opt(0).unwrap();
    let out = engine::process(&store, rules, &Config::default(), &event, &[], tz).unwrap();
    out.unlocks.iter().any(|u| u.rule_id == expect)
}

/// `n` events built by `make(i)`, for ladder histories.
fn many(n: usize, make: impl Fn(usize) -> Event) -> Vec<Event> {
    (0..n).map(make).collect()
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

// ---- ladders ----

fn commit_data(ts: &str, data: Value) -> Event {
    ev(EventKind::Commit, ts, data)
}

#[test]
fn committed() {
    let r = starter();
    let h = many(99, |_| commit("2026-11-23T10:00:00Z"));
    assert!(fires_seeded(&r, &h, &[], commit("2026-11-24T10:00:00Z"), "committed.bronze"));
    assert!(!fires_seeded(&r, &h[..98], &[], commit("2026-11-24T10:00:00Z"), "committed.bronze"));
}

#[test]
fn merge_conflict_of_interest() {
    let r = starter();
    let h = many(9, |_| ev(EventKind::PrMerged, "2026-11-23T10:00:00Z", json!({})));
    let next = ev(EventKind::PrMerged, "2026-11-24T10:00:00Z", json!({}));
    assert!(fires_seeded(&r, &h, &[], next.clone(), "merge_conflict_of_interest.bronze"));
    assert!(!fires_seeded(&r, &h[..8], &[], next, "merge_conflict_of_interest.bronze"));
}

#[test]
fn professional_critic() {
    let r = starter();
    let h = many(24, |_| ev(EventKind::ReviewSubmitted, "2026-11-23T10:00:00Z", json!({})));
    let next = ev(EventKind::ReviewSubmitted, "2026-11-24T10:00:00Z", json!({}));
    assert!(fires_seeded(&r, &h, &[], next.clone(), "professional_critic.bronze"));
    assert!(!fires_seeded(&r, &h[..23], &[], next, "professional_critic.bronze"));
}

#[test]
fn lumberjack() {
    let r = starter();
    let removed = |n: u64| commit_data("2026-11-24T10:00:00Z", json!({"lines_removed": n}));
    assert!(fires_seeded(&r, &[removed(999)], &[], removed(1), "lumberjack.bronze"));
    assert!(!fires_seeded(&r, &[removed(998)], &[], removed(1), "lumberjack.bronze"));
}

#[test]
fn frequent_flyer() {
    let r = starter();
    let in_repo = |name: &str| {
        let mut e = commit("2026-11-23T10:00:00Z");
        e.repo = Some(name.into());
        e
    };
    let h: Vec<Event> = ["a", "b", "c", "d"].iter().map(|n| in_repo(n)).collect();
    // A fifth distinct repo reaches bronze; a repeat of a known one does not.
    assert!(fires_seeded(&r, &h, &[], in_repo("e"), "frequent_flyer.bronze"));
    assert!(!fires_seeded(&r, &h[..3], &[], in_repo("e"), "frequent_flyer.bronze"));
    assert!(!fires_seeded(&r, &h, &[], in_repo("a"), "frequent_flyer.bronze"));
    // Worktrees: two local paths of one GitHub repo count once.
    let worktree = |path: &str, gh: &str| {
        let mut e = commit_data("2026-11-23T10:00:00Z", json!({"github_repo": gh}));
        e.repo = Some(path.into());
        e
    };
    let mut wt = h[..3].to_vec();
    wt.push(worktree("C:/work/rpg", "me/rpg"));
    assert!(!fires_seeded(&r, &wt, &[], worktree("C:/work/rpg-wt/feature", "me/rpg"), "frequent_flyer.bronze"));
    assert!(fires_seeded(&r, &wt, &[], worktree("C:/work/other", "me/other"), "frequent_flyer.bronze"));
}

#[test]
fn claudes_favorite() {
    let r = starter();
    let end = |ts: &str| ev(EventKind::ClaudeSessionEnd, ts, json!({"session_minutes": 7}));
    let h = many(49, |_| end("2026-11-23T10:00:00Z"));
    assert!(fires_seeded(&r, &h, &[], end("2026-11-24T10:00:00Z"), "claudes_favorite.bronze"));
    assert!(!fires_seeded(&r, &h[..48], &[], end("2026-11-24T10:00:00Z"), "claudes_favorite.bronze"));
    // An end without a known start (no session_minutes) is not counted.
    let unknown = ev(EventKind::ClaudeSessionEnd, "2026-11-24T10:00:00Z", json!({}));
    assert!(!fires_seeded(&r, &h, &[], unknown, "claudes_favorite.bronze"));
}

#[test]
fn spellcheck_paladin() {
    let r = starter();
    let msg = |m: &str| commit_data("2026-11-24T10:00:00Z", json!({"message": m}));
    let h = many(4, |_| msg("fix typo in readme"));
    assert!(fires_seeded(&r, &h, &[], msg("Fix typos"), "spellcheck_paladin.bronze"));
    assert!(!fires_seeded(&r, &h[..3], &[], msg("Fix typos"), "spellcheck_paladin.bronze"));
    assert!(!fires_seeded(&r, &h, &[], msg("typography tweaks"), "spellcheck_paladin.bronze"));
}

#[test]
fn ticket_machine() {
    let r = starter();
    let msg = |m: &str| commit_data("2026-11-24T10:00:00Z", json!({"message": m}));
    let h = many(9, |_| msg("fix login (#123)"));
    assert!(fires_seeded(&r, &h, &[], msg("closes #7"), "ticket_machine.bronze"));
    assert!(!fires_seeded(&r, &h[..8], &[], msg("closes #7"), "ticket_machine.bronze"));
    assert!(!fires_seeded(&r, &h, &[], msg("no ticket here"), "ticket_machine.bronze"));
}

#[test]
fn test_believer() {
    let r = starter();
    let touching = |path: &str| {
        commit_data("2026-11-24T10:00:00Z", json!({"files": [{"path": path, "added": 1, "removed": 0}]}))
    };
    let h = many(9, |_| touching("tests/a.rs"));
    assert!(fires_seeded(&r, &h, &[], touching("src/foo_test.go"), "test_believer.bronze"));
    assert!(fires_seeded(&r, &h, &[], touching("web/app.spec.ts"), "test_believer.bronze"));
    assert!(!fires_seeded(&r, &h[..8], &[], touching("tests/b.rs"), "test_believer.bronze"));
    assert!(!fires_seeded(&r, &h, &[], touching("src/main.rs"), "test_believer.bronze"));
}

#[test]
fn on_a_roll() {
    let r = starter();
    let days = |n: usize| many(n, |i| commit(&format!("2026-11-{:02}T10:00:00Z", 23 - i)));
    // Four earlier days plus today is five in a row.
    assert!(fires_seeded(&r, &days(4), &[], commit("2026-11-24T10:00:00Z"), "on_a_roll.bronze"));
    assert!(!fires_seeded(&r, &days(3), &[], commit("2026-11-24T10:00:00Z"), "on_a_roll.bronze"));
    // A gap breaks the streak.
    let mut gap = days(4);
    gap[1].ts = at("2026-11-10T10:00:00Z");
    assert!(!fires_seeded(&r, &gap, &[], commit("2026-11-24T10:00:00Z"), "on_a_roll.bronze"));
}

#[test]
fn achievement_hunter() {
    let r = starter();
    let ids = |n: usize| -> Vec<String> { (0..n).map(|i| format!("seed_{i}")).collect() };
    // The commit is not the first and falls at a plain time, so it unlocks nothing else:
    // only the seeded unlock count moves the meta ladder.
    let earlier = [commit("2026-11-23T10:00:00Z")];
    let event = || commit("2026-11-24T10:00:00Z");
    assert!(fires_seeded(&r, &earlier, &ids(10), event(), "achievement_hunter.bronze"));
    assert!(!fires_seeded(&r, &earlier, &ids(9), event(), "achievement_hunter.bronze"));
}

#[test]
fn skill_master() {
    use rpg_core::xp::{level_for, threshold};
    let mut r = starter();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
    let (skills, errs) = rpg_core::skills::load_dir(&dir);
    assert!(errs.is_empty(), "{errs:?}");
    r.skills = skills;
    let rust_xp = r.skills.iter().find(|s| s.id == "rust").expect("rust skill").xp;
    let curve = Config::default().skill_levels.curve();
    // Commits touching *.rs needed for Rust level 5, computed from the curve.
    let need = threshold(5, &curve).div_ceil(rust_xp) as usize;
    assert_eq!(level_for(need as u64 * rust_xp, &curve), 5);
    assert!(level_for((need as u64 - 1) * rust_xp, &curve) < 5);
    let rs_commit = |i: usize| {
        commit_data(
            &format!("2026-11-24T09:{:02}:{:02}Z", i / 60 % 60, i % 60),
            json!({"files": [{"path": "src/lib.rs", "added": 1, "removed": 0}]}),
        )
    };
    let history: Vec<Event> = (0..need - 1).map(rs_commit).collect();
    assert!(fires(&r, &history, rs_commit(need), "skill_master.rust.bronze"));
    assert!(!fires(&r, &history[..need - 2], rs_commit(need), "skill_master.rust.bronze"));
}

/// The starter rules with the starter skills loaded.
fn starter_with_skills() -> Rules {
    let mut r = starter();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
    let (skills, errs) = rpg_core::skills::load_dir(&dir);
    assert!(errs.is_empty(), "{errs:?}");
    r.skills = skills;
    r
}

/// Skill XP needed for `level` on the default skill curve.
fn skill_xp_for(level: u32) -> u64 {
    rpg_core::xp::threshold(level, &Config::default().skill_levels.curve())
}

/// Every kind a starter skill earns on triggers Skill Master: Shipping levels on merged PRs.
#[test]
fn skill_master_triggers_on_every_skill_kind() {
    let r = starter_with_skills();
    let rule = r.list.iter().find(|x| x.id == "skill_master").unwrap();
    for skill in &r.skills {
        for kind in &skill.events {
            assert!(rule.trigger.contains(kind), "skill {} earns on {kind:?}", skill.id);
        }
    }
    let store = Store::open_in_memory().unwrap();
    let shipping = r.skills.iter().find(|s| s.id == "shipping").unwrap();
    store.add_skill_xp("seed", "shipping", "Shipping", skill_xp_for(5) - shipping.xp, at("2026-11-23T10:00:00Z")).unwrap();
    let tz = FixedOffset::east_opt(0).unwrap();
    let out = engine::process(&store, &r, &Config::default(), &ev(EventKind::PrMerged, T, json!({})), &[], tz).unwrap();
    assert!(out.unlocks.iter().any(|u| u.rule_id == "skill_master.shipping.bronze"), "{:?}", out.unlocks);
}

/// A skill already past a step (skill XP earned while the rule was off, say) unlocks it on the
/// next matching event, even one that levels no skill.
#[test]
fn skill_master_catches_up() {
    let r = starter_with_skills();
    let store = Store::open_in_memory().unwrap();
    let old = at("2026-11-01T10:00:00Z");
    store.add_skill_xp("seed", "rust", "Rust", skill_xp_for(11), old).unwrap();
    store.add_skill_xp("seed", "yaml", "YAML Wrangling", skill_xp_for(5) - 1, old).unwrap();
    let tz = FixedOffset::east_opt(0).unwrap();
    let cfg = Config::default();
    // A commit that earns no skill XP at all.
    let out = engine::process(&store, &r, &cfg, &commit(T), &[], tz).unwrap();
    assert!(out.skill_level_ups.is_empty());
    let keys: Vec<&str> = out.unlocks.iter().map(|u| u.rule_id.as_str()).collect();
    assert!(keys.contains(&"skill_master.rust.bronze") && keys.contains(&"skill_master.rust.silver"), "{keys:?}");
    assert!(!keys.iter().any(|k| k.starts_with("skill_master.yaml")), "yaml is one XP short: {keys:?}");
    // Already unlocked: the next event does not unlock them again.
    let out = engine::process(&store, &r, &cfg, &commit("2026-11-24T11:00:00Z"), &[], tz).unwrap();
    assert!(!out.unlocks.iter().any(|u| u.base_rule_id == "skill_master"), "{:?}", out.unlocks);
}

// ---- streaks, rhythm, Claude sessions ----

#[test]
fn touch_grass_denied() {
    let r = starter();
    // A commit on each of 1-29 November; the 30th is the last day and fires.
    let days = |n: usize| many(n, |i| commit(&format!("2026-11-{:02}T10:00:00Z", i + 1)));
    assert!(fires(&r, &days(29), commit("2026-11-30T10:00:00Z"), "touch_grass_denied"));
    // A missed day (the 15th) means the month is not full.
    let mut gap = days(29);
    gap.remove(14);
    assert!(!fires(&r, &gap, commit("2026-11-30T10:00:00Z"), "touch_grass_denied"));
}

#[test]
fn the_return() {
    let r = starter();
    assert!(fires(&r, &[commit("2026-11-10T10:00:00Z")], commit("2026-11-24T10:00:00Z"), "the_return"));
    assert!(!fires(&r, &[commit("2026-11-11T10:00:00Z")], commit("2026-11-24T10:00:00Z"), "the_return"));
    // The very first commit has nothing to return from.
    assert!(!fires(&r, &[], commit("2026-11-24T10:00:00Z"), "the_return"));
}

#[test]
fn necromancer() {
    let r = starter();
    // The same repo, 366 days earlier, fires; 364 days does not.
    assert!(fires(&r, &[commit("2025-11-23T10:00:00Z")], commit("2026-11-24T10:00:00Z"), "necromancer"));
    assert!(!fires(&r, &[commit("2025-11-25T10:00:00Z")], commit("2026-11-24T10:00:00Z"), "necromancer"));
    // A year-old commit in another repo does not count for this repo.
    let mut other = commit("2025-11-23T10:00:00Z");
    other.repo = Some("elsewhere".into());
    assert!(!fires(&r, &[other], commit("2026-11-24T10:00:00Z"), "necromancer"));
}

#[test]
fn full_combo() {
    let r = starter();
    let gh = json!({"github_repo": "a/b"});
    let c = |ts: &str| ev(EventKind::Commit, ts, gh.clone());
    let p = |ts: &str| ev(EventKind::Push, ts, gh.clone());
    let pr = |ts: &str, repo: &str| {
        let mut e = ev(EventKind::PrOpened, ts, json!({}));
        e.repo = Some(repo.into());
        e
    };
    let h = [c("2026-11-24T10:00:00Z"), p("2026-11-24T10:05:00Z")];
    assert!(fires(&r, &h, pr("2026-11-24T10:10:00Z", "a/b"), "full_combo"));
    // Outside the 15 minute window.
    assert!(!fires(&r, &h, pr("2026-11-24T10:20:00Z", "a/b"), "full_combo"));
    // A PR in another GitHub repo.
    assert!(!fires(&r, &h, pr("2026-11-24T10:10:00Z", "a/other"), "full_combo"));
    // Missing the push.
    assert!(!fires(&r, &h[..1], pr("2026-11-24T10:10:00Z", "a/b"), "full_combo"));
    // Twenty-four hour cooldown: a second combo the same day does not fire.
    let mut twice = h.to_vec();
    twice.extend([pr("2026-11-24T10:10:00Z", "a/b"), c("2026-11-24T15:00:00Z"), p("2026-11-24T15:05:00Z")]);
    assert!(!fires(&r, &twice, pr("2026-11-24T15:10:00Z", "a/b"), "full_combo"));
}

fn merged(created: &str, merged_at: &str) -> Event {
    ev(EventKind::PrMerged, merged_at, json!({"created_at": created, "merged_at": merged_at}))
}

#[test]
fn lgtm_speedrun() {
    let r = starter();
    assert!(fires(&r, &[], merged("2026-11-24T10:00:00Z", "2026-11-24T10:10:00Z"), "lgtm_speedrun"));
    assert!(!fires(&r, &[], merged("2026-11-24T10:00:00Z", "2026-11-24T10:11:00Z"), "lgtm_speedrun"));
}

#[test]
fn thousand_year_branch() {
    let r = starter();
    // 43200 minutes is 30 days.
    assert!(fires(&r, &[], merged("2026-10-25T10:00:00Z", "2026-11-24T10:00:00Z"), "thousand_year_branch"));
    assert!(!fires(&r, &[], merged("2026-10-25T10:01:00Z", "2026-11-24T10:00:00Z"), "thousand_year_branch"));
}

#[test]
fn groundhog_day() {
    let r = starter();
    // A non-first commit at a plain time unlocks nothing else, so only the seeds move the meta.
    let earlier = [commit("2026-11-23T10:00:00Z")];
    let event = || commit("2026-11-24T10:00:00Z");
    let seeds = |n: usize| -> Vec<String> { vec!["some_rule".to_string(); n] };
    assert!(fires_seeded(&r, &earlier, &seeds(10), event(), "groundhog_day"));
    assert!(!fires_seeded(&r, &earlier, &seeds(9), event(), "groundhog_day"));
}

/// A Claude session `minutes` long (seconds precision), started at `start`.
fn session(id: &str, start: &str, seconds: i64) -> (Event, Event) {
    let s = ev(EventKind::ClaudeSessionStart, start, json!({"session_id": id}));
    let mut e = ev(EventKind::ClaudeSessionEnd, start, json!({"session_id": id}));
    e.ts = s.ts + Duration::seconds(seconds);
    (s, e)
}

#[test]
fn marathon() {
    let r = starter();
    let (s, e) = session("a", "2026-11-24T08:00:00Z", 180 * 60);
    assert!(fires(&r, &[s], e, "marathon"));
    let (s, e) = session("a", "2026-11-24T08:00:00Z", 179 * 60);
    assert!(!fires(&r, &[s], e, "marathon"));
    // Twenty-four hour cooldown: a second marathon ending 23 hours after the first does not fire.
    let (s1, e1) = session("a", "2026-11-24T08:00:00Z", 180 * 60);
    let (s2, e2) = session("b", "2026-11-25T07:00:00Z", 180 * 60);
    assert!(!fires(&r, &[s1.clone(), e1.clone(), s2], e2, "marathon"));
    let (s3, e3) = session("c", "2026-11-25T08:00:00Z", 180 * 60);
    assert!(fires(&r, &[s1, e1, s3], e3, "marathon"));
}

#[test]
fn speed_date() {
    let r = starter();
    let (s, e) = session("a", "2026-11-24T08:00:00Z", 30);
    assert!(fires(&r, &[s], e, "speed_date"));
    let (s, e) = session("a", "2026-11-24T08:00:00Z", 31);
    assert!(!fires(&r, &[s], e, "speed_date"));
    // An end with no known start has no length and is not a speed date.
    let (_, e) = session("a", "2026-11-24T08:00:00Z", 30);
    assert!(!fires(&r, &[], e, "speed_date"));
}

#[test]
fn pair_programming() {
    let r = starter();
    let start = |id: &str, ts: &str| ev(EventKind::ClaudeSessionStart, ts, json!({"session_id": id}));
    let h = [start("a", "2026-11-24T08:00:00Z"), start("b", "2026-11-24T08:10:00Z")];
    assert!(fires(&r, &h, start("c", "2026-11-24T08:20:00Z"), "pair_programming"));
    assert!(!fires(&r, &h[..1], start("c", "2026-11-24T08:20:00Z"), "pair_programming"));
}

// ---- messages, files, branches, hashes ----

const T: &str = "2026-11-24T10:00:00Z";

fn msg(m: &str) -> Event {
    commit_data(T, json!({"message": m}))
}

fn with_files(paths: &[&str]) -> Event {
    let files: Vec<Value> = paths.iter().map(|p| json!({"path": p, "added": 1, "removed": 0})).collect();
    commit_data(T, json!({"files": files}))
}

fn on_branch(kind: EventKind, branch: &str) -> Event {
    ev(kind, T, json!({"branch": branch}))
}

fn with_hash(h: &str) -> Event {
    commit_data(T, json!({"hash": h}))
}

#[test]
fn wip_wip_wip() {
    let r = starter();
    // Runs are per repo: the last three commits matched and the one before them did not.
    let at_min = |m: u32, text: &str| commit_data(&format!("2026-11-24T10:{m:02}:00Z"), json!({"message": text}));
    let h = [at_min(0, "add parser"), at_min(1, "wip"), at_min(2, "WIP again")];
    assert!(fires(&r, &h, at_min(3, "wip: still going"), "wip_wip_wip"));
    // A run of two so far does not.
    assert!(!fires(&r, &h[..2], at_min(3, "wip"), "wip_wip_wip"));
    // "wiper" is not the word.
    assert!(!fires(&r, &h, at_min(3, "wiper blades"), "wip_wip_wip"));
}

#[test]
fn famous_last_words() {
    let r = starter();
    assert!(fires(&r, &[], msg("Final fix, should work now"), "famous_last_words"));
    assert!(fires(&r, &[], msg("fixed for real"), "famous_last_words"));
    assert!(fires(&r, &[], msg("final final version"), "famous_last_words"));
    assert!(!fires(&r, &[], msg("finalize the parser"), "famous_last_words"));
    // A bare "final" is ordinary vocabulary, not a promise.
    assert!(!fires(&r, &[], msg("make the field final"), "famous_last_words"));
    assert!(!fires(&r, &[], msg("final cleanup before release"), "famous_last_words"));
    // Seventy-two hour cooldown.
    let h = [msg("last fix")];
    assert!(!fires(&r, &h, commit_data("2026-11-24T12:00:00Z", json!({"message": "last fix"})), "famous_last_words"));
    assert!(fires(&r, &h, commit_data("2026-11-27T11:00:00Z", json!({"message": "last fix"})), "famous_last_words"));
}

#[test]
fn always_dns() {
    let r = starter();
    let pr = |t: &str| ev(EventKind::PrOpened, T, json!({"title": t}));
    assert!(fires(&r, &[], pr("Fix DNS resolution"), "always_dns"));
    assert!(!fires(&r, &[], pr("Fix dnsmasq config"), "always_dns"));
}

#[test]
fn profanity_filter_disabled() {
    let r = starter();
    assert!(fires(&r, &[], msg("well damn, that was the wrong branch"), "profanity_filter_disabled"));
    // A word that merely contains a listed one (the Scunthorpe problem) is clean.
    assert!(!fires(&r, &[], msg("fix the damnation of the build"), "profanity_filter_disabled"));
    assert!(!fires(&r, &[], msg("fix dependency hell"), "profanity_filter_disabled"));
}

#[test]
fn eloquent() {
    let r = starter();
    assert!(fires(&r, &[], msg("fix"), "eloquent"));
    assert!(fires(&r, &[], msg("  stuff \n"), "eloquent"));
    assert!(!fires(&r, &[], msg("fix stuff"), "eloquent"));
}

#[test]
fn the_novelist() {
    let r = starter();
    assert!(fires(&r, &[], msg(&"a".repeat(500)), "the_novelist"));
    assert!(!fires(&r, &[], msg(&"a".repeat(499)), "the_novelist"));
    // Length counts characters, not bytes: 499 two-byte chars stay under.
    assert!(!fires(&r, &[], msg(&"é".repeat(499)), "the_novelist"));
    // Twenty-four hour cooldown.
    let long = |ts: &str| commit_data(ts, json!({"message": "a".repeat(500)}));
    assert!(!fires(&r, &[long(T)], long("2026-11-25T09:59:00Z"), "the_novelist"));
    assert!(fires(&r, &[long(T)], long("2026-11-25T10:00:00Z"), "the_novelist"));
}

#[test]
fn emotional_support_emoji() {
    let r = starter();
    assert!(fires(&r, &[], msg("ship it \u{1F680}"), "emotional_support_emoji"));
    assert!(!fires(&r, &[], msg("ship it"), "emotional_support_emoji"));
}

#[test]
fn polyglot() {
    let r = starter();
    assert!(fires(&r, &[], with_files(&["a.rs", "b.ts", "c.py", "d.md"]), "polyglot"));
    assert!(!fires(&r, &[], with_files(&["a.rs", "b.ts", "c.py", "d.py"]), "polyglot"));
}

#[test]
fn tiny_violence() {
    let r = starter();
    let pr = |a: u64, d: u64| ev(EventKind::PrMerged, T, json!({"additions": a, "deletions": d}));
    assert!(fires(&r, &[], pr(2, 2), "tiny_violence"));
    assert!(fires(&r, &[], pr(1, 0), "tiny_violence"));
    assert!(!fires(&r, &[], pr(3, 2), "tiny_violence"));
    assert!(!fires(&r, &[], pr(0, 0), "tiny_violence"));
}

#[test]
fn find_and_replace_all() {
    let r = starter();
    let paths = |n: usize| (0..n).map(|i| format!("f{i}.rs")).collect::<Vec<_>>();
    let ev_n = |n: usize| {
        let p = paths(n);
        with_files(&p.iter().map(String::as_str).collect::<Vec<_>>())
    };
    assert!(fires(&r, &[], ev_n(100), "find_and_replace_all"));
    assert!(!fires(&r, &[], ev_n(99), "find_and_replace_all"));
}

#[test]
fn documentation_gremlin() {
    let r = starter();
    assert!(fires(&r, &[], with_files(&["README.md", "docs/guide.mdx"]), "documentation_gremlin"));
    assert!(!fires(&r, &[], with_files(&["README.md", "src/main.rs"]), "documentation_gremlin"));
}

#[test]
fn lawyered() {
    let r = starter();
    assert!(fires(&r, &[], with_files(&["src/a.rs", "LICENSE"]), "lawyered"));
    assert!(fires(&r, &[], with_files(&["vendor/x/COPYING.txt"]), "lawyered"));
    assert!(fires(&r, &[], with_files(&["LICENSE-MIT"]), "lawyered"));
    assert!(fires(&r, &[], with_files(&["third_party/lib/license.txt"]), "lawyered"));
    assert!(fires(&r, &[], with_files(&["COPYING"]), "lawyered"));
    assert!(!fires(&r, &[], with_files(&["src/relicense.rs"]), "lawyered"));
    // Source that merely starts with the word is not the license file.
    assert!(!fires(&r, &[], with_files(&["src/license_check.rs"]), "lawyered"));
    assert!(!fires(&r, &[], with_files(&["copyright_header.py"]), "lawyered"));
}

#[test]
fn dependency_hell() {
    let r = starter();
    assert!(fires(&r, &[], with_files(&["Cargo.lock", "package-lock.json"]), "dependency_hell"));
    assert!(!fires(&r, &[], with_files(&["Cargo.lock", "Cargo.toml"]), "dependency_hell"));
}

#[test]
fn naming_things_is_hard() {
    let r = starter();
    assert!(fires(&r, &[], on_branch(EventKind::Commit, "asdf"), "naming_things_is_hard"));
    assert!(!fires(&r, &[], on_branch(EventKind::Commit, "feature/tests"), "naming_things_is_hard"));
}

#[test]
fn self_inflicted() {
    let r = starter();
    assert!(fires(&r, &[], on_branch(EventKind::Push, "main"), "self_inflicted"));
    assert!(!fires(&r, &[], on_branch(EventKind::Push, "feature/x"), "self_inflicted"));
}

#[test]
fn palindrome() {
    let r = starter();
    assert!(fires(&r, &[], with_hash("abcdcba123"), "palindrome"));
    assert!(!fires(&r, &[], with_hash("abcdcbx123"), "palindrome"));
}

#[test]
fn hash_brown() {
    let r = starter();
    assert!(fires(&r, &[], with_hash("deadbeef01"), "hash_brown"));
    assert!(fires(&r, &[], with_hash("f00d123456"), "hash_brown"));
    assert!(!fires(&r, &[], with_hash("1deadbeef0"), "hash_brown"));
}

#[test]
fn lucky_sevens() {
    let r = starter();
    assert!(fires(&r, &[], with_hash("ab777c1234"), "lucky_sevens"));
    assert!(!fires(&r, &[], with_hash("ab77c71234"), "lucky_sevens"));
}

// ---- banner budget ----

/// One workday on `day` ("YYYY-MM-DD", UTC) in two repos. `special` adds the commits a plain
/// day lacks (a one-line change, a multi-language commit, a 500+ character message, a
/// famous-last-words message) and makes the long Claude session run past three hours.
fn workday(day: &str, special: bool) -> Vec<Event> {
    let t = |hm: &str| format!("{day}T{hm}:00Z");
    let mut out = Vec::new();
    let repos = [("C:/work/alpha", "me/alpha", "src/lib.rs"), ("C:/work/beta", "me/beta", "web/app.ts")];
    // 15 commits, 09:00 to 16:00 every half hour, alternating repos.
    for i in 0..15usize {
        let (repo, gh, file) = repos[i % 2];
        let mut files = vec![json!({"path": file, "added": 20, "removed": 5})];
        if i % 4 == 0 {
            files.push(json!({"path": "tests/it.rs", "added": 10, "removed": 0}));
        }
        let mut message = match i % 5 {
            0 => "Fix the parser edge case".to_string(),
            1 => "Refactor config loading (#42)".to_string(),
            2 => "Add retry to the poller\n\nThe GitHub API sometimes answers 502 under load, so the poller now retries\nwith backoff instead of dropping the whole cycle. Tests cover the retry path.".to_string(),
            3 => "Tidy imports".to_string(),
            _ => "Update docs for the new flag".to_string(),
        };
        if special {
            match i {
                3 => {
                    files = vec![json!({"path": file, "added": 1, "removed": 0})];
                    message = "Fix off-by-one".into();
                }
                5 => {
                    files = ["src/a.rs", "web/b.ts", "tools/c.py", "Cargo.toml"]
                        .iter()
                        .map(|p| json!({"path": p, "added": 5, "removed": 1}))
                        .collect();
                }
                7 => message = format!("Rework the event store\n\n{}", "Long explanation of the change. ".repeat(17)),
                9 => message = "Final fix for the flaky timeout test".into(),
                _ => {}
            }
        }
        let added: u64 = files.iter().map(|f| f["added"].as_u64().unwrap()).sum();
        let removed: u64 = files.iter().map(|f| f["removed"].as_u64().unwrap()).sum();
        let ts = format!("{day}T{:02}:{:02}:00Z", 9 + i / 2, (i % 2) * 30);
        let mut e = ev(
            EventKind::Commit,
            &ts,
            json!({
                "message": message,
                "hash": format!("a1b2c3d4e5{i:02}{}", day.replace('-', "")),
                "branch": format!("feature/{}", i % 3),
                "files": files,
                "lines_added": added,
                "lines_removed": removed,
                "github_repo": gh,
            }),
        );
        e.repo = Some(repo.into());
        out.push(e);
    }
    // Two pushes; the second follows the 14:30 commit in beta, and a beta PR follows within 15 minutes.
    for (hm, (repo, gh, _)) in [("10:20", repos[0]), ("14:35", repos[1])] {
        let mut e = ev(EventKind::Push, &t(hm), json!({"branch": "feature/1", "remote": "origin", "github_repo": gh}));
        e.repo = Some(repo.into());
        out.push(e);
    }
    let gh_ev = |kind: EventKind, hm: &str, repo: &str, data: Value| {
        let mut e = ev(kind, &t(hm), data);
        e.repo = Some(repo.into());
        e
    };
    let pr = |n: u32, title: &str| json!({"number": n, "title": title, "additions": 120, "deletions": 30, "created_at": t("09:00")});
    out.push(gh_ev(EventKind::PrOpened, "11:40", "me/alpha", pr(1, "Retry the poller")));
    out.push(gh_ev(EventKind::PrOpened, "14:45", "me/beta", pr(2, "Add the settings page")));
    let mut merged = pr(3, "Parser fixes");
    merged["merged_at"] = json!(t("15:30"));
    out.push(gh_ev(EventKind::PrMerged, "15:30", "me/alpha", merged));
    for hm in ["11:00", "13:00", "16:30"] {
        out.push(gh_ev(EventKind::ReviewSubmitted, hm, "other/lib", json!({"url": "https://github.com/other/lib/pull/9"})));
    }
    // 12 Claude sessions starting every 40 minutes from 09:05, 25 minutes each (so some
    // overlap), except the first, which runs long.
    for i in 0..12i64 {
        let sid = format!("{day}-s{i}");
        let mut start = ev(EventKind::ClaudeSessionStart, &t("09:05"), json!({"session_id": sid}));
        start.ts += Duration::minutes(40 * i);
        let minutes = match i {
            0 if special => 200,
            0 => 120,
            _ => 25,
        };
        let mut end = ev(EventKind::ClaudeSessionEnd, &t("09:05"), json!({"session_id": sid}));
        end.ts = start.ts + Duration::minutes(minutes);
        out.push(start);
        out.push(end);
    }
    out.sort_by_key(|e| e.ts);
    out
}

/// Silvers that a normal day can reach are announced with a toast, not a banner.
#[test]
fn routine_silvers_are_toasts() {
    use rpg_core::rules::Announce;
    let r = starter();
    for id in [
        "surgical_strike",
        "speed_run",
        "codependent",
        "pair_programming",
        "polyglot",
        "famous_last_words",
        "the_novelist",
        "marathon",
        "full_combo",
        "hat_trick",
    ] {
        let rule = r.list.iter().find(|x| x.id == id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(rule.announce, Some(Announce::Toast), "{id}");
    }
    for id in ["full_combo", "the_novelist", "marathon"] {
        assert_eq!(r.list.iter().find(|x| x.id == id).unwrap().cooldown_hours, Some(24), "{id}");
    }
}

/// The unlocks (and how each is announced) of a typical workday, after a week of workdays
/// (the first one busy, the rest plain) so first-ever unlocks, new-repo unlocks and the
/// specials' first-time banners are out of the way.
fn typical_day_unlocks(cfg: &Config) -> Vec<(String, rpg_core::rules::Announce)> {
    day_unlocks(cfg, true)
}

/// The unlocks of the busy workday, after a week of workdays (the first one busy when `seeded`).
fn day_unlocks(cfg: &Config, seeded: bool) -> Vec<(String, rpg_core::rules::Announce)> {
    let mut r = starter();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
    let (skills, errs) = rpg_core::skills::load_dir(&dir);
    assert!(errs.is_empty(), "{errs:?}");
    r.skills = skills;
    let store = Store::open_in_memory().unwrap();
    let tz = FixedOffset::east_opt(0).unwrap();
    for d in 17..=23 {
        for e in workday(&format!("2026-11-{d}"), seeded && d == 17) {
            engine::process(&store, &r, cfg, &e, &[], tz).unwrap();
        }
    }
    let mut unlocks = Vec::new();
    for e in workday("2026-11-24", true) {
        let out = engine::process(&store, &r, cfg, &e, &[], tz).unwrap();
        unlocks.extend(out.unlocks.into_iter().map(|u| (u.rule_id, u.announce)));
    }
    unlocks
}

/// The first time the specials unlock they are news: banners, though the catalog makes
/// them toasts. With `first_banner = false` they stay toasts.
#[test]
fn specials_first_unlock_is_a_banner() {
    use rpg_core::rules::Announce;
    let ids = ["surgical_strike", "polyglot", "the_novelist", "famous_last_words", "marathon"];
    let unlocks = day_unlocks(&Config::default(), false);
    for id in ids {
        assert!(unlocks.iter().any(|(u, a)| u == id && *a == Announce::Banner), "{id} as a banner: {unlocks:?}");
    }
    let cfg: Config = toml::from_str("[announce]\nfirst_banner = false\n").unwrap();
    let unlocks = day_unlocks(&cfg, false);
    for id in ids {
        assert!(unlocks.iter().any(|(u, a)| u == id && *a == Announce::Toast), "{id} as a toast: {unlocks:?}");
    }
}

/// `[announce] all` overrides the catalog's own settings: Polyglot (`announce = "toast"`)
/// becomes a banner, and with `all = "toast"` nothing is a banner.
#[test]
fn announce_all_overrides_the_catalog() {
    use rpg_core::rules::Announce;
    let cfg: Config = toml::from_str("[announce]\nall = \"banner\"\n").unwrap();
    let unlocks = typical_day_unlocks(&cfg);
    assert!(unlocks.iter().any(|(u, a)| u == "polyglot" && *a == Announce::Banner), "{unlocks:?}");
    assert!(unlocks.iter().all(|(_, a)| *a == Announce::Banner), "{unlocks:?}");
    let cfg: Config = toml::from_str("[announce]\nall = \"toast\"\n").unwrap();
    let unlocks = typical_day_unlocks(&cfg);
    assert!(!unlocks.is_empty());
    assert!(unlocks.iter().all(|(_, a)| *a == Announce::Toast), "{unlocks:?}");
}

/// The spec's anti-goal: a normal workday shows at most one or two banners. This budget
/// assumes `[announce] all` is unset; `all = "banner"` breaks it on purpose.
#[test]
fn typical_day_has_at_most_two_banners() {
    use rpg_core::rules::Announce;
    let unlocks = typical_day_unlocks(&Config::default());
    let banners: Vec<&String> = unlocks.iter().filter(|(_, a)| *a == Announce::Banner).map(|(id, _)| id).collect();
    assert!(banners.len() <= 2, "banners: {banners:?}\nall unlocks: {unlocks:?}");
    // The day is not empty: the specials still unlock, as toasts.
    for id in ["surgical_strike", "polyglot", "the_novelist", "famous_last_words", "marathon", "full_combo", "speed_run", "codependent"] {
        assert!(unlocks.iter().any(|(u, a)| u == id && *a == Announce::Toast), "{id} as a toast: {unlocks:?}");
    }
}

// quests

fn quest_done(ts: &str, chain: u64) -> Event {
    let mut e = ev(EventKind::QuestCompleted, ts, json!({"quest_id": "hot_streak", "tier": "bronze", "chain": chain, "multiplier": 1.0, "xp": 125}));
    e.source = Source::Quest;
    e
}

fn quest_lost(ts: &str) -> Event {
    let mut e = ev(EventKind::QuestFailed, ts, json!({"quest_id": "hot_streak", "tier": "bronze", "chain_lost": 1}));
    e.source = Source::Quest;
    e
}

#[test]
fn quest_givers_pet() {
    let r = starter();
    let h = many(9, |_| quest_done("2026-11-23T10:00:00Z", 1));
    let next = quest_done("2026-11-24T10:00:00Z", 1);
    assert!(fires_seeded(&r, &h, &[], next.clone(), "quest_givers_pet.bronze"));
    assert!(!fires_seeded(&r, &h[..8], &[], next, "quest_givers_pet.bronze"));
}

#[test]
fn chain_gang() {
    let r = starter();
    assert!(fires(&r, &[], quest_done("2026-11-24T10:00:00Z", 5), "chain_gang"));
    assert!(!fires(&r, &[], quest_done("2026-11-24T10:00:00Z", 4), "chain_gang"));
    // Once per run that reaches 5: the completions after it don't fire again.
    assert!(!fires(&r, &[], quest_done("2026-11-24T10:00:00Z", 6), "chain_gang"));
    let run = [quest_done("2026-11-24T09:00:00Z", 5)];
    assert!(!fires(&r, &run, quest_done("2026-11-24T10:00:00Z", 6), "chain_gang"));
}

#[test]
fn fumble() {
    let r = starter();
    let h = [quest_lost("2026-11-24T10:00:00Z"), quest_lost("2026-11-24T11:00:00Z")];
    assert!(fires(&r, &h, quest_lost("2026-11-24T12:00:00Z"), "fumble"));
    // Only two failures in the day: not yet.
    assert!(!fires(&r, &h[..1], quest_lost("2026-11-24T12:00:00Z"), "fumble"));
    // Three failures spread over two days don't count.
    let split = [quest_lost("2026-11-23T10:00:00Z"), quest_lost("2026-11-24T11:00:00Z")];
    assert!(!fires(&r, &split, quest_lost("2026-11-24T12:00:00Z"), "fumble"));
}
