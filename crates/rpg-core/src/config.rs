use crate::event::normalize_repo;
use crate::rules::{Announce, Tier};
use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub xp: XpConfig,
    pub levels: LevelConfig,
    pub skill_levels: SkillLevelConfig,
    pub github: GithubConfig,
    pub llm: LlmConfig,
    pub sound: SoundConfig,
    pub site: SiteConfig,
    pub paths: PathFilter,
    pub updates: UpdatesConfig,
    pub announce: AnnounceConfig,
    pub quests: QuestsConfig,
}

/// Timed quests. `enabled = false` turns the feature off (an active quest ends quietly).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct QuestsConfig {
    pub enabled: bool,
    /// How long an offer waits for Accept.
    pub offer_seconds: u64,
    /// Passes (or ignored offers) in a row that break the chain.
    pub max_passes: u32,
    /// Multiplier gained per completion.
    pub chain_step: f64,
    pub chain_max: f64,
    /// Minimum time between any two offers.
    pub min_gap_minutes: u64,
}

impl Default for QuestsConfig {
    fn default() -> Self {
        Self { enabled: true, offer_seconds: 60, max_passes: 3, chain_step: 0.5, chain_max: 5.0, min_gap_minutes: 30 }
    }
}

/// How an unlock of each tier is announced when its rule doesn't say: a banner or just a toast.
/// `all`, when set, overrides every rule, step and tier setting (and `first_banner`).
/// `first_banner` makes an achievement's first-ever unlock a banner.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AnnounceConfig {
    pub all: Option<Announce>,
    pub first_banner: bool,
    pub bronze: Announce,
    pub silver: Announce,
    pub gold: Announce,
    pub platinum: Announce,
    pub legendary: Announce,
    pub celestial: Announce,
}

impl Default for AnnounceConfig {
    fn default() -> Self {
        Self { all: None, first_banner: true, bronze: Announce::Toast, silver: Announce::Banner, gold: Announce::Banner, platinum: Announce::Banner, legendary: Announce::Banner, celestial: Announce::Banner }
    }
}

impl AnnounceConfig {
    pub fn for_tier(&self, tier: Tier) -> Announce {
        match tier {
            Tier::Bronze => self.bronze,
            Tier::Silver => self.silver,
            Tier::Gold => self.gold,
            Tier::Platinum => self.platinum,
            Tier::Legendary => self.legendary,
            Tier::Celestial => self.celestial,
        }
    }
}

