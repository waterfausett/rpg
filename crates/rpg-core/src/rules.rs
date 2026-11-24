//! Achievement rules as data: TOML files parsed into typed rules.
use crate::event::EventKind;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Bronze,
    Silver,
    Gold,
}

impl Tier {
    pub fn default_xp(self) -> u64 {
        match self {
            Tier::Bronze => 250,
            Tier::Silver => 500,
            Tier::Gold => 1000,
        }
    }
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
pub enum Window {
    /// Local calendar day.
    Day,
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
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Detector {
    CountInWindow { count: u32, window: Window },
    PersonalRecord { metric: Metric },
    FileAge { min_years: f64 },
    /// Local time, `end_hour` exclusive.
    TimeOfDay { start_hour: u32, end_hour: u32 },
    DiffSize {
        #[serde(default)]
        min_lines: Option<u64>,
        #[serde(default)]
        max_lines: Option<u64>,
        #[serde(default)]
        min_delete_ratio: Option<f64>,
    },
    First { scope: FirstScope },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub tier: Tier,
    pub xp: u64,
    pub fallback: String,
    pub hint: Option<String>,
    pub trigger: Vec<EventKind>,
    pub detector: Detector,
    pub repeatable: bool,
    pub cooldown_hours: Option<u64>,
    pub flavor: Flavor,
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    name: String,
    tier: Tier,
    xp: Option<u64>,
    fallback: String,
    hint: Option<String>,
    trigger: Vec<EventKind>,
    detector: Detector,
    #[serde(default)]
    repeatable: bool,
    cooldown_hours: Option<u64>,
    #[serde(default)]
    flavor: Flavor,
}

pub fn parse_rule(text: &str) -> Result<Rule, toml::de::Error> {
    let r: RawRule = toml::from_str(text)?;
    Ok(Rule {
        xp: r.xp.unwrap_or_else(|| r.tier.default_xp()),
        id: r.id,
        name: r.name,
        tier: r.tier,
        fallback: r.fallback,
        hint: r.hint,
        trigger: r.trigger,
        detector: r.detector,
        repeatable: r.repeatable,
        cooldown_hours: r.cooldown_hours,
        flavor: r.flavor,
    })
}

#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub list: Vec<Rule>,
}

impl Rules {
    pub fn for_kind(&self, kind: EventKind) -> impl Iterator<Item = &Rule> {
        self.list.iter().filter(move |r| r.trigger.contains(&kind))
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
            .and_then(|t| parse_rule(&t).map_err(|e| e.to_string()));
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
    use crate::event::EventKind;
    use std::fs;

    const ARCH: &str = r#"
id = "archaeologist"
name = "Archaeologist"
tier = "bronze"
xp = 500
trigger = ["commit"]
detector = { type = "file_age", min_years = 5 }
repeatable = false
fallback = "You modified a file nobody has touched in over five years."
flavor = { type = "llm", prompt = "Rewrite: \"{fallback}\". Context: {context}" }
"#;

    #[test]
    fn parses_archaeologist_example() {
        let r = parse_rule(ARCH).unwrap();
        assert_eq!(r.id, "archaeologist");
        assert_eq!(r.tier, Tier::Bronze);
        assert_eq!(r.xp, 500);
        assert_eq!(r.trigger, vec![EventKind::Commit]);
        assert_eq!(r.detector, Detector::FileAge { min_years: 5.0 });
        assert!(matches!(r.flavor, Flavor::Llm { .. }));
    }

    #[test]
    fn flavor_defaults_static_and_xp_defaults_by_tier() {
        let mk = |tier: &str| format!(
            "id=\"x\"\nname=\"X\"\ntier=\"{tier}\"\ntrigger=[\"commit\"]\nfallback=\"f\"\ndetector={{type=\"first\",scope=\"ever\"}}\n"
        );
        let b = parse_rule(&mk("bronze")).unwrap();
        assert_eq!(b.flavor, Flavor::Static);
        assert_eq!(b.xp, 250);
        assert!(!b.repeatable);
        assert_eq!(parse_rule(&mk("silver")).unwrap().xp, 500);
        assert_eq!(parse_rule(&mk("gold")).unwrap().xp, 1000);
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
    fn starter_rules_all_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rules");
        let (rules, errors) = load_dir(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(rules.list.len(), 10);
        assert!(rules.list.iter().all(|r| !r.fallback.is_empty()));
        for r in rules.list.iter().filter(|r| matches!(r.flavor, Flavor::Llm { .. })) {
            if let Flavor::Llm { prompt } = &r.flavor {
                assert!(prompt.contains("{fallback}") && prompt.contains("{context}"), "{}", r.id);
            }
        }
    }
}
