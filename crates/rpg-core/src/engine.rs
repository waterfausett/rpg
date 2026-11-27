//! The engine: one event in, XP gains / unlocks / level-ups out.
use crate::config::Config;
use crate::counters::{evaluate, EvalCtx};
use crate::enrich::FileAge;
use crate::event::{Event, EventKind};
use crate::records;
use crate::rules::{Announce, Counter, Rule, Rules, Tier};
use crate::store::{NewUnlock, Store};
use crate::xp::level_for;
use anyhow::Result;
use chrono::{DateTime, Duration, FixedOffset, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct XpGain {
    pub amount: u64,
    pub label: String,
    /// `Some` for the exp an unlock paid: its tier and how it is announced. `None` for base event exp.
    pub achievement: Option<(Tier, Announce)>,
}

#[derive(Debug, Clone)]
pub struct Unlock {
    pub unlock_id: i64,
    /// The stored unlock key: the rule id, or `id.tier` for a ladder step.
    pub rule_id: String,
    /// The id of the rule that fired.
    pub base_rule_id: String,
    /// Stored and in history; a ladder step's includes the tier, e.g. "Committed (Silver)".
    pub name: String,
    /// `name` without the tier, for the banner and toast, which show the tier as color.
    pub title: String,
    pub tier: Tier,
    pub xp: u64,
    /// Banner or toast: the step's setting, else the rule's, else the config's for its tier.
    pub announce: Announce,
    pub findings: serde_json::Value,
    /// Includes this unlock.
    pub unlock_count: u32,
    pub last_unlocked_at: Option<DateTime<Utc>>,
}

/// Skill XP an event earned. Separate from character XP.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillGain {
    pub skill_id: String,
    pub name: String,
    pub amount: u64,
    /// The skill's level after this gain.
    pub level: u32,
}

/// A skill reaching a new level.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillLevelUp {
    pub skill_id: String,
    pub name: String,
    pub level: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub xp_gains: Vec<XpGain>,
    pub unlocks: Vec<Unlock>,
    pub level_ups: Vec<u32>,
    pub total_xp: u64,
    pub level: u32,
    pub skill_gains: Vec<SkillGain>,
    pub skill_level_ups: Vec<SkillLevelUp>,
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

/// The ledger reason for an event kind's base XP. Any other reason on an event's
/// ledger entries is the name of an achievement it unlocked.
pub fn base_label(kind: EventKind) -> Option<&'static str> {
    match kind {
        EventKind::Commit => Some("commit"),
        EventKind::MergeConflictResolved => Some("merge conflict"),
        EventKind::Push => Some("push"),
        EventKind::PrOpened => Some("PR opened"),
        EventKind::PrMerged => Some("PR merged"),
        EventKind::ReviewSubmitted => Some("review"),
        EventKind::ClaudeSessionStart => None,
        EventKind::ClaudeSessionEnd => Some("Claude session"),
    }
}

