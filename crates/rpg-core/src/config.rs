use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub xp: XpConfig,
    pub levels: LevelConfig,
    pub github: GithubConfig,
    pub llm: LlmConfig,
    pub sound: SoundConfig,
    pub site: SiteConfig,
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
    use super::*;

    #[test]
    fn missing_config_gives_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let c = Config::load(&tmp.path().join("nope.toml")).unwrap();
        assert_eq!(c.xp.pr_merged, 100);
        assert_eq!(c.levels.exponent, 1.5);
        assert_eq!(c.site.port, 47474);
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
}
