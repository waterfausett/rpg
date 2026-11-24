use assert_cmd::Command;

fn rpg(home: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("rpg").unwrap();
    c.env("RPG_HOME", home).env_remove("RPG_INTERNAL");
    c
}

fn inbox_files(home: &std::path::Path) -> usize {
    std::fs::read_dir(home.join("data").join("inbox")).map(|r| r.count()).unwrap_or(0)
}

#[test]
fn emit_is_noop_when_rpg_internal_set() {
    let home = tempfile::tempdir().unwrap();
    rpg(home.path())
        .env("RPG_INTERNAL", "1")
        .args(["emit", "claude-session-start"])
        .write_stdin(r#"{"session_id":"s","cwd":"/x"}"#)
        .assert()
        .success();
    assert_eq!(inbox_files(home.path()), 0);
}

#[test]
fn emit_writes_claude_event() {
    let home = tempfile::tempdir().unwrap();
    rpg(home.path())
        .args(["emit", "claude-session-start"])
        .write_stdin(r#"{"session_id":"s","cwd":"/x"}"#)
        .assert()
        .success();
    assert_eq!(inbox_files(home.path()), 1);
}

#[test]
fn emit_exits_zero_on_error() {
    let home = tempfile::tempdir().unwrap();
    let notrepo = tempfile::tempdir().unwrap();
    rpg(home.path())
        .args(["emit", "commit", "--repo"])
        .arg(notrepo.path())
        .args(["--hash", "x"])
        .assert()
        .success();
    let log = std::fs::read_to_string(home.path().join("data").join("rpg.log")).unwrap();
    assert!(!log.trim().is_empty());
}
