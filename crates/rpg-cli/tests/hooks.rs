//! End-to-end: git hook shims calling the real `rpg` binary. All git config / HOME is redirected
//! to a temp dir so the real user's global git setup is never touched.
use rpg_core::install::{self, GitEnv};
use rpg_core::paths::Paths;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn commit_emits_event() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        root.join("gitconfig"),
        "[user]\n\tname = t\n\temail = t@example.com\n[commit]\n\tgpgsign = false\n",
    )
    .unwrap();
    let vars: Vec<(&str, PathBuf)> = vec![
        ("GIT_CONFIG_GLOBAL", root.join("gitconfig")),
        ("GIT_CONFIG_NOSYSTEM", PathBuf::from("1")),
        ("HOME", home.clone()),
        ("USERPROFILE", home),
        ("RPG_HOME", root.join("rpghome")),
    ];
    let genv = vars.iter().fold(GitEnv::inherit(), |g, (k, v)| g.with(k, v));
    let git = |dir: &Path, args: &[&str]| {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir).args(args).env_remove("RPG_INTERNAL");
        for (k, v) in &vars {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };

    let paths = Paths::from_home(Some(root.join("rpghome"))).unwrap();
    install::install_git_hooks(&paths, &assert_cmd::cargo::cargo_bin("rpg"), &genv).unwrap();

    let repo = root.join("r");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "1\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "x"]);

    let inbox = paths.inbox();
    let start = Instant::now();
    let events = loop {
        let evs: Vec<serde_json::Value> = std::fs::read_dir(&inbox)
            .map(|r| {
                r.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                    .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
                    .collect()
            })
            .unwrap_or_default();
        if !evs.is_empty() || start.elapsed() > Duration::from_secs(5) {
            break evs;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let commits = events.iter().filter(|e| e["kind"] == "commit").count();
    assert_eq!(commits, 1, "events: {events:?}");
}

#[test]
fn install_and_uninstall_hooks_cli_use_redirected_env() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let gitconfig = root.join("gitconfig");
    std::fs::write(&gitconfig, "").unwrap();
    let rpg = |cmd: &str| {
        let out = assert_cmd::Command::cargo_bin("rpg")
            .unwrap()
            .arg(cmd)
            .env("RPG_HOME", root.join("rpghome"))
            .env("GIT_CONFIG_GLOBAL", &gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("RPG_INTERNAL")
            .assert()
            .success();
        String::from_utf8_lossy(&out.get_output().stdout).to_string()
    };
    let settings = home.join(".claude").join("settings.json");

    let said = rpg("install-hooks");
    assert!(said.contains("core.hooksPath"), "{said}");
    assert!(std::fs::read_to_string(&gitconfig).unwrap().contains("hooksPath"));
    let cmds = std::fs::read_to_string(&settings).unwrap();
    assert!(cmds.contains("emit claude-session-start"), "{cmds}");
    assert!(cmds.contains("--home"), "RPG_HOME must be baked into the Claude hook: {cmds}");

    rpg("uninstall-hooks");
    assert!(!std::fs::read_to_string(&gitconfig).unwrap().contains("hooksPath"));
    assert!(!std::fs::read_to_string(&settings).unwrap().contains("emit claude-session-"));
}

fn json_events(inbox: &Path) -> Vec<serde_json::Value> {
    std::fs::read_dir(inbox)
        .map(|r| {
            r.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Shims installed under a given `RPG_HOME` report into that home, whatever `RPG_HOME` the
/// committing shell has (here a decoy, standing in for "unset, i.e. the real data dir").
#[test]
fn shim_emits_into_install_home() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        root.join("gitconfig"),
        "[user]\n\tname = t\n\temail = t@example.com\n[commit]\n\tgpgsign = false\n",
    )
    .unwrap();
    let vars: Vec<(&str, PathBuf)> = vec![
        ("GIT_CONFIG_GLOBAL", root.join("gitconfig")),
        ("GIT_CONFIG_NOSYSTEM", PathBuf::from("1")),
        ("HOME", home.clone()),
        ("USERPROFILE", home),
    ];
    let genv = vars.iter().fold(GitEnv::inherit(), |g, (k, v)| g.with(k, v));
    let paths = Paths::from_home(Some(root.join("rpg home"))).unwrap();
    install::install_git_hooks(&paths, &assert_cmd::cargo::cargo_bin("rpg"), &genv).unwrap();

    let decoy = root.join("decoy");
    let repo = root.join("r");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.arg("-C").arg(&repo).args(args).env_remove("RPG_INTERNAL").env("RPG_HOME", &decoy);
        for (k, v) in &vars {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "x"]);

    let start = Instant::now();
    let (ours, theirs) = loop {
        let ours = json_events(&paths.inbox());
        let theirs = json_events(&decoy.join("data").join("inbox"));
        if !ours.is_empty() || !theirs.is_empty() || start.elapsed() > Duration::from_secs(5) {
            break (ours, theirs);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(ours.iter().filter(|e| e["kind"] == "commit").count(), 1, "install home inbox: {ours:?}");
    assert!(theirs.is_empty(), "event leaked into the shell's RPG_HOME: {theirs:?}");
}
