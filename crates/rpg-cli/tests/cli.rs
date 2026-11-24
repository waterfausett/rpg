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

#[test]
fn emit_home_flag_overrides_rpg_home_env() {
    let env_home = tempfile::tempdir().unwrap();
    let flag_home = tempfile::tempdir().unwrap();
    rpg(env_home.path())
        .args(["emit", "claude-session-start", "--home"])
        .arg(flag_home.path())
        .write_stdin(r#"{"session_id":"s","cwd":"/x"}"#)
        .assert()
        .success();
    assert_eq!(inbox_files(flag_home.path()), 1);
    assert_eq!(inbox_files(env_home.path()), 0);
}

#[test]
fn rpg_internal_only_silences_emit() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let gitconfig = tmp.path().join("gitconfig");
    std::fs::write(&gitconfig, "").unwrap();
    let out = rpg(&tmp.path().join("rpghome"))
        .env("RPG_INTERNAL", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .arg("install-hooks")
        .assert()
        .success();
    let said = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(said.contains("git hooks:"), "install-hooks must not be silenced: {said:?}");
    assert!(std::fs::read_to_string(&gitconfig).unwrap().contains("hooksPath"));
    let out = rpg(&tmp.path().join("rpghome"))
        .env("RPG_INTERNAL", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .arg("uninstall-hooks")
        .assert()
        .success();
    let said = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(said.contains("git hooks:"), "uninstall-hooks must not be silenced: {said:?}");
    assert!(!std::fs::read_to_string(&gitconfig).unwrap().contains("hooksPath"));
}
