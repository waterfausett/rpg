//! Skills as data: TOML files that turn matching events into per-skill XP.
//!
//! A skill matches an event when its kind is in `events` (default: commits), its repo
//! matches one of `repos` (if given), and at least one of its files matches one of
//! `globs` (if given). Only commits carry files, so `globs` require commit-only `events`.
use crate::event::{Event, EventKind};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Skill XP per matching event when a skill file does not set `xp`.
pub const DEFAULT_SKILL_XP: u64 = 10;

#[derive(Debug, Clone)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub xp: u64,
    pub events: Vec<EventKind>,
    globs: Option<GlobSet>,
    repos: Option<GlobSet>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkill {
    id: String,
    name: String,
    xp: Option<u64>,
    events: Option<Vec<EventKind>>,
    globs: Option<Vec<String>>,
    repos: Option<Vec<String>>,
}

fn glob(pattern: &str) -> Result<Glob, globset::Error> {
    // `*` crosses `/`, so `*.ts` matches `src/app.ts`; case-insensitive for Windows paths.
    GlobBuilder::new(pattern).case_insensitive(true).build()
}

fn glob_set(field: &str, patterns: Option<Vec<String>>) -> Result<Option<GlobSet>, String> {
    let Some(patterns) = patterns else { return Ok(None) };
    if patterns.is_empty() {
        return Err(format!("`{field}` is empty (leave it out to match everything)"));
    }
    let mut b = GlobSetBuilder::new();
    for p in &patterns {
        b.add(glob(p).map_err(|e| format!("bad {field} pattern `{p}`: {e}"))?);
    }
    b.build().map(Some).map_err(|e| e.to_string())
}

pub fn parse_skill(text: &str) -> Result<Skill, String> {
    let r: RawSkill = toml::from_str(text).map_err(|e| e.to_string())?;
    let events = r.events.unwrap_or_else(|| vec![EventKind::Commit]);
    if events.is_empty() {
        return Err("`events` is empty".into());
    }
    if r.globs.is_some() && events.iter().any(|k| *k != EventKind::Commit) {
        return Err("`globs` only work with `events = [\"commit\"]` (other events carry no files)".into());
    }
    Ok(Skill {
        xp: r.xp.unwrap_or(DEFAULT_SKILL_XP),
        globs: glob_set("globs", r.globs)?,
        repos: glob_set("repos", r.repos)?,
        id: r.id,
        name: r.name,
        events,
    })
}

impl Skill {
    pub fn matches(&self, ev: &Event) -> bool {
        if !self.events.contains(&ev.kind) {
            return false;
        }
        if let Some(repos) = &self.repos {
            if !ev.repo.as_deref().is_some_and(|r| repos.is_match(r)) {
                return false;
            }
        }
        match &self.globs {
            None => true,
            Some(globs) => ev.data["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["path"].as_str().or(f.as_str()))
                .any(|p| globs.is_match(p)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillError {
    pub file: PathBuf,
    pub message: String,
}

/// Load every `*.toml` in `dir` (sorted by filename). A missing dir means no skills.
/// Bad files are skipped and reported; on duplicate ids the first wins.
pub fn load_dir(dir: &Path) -> (Vec<Skill>, Vec<SkillError>) {
    let mut skills: Vec<Skill> = Vec::new();
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (skills, errors),
        Err(e) => {
            errors.push(SkillError { file: dir.to_path_buf(), message: e.to_string() });
            return (skills, errors);
        }
    };
    files.sort();
    for file in files {
        let parsed = std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| parse_skill(&t));
        match parsed {
            Ok(s) if skills.iter().any(|x| x.id == s.id) => {
                errors.push(SkillError { message: format!("duplicate id `{}`", s.id), file })
            }
            Ok(s) => skills.push(s),
            Err(message) => errors.push(SkillError { file, message }),
        }
    }
    (skills, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use serde_json::json;

    fn commit(repo: &str, paths: &[&str]) -> Event {
        let files: Vec<_> = paths.iter().map(|p| json!({ "path": p, "added": 1, "removed": 0 })).collect();
        Event::new(EventKind::Commit, Source::Git, Some(repo.into()), json!({ "files": files }))
    }

    const TS: &str = r#"
id = "typescript"
name = "TypeScript"
globs = ["*.ts", "*.tsx"]
"#;

    #[test]
    fn globs_match_any_file_in_any_folder() {
        let s = parse_skill(TS).unwrap();
        assert_eq!((s.xp, s.events.clone()), (DEFAULT_SKILL_XP, vec![EventKind::Commit]));
        assert!(s.matches(&commit("C:/w/app", &["README.md", "src/ui/App.TSX"])));
        assert!(!s.matches(&commit("C:/w/app", &["README.md", "src/main.rs"])));
        assert!(!s.matches(&commit("C:/w/app", &[])));
        let push = Event::new(EventKind::Push, Source::Git, Some("C:/w/app".into()), json!({}));
        assert!(!s.matches(&push));
    }

    #[test]
    fn event_only_and_repo_skills() {
        let review = parse_skill("id = \"review\"\nname = \"Code Review\"\nevents = [\"review_submitted\"]\nxp = 25\n").unwrap();
        let ev = Event::new(EventKind::ReviewSubmitted, Source::Github, Some("octo/widgets".into()), json!({}));
        assert!(review.matches(&ev));
        assert_eq!(review.xp, 25);
        assert!(!review.matches(&commit("octo/widgets", &["a.ts"])));

        let repo = parse_skill("id = \"rpg\"\nname = \"RPG Dev\"\nrepos = [\"**/rpg\"]\n").unwrap();
        assert!(repo.matches(&commit("C:/Users/me/rpg", &["x"])));
        assert!(!repo.matches(&commit("C:/Users/me/other", &["x"])));
    }

    #[test]
    fn invalid_skills_are_rejected() {
        for bad in [
            "id = \"a\"\nname = \"A\"\nglobs = [\"*.ts\"]\nevents = [\"pr_merged\"]\n",
            "id = \"a\"\nname = \"A\"\nglobs = []\n",
            "id = \"a\"\nname = \"A\"\nevents = []\n",
            "id = \"a\"\nname = \"A\"\nglobs = [\"[\"]\n",
            "id = \"a\"\nname = \"A\"\nglob = [\"*.ts\"]\n", // typo'd field
            "name = \"A\"\n",
        ] {
            assert!(parse_skill(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn load_dir_skips_bad_and_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.toml"), TS).unwrap();
        std::fs::write(tmp.path().join("b.toml"), TS).unwrap();
        std::fs::write(tmp.path().join("c.toml"), "nope").unwrap();
        std::fs::write(tmp.path().join("d.txt"), TS).unwrap();
        let (skills, errors) = load_dir(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(errors.len(), 2);
        let (none, errs) = load_dir(&tmp.path().join("missing"));
        assert!(none.is_empty() && errs.is_empty());
    }

    #[test]
    fn starter_skills_parse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
        let (skills, errors) = load_dir(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(skills.len() >= 10, "{}", skills.len());
    }
}
