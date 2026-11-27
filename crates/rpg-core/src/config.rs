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
}

/// How an unlock of each tier is announced when its rule doesn't say: a banner or just a toast.
/// `all`, when set, overrides every rule, step and tier setting (and `first_banner`).
/// `first_banner` makes an achievement's first-ever unlock a banner.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AnnounceConfig {
    pub all: Option<Announce>,
    pub first_banner: bool,
    /// Do not disturb: suppress unlock announcements. Set from the tray.
    pub dnd: bool,
    pub bronze: Announce,
    pub silver: Announce,
    pub gold: Announce,
    pub platinum: Announce,
    pub legendary: Announce,
    pub celestial: Announce,
}

impl Default for AnnounceConfig {
    fn default() -> Self {
        Self { all: None, first_banner: true, dnd: false, bronze: Announce::Toast, silver: Announce::Banner, gold: Announce::Banner, platinum: Announce::Banner, legendary: Announce::Banner, celestial: Announce::Banner }
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
    /// Mute all sounds. Set from the tray or the Settings window.
    pub muted: bool,
    pub achievement: Option<String>,
    pub level_up: Option<String>,
    pub by_tier: HashMap<String, String>,
    pub by_rule: HashMap<String, String>,
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            volume: 0.8,
            muted: false,
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

/// Check one path pattern against the same rules `paths.include` / `paths.exclude` use.
pub fn validate_path_pattern(raw: &str) -> Result<(), String> {
    path_set("pattern", &[raw.to_string()]).map(|_| ())
}

/// The `paths.include` and `paths.exclude` strings as written; empty if the text doesn't parse.
pub fn raw_paths(text: &str) -> (Vec<String>, Vec<String>) {
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Doc {
        paths: RawPathFilter,
    }
    toml::from_str::<Doc>(text).map(|d| (d.paths.include, d.paths.exclude)).unwrap_or_default()
}

/// A folder path as a `[paths]` pattern: glob characters are escaped as one-character classes
/// (`[` → `[[]`), so the pattern still counts as a folder and matches itself and everything under it.
pub fn escape_folder(folder: &str) -> String {
    let mut out = String::with_capacity(folder.len());
    for c in folder.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}') {
            out.extend(['[', c, ']']);
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `p` names one exact path: no wildcards or alternatives, and every `[` opens a
/// one-character class such as `[[]` (how `escape_folder` writes a literal glob character).
fn is_literal(p: &str) -> bool {
    let c: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '*' | '?' | '{' | '}' => return false,
            '[' if c.get(i + 2) == Some(&']') => i += 3,
            '[' => return false,
            _ => i += 1,
        }
    }
    true
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
        if is_literal(&p) {
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
    /// Parse config text.
    pub fn parse(text: &str) -> Result<Config> {
        toml::from_str(text).map_err(Into::into)
    }

    /// Load config from `path`. A missing file yields defaults.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => Config::parse(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn muted_and_dnd_default_off_and_parse() {
        let c = Config::default();
        assert!(!c.sound.muted && !c.announce.dnd);
        let c = Config::parse("[sound]\nmuted = true\n[announce]\ndnd = true\n").unwrap();
        assert!(c.sound.muted && c.announce.dnd);
    }

    #[test]
    fn validate_path_pattern_matches_path_set_rules() {
        assert!(validate_path_pattern("C:/work").is_ok());
        assert!(validate_path_pattern("octo/*").is_ok());
        assert!(validate_path_pattern("[").is_err());
        assert!(validate_path_pattern("  ").is_err());
    }

    #[test]
    fn raw_paths_returns_strings_as_written() {
        let (i, e) = raw_paths("[paths]\ninclude = ['C:\\work']\nexclude = ['**/tmp*']\n");
        assert_eq!(i, vec![r"C:\work".to_string()]);
        assert_eq!(e, vec!["**/tmp*".to_string()]);
        assert_eq!(raw_paths("not toml ["), (vec![], vec![]));
    }

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

    #[test]
    fn escaped_folder_matches_itself_and_below() {
        for folder in [r"C:\work\My Repos [old]", "C:/work/a{b}*?c", "C:/work/x]y"] {
            let pat = escape_folder(folder);
            assert!(validate_path_pattern(&pat).is_ok(), "{pat}");
            let f = filter(&format!("[paths]\nexclude = ['{pat}']\n"));
            let norm = normalize_repo(folder);
            assert!(!f.allows(Some(folder)), "{pat} should match the folder itself");
            assert!(!f.allows(Some(&format!("{norm}/sub/repo"))), "{pat} should match below the folder");
            assert!(f.allows(Some(&format!("{norm}x"))), "{pat} must not match a sibling");
        }
        // `[old]` must not act as a character class.
        let f = filter(&format!("[paths]\nexclude = ['{}']\n", escape_folder("C:/w/[old]")));
        assert!(f.allows(Some("C:/w/o")));
        // Plain folders don't change, and real globs stay globs.
        assert_eq!(escape_folder("C:/work/plain"), "C:/work/plain");
        let f = filter("[paths]\nexclude = ['C:/w/[ab]']\n");
        assert!(!f.allows(Some("C:/w/a")) && f.allows(Some("C:/w/a/sub")));
    }
}