fn base_xp(store: &Store, cfg: &Config, ev: &Event) -> Result<Option<(u64, &'static str)>> {
    let amount = match ev.kind {
        EventKind::Commit => cfg.xp.commit,
        // The concluding commit already earned its XP; only the achievements pay out.
        EventKind::MergeConflictResolved => 0,
        EventKind::Push => cfg.xp.push,
        EventKind::PrOpened => cfg.xp.pr_opened,
        EventKind::PrMerged => cfg.xp.pr_merged,
        EventKind::ReviewSubmitted => cfg.xp.review,
        EventKind::ClaudeSessionStart => return Ok(None),
        EventKind::ClaudeSessionEnd => {
            let has_start = match ev.data["session_id"].as_str() {
                Some(id) => store.find_session_start(id)?.is_some(),
                None => false,
            };
            if !has_start {
                return Ok(None);
            }
            cfg.xp.claude_session
        }
    };
    Ok(base_label(ev.kind).map(|label| (amount, label)))
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

    // Excluded repos (`[paths]` in config) are dropped entirely: not stored, so they feed no
    // XP, records, skills or achievements, and don't show on the character sheet.
    if !cfg.paths.allows(ev.repo.as_deref()) {
        return Ok(out);
    }
    // Derived fields (session length, concurrency) are persisted with the event so later
    // history scans and this event's own rules can filter on them.
    let mut derived = ev.clone();
    crate::derive::derive(store, &mut derived)?;
    let ev = &derived;
    if !store.insert_event(ev)? {
        return Ok(out);
    }
    let broken = records::update(store, ev, tz)?;
    if ev.backfill {
        return Ok(out);
    }

    // Skills only count events that count toward the character (not session starts or
    // session ends with no start).
    if let Some((amount, label)) = base_xp(store, cfg, ev)? {
        if amount > 0 {
            store.add_xp(&ev.id, amount, label, None, ev.ts)?;
            out.xp_gains.push(XpGain { amount, label: label.to_string(), achievement: None });
        }
        award_skills(store, rules, cfg, ev, &mut out)?;
    }

    // Meta rules run last so their counts include this event's unlocks.
    let is_meta = |r: &&Rule| matches!(r.counter, Counter::Meta { .. });
    let ordered = rules.for_kind(ev.kind).filter(|r| !is_meta(r)).chain(rules.for_kind(ev.kind).filter(is_meta));
    for rule in ordered {
        if !rule.filter.matches(ev, tz) {
            continue;
        }
        // Threshold rules unlock each step once (checked in `evaluate`); other single rules
        // keep repeatable/cooldown.
        if !rule.ladder && !rule.counter.is_threshold() {
            if !rule.repeatable && store.unlock_count(&rule.id)? > 0 {
                continue;
            }
            if let (Some(hours), Some(last)) = (rule.cooldown_hours, store.last_unlock_at(&rule.id)?) {
                if ev.ts < last + Duration::hours(hours as i64) {
                    continue;
                }
            }
        }
        let ctx = EvalCtx { ev, store, broken: &broken, file_ages, tz, skill_curve: cfg.skill_levels.curve() };
        for f in evaluate(rule, &ctx)? {
            let count = store.unlock_count(&f.unlock_key)?;
            let last = store.last_unlock_at(&f.unlock_key)?;
            // The first unlock of the achievement (for a ladder, of any of its steps) is news.
            let first = cfg.announce.first_banner && !store.rule_ever_unlocked(&rule.id)?;
            let unlock_id = store.insert_unlock(&NewUnlock {
                rule_id: f.unlock_key.clone(),
                name: f.name.clone(),
                tier: f.step.tier.as_str().to_string(),
                xp: f.step.xp,
                context: f.findings.clone(),
                ts: ev.ts,
            })?;
            let announce = match cfg.announce.all {
                Some(all) => all,
                None if first => Announce::Banner,
                None => f.step.announce.or(rule.announce).unwrap_or(cfg.announce.for_tier(f.step.tier)),
            };
            if f.step.xp > 0 {
                store.add_xp(&ev.id, f.step.xp, &f.name, Some(unlock_id), ev.ts)?;
                out.xp_gains.push(XpGain { amount: f.step.xp, label: f.title.clone(), achievement: Some((f.step.tier, announce)) });
            }
            out.unlocks.push(Unlock {
                unlock_id,
                rule_id: f.unlock_key,
                base_rule_id: rule.id.clone(),
                name: f.name,
                title: f.title,
                tier: f.step.tier,
                xp: f.step.xp,
                announce,
                findings: f.findings,
                unlock_count: count + 1,
                last_unlocked_at: last,
            });
        }
    }

    out.total_xp = store.total_xp()?;
    out.level = level_for(out.total_xp, &cfg.levels);
    out.level_ups = (level_for(before, &cfg.levels) + 1..=out.level).collect();
    Ok(out)
}

