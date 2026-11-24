use rpg_core::enrich::file_ages;
use std::path::Path;
use std::process::Command;

/// Runs git without the user's global/system config, so an installed rpg `core.hooksPath`
/// can't record these test commits as real work.
fn git(dir: &Path, args: &[&str], date: Option<&str>) {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    c.env("GIT_CONFIG_GLOBAL", dir.join("no-global-gitconfig")).env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(d) = date {
        c.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
    }
    assert!(c.status().unwrap().success(), "git {args:?}");
}

#[test]
fn enrich_file_ages_real_git() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"], None);
    git(p, &["config", "user.name", "Old Author"], None);
    git(p, &["config", "user.email", "o@example.com"], None);
    git(p, &["config", "commit.gpgsign", "false"], None);
    git(p, &["config", "core.autocrlf", "false"], None);
    std::fs::write(p.join("old.txt"), "1\n").unwrap();
    git(p, &["add", "."], None);
    let six_years_ago = (chrono::Utc::now() - chrono::Duration::days(2192)).to_rfc3339();
    git(p, &["commit", "-q", "-m", "old"], Some(&six_years_ago));
    std::fs::write(p.join("old.txt"), "1\n2\n").unwrap();
    std::fs::write(p.join("new.txt"), "x\n").unwrap();
    git(p, &["add", "."], None);
    git(p, &["commit", "-q", "-m", "touch"], None);
    let out = Command::new("git").arg("-C").arg(p).args(["rev-parse", "HEAD"]).output().unwrap();
    let head = String::from_utf8(out.stdout).unwrap().trim().to_string();

    let ages = file_ages(p, &head, &["old.txt".into(), "new.txt".into()], 50).unwrap();
    assert_eq!(ages.len(), 1, "new.txt has no prior history: {ages:?}");
    assert_eq!(ages[0].path, "old.txt");
    assert!(ages[0].years > 5.9, "{}", ages[0].years);
    assert_eq!(ages[0].last_author, "Old Author");
}
