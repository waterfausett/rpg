//! Achievement rules as data: TOML files parsed into typed rules. A rule's `filter` says
//! which events count and its `counter` what to do with them; `steps` are its tiers.
use crate::event::EventKind;
use crate::filters::{Filter, RawFilter};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Bronze,
    Silver,
    Gold,
    Platinum,
    Legendary,
    Celestial,
}

impl Tier {
    pub fn default_xp(self) -> u64 {
        match self {
            Tier::Bronze => 250,
            Tier::Silver => 500,
            Tier::Gold => 1000,
            Tier::Platinum => 2000,
            Tier::Legendary => 4000,
            Tier::Celestial => 8000,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Bronze => "bronze",
            Tier::Silver => "silver",
            Tier::Gold => "gold",
            Tier::Platinum => "platinum",
            Tier::Legendary => "legendary",
            Tier::Celestial => "celestial",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tier::Bronze => "Bronze",
            Tier::Silver => "Silver",
            Tier::Gold => "Gold",
            Tier::Platinum => "Platinum",
            Tier::Legendary => "Legendary",
            Tier::Celestial => "Celestial",
        }
    }

    /// 0 (bronze) ..= 5 (celestial).
    pub fn rank(self) -> u8 {
        match self {
            Tier::Bronze => 0,
            Tier::Silver => 1,
            Tier::Gold => 2,
            Tier::Platinum => 3,
            Tier::Legendary => 4,
            Tier::Celestial => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Announce {
    Banner,
    Toast,
}

/// One unlockable step: the whole achievement for a single rule, one tier of a ladder.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub tier: Tier,
    /// Threshold for threshold counters; `None` for moment counters.
    pub at: Option<u64>,
    pub xp: u64,
    pub announce: Option<Announce>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Flavor {
    #[default]
    Static,
    Llm { prompt: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    PrsMergedPerDay,
    CommitsPerDay,
    PrDeletions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstScope {
    Ever,
    Repo,
    /// The event's local calendar year.
    Year,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Ever,
    Repo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaOf {
    DistinctUnlocks,
    SameUnlock,
}

/// What a rule does with its matching events. Moment counters look at the current event;
/// threshold counters compare a running total with each step's `at`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Counter {
    #[default]
    Each,
    PerDay { at: u32 },
    Run { at: u32 },
    Combo { kinds: Vec<EventKind>, within_minutes: u32 },
    GapDays { min: u32, scope: Scope },
    First { scope: FirstScope },
    PersonalRecord { metric: Metric },
    FileAge { min_years: f64 },
    /// Fires on the event that completes a month with a matching event every day.
    MonthEveryDay,
    Lifetime {
        #[serde(default)]
        sum: Option<String>,
        #[serde(default)]
        distinct: Option<String>,
    },
    StreakDays,
    Meta { of: MetaOf },
    SkillLevel,
}

impl Counter {
    pub fn is_threshold(&self) -> bool {
        matches!(self, Counter::Lifetime { .. } | Counter::StreakDays | Counter::Meta { .. } | Counter::SkillLevel)
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub fallback: String,
    pub hint: Option<String>,
    pub trigger: Vec<EventKind>,
    pub filter: Filter,
    pub counter: Counter,
    /// At least one. A ladder's steps are in increasing `at` order.
    pub steps: Vec<Step>,
    /// Set with `tiers`: each step is its own unlock, `<id>.<tier>`.
    pub ladder: bool,
    pub repeatable: bool,
    pub cooldown_hours: Option<u64>,
    pub flavor: Flavor,
    pub announce: Option<Announce>,
    pub hidden: bool,
    pub share_text: bool,
    /// `false` keeps the rule listed but the engine never evaluates it.
    pub enabled: bool,
}

impl Rule {
    /// The highest step (the only one for a single rule).
    pub fn top(&self) -> &Step {
        self.steps.last().expect("a rule has at least one step")
    }

    /// The unlock id for `tier`: `id` for a single rule, `id.tier` for a ladder.
    pub fn step_id(&self, tier: Tier) -> String {
        if self.ladder {
            format!("{}.{}", self.id, tier.as_str())
        } else {
            self.id.clone()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    tier: Tier,
    at: Option<u64>,
    xp: Option<u64>,
    announce: Option<Announce>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    name: String,
    tier: Option<Tier>,
    tiers: Option<Vec<RawStep>>,
    at: Option<u64>,
    xp: Option<u64>,
    fallback: String,
    hint: Option<String>,
    trigger: Vec<EventKind>,
    filter: Option<RawFilter>,
    #[serde(default)]
    counter: Counter,
    #[serde(default)]
    repeatable: bool,
    cooldown_hours: Option<u64>,
    #[serde(default)]
    flavor: Flavor,
    announce: Option<Announce>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    share_text: bool,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

/// The rule's steps, checked against its counter.
fn steps(r: &mut RawRule) -> Result<Vec<Step>, String> {
    let threshold = r.counter.is_threshold();
    let steps = match (r.tier, r.tiers.take()) {
        (Some(_), Some(_)) => return Err("set `tier` or `tiers`, not both".into()),
        (None, None) => return Err("missing `tier` (or `tiers` for a ladder)".into()),
        (Some(tier), None) => {
            if threshold && r.at.is_none() {
                return Err("this counter needs `at` (or `tiers` with `at` on every step)".into());
            }
            if !threshold && r.at.is_some() {
                return Err("`at` only applies to threshold counters (lifetime, streak_days, meta, skill_level)".into());
            }
            vec![Step { tier, at: r.at, xp: r.xp.unwrap_or_else(|| tier.default_xp()), announce: None }]
        }
        (None, Some(tiers)) => {
            if !threshold {
                return Err("`tiers` only applies to threshold counters (lifetime, streak_days, meta, skill_level)".into());
            }
            for (field, set) in [
                ("at", r.at.is_some()),
                ("xp", r.xp.is_some()),
                ("repeatable", r.repeatable),
                ("cooldown_hours", r.cooldown_hours.is_some()),
            ] {
                if set {
                    return Err(format!("`{field}` is not allowed with `tiers` (set `at`/`xp` per step)"));
                }
            }
            if tiers.is_empty() {
                return Err("`tiers` is empty".into());
            }
            let mut out: Vec<Step> = Vec::new();
            for s in tiers {
                if out.iter().any(|o| o.tier == s.tier) {
                    return Err(format!("`tiers`: tier `{}` appears more than once", s.tier.as_str()));
                }
                let Some(at) = s.at else {
                    return Err(format!("`tiers`: step `{}` needs `at`", s.tier.as_str()));
                };
                if out.last().is_some_and(|prev| prev.at >= Some(at)) {
                    return Err(format!("`tiers`: `at` must be strictly increasing (step `{}`)", s.tier.as_str()));
                }
                out.push(Step { tier: s.tier, at: Some(at), xp: s.xp.unwrap_or_else(|| s.tier.default_xp()), announce: s.announce });
            }
            out
        }
    };
    Ok(steps)
}

fn check_counter(c: &Counter, trigger: &[EventKind]) -> Result<(), String> {
    if let Counter::Combo { kinds, .. } = c {
        match kinds.last() {
            None => return Err("`counter.kinds` is empty".into()),
            Some(k) if !trigger.contains(k) => {
                return Err(format!("`counter.kinds`: the last kind `{}` must be one of the rule's `trigger` kinds", k.as_str()))
            }
            Some(_) => {}
        }
    }
    if let Counter::Lifetime { sum, distinct } = c {
        if sum.is_some() && distinct.is_some() {
            return Err("counter `lifetime`: set `sum` or `distinct`, not both".into());
        }
        if let Some(d) = distinct.as_deref().filter(|d| *d != "repo") {
            return Err(format!("counter `lifetime`: bad `distinct` value `{d}` (only \"repo\")"));
        }
    }
    Ok(())
}

pub fn parse_rule(text: &str) -> Result<Rule, String> {
    let mut r: RawRule = toml::from_str(text).map_err(|e| e.to_string())?;
    // `.` separates ladder and skill steps in stored unlock ids, so ids can't contain it.
    if r.id.is_empty() || !r.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("bad `id` `{}`: use only letters, digits, `_` and `-`", r.id));
    }
    let steps = steps(&mut r)?;
    check_counter(&r.counter, &r.trigger)?;
    let filter = Filter::try_from(r.filter.unwrap_or_default()).map_err(|e| format!("`filter`: {e}"))?;
    filter.kinds_ok(&r.trigger).map_err(|e| format!("`filter`: {e}"))?;
    Ok(Rule {
        ladder: r.tier.is_none(),
        id: r.id,
        name: r.name,
        fallback: r.fallback,
        hint: r.hint,
        trigger: r.trigger,
        filter,
        counter: r.counter,
        steps,
        repeatable: r.repeatable,
        cooldown_hours: r.cooldown_hours,
        flavor: r.flavor,
        announce: r.announce,
        hidden: r.hidden,
        share_text: r.share_text,
        enabled: r.enabled,
    })
}

/// Everything loaded from the rules folder: achievements, plus skills from `rules/skills/`.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub list: Vec<Rule>,
    pub skills: Vec<crate::skills::Skill>,
    pub quests: Vec<crate::quests::QuestDef>,
}

impl Rules {
    pub fn for_kind(&self, kind: EventKind) -> impl Iterator<Item = &Rule> {
        self.list.iter().filter(move |r| r.enabled && r.trigger.contains(&kind))
    }
}

#[derive(Debug, Clone)]
pub struct RuleError {
    pub file: PathBuf,
    pub message: String,
}

/// Load every `*.toml` in `dir` (sorted by filename). Bad files are skipped and
/// reported; on duplicate ids the first wins.
pub fn load_dir(dir: &Path) -> (Rules, Vec<RuleError>) {
    let mut rules = Rules::default();
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect(),
        Err(e) => {
            errors.push(RuleError { file: dir.to_path_buf(), message: e.to_string() });
            return (rules, errors);
        }
    };
    files.sort();
    for file in files {
        let parsed = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| parse_rule(&t));
        match parsed {
            Ok(rule) if rules.list.iter().any(|r| r.id == rule.id) => errors.push(RuleError {
                message: format!("duplicate id `{}`", rule.id),
                file,
            }),
            Ok(rule) => rules.list.push(rule),
            Err(message) => errors.push(RuleError { file, message }),
        }
    }
    (rules, errors)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind, Source};
    use chrono::{DateTime, FixedOffset, Utc};
    use std::fs;

    const ARCH: &str = r#"
id = "archaeologist"
name = "Archaeologist"
tier = "bronze"
xp = 500
trigger = ["commit"]
counter = { type = "file_age", min_years = 5 }
repeatable = false
fallback = "You modified a file nobody has touched in over five years."
flavor = { type = "llm", prompt = "Rewrite: \"{fallback}\". Context: {context}" }
"#;

    const COMMITTED: &str = r#"
id = "committed"
name = "Committed"
trigger = ["commit"]
counter = { type = "lifetime" }
tiers = [
  { tier = "bronze", at = 10 },
  { tier = "silver", at = 100 },
  { tier = "gold", at = 500, xp = 1500 },
]
fallback = "f"
"#;

    fn base(extra: &str) -> String {
        format!("id=\"x\"\nname=\"X\"\ntrigger=[\"commit\"]\nfallback=\"f\"\n{extra}\n")
    }

    fn err(extra: &str) -> String {
        match parse_rule(&base(extra)) {
            Ok(_) => panic!("expected an error for:\n{extra}"),
            Err(e) => e,
        }
    }

    #[test]
    fn parses_archaeologist_example() {
        let r = parse_rule(ARCH).unwrap();
        assert_eq!(r.id, "archaeologist");
        assert_eq!(r.steps.len(), 1);
        assert!(!r.ladder);
        assert_eq!(r.top().tier, Tier::Bronze);
        assert_eq!(r.top().xp, 500);
        assert_eq!(r.top().at, None);
        assert_eq!(r.trigger, vec![EventKind::Commit]);
        assert_eq!(r.counter, Counter::FileAge { min_years: 5.0 });
        assert!(matches!(r.flavor, Flavor::Llm { .. }));
        assert!(!r.hidden && !r.share_text && r.announce.is_none());
    }

    #[test]
    fn defaults_static_flavor_each_counter_and_tier_xp() {
        let b = parse_rule(&base("tier=\"bronze\"")).unwrap();
        assert_eq!(b.flavor, Flavor::Static);
        assert_eq!(b.counter, Counter::Each);
        assert_eq!(b.top().xp, 250);
        assert!(!b.repeatable);
        assert_eq!(parse_rule(&base("tier=\"silver\"")).unwrap().top().xp, 500);
        assert_eq!(parse_rule(&base("tier=\"gold\"")).unwrap().top().xp, 1000);
    }

    #[test]
    fn platinum_default_xp_is_2000() {
        assert_eq!(Tier::Platinum.default_xp(), 2000);
        let p = parse_rule(&base("tier=\"platinum\"")).unwrap();
        assert_eq!((p.top().tier, p.top().xp), (Tier::Platinum, 2000));
        assert_eq!(Tier::Legendary.default_xp(), 4000);
        assert_eq!(Tier::Celestial.default_xp(), 8000);
        let l = parse_rule(&base("tier=\"legendary\"")).unwrap();
        let c = parse_rule(&base("tier=\"celestial\"")).unwrap();
        assert_eq!((l.top().tier, c.top().tier), (Tier::Legendary, Tier::Celestial));
        let all = [Tier::Bronze, Tier::Silver, Tier::Gold, Tier::Platinum, Tier::Legendary, Tier::Celestial];
        assert_eq!(all.map(Tier::as_str), ["bronze", "silver", "gold", "platinum", "legendary", "celestial"]);
        assert_eq!(all.map(Tier::label), ["Bronze", "Silver", "Gold", "Platinum", "Legendary", "Celestial"]);
        assert_eq!(all.map(Tier::rank), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn optional_fields_parse() {
        let r = parse_rule(&base(
            "tier=\"gold\"\nannounce=\"toast\"\nhidden=true\nshare_text=true\nfilter={message='typo'}\ncounter={type=\"per_day\",at=3}",
        ))
        .unwrap();
        assert_eq!(r.announce, Some(Announce::Toast));
        assert!(r.hidden && r.share_text);
        assert_eq!(r.counter, Counter::PerDay { at: 3 });
    }

    #[test]
    fn ladder_parses() {
        let r = parse_rule(COMMITTED).unwrap();
        assert!(r.ladder);
        assert_eq!(r.steps.len(), 3);
        assert_eq!(r.counter, Counter::Lifetime { sum: None, distinct: None });
        let got: Vec<_> = r.steps.iter().map(|s| (s.tier, s.at, s.xp)).collect();
        assert_eq!(
            got,
            vec![(Tier::Bronze, Some(10), 250), (Tier::Silver, Some(100), 500), (Tier::Gold, Some(500), 1500)]
        );
        assert_eq!(r.top().tier, Tier::Gold);
        assert!(r.counter.is_threshold());
    }

    #[test]
    fn ladder_step_announce_parses() {
        let r = parse_rule(&base(
            "counter={type=\"lifetime\"}\nannounce=\"toast\"\ntiers=[{tier=\"bronze\",at=1},{tier=\"gold\",at=9,announce=\"banner\"}]",
        ))
        .unwrap();
        assert_eq!(r.announce, Some(Announce::Toast));
        assert_eq!(r.steps[0].announce, None);
        assert_eq!(r.steps[1].announce, Some(Announce::Banner));
    }

    #[test]
    fn single_threshold_rule_takes_top_level_at() {
        let r = parse_rule(&base("tier=\"silver\"\ncounter={type=\"streak_days\"}\nat=7")).unwrap();
        assert!(!r.ladder);
        assert_eq!(r.steps.len(), 1);
        assert_eq!((r.top().tier, r.top().at), (Tier::Silver, Some(7)));
    }

    #[test]
    fn step_ids() {
        let r = parse_rule(COMMITTED).unwrap();
        assert_eq!(r.step_id(Tier::Silver), "committed.silver");
        let single = parse_rule(ARCH).unwrap();
        assert_eq!(single.step_id(Tier::Bronze), "archaeologist");
    }

    #[test]
    fn counter_kinds() {
        let threshold = [
            Counter::Lifetime { sum: None, distinct: None },
            Counter::StreakDays,
            Counter::Meta { of: MetaOf::DistinctUnlocks },
            Counter::SkillLevel,
        ];
        assert!(threshold.iter().all(Counter::is_threshold));
        let moment = [
            Counter::Each,
            Counter::PerDay { at: 1 },
            Counter::Run { at: 2 },
            Counter::Combo { kinds: vec![EventKind::Commit], within_minutes: 5 },
            Counter::GapDays { min: 30, scope: Scope::Repo },
            Counter::First { scope: FirstScope::Year },
            Counter::PersonalRecord { metric: Metric::CommitsPerDay },
            Counter::FileAge { min_years: 1.0 },
            Counter::MonthEveryDay,
        ];
        assert!(!moment.iter().any(Counter::is_threshold));
    }

    #[test]
    fn counters_parse_from_toml() {
        let c = |src: &str| parse_rule(&base(&format!("tier=\"bronze\"\ncounter={src}"))).unwrap().counter;
        assert_eq!(c("{type=\"run\",at=3}"), Counter::Run { at: 3 });
        assert_eq!(
            c("{type=\"combo\",kinds=[\"push\",\"commit\"],within_minutes=15}"),
            Counter::Combo { kinds: vec![EventKind::Push, EventKind::Commit], within_minutes: 15 }
        );
        assert_eq!(c("{type=\"gap_days\",min=30,scope=\"repo\"}"), Counter::GapDays { min: 30, scope: Scope::Repo });
        assert_eq!(c("{type=\"first\",scope=\"year\"}"), Counter::First { scope: FirstScope::Year });
        assert_eq!(c("{type=\"month_every_day\"}"), Counter::MonthEveryDay);
        let m = parse_rule(&base("tiers=[{tier=\"bronze\",at=10}]\ncounter={type=\"meta\",of=\"same_unlock\"}")).unwrap();
        assert_eq!(m.counter, Counter::Meta { of: MetaOf::SameUnlock });
        let l = parse_rule(&base("tier=\"gold\"\nat=1000\ncounter={type=\"lifetime\",sum=\"deletions\"}")).unwrap();
        assert_eq!(l.counter, Counter::Lifetime { sum: Some("deletions".into()), distinct: None });
        assert!(err("tier=\"bronze\"\ncounter={type=\"run\",at=3,within=2}").contains("within"));
        assert!(err("tier=\"bronze\"\ncounter={type=\"time_of_day\"}").contains("time_of_day"));
    }

    #[test]
    fn combo_kinds_must_end_in_a_trigger_kind() {
        let empty = err("tier=\"bronze\"
counter={type=\"combo\",kinds=[],within_minutes=5}");
        assert!(empty.contains("counter.kinds"), "{empty}");
        let wrong = err("tier=\"bronze\"
counter={type=\"combo\",kinds=[\"commit\",\"push\"],within_minutes=5}");
        assert!(wrong.contains("counter.kinds") && wrong.contains("push"), "{wrong}");
        assert!(parse_rule(&base("tier=\"bronze\"
counter={type=\"combo\",kinds=[\"push\",\"commit\"],within_minutes=5}")).is_ok());
    }

    #[test]
    fn exactly_one_of_tier_and_tiers() {
        assert!(err("").contains("tier"));
        let both = err("tier=\"bronze\"\ncounter={type=\"lifetime\"}\ntiers=[{tier=\"bronze\",at=1}]");
        assert!(both.contains("`tier`") && both.contains("`tiers`"), "{both}");
    }

    #[test]
    fn repeated_tier_rejected() {
        let e = err("counter={type=\"lifetime\"}\ntiers=[{tier=\"bronze\",at=1},{tier=\"bronze\",at=2}]");
        assert!(e.contains("tiers") && e.contains("bronze"), "{e}");
    }

    #[test]
    fn threshold_counter_needs_at() {
        assert!(err("tier=\"bronze\"\ncounter={type=\"lifetime\"}").contains("`at`"));
        let missing = err("counter={type=\"streak_days\"}\ntiers=[{tier=\"bronze\",at=1},{tier=\"silver\"}]");
        assert!(missing.contains("`at`"), "{missing}");
        let flat = err("counter={type=\"lifetime\"}\ntiers=[{tier=\"bronze\",at=5},{tier=\"silver\",at=5}]");
        assert!(flat.contains("increasing"), "{flat}");
        let down = err("counter={type=\"lifetime\"}\ntiers=[{tier=\"bronze\",at=5},{tier=\"silver\",at=2}]");
        assert!(down.contains("increasing"), "{down}");
    }

    #[test]
    fn moment_counter_rejects_tiers() {
        let e = err("tiers=[{tier=\"bronze\",at=1},{tier=\"silver\",at=2}]");
        assert!(e.contains("`tiers`"), "{e}");
        let at = err("tier=\"bronze\"\nat=3\ncounter={type=\"per_day\",at=3}");
        assert!(at.contains("`at`"), "{at}");
    }

    #[test]
    fn ladders_reject_repeatable_cooldown_and_top_level_xp() {
        let ladder = "counter={type=\"lifetime\"}\ntiers=[{tier=\"bronze\",at=1}]";
        assert!(err(&format!("{ladder}\nrepeatable=true")).contains("repeatable"));
        assert!(err(&format!("{ladder}\ncooldown_hours=2")).contains("cooldown_hours"));
        assert!(err(&format!("{ladder}\nxp=9")).contains("`xp`"));
        assert!(err(&format!("{ladder}\nat=9")).contains("`at`"));
    }

    #[test]
    fn lifetime_distinct_only_repo() {
        let e = err("tier=\"bronze\"\nat=3\ncounter={type=\"lifetime\",distinct=\"branch\"}");
        assert!(e.contains("distinct"), "{e}");
        let r = parse_rule(&base("tier=\"bronze\"\nat=3\ncounter={type=\"lifetime\",distinct=\"repo\"}")).unwrap();
        assert_eq!(r.counter, Counter::Lifetime { sum: None, distinct: Some("repo".into()) });
    }

    #[test]
    fn filter_must_fit_trigger() {
        let e = parse_rule(
            "id=\"x\"\nname=\"X\"\ntier=\"bronze\"\ntrigger=[\"review_submitted\"]\nfallback=\"f\"\nfilter={message='typo'}",
        )
        .unwrap_err();
        assert!(e.contains("filter") && e.contains("review_submitted"), "{e}");
    }

    #[test]
    fn detector_field_rejected() {
        let e = err("tier=\"bronze\"\ndetector={type=\"first\",scope=\"ever\"}");
        assert!(e.contains("detector"), "{e}");
    }

    #[test]
    fn bad_regex_is_a_rule_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.toml"), ARCH).unwrap();
        fs::write(d.path().join("b.toml"), base("tier=\"bronze\"\nfilter={message='(unclosed'}")).unwrap();
        let (rules, errors) = load_dir(d.path());
        assert_eq!(rules.list.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].file.ends_with("b.toml"));
        assert!(errors[0].message.contains("message"), "{}", errors[0].message);
    }

    #[test]
    fn bad_file_is_skipped_others_load() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.toml"), ARCH).unwrap();
        fs::write(d.path().join("b.toml"), "id = \"b\"\nname = \"B").unwrap();
        let (rules, errors) = load_dir(d.path());
        assert_eq!(rules.list.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].file.ends_with("b.toml"));
    }

    #[test]
    fn duplicate_ids_reported() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.toml"), ARCH).unwrap();
        fs::write(d.path().join("b.toml"), ARCH).unwrap();
        let (rules, errors) = load_dir(d.path());
        assert_eq!(rules.list.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("duplicate"));
    }

    #[test]
    fn for_kind_filters() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.toml"), ARCH).unwrap();
        let (rules, _) = load_dir(d.path());
        assert_eq!(rules.for_kind(EventKind::Commit).count(), 1);
        assert_eq!(rules.for_kind(EventKind::Push).count(), 0);
    }

    #[test]
    fn rule_id_without_dot_required() {
        let with_id = |id: &str| parse_rule(&format!("id=\"{id}\"\nname=\"X\"\ntier=\"bronze\"\ntrigger=[\"commit\"]\nfallback=\"f\""));
        for bad in ["a.b", "", "has space", "é"] {
            let e = with_id(bad).unwrap_err();
            assert!(e.contains("`id`"), "{bad}: {e}");
        }
        for ok in ["a", "night_shift", "x-2", "ABC9"] {
            assert!(with_id(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn docs_examples_parse() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/rules.md");
        let doc = fs::read_to_string(path).unwrap().replace("\r\n", "\n");
        let blocks: Vec<&str> = doc.split("```toml\n").skip(1).map(|b| b.split("```").next().unwrap()).collect();
        // The intro example plus one per counter.
        assert_eq!(blocks.len(), 14);
        for b in blocks {
            if let Err(e) = parse_rule(b) {
                panic!("{e}\n{b}");
            }
        }
    }

    fn commit_at(ts: &str) -> Event {
        let mut e = Event::new(EventKind::Commit, Source::Git, Some("r".into()), serde_json::json!({}));
        e.ts = DateTime::parse_from_rfc3339(ts).unwrap().with_timezone(&Utc);
        e
    }

    #[test]
    fn v0_starters_parse_in_new_format() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rules");
        let (rules, errors) = load_dir(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        // The catalog grows; the v0 starters are the ones checked here.
        const V0: [&str; 10] = [
            "archaeologist", "demolitions_expert", "first_blood", "hat_trick", "leviathan",
            "new_territory", "night_shift", "peer_pressure", "personal_best", "surgical_strike",
        ];
        assert!(V0.iter().all(|id| rules.list.iter().any(|r| r.id == *id)));
        assert!(rules
            .list
            .iter()
            .filter(|r| V0.contains(&r.id.as_str()))
            .all(|r| !r.fallback.is_empty() && !r.ladder && r.steps.len() == 1));
        for r in rules.list.iter().filter(|r| matches!(r.flavor, Flavor::Llm { .. })) {
            if let Flavor::Llm { prompt } = &r.flavor {
                assert!(prompt.contains("{fallback}") && prompt.contains("{context}"), "{}", r.id);
            }
        }
        let by_id = |id: &str| rules.list.iter().find(|r| r.id == id).unwrap();
        // night_shift: an `hours` filter (00:00..04:00, end exclusive) plus `each`.
        let night = by_id("night_shift");
        assert_eq!(night.counter, Counter::Each);
        let utc = FixedOffset::east_opt(0).unwrap();
        assert!(night.filter.matches(&commit_at("2026-03-01T03:59:00Z"), utc));
        assert!(!night.filter.matches(&commit_at("2026-03-01T04:00:00Z"), utc));
        assert_eq!(by_id("hat_trick").counter, Counter::PerDay { at: 3 });
        assert_eq!(by_id("peer_pressure").counter, Counter::PerDay { at: 5 });
        assert_eq!(by_id("first_blood").counter, Counter::First { scope: FirstScope::Ever });
        assert_eq!(by_id("new_territory").counter, Counter::First { scope: FirstScope::Repo });
        assert_eq!(by_id("personal_best").counter, Counter::PersonalRecord { metric: Metric::PrsMergedPerDay });
        assert_eq!(by_id("archaeologist").counter, Counter::FileAge { min_years: 5.0 });
        assert_eq!(by_id("archaeologist").top().xp, 500);
        for id in ["demolitions_expert", "leviathan", "surgical_strike"] {
            assert_eq!(by_id(id).counter, Counter::Each, "{id}");
        }
    }
}