/// Background checks for a new release. The tray's "Check for updates" works either way.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UpdatesConfig {
    /// Hours between automatic checks (the first runs shortly after startup); 0 turns them off.
    pub check_hours: u64,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { check_hours: 6 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct XpConfig {
    pub commit: u64,
    pub push: u64,
    pub pr_opened: u64,
    pub pr_merged: u64,
    pub review: u64,
    pub claude_session: u64,
}

impl Default for XpConfig {
    fn default() -> Self {
        Self { commit: 10, push: 25, pr_opened: 50, pr_merged: 100, review: 40, claude_session: 15 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LevelConfig {
    pub base: f64,
    pub exponent: f64,
}

impl Default for LevelConfig {
    fn default() -> Self {
        Self { base: 1000.0, exponent: 1.5 }
    }
}

/// The level curve for skills: same shape as `levels`, but a lower base so skills level faster.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SkillLevelConfig {
    pub base: f64,
    pub exponent: f64,
}

impl Default for SkillLevelConfig {
    fn default() -> Self {
        Self { base: 100.0, exponent: 1.5 }
    }
}

impl SkillLevelConfig {
    pub fn curve(&self) -> LevelConfig {
        LevelConfig { base: self.base, exponent: self.exponent }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GithubConfig {
    pub enabled: bool,
    pub poll_minutes: u64,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self { enabled: true, poll_minutes: 10 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub timeout_secs: u64,
    pub model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self { enabled: true, timeout_secs: 10, model: "haiku".into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub volume: f32,
    pub achievement: Option<String>,
    pub level_up: Option<String>,
    pub by_tier: HashMap<String, String>,
    pub by_rule: HashMap<String, String>,
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            volume: 0.8,
            achievement: None,
            level_up: None,
            by_tier: HashMap::new(),
            by_rule: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SiteConfig {
    pub port: u16,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self { port: 47474 }
    }
}

/// Which repos/folders count. An event whose repo (a local path, or `owner/name` for GitHub)
/// matches `exclude`, or misses a non-empty `include`, is dropped before it earns anything.
/// Events with no repo always count.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(try_from = "RawPathFilter")]
pub struct PathFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawPathFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

/// A pattern with no glob characters is a folder: it matches itself and everything under it.
/// Backslashes and drive-letter case are normalized like repo keys, and matching ignores case.
fn path_set(field: &str, patterns: &[String]) -> Result<Option<GlobSet>, String> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for raw in patterns {
        let p = normalize_repo(raw.trim());
        if p.is_empty() {
            return Err(format!("empty pattern in `paths.{field}`"));
        }
        let mut globs = vec![p.clone()];
        if !p.contains(['*', '?', '[', '{']) {
            globs.push(format!("{}/**", p.trim_end_matches('/')));
        }
        for g in globs {
            let glob = GlobBuilder::new(&g)
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("bad `paths.{field}` pattern `{raw}`: {e}"))?;
            b.add(glob);
        }
    }
    b.build().map(Some).map_err(|e| e.to_string())
}

impl TryFrom<RawPathFilter> for PathFilter {
    type Error = String;
    fn try_from(r: RawPathFilter) -> Result<Self, String> {
        Ok(PathFilter { include: path_set("include", &r.include)?, exclude: path_set("exclude", &r.exclude)? })
    }
}

impl PathFilter {
    /// Whether an event in `repo` counts.
    pub fn allows(&self, repo: Option<&str>) -> bool {
        let Some(repo) = repo else { return true };
        let repo = normalize_repo(repo);
        if self.exclude.as_ref().is_some_and(|x| x.is_match(&repo)) {
            return false;
        }
        self.include.as_ref().is_none_or(|i| i.is_match(&repo))
    }
}

impl Config {
    /// Load config from `path`. A missing file yields defaults.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quests_config_defaults() {
        let c = Config::default().quests;
        assert_eq!(
            c,
            QuestsConfig { enabled: true, offer_seconds: 60, max_passes: 3, chain_step: 0.5, chain_max: 5.0, min_gap_minutes: 30 }
        );
        let off: Config = toml::from_str("[quests]
enabled = false").unwrap();
        assert!(!off.quests.enabled);
        assert_eq!(off.quests.offer_seconds, 60);
        let shipped: Config = toml::from_str(include_str!("../assets/config.default.toml")).unwrap();
        assert_eq!(shipped.quests, c);
    }

    use super::*;

    #[test]
    fn missing_config_gives_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let c = Config::load(&tmp.path().join("nope.toml")).unwrap();
        assert_eq!(c.xp.pr_merged, 100);
        assert_eq!(c.levels.exponent, 1.5);
        assert_eq!(c.site.port, 47474);
        assert_eq!(c.skill_levels.base, 100.0);
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("config.toml");
        std::fs::write(&f, "[xp]\ncommit = 99\n").unwrap();
        let c = Config::load(&f).unwrap();
        assert_eq!(c.xp.commit, 99);
        assert_eq!(c.xp.push, 25);
    }

    #[test]
    fn announce_defaults() {
        let c = Config::default();
        let got: Vec<Announce> = [Tier::Bronze, Tier::Silver, Tier::Gold, Tier::Platinum, Tier::Legendary, Tier::Celestial].into_iter().map(|t| c.announce.for_tier(t)).collect();
        assert_eq!(got, vec![Announce::Toast, Announce::Banner, Announce::Banner, Announce::Banner, Announce::Banner, Announce::Banner]);
        let c: Config = toml::from_str("[announce]\nbronze = \"banner\"\ngold = \"toast\"\n").unwrap();
        assert_eq!((c.announce.bronze, c.announce.silver, c.announce.gold), (Announce::Banner, Announce::Banner, Announce::Toast));
        assert_eq!(c.announce.all, None);
        assert!(c.announce.first_banner);
        let c: Config = toml::from_str("[announce]
first_banner = false
").unwrap();
        assert!(!c.announce.first_banner);
        let c: Config = toml::from_str("[announce]\nall = \"toast\"\n").unwrap();
        assert_eq!((c.announce.all, c.announce.gold), (Some(Announce::Toast), Announce::Banner));
    }

    fn filter(toml_text: &str) -> PathFilter {
        toml::from_str::<Config>(toml_text).unwrap().paths
    }

    #[test]
    fn default_config_file_parses() {
        let c: Config = toml::from_str(include_str!("../assets/config.default.toml")).unwrap();
        assert!(c.paths.allows(Some("C:/anything")));
    }

    #[test]
    fn no_filter_allows_everything() {
        let f = PathFilter::default();
        assert!(f.allows(Some("C:/anything")) && f.allows(None));
    }

    #[test]
    fn exclude_folder_covers_itself_and_below() {
        let f = filter(r#"
[paths]
exclude = ['C:\Users\me\AppData\Local\Temp\']
"#);
        assert!(!f.allows(Some("C:/Users/me/AppData/Local/Temp")));
        assert!(!f.allows(Some(r"c:\users\me\appdata\local\temp\x\repo")));
        assert!(f.allows(Some("C:/Users/me/AppData/Local/TempX")));
        assert!(f.allows(Some("C:/Users/me/rpg")));
        assert!(f.allows(None));
    }

    #[test]
    fn include_limits_and_exclude_wins() {
        let f = filter("[paths]\ninclude = ['C:/work', 'octo/*']\nexclude = ['**/scratch*']\n");
        assert!(f.allows(Some("C:/work/app")));
        assert!(f.allows(Some("octo/widgets")));
        assert!(!f.allows(Some("C:/play/app")));
        assert!(!f.allows(Some("C:/work/scratch-1")));
        assert!(f.allows(None), "events with no repo always count");
    }

    #[test]
    fn bad_patterns_and_keys_are_errors() {
        for bad in ["[paths]\nexclude = ['[']\n", "[paths]\nexclude = ['']\n", "[paths]\nexlude = ['x']\n"] {
            assert!(toml::from_str::<Config>(bad).is_err(), "{bad}");
        }
    }
}
