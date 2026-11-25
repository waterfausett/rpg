use anyhow::{anyhow, Context, Result};
use include_dir::{include_dir, Dir};
use std::path::PathBuf;

static STARTER_RULES: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/rules");
static STARTER_SKILLS: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/skills");
const DEFAULT_CONFIG: &str = include_str!("../assets/config.default.toml");

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
    /// The `RPG_HOME` override in effect, if any. Baked into installed hooks so they report
    /// to this home rather than to whatever `RPG_HOME` the committing shell happens to have.
    pub home: Option<PathBuf>,
}

impl Paths {
    /// Resolve from the `RPG_HOME` env var, falling back to platform dirs.
    pub fn resolve() -> Result<Paths> {
        Self::from_home(std::env::var_os("RPG_HOME").map(PathBuf::from))
    }

    /// `home` overrides the platform dirs: data under `<home>/data`, config under `<home>/config`.
    /// An empty `home` (e.g. `RPG_HOME=`) counts as unset. A relative `home` is made absolute
    /// (hooks bake it in and run from other directories).
    pub fn from_home(home: Option<PathBuf>) -> Result<Paths> {
        match home.filter(|h| !h.as_os_str().is_empty()) {
            Some(h) => {
                let h = std::path::absolute(&h).with_context(|| format!("resolving {}", h.display()))?;
                Ok(Paths { data: h.join("data"), config: h.join("config"), home: Some(h) })
            }
            None => {
                let d = directories::ProjectDirs::from("", "", "rpg")
                    .ok_or_else(|| anyhow!("cannot determine home directory"))?;
                Ok(Paths { data: d.data_dir().to_path_buf(), config: d.config_dir().to_path_buf(), home: None })
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
    /// Skill files live in a subfolder of `rules/`, so "Open rules folder" reaches them.
    pub fn skills(&self) -> PathBuf {
        self.rules().join("skills")
    }
    /// Starter rule file names already seeded (or deliberately skipped), one per line.
    pub fn seeded_manifest(&self) -> PathBuf {
        self.data.join("seeded-rules.txt")
    }
    pub fn sounds(&self) -> PathBuf {
        self.config.join("sounds")
    }

    /// Seed starter rules not yet in the manifest, so new starters arrive on upgrade while
    /// deleted ones stay deleted. A missing manifest starts from the starters already in `rules/`.
    /// Never overwrites an existing file.
    fn seed_starter_rules(&self) -> Result<()> {
        let rules = self.rules();
        let manifest_path = self.seeded_manifest();
        let starters: Vec<(String, &[u8])> = STARTER_RULES
            .files()
            .filter_map(|f| Some((f.path().file_name()?.to_string_lossy().into_owned(), f.contents())))
            .collect();
        let mut seeded: std::collections::BTreeSet<String> = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                starters.iter().filter(|(n, _)| rules.join(n).is_file()).map(|(n, _)| n.clone()).collect()
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", manifest_path.display())),
        };
        let before = std::fs::read_to_string(&manifest_path).ok();
        for (name, contents) in &starters {
            if seeded.contains(name) {
                continue;
            }
            let dest = rules.join(name);
            if !dest.exists() {
                std::fs::write(&dest, contents).with_context(|| format!("seeding rule {name}"))?;
            }
            seeded.insert(name.clone());
        }
        let text: String = seeded.iter().map(|n| format!("{n}
")).collect();
        if before.as_deref() != Some(text.as_str()) {
            std::fs::write(&manifest_path, text).with_context(|| format!("writing {}", manifest_path.display()))?;
        }
        Ok(())
    }

    /// Create dirs, write the default config if missing, seed starter rules not yet seeded
    /// (see `seed_starter_rules`) and starter skills if `rules/skills/` is empty (so existing installs get skills too).
    pub fn ensure(&self) -> Result<()> {
        for d in [self.data.clone(), self.inbox(), self.git_hooks(), self.config.clone(), self.rules(), self.sounds()] {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        if !self.config_file().exists() {
            std::fs::write(self.config_file(), DEFAULT_CONFIG).context("writing default config")?;
        }
        // Before creating `skills/`, which would make `rules/` non-empty.
        self.seed_starter_rules()?;
        std::fs::create_dir_all(self.skills()).with_context(|| format!("creating {}", self.skills().display()))?;
        if std::fs::read_dir(self.skills())?.next().is_none() {
            seed(&STARTER_SKILLS, &self.skills(), "skill")?;
        }
        Ok(())
    }
}

fn seed(from: &Dir, to: &std::path::Path, what: &str) -> Result<()> {
    for f in from.files() {
        let name = f.path().file_name().ok_or_else(|| anyhow!("bad embedded {what} path"))?;
        std::fs::write(to.join(name), f.contents()).with_context(|| format!("seeding {what} {}", name.to_string_lossy()))?;
    }
    Ok(())
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
    fn empty_rpg_home_means_default_dirs() {
        let empty = Paths::from_home(Some(PathBuf::new())).unwrap();
        let default = Paths::from_home(None).unwrap();
        assert_eq!(empty.data, default.data);
        assert_eq!(empty.config, default.config);
        assert_eq!(empty.home, None);
        let set = Paths::from_home(Some(PathBuf::from("x"))).unwrap();
        assert!(set.data.is_absolute(), "relative RPG_HOME is made absolute");
        assert_eq!(set.home, Some(std::path::absolute("x").unwrap()));
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
        assert_eq!(n, STARTER_RULES.files().count());
        let skills = std::fs::read_dir(p.skills()).unwrap().count();
        assert_eq!(skills, STARTER_SKILLS.files().count());
        assert!(skills >= 10);
    }

    #[test]
    fn ensure_adds_skills_to_an_existing_install_without_overwriting_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::from_home(Some(tmp.path().to_path_buf())).unwrap();
        std::fs::create_dir_all(p.rules()).unwrap();
        std::fs::write(p.rules().join("mine.toml"), "x").unwrap();
        p.ensure().unwrap();
        assert_eq!(std::fs::read_to_string(p.rules().join("mine.toml")).unwrap(), "x");
        assert!(std::fs::read_dir(p.skills()).unwrap().count() >= 10);
        // Deleting one starter skill sticks (only an empty folder is reseeded, as with rules).
        std::fs::remove_file(p.skills().join("rust.toml")).unwrap();
        p.ensure().unwrap();
        assert!(!p.skills().join("rust.toml").exists());
    }

    fn starter_names() -> Vec<String> {
        STARTER_RULES.files().map(|f| f.path().file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    fn setup() -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::from_home(Some(tmp.path().to_path_buf())).unwrap();
        (tmp, p)
    }

    #[test]
    fn fresh_install_seeds_all_and_writes_manifest() {
        let (_t, p) = setup();
        p.ensure().unwrap();
        let mut names = starter_names();
        names.sort();
        for n in &names {
            assert!(p.rules().join(n).is_file(), "{n}");
        }
        let m = std::fs::read_to_string(p.seeded_manifest()).unwrap();
        assert_eq!(m, names.iter().map(|n| format!("{n}
")).collect::<String>());
    }

    #[test]
    fn manifest_created_from_existing_files() {
        let (_t, p) = setup();
        std::fs::create_dir_all(p.rules()).unwrap();
        let names = starter_names();
        for n in names.iter().take(3) {
            std::fs::write(p.rules().join(n), "mine").unwrap();
        }
        p.ensure().unwrap();
        for (i, n) in names.iter().enumerate() {
            assert!(p.rules().join(n).is_file(), "{n}");
            if i < 3 {
                assert_eq!(std::fs::read_to_string(p.rules().join(n)).unwrap(), "mine");
            }
        }
    }

    #[test]
    fn deleted_starter_stays_deleted() {
        let (_t, p) = setup();
        p.ensure().unwrap();
        std::fs::remove_file(p.rules().join("night_shift.toml")).unwrap();
        p.ensure().unwrap();
        assert!(!p.rules().join("night_shift.toml").exists());
    }

    #[test]
    fn new_starter_added_on_upgrade() {
        let (_t, p) = setup();
        p.ensure().unwrap();
        std::fs::remove_file(p.rules().join("hat_trick.toml")).unwrap();
        let m = std::fs::read_to_string(p.seeded_manifest()).unwrap();
        std::fs::write(p.seeded_manifest(), m.replace("hat_trick.toml
", "")).unwrap();
        p.ensure().unwrap();
        assert!(p.rules().join("hat_trick.toml").is_file());
        assert!(std::fs::read_to_string(p.seeded_manifest()).unwrap().contains("hat_trick.toml"));
    }
}
