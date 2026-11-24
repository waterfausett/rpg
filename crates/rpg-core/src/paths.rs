use anyhow::{anyhow, Context, Result};
use include_dir::{include_dir, Dir};
use std::path::PathBuf;

static STARTER_RULES: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/rules");
const DEFAULT_CONFIG: &str = include_str!("../assets/config.default.toml");

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    /// Resolve from the `RPG_HOME` env var, falling back to platform dirs.
    pub fn resolve() -> Result<Paths> {
        Self::from_home(std::env::var_os("RPG_HOME").map(PathBuf::from))
    }

    /// `home` overrides the platform dirs: data under `<home>/data`, config under `<home>/config`.
    pub fn from_home(home: Option<PathBuf>) -> Result<Paths> {
        match home {
            Some(h) => Ok(Paths { data: h.join("data"), config: h.join("config") }),
            None => {
                let d = directories::ProjectDirs::from("", "", "rpg")
                    .ok_or_else(|| anyhow!("cannot determine home directory"))?;
                Ok(Paths { data: d.data_dir().to_path_buf(), config: d.config_dir().to_path_buf() })
            }
        }
    }

    pub fn db(&self) -> PathBuf {
        self.data.join("rpg.db")
    }
    pub fn inbox(&self) -> PathBuf {
        self.data.join("inbox")
    }
    pub fn git_hooks(&self) -> PathBuf {
        self.data.join("git-hooks")
    }
    pub fn log(&self) -> PathBuf {
        self.data.join("rpg.log")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
    pub fn rules(&self) -> PathBuf {
        self.config.join("rules")
    }
    pub fn sounds(&self) -> PathBuf {
        self.config.join("sounds")
    }

    /// Create dirs, write the default config if missing, seed starter rules if `rules/` is empty.
    pub fn ensure(&self) -> Result<()> {
        for d in [self.data.clone(), self.inbox(), self.git_hooks(), self.config.clone(), self.rules(), self.sounds()] {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        if !self.config_file().exists() {
            std::fs::write(self.config_file(), DEFAULT_CONFIG).context("writing default config")?;
        }
        if std::fs::read_dir(self.rules())?.next().is_none() {
            for f in STARTER_RULES.files() {
                let name = f.path().file_name().ok_or_else(|| anyhow!("bad embedded rule path"))?;
                std::fs::write(self.rules().join(name), f.contents())
                    .with_context(|| format!("seeding rule {}", name.to_string_lossy()))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_respect_rpg_home() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::from_home(Some(tmp.path().to_path_buf())).unwrap();
        assert_eq!(p.db(), tmp.path().join("data").join("rpg.db"));
        assert_eq!(p.rules(), tmp.path().join("config").join("rules"));
    }

    #[test]
    fn ensure_seeds_rules_and_config() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::from_home(Some(tmp.path().to_path_buf())).unwrap();
        p.ensure().unwrap();
        assert!(p.config_file().exists());
        let n = std::fs::read_dir(p.rules())
            .unwrap()
            .filter(|e| {
                e.as_ref().unwrap().path().extension().and_then(|x| x.to_str()) == Some("toml")
            })
            .count();
        assert_eq!(n, 10);
    }
}
