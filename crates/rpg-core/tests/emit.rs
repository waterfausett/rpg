use rpg_core::emit::{commit_event, push_event};
use rpg_core::event::{Event, EventKind, Source};
use rpg_core::inbox;
use std::path::Path;
use std::process::Command;

/// Runs git without the user's global/system config, so an installed rpg `core.hooksPath`
/// can't record these test commits as real work.
fn git(dir: &Path, args: &[&str]) {
    let s = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", dir.join("no-global-gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(s.success(), "git {args:?}");
}

fn temp_repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    git(d.path(), &["config", "user.name", "t"]);
    git(d.path(), &["config", "user.email", "t@example.com"]);
    git(d.path(), &["config", "commit.gpgsign", "false"]);
    git(d.path(), &["config", "core.autocrlf", "false"]);
    std::fs::write(d.path().join("a.txt"), "1\n2\n3\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn ev(n: u32) -> Event {
    Event::new(EventKind::Push, Source::Git, Some("r".into()), serde_json::json!({ "n": n }))
}

#[test]
fn commit_event_counts_lines() {
    let d = temp_repo();
    let e = commit_event(d.path(), "HEAD").unwrap();
    assert_eq!(e.kind, EventKind::Commit);
    assert_eq!(e.data["lines_added"], 3);
    assert_eq!(e.data["files"][0]["path"], "a.txt");
    let top = e.repo.expect("repo set");
    assert_eq!(
        std::fs::canonicalize(&top).unwrap(),
        std::fs::canonicalize(d.path()).unwrap()
    );
}

#[test]
fn write_then_drain_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (ev(1), ev(2));
    inbox::write_event(d.path(), &a).unwrap();
    inbox::write_event(d.path(), &b).unwrap();
    let got = inbox::drain(d.path()).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].1, a);
    assert_eq!(got[1].1, b);
    let tmps = std::fs::read_dir(d.path())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(tmps, 0);
}

#[test]
fn drain_quarantines_garbage() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("junk.json"), "{").unwrap();
    assert!(inbox::drain(d.path()).unwrap().is_empty());
    assert!(d.path().join("bad").join("junk.json").exists());
    assert!(!d.path().join("junk.json").exists());
}

#[test]
fn drain_continues_when_quarantine_fails() {
    let d = tempfile::tempdir().unwrap();
    // A plain file named `bad` makes the quarantine dir impossible to create.
    std::fs::write(d.path().join("bad"), "not a dir").unwrap();
    std::fs::write(d.path().join("00-junk.json"), "{").unwrap();
    let good = ev(1);
    inbox::write_event(d.path(), &good).unwrap();
    let got = inbox::drain(d.path()).expect("one bad file must not abort the drain");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, good);
    // The unquarantinable file is left in place for a later attempt.
    assert!(d.path().join("00-junk.json").exists());
}

#[test]
fn commit_event_records_new_path_for_renames_and_unquoted_unicode() {
    let d = temp_repo();
    let p = d.path();
    std::fs::create_dir_all(p.join("dir/old")).unwrap();
    let body: String = (0..30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(p.join("dir/old/f.txt"), &body).unwrap();
    std::fs::write(p.join("plain.txt"), &body).unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-q", "-m", "files"]);
    std::fs::create_dir_all(p.join("dir/new")).unwrap();
    git(p, &["mv", "dir/old/f.txt", "dir/new/f.txt"]);
    git(p, &["mv", "plain.txt", "r\u{e9}nomm\u{e9}.txt"]);
    git(p, &["commit", "-q", "-m", "renames"]);
    let e = commit_event(p, "HEAD").unwrap();
    let paths: Vec<String> =
        e.data["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
    assert!(paths.contains(&"dir/new/f.txt".to_string()), "{paths:?}");
    assert!(paths.contains(&"r\u{e9}nomm\u{e9}.txt".to_string()), "{paths:?}");
    assert!(paths.iter().all(|p| !p.contains("=>") && !p.contains('"')), "{paths:?}");
}

#[test]
fn commit_event_has_message() {
    let d = temp_repo();
    let e = commit_event(d.path(), "HEAD").unwrap();
    assert_eq!(e.data["message"], "init");
    assert!(e.data["github_repo"].is_null()); // no origin
}

#[test]
fn push_event_without_origin_has_null_repo() {
    let d = temp_repo();
    let e = push_event(d.path(), "origin").unwrap();
    assert!(e.data["github_repo"].is_null());
}

#[test]
fn github_repo_parsing() {
    use rpg_core::emit::github_repo_from_url as p;
    assert_eq!(p("https://github.com/acme/app.git").as_deref(), Some("acme/app"));
    assert_eq!(p("git@github.com:acme/app.git").as_deref(), Some("acme/app"));
    assert_eq!(p("https://github.com/acme/app").as_deref(), Some("acme/app"));
    assert_eq!(p("https://gitlab.com/acme/app.git"), None);
}

#[test]
fn events_record_github_origin() {
    let d = temp_repo();
    git(d.path(), &["remote", "add", "origin", "https://github.com/acme/app.git"]);
    let c = commit_event(d.path(), "HEAD").unwrap();
    assert_eq!(c.data["github_repo"], "acme/app");
    let p = push_event(d.path(), "origin").unwrap();
    assert_eq!(p.data["github_repo"], "acme/app");
}
