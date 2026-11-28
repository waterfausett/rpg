//! Quest files as data: TOML files describing a timed challenge offered after a trigger event.
use crate::event::EventKind;
use crate::filters::{Filter, RawFilter};
use crate::rules::{Flavor, RuleError, Tier};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Same {
    Repo,
    Branch,
    Pr,
}

#[derive(Debug, Clone)]
pub struct QuestStep {
    pub kind: EventKind,
    pub filter: Filter,
    pub count: u32,
    pub label: String,
    /// The step's TOML table as written, kept for the snapshot stored with an accepted quest.
    pub source: toml::Value,
}

#[derive(Debug, Clone)]
pub struct QuestDef {
    pub id: String,
    pub name: String,
    pub tier: Tier,
    pub trigger: Vec<EventKind>,
    pub filter: Filter,
    pub chance: f64,
    pub cooldown_hours: u64,
    pub minutes: u32,
    pub same: Option<Same>,
    pub steps: Vec<QuestStep>,
    pub xp: u64,
    pub hint: Option<String>,
    pub fallback: String,
    pub flavor: Flavor,
    pub enabled: bool,
}

impl QuestDef {
    pub fn has_github_step(&self) -> bool {
        self.steps.iter().any(|s| is_pr_kind(s.kind))
    }
}

fn is_pr_kind(k: EventKind) -> bool {
    matches!(k, EventKind::PrOpened | EventKind::PrMerged | EventKind::ReviewSubmitted)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuestStep {
    kind: EventKind,
    filter: Option<RawFilter>,
    count: Option<u32>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuest {
    id: String,
    name: String,
    tier: Tier,
    trigger: Vec<EventKind>,
    filter: Option<RawFilter>,
    chance: f64,
    #[serde(default)]
    cooldown_hours: u64,
    minutes: u32,
    same: Option<Same>,
    steps: Vec<toml::Value>,
    xp: Option<u64>,
    hint: Option<String>,
    fallback: String,
    #[serde(default)]
    flavor: Flavor,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

fn default_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Commit => "Commit",
        EventKind::MergeConflictResolved => "Resolve a merge conflict",
        EventKind::Push => "Push",
        EventKind::PrOpened => "Open a PR",
        EventKind::PrMerged => "Merge a PR",
        EventKind::ReviewSubmitted => "Review a PR",
        EventKind::ClaudeSessionStart => "Start a Claude session",
        EventKind::ClaudeSessionEnd => "Finish a Claude session",
        EventKind::QuestCompleted => "Complete a quest",
        EventKind::QuestFailed => "Fail a quest",
    }
}

fn parse_step(n: usize, source: toml::Value) -> Result<QuestStep, String> {
    let at = |e: String| format!("`steps[{n}]`: {e}");
    let raw: RawQuestStep = source.clone().try_into().map_err(|e: toml::de::Error| at(e.to_string()))?;
    if raw.kind.is_quest() {
        return Err(at("`kind` can't be a quest event (quest_completed, quest_failed)".into()));
    }
    let count = raw.count.unwrap_or(1);
    if count < 1 {
        return Err(at("`count` must be at least 1".into()));
    }
    let filter = Filter::try_from(raw.filter.unwrap_or_default()).map_err(|e| at(format!("`filter`: {e}")))?;
    filter.kinds_ok(&[raw.kind]).map_err(|e| at(format!("`filter`: {e}")))?;
    let mut label = raw.label.unwrap_or_else(|| default_label(raw.kind).to_string());
    if count > 1 {
        label.push_str(&format!(" \u{d7}{count}"));
    }
    Ok(QuestStep { kind: raw.kind, filter, count, label, source })
}

pub fn parse_quest(text: &str) -> Result<QuestDef, String> {
    let r: RawQuest = toml::from_str(text).map_err(|e| e.to_string())?;
    if r.id.is_empty() || !r.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("bad `id` `{}`: use only letters, digits, `_` and `-`", r.id));
    }
    if r.trigger.is_empty() {
        return Err("`trigger` is empty".into());
    }
    if r.trigger.iter().any(|k| k.is_quest()) {
        return Err("`trigger` can't include quest events (quest_completed, quest_failed)".into());
    }
    if !(r.chance > 0.0 && r.chance <= 1.0) {
        return Err(format!("`chance` must be above 0 and at most 1 (got {})", r.chance));
    }
    if r.minutes < 1 {
        return Err("`minutes` must be at least 1".into());
    }
    if r.steps.is_empty() {
        return Err("`steps` is empty (a quest needs at least one step)".into());
    }
    if r.same == Some(Same::Pr) && !r.trigger.iter().all(|&k| is_pr_kind(k)) {
        return Err("`same = \"pr\"` needs a `trigger` made only of PR kinds (pr_opened, pr_merged, review_submitted)".into());
    }
    let filter = Filter::try_from(r.filter.unwrap_or_default()).map_err(|e| format!("`filter`: {e}"))?;
    filter.kinds_ok(&r.trigger).map_err(|e| format!("`filter`: {e}"))?;
    let steps = r.steps.into_iter().enumerate().map(|(i, s)| parse_step(i + 1, s)).collect::<Result<Vec<_>, _>>()?;
    if r.same == Some(Same::Branch) && steps.iter().any(|s| is_pr_kind(s.kind)) {
        return Err("`same = \"branch\"` can't have a PR step (pr_opened, pr_merged, review_submitted): PR events have no branch. Use `same = \"repo\"`".into());
    }
    Ok(QuestDef {
        xp: r.xp.unwrap_or_else(|| r.tier.default_xp() / 2),
        id: r.id,
        name: r.name,
        tier: r.tier,
        trigger: r.trigger,
        filter,
        chance: r.chance,
        cooldown_hours: r.cooldown_hours,
        minutes: r.minutes,
        same: r.same,
        steps,
        hint: r.hint,
        fallback: r.fallback,
        flavor: r.flavor,
        enabled: r.enabled,
    })
}