fn award_skills(store: &Store, rules: &Rules, cfg: &Config, ev: &Event, out: &mut Outcome) -> Result<()> {
    let curve = cfg.skill_levels.curve();
    for skill in rules.skills.iter().filter(|s| s.xp > 0 && s.matches(ev)) {
        let before = store.skill_xp(&skill.id)?;
        store.add_skill_xp(&ev.id, &skill.id, &skill.name, skill.xp, ev.ts)?;
        let (from, to) = (level_for(before, &curve), level_for(before + skill.xp, &curve));
        out.skill_gains.push(SkillGain { skill_id: skill.id.clone(), name: skill.name.clone(), amount: skill.xp, level: to });
        out.skill_level_ups.extend((from + 1..=to).map(|level| SkillLevelUp {
            skill_id: skill.id.clone(),
            name: skill.name.clone(),
            level,
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use crate::rules::{parse_rule, Announce, Rule};

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

    /// A bronze rule; `extra` holds the counter/filter (and any other) TOML lines.
    fn rule(id: &str, kind: EventKind, extra: &str, xp: u64) -> Rule {
        let kind = kind.as_str();
        parse_rule(&format!(
            r#"
id = "{id}"
name = "Name {id}"
tier = "bronze"
xp = {xp}
fallback = "f"
trigger = ["{kind}"]
{extra}
"#
        ))
        .unwrap()
    }

    fn run(store: &Store, rules: &Rules, ev: &Event) -> Outcome {
        process(store, rules, &Config::default(), ev, &[], tz()).unwrap()
    }

    #[test]
    fn excluded_repo_counts_for_nothing() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules {
            list: vec![rule("fb", EventKind::Commit, "counter = { type = \"first\", scope = \"ever\" }", 5)],
            ..Default::default()
        };
        let cfg: Config = toml::from_str("[paths]\nexclude = ['C:/Temp']\n").unwrap();
        let mut e = mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({}));
        e.repo = Some("C:/Temp/scratch".into());
        let o = process(&store, &rules, &cfg, &e, &[], tz()).unwrap();
        assert!(o.xp_gains.is_empty() && o.unlocks.is_empty());
        assert_eq!(store.total_xp().unwrap(), 0);
        // Not stored either, so it can still count if the filter is later removed.
        e.repo = Some("C:/work/app".into());
        let o = process(&store, &rules, &cfg, &e, &[], tz()).unwrap();
        assert_eq!(o.unlocks.len(), 1);
        assert_eq!(store.total_xp().unwrap(), 15);
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
            list: vec![rule("pb", EventKind::PrMerged, "counter = { type = \"personal_record\", metric = \"prs_merged_per_day\" }", 7)],
            ..Default::default()
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
        let rules = Rules { list: vec![rule("any", EventKind::Commit, "", 5)], ..Default::default() };
        let a = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({})));
        let b = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T11:00:00Z", serde_json::json!({})));
        assert_eq!(a.unlocks.len(), 1);
        assert!(b.unlocks.is_empty());
        assert_eq!(store.unlock_count("any").unwrap(), 1);
    }

    #[test]
    fn cooldown_respected() {
        let store = Store::open_in_memory().unwrap();
        let mut r = rule("rep", EventKind::Commit, "", 5);
        r.repeatable = true;
        r.cooldown_hours = Some(2);
        let rules = Rules { list: vec![r], ..Default::default() };
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
        assert_eq!(ups, vec![2], "level 2 at exactly 1000 exp");
        assert_eq!((last.total_xp, last.level), (1000, 2));
        assert_eq!(last.xp_gains[0], XpGain { amount: 100, label: "PR merged".into(), achievement: None });
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
        assert_eq!(o.xp_gains, vec![XpGain { amount: 15, label: "Claude session".into(), achievement: None }]);
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

    fn skills(texts: &[&str]) -> Rules {
        Rules { skills: texts.iter().map(|t| crate::skills::parse_skill(t).unwrap()).collect(), ..Default::default() }
    }

    fn commit_with(ts: &str, paths: &[&str]) -> Event {
        let files: Vec<_> = paths.iter().map(|p| serde_json::json!({ "path": p })).collect();
        mk(EventKind::Commit, ts, serde_json::json!({ "files": files }))
    }

    #[test]
    fn skills_earn_their_own_xp_and_level_up() {
        let store = Store::open_in_memory().unwrap();
        let rules = skills(&[
            "id = \"rust\"
name = \"Rust\"
globs = [\"*.rs\"]
xp = 60
",
            "id = \"docs\"
name = \"Scribe\"
globs = [\"*.md\"]
",
        ]);
        let a = run(&store, &rules, &commit_with("2026-01-01T10:00:00Z", &["src/a.rs", "src/b.rs"]));
        assert_eq!(a.skill_gains, vec![SkillGain { skill_id: "rust".into(), name: "Rust".into(), amount: 60, level: 1 }]);
        assert!(a.skill_level_ups.is_empty());
        assert_eq!(a.total_xp, 10, "skill xp does not add to character xp");

        // Skill level 2 at 100 skill xp (default skill curve).
        let b = run(&store, &rules, &commit_with("2026-01-01T11:00:00Z", &["x.rs", "README.md"]));
        assert_eq!(b.skill_gains.len(), 2);
        assert_eq!(b.skill_level_ups, vec![SkillLevelUp { skill_id: "rust".into(), name: "Rust".into(), level: 2 }]);
        assert_eq!(store.skill_xp("rust").unwrap(), 120);
        assert_eq!(store.skill_xp("docs").unwrap(), 10);
    }

    #[test]
    fn skills_skip_backfill_duplicates_and_unmatched_session_ends() {
        let store = Store::open_in_memory().unwrap();
        let rules = skills(&[
            "id = \"rust\"
name = \"Rust\"
globs = [\"*.rs\"]
",
            "id = \"claude\"
name = \"Claude\"
events = [\"claude_session_start\", \"claude_session_end\"]
",
        ]);
        let mut old = commit_with("2026-01-01T10:00:00Z", &["a.rs"]);
        old.backfill = true;
        assert!(run(&store, &rules, &old).skill_gains.is_empty());
        let e = commit_with("2026-01-01T11:00:00Z", &["a.rs"]);
        assert_eq!(run(&store, &rules, &e).skill_gains.len(), 1);
        assert!(run(&store, &rules, &e).skill_gains.is_empty(), "duplicate event");
        let start = mk(EventKind::ClaudeSessionStart, "2026-01-01T12:00:00Z", serde_json::json!({"session_id": "s"}));
        assert!(run(&store, &rules, &start).skill_gains.is_empty(), "session starts earn nothing");
        let orphan = mk(EventKind::ClaudeSessionEnd, "2026-01-01T13:00:00Z", serde_json::json!({"session_id": "x"}));
        assert!(run(&store, &rules, &orphan).skill_gains.is_empty());
        let end = mk(EventKind::ClaudeSessionEnd, "2026-01-01T14:00:00Z", serde_json::json!({"session_id": "s"}));
        assert_eq!(run(&store, &rules, &end).skill_gains.len(), 1);
    }

    #[test]
    fn count_rule_uses_per_day_counter() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules { list: vec![rule("c2", EventKind::Commit, "counter = { type = \"per_day\", at = 2 }", 0)], ..Default::default() };
        let a = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({})));
        let b = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T11:00:00Z", serde_json::json!({})));
        assert!(a.unlocks.is_empty());
        assert_eq!(b.unlocks.len(), 1);
    }

    #[test]
    fn filter_gates_rules() {
        let store = Store::open_in_memory().unwrap();
        let mut r = rule("night", EventKind::Commit, "filter = { hours = { start = 0, end = 4 } }", 5);
        r.repeatable = true;
        let rules = Rules { list: vec![r], ..Default::default() };
        assert!(run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({}))).unlocks.is_empty());
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T03:00:00Z", serde_json::json!({})));
        assert_eq!(o.unlocks.len(), 1);
        assert_eq!((o.unlocks[0].tier, o.unlocks[0].xp), (Tier::Bronze, 5));
    }

    /// `n` commits stored directly (no rule evaluation), one minute apart from `start`.
    fn store_commits(store: &Store, n: usize, start: DateTime<Utc>, data: serde_json::Value, backfill: bool) {
        for i in 0..n {
            let mut e = Event::new(EventKind::Commit, Source::Git, Some("r".into()), data.clone());
            e.ts = start + Duration::minutes(i as i64);
            e.backfill = backfill;
            store.insert_event(&e).unwrap();
        }
    }

    fn committed_ladder() -> Rules {
        let r = parse_rule(
            "id=\"committed\"
name=\"Committed\"
fallback=\"f\"
trigger=[\"commit\"]
counter={type=\"lifetime\"}
tiers=[{tier=\"bronze\",at=100,xp=5},{tier=\"silver\",at=1000,xp=50}]",
        )
        .unwrap();
        Rules { list: vec![r], ..Default::default() }
    }

    fn keys(o: &Outcome) -> Vec<&str> {
        o.unlocks.iter().map(|u| u.rule_id.as_str()).collect()
    }

    #[test]
    fn lifetime_counts_filtered_history_including_backfill() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules {
            list: vec![rule("forty_one", EventKind::Commit, "filter = { message = 'fix' }
at = 41
counter = { type = \"lifetime\" }", 9)],
            ..Default::default()
        };
        let fix = serde_json::json!({ "message": "fix bug" });
        store_commits(&store, 10, at("2026-01-01T00:00:00Z"), fix.clone(), true);
        store_commits(&store, 30, at("2026-01-02T00:00:00Z"), fix.clone(), false);
        // Non-matching commits don't count.
        store_commits(&store, 5, at("2026-01-03T00:00:00Z"), serde_json::json!({ "message": "wip" }), false);
        let wip = mk(EventKind::Commit, "2026-01-04T00:00:00Z", serde_json::json!({ "message": "wip" }));
        assert!(run(&store, &rules, &wip).unlocks.is_empty());
        let e = mk(EventKind::Commit, "2026-01-04T01:00:00Z", fix.clone());
        let o = run(&store, &rules, &e);
        assert_eq!(keys(&o), vec!["forty_one"]);
        assert_eq!(o.unlocks[0].findings, serde_json::json!({ "progress": 41, "at": 41 }));
        // Already unlocked: the next matching commit unlocks nothing.
        let e = mk(EventKind::Commit, "2026-01-04T02:00:00Z", fix);
        assert!(run(&store, &rules, &e).unlocks.is_empty());
    }

    #[test]
    fn threshold_ignores_repeatable_and_cooldown() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules {
            list: vec![rule("two", EventKind::Commit, "repeatable = true
cooldown_hours = 1
at = 2
counter = { type = \"lifetime\" }", 1)],
            ..Default::default()
        };
        let mut fired = vec![];
        for h in 0..5 {
            let o = run(&store, &rules, &mk(EventKind::Commit, &format!("2026-01-01T0{h}:00:00Z"), serde_json::json!({})));
            fired.push(o.unlocks.len());
        }
        assert_eq!(fired, vec![0, 1, 0, 0, 0]);
    }

    #[test]
    fn ladder_catches_up_on_next_event() {
        let store = Store::open_in_memory().unwrap();
        let rules = committed_ladder();
        store_commits(&store, 150, at("2026-01-01T00:00:00Z"), serde_json::json!({}), false);
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-02T00:00:00Z", serde_json::json!({})));
        assert_eq!(keys(&o), vec!["committed.bronze"]);
        let u = &o.unlocks[0];
        assert_eq!((u.base_rule_id.as_str(), u.name.as_str(), u.tier, u.xp), ("committed", "Committed (Bronze)", Tier::Bronze, 5));
        // The tier shows as color on the banner and toast, so their text leaves it out.
        assert_eq!(u.title, "Committed");
        assert!(o.xp_gains.iter().any(|g| g.achievement.is_some() && g.label == "Committed"), "{:?}", o.xp_gains);
        assert_eq!(u.findings, serde_json::json!({ "progress": 151, "at": 100 }));
    }

    #[test]
    fn disabled_rule_loads_but_never_fires() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules {
            list: vec![
                rule("off", EventKind::Commit, "enabled = false", 5),
                rule("m", EventKind::Commit, "enabled = false
at = 1
counter = { type = \"meta\", of = \"distinct_unlocks\" }", 5),
                rule("on", EventKind::Commit, "", 1),
            ],
            ..Default::default()
        };
        assert_eq!(rules.list.len(), 3, "a disabled rule stays in the list");
        assert!(!rules.list[0].enabled && rules.list[2].enabled);
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({})));
        assert_eq!(keys(&o), vec!["on"]);
    }

    #[test]
    fn reenabled_ladder_catches_up() {
        let store = Store::open_in_memory().unwrap();
        let mut rules = committed_ladder();
        rules.list[0].enabled = false;
        store_commits(&store, 150, at("2026-01-01T00:00:00Z"), serde_json::json!({}), false);
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-02T00:00:00Z", serde_json::json!({})));
        assert!(o.unlocks.is_empty());
        rules.list[0].enabled = true;
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-02T01:00:00Z", serde_json::json!({})));
        assert_eq!(keys(&o), vec!["committed.bronze"]);
    }

    #[test]
    fn meta_sees_same_event_unlocks() {
        let store = Store::open_in_memory().unwrap();
        // The meta rule is listed first; it still runs after the others.
        let meta = rule("hunter", EventKind::Commit, "at = 2
counter = { type = \"meta\", of = \"distinct_unlocks\" }", 3);
        let rules = Rules {
            list: vec![meta, rule("a", EventKind::Commit, "", 1), rule("b", EventKind::Commit, "", 1)],
            ..Default::default()
        };
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-01T10:00:00Z", serde_json::json!({})));
        assert_eq!(keys(&o), vec!["a", "b", "hunter"]);
        assert_eq!(o.unlocks[2].findings, serde_json::json!({ "progress": 2, "at": 2 }));
        assert_eq!(store.total_xp().unwrap(), 10 + 1 + 1 + 3);
    }

    #[test]
    fn skill_level_rule_reads_this_events_level_ups() {
        let store = Store::open_in_memory().unwrap();
        let mut rules = skills(&["id = \"rust\"
name = \"Rust\"
globs = [\"*.rs\"]
xp = 100
"]);
        rules.list.push(rule("sm", EventKind::Commit, "at = 2
counter = { type = \"skill_level\" }", 4));
        // 100 skill xp reaches level 2 (default skill curve).
        let o = run(&store, &rules, &commit_with("2026-01-01T10:00:00Z", &["a.rs"]));
        assert_eq!(keys(&o), vec!["sm.rust.bronze"]);
        assert_eq!((o.unlocks[0].base_rule_id.as_str(), o.unlocks[0].name.as_str()), ("sm", "Name sm: Rust (Bronze)"));
        assert!(run(&store, &rules, &commit_with("2026-01-01T11:00:00Z", &["b.rs"])).unlocks.is_empty());
    }

    #[test]
    fn announce_resolution_order() {
        let ladder = parse_rule(
            "id=\"lad\"
name=\"Lad\"
fallback=\"f\"
trigger=[\"commit\"]
announce=\"banner\"
counter={type=\"lifetime\"}
tiers=[{tier=\"bronze\",at=100,xp=5},{tier=\"silver\",at=1000,xp=50,announce=\"toast\"}]",
        )
        .unwrap();
        let plain = rule("plain", EventKind::Commit, "", 7);
        let rules = Rules { list: vec![ladder, plain], ..Default::default() };
        let go = |cfg: &Config| {
            let store = Store::open_in_memory().unwrap();
            store_commits(&store, 1200, at("2026-01-01T00:00:00Z"), serde_json::json!({}), false);
            let ev = mk(EventKind::Commit, "2026-01-02T00:00:00Z", serde_json::json!({}));
            let o = process(&store, &rules, cfg, &ev, &[], tz()).unwrap();
            let unlocks: Vec<(String, Announce)> = o.unlocks.iter().map(|u| (u.rule_id.clone(), u.announce)).collect();
            let gains: Vec<Option<(Tier, Announce)>> = o.xp_gains.iter().map(|g| g.achievement).collect();
            (unlocks, gains)
        };
        // Step over rule over config: the rule's banner beats the config, the silver step's toast beats the rule.
        // These are all first unlocks, so `first_banner` is off to see the usual order.
        let (unlocks, gains) = go(&toml::from_str("[announce]\nfirst_banner = false\n").unwrap());
        assert_eq!(
            unlocks,
            vec![
                ("lad.bronze".to_string(), Announce::Banner),
                ("lad.silver".to_string(), Announce::Toast),
                ("plain".to_string(), Announce::Toast),
            ]
        );
        assert_eq!(
            gains,
            vec![
                None,
                Some((Tier::Bronze, Announce::Banner)),
                Some((Tier::Silver, Announce::Toast)),
                Some((Tier::Bronze, Announce::Toast)),
            ]
        );
        // With neither set, the config decides by tier.
        let cfg: Config = toml::from_str("[announce]\nfirst_banner = false\nbronze = \"banner\"\n").unwrap();
        assert_eq!(go(&cfg).0[2], ("plain".to_string(), Announce::Banner));
    }

    #[test]
    fn announce_all_overrides_step_rule_and_tier() {
        let ladder = parse_rule(
            "id=\"lad\"
name=\"Lad\"
fallback=\"f\"
trigger=[\"commit\"]
announce=\"toast\"
counter={type=\"lifetime\"}
tiers=[{tier=\"bronze\",at=100,xp=5},{tier=\"silver\",at=1000,xp=50,announce=\"toast\"}]",
        )
        .unwrap();
        let gold = parse_rule("id=\"g\"\nname=\"G\"\ntier=\"gold\"\nfallback=\"f\"\ntrigger=[\"commit\"]\nannounce=\"banner\"").unwrap();
        let plat = parse_rule("id=\"p\"\nname=\"P\"\ntier=\"platinum\"\nfallback=\"f\"\ntrigger=[\"commit\"]").unwrap();
        let rules = Rules { list: vec![ladder, gold, plat], ..Default::default() };
        let go = |toml_text: &str| {
            let cfg: Config = toml::from_str(toml_text).unwrap();
            let store = Store::open_in_memory().unwrap();
            store_commits(&store, 1200, at("2026-01-01T00:00:00Z"), serde_json::json!({}), false);
            let ev = mk(EventKind::Commit, "2026-01-02T00:00:00Z", serde_json::json!({}));
            let o = process(&store, &rules, &cfg, &ev, &[], tz()).unwrap();
            let gains: Vec<Announce> = o.xp_gains.iter().filter_map(|g| g.achievement.map(|(_, a)| a)).collect();
            let unlocks: Vec<(String, Announce)> = o.unlocks.into_iter().map(|u| (u.rule_id, u.announce)).collect();
            (unlocks, gains)
        };
        let ids = ["lad.bronze", "lad.silver", "g", "p"];
        for want in [Announce::Banner, Announce::Toast] {
            let name = if want == Announce::Banner { "banner" } else { "toast" };
            let (unlocks, gains) = go(&format!("[announce]\nall = \"{name}\"\n"));
            assert_eq!(unlocks, ids.iter().map(|id| (id.to_string(), want)).collect::<Vec<_>>(), "all = {name}");
            assert_eq!(gains, vec![want; 4], "all = {name}");
        }
        // Unset: the step, rule and tier settings decide as usual (first unlocks aside).
        let (unlocks, _) = go("[announce]\nfirst_banner = false\n");
        let got: Vec<Announce> = unlocks.into_iter().map(|(_, a)| a).collect();
        assert_eq!(got, vec![Announce::Toast, Announce::Toast, Announce::Banner, Announce::Banner]);
    }

    #[test]
    fn first_unlock_is_a_banner_then_the_usual_setting() {
        let rules = Rules { list: vec![rule("poly", EventKind::Commit, "repeatable = true\nannounce = \"toast\"", 5)], ..Default::default() };
        let go = |toml_text: &str| {
            let cfg: Config = toml::from_str(toml_text).unwrap();
            let store = Store::open_in_memory().unwrap();
            ["2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z"]
                .iter()
                .map(|ts| {
                    let o = process(&store, &rules, &cfg, &mk(EventKind::Commit, ts, serde_json::json!({})), &[], tz()).unwrap();
                    let gain = o.xp_gains.iter().find_map(|g| g.achievement.map(|(_, a)| a));
                    assert_eq!(gain, Some(o.unlocks[0].announce), "the exp toast agrees with the unlock");
                    o.unlocks[0].announce
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(go(""), vec![Announce::Banner, Announce::Toast]);
        assert_eq!(go("[announce]\nfirst_banner = false\n"), vec![Announce::Toast, Announce::Toast]);
        // An explicit `all` wins over the first-time banner.
        assert_eq!(go("[announce]\nall = \"toast\"\n"), vec![Announce::Toast, Announce::Toast]);
    }

    #[test]
    fn only_a_ladders_first_step_gets_the_first_time_banner() {
        let ladder = parse_rule(
            "id=\"lad\"
name=\"Lad\"
fallback=\"f\"
trigger=[\"commit\"]
announce=\"toast\"
counter={type=\"lifetime\"}
tiers=[{tier=\"bronze\",at=100,xp=5},{tier=\"silver\",at=1000,xp=50},{tier=\"gold\",at=1202,xp=50}]",
        )
        .unwrap();
        let rules = Rules { list: vec![ladder], ..Default::default() };
        let store = Store::open_in_memory().unwrap();
        store_commits(&store, 1200, at("2026-01-01T00:00:00Z"), serde_json::json!({}), false);
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-02T00:00:00Z", serde_json::json!({})));
        let got: Vec<(String, Announce)> = o.unlocks.into_iter().map(|u| (u.rule_id, u.announce)).collect();
        assert_eq!(got, vec![("lad.bronze".to_string(), Announce::Banner), ("lad.silver".to_string(), Announce::Toast)]);
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-02T00:01:00Z", serde_json::json!({})));
        assert_eq!((o.unlocks[0].rule_id.as_str(), o.unlocks[0].announce), ("lad.gold", Announce::Toast));
    }

    #[test]
    fn one_event_unlocks_all_crossed_steps_once() {
        let store = Store::open_in_memory().unwrap();
        let rules = committed_ladder();
        store_commits(&store, 1200, at("2026-01-01T00:00:00Z"), serde_json::json!({}), false);
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-02T00:00:00Z", serde_json::json!({})));
        assert_eq!(keys(&o), vec!["committed.bronze", "committed.silver"]);
        assert_eq!(store.total_xp().unwrap(), 10 + 5 + 50);
        let o = run(&store, &rules, &mk(EventKind::Commit, "2026-01-02T00:01:00Z", serde_json::json!({})));
        assert!(o.unlocks.is_empty());
        assert_eq!(store.unlock_count("committed.bronze").unwrap(), 1);
        assert_eq!(store.unlock_count("committed.silver").unwrap(), 1);
    }
}