/// Load every `*.toml` in `dir` (sorted by filename). A missing dir means no quests.
/// Bad files are skipped and reported; on duplicate ids the first wins.
pub fn load_dir(dir: &Path) -> (Vec<QuestDef>, Vec<RuleError>) {
    let mut quests: Vec<QuestDef> = Vec::new();
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (quests, errors),
        Err(e) => {
            errors.push(RuleError { file: dir.to_path_buf(), message: e.to_string() });
            return (quests, errors);
        }
    };
    files.sort();
    for file in files {
        let parsed = std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| parse_quest(&t));
        match parsed {
            Ok(q) if quests.iter().any(|x| x.id == q.id) => {
                errors.push(RuleError { message: format!("duplicate id `{}`", q.id), file })
            }
            Ok(q) => quests.push(q),
            Err(message) => errors.push(RuleError { file, message }),
        }
    }
    (quests, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIP_IT: &str = r#"
id = "ship_it_speedrun"
name = "Ship It (Speedrun)"
tier = "gold"
trigger = ["commit"]
filter = { }
chance = 0.10
cooldown_hours = 24
minutes = 15
same = "repo"
steps = [
  { kind = "push" },
  { kind = "pr_opened" },
]
hint = "Commit, push, PR. Go."
fallback = "You committed. Bold."
"#;

    #[test]
    fn parses_ship_it() {
        let q = parse_quest(SHIP_IT).unwrap();
        assert_eq!(q.steps.len(), 2);
        assert_eq!(q.steps[1].label, "Open a PR");
        assert_eq!(q.xp, 500);
        assert_eq!(q.same, Some(Same::Repo));
        assert!(q.has_github_step());
    }

    #[test]
    fn rejects_bad_quests() {
        let with = |from: &str, to: &str| SHIP_IT.replace(from, to);
        let cases: Vec<(String, &str)> = vec![
            (format!("{SHIP_IT}bogus = 1\n"), "unknown field"),
            (with("steps = [\n  { kind = \"push\" },\n  { kind = \"pr_opened\" },\n]", "steps = []"), "steps"),
            (with("{ kind = \"push\" }", "{ kind = \"quest_completed\" }"), "quest"),
            (with("trigger = [\"commit\"]", "trigger = [\"quest_failed\"]"), "quest"),
            (with("chance = 0.10", "chance = 0"), "chance"),
            (with("chance = 0.10", "chance = 1.5"), "chance"),
            (with("minutes = 15", "minutes = 0"), "minutes"),
            (with("{ kind = \"push\" }", "{ kind = \"push\", count = 0 }"), "count"),
            (with("same = \"repo\"", "same = \"pr\""), "same"),
            // PR events carry no branch, so a branch binding could never credit a PR step.
            (with("same = \"repo\"", "same = \"branch\""), "same"),
            (with("{ kind = \"push\" }", "{ kind = \"push\", filter = { message = \"x\" } }"), "filter"),
            (with("id = \"ship_it_speedrun\"", "id = \"a.b\""), "id"),
        ];
        for (text, frag) in cases {
            let e = parse_quest(&text).expect_err(frag);
            assert!(e.contains(frag), "{frag}: {e}");
        }
    }

    #[test]
    fn starter_quests_all_load() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/quests");
        let (q, errs) = load_dir(&dir);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(q.len(), 6);
    }
}
