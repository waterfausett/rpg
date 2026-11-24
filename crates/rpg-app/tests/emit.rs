//! `rpg-app` doubles as the hook target: `rpg-app emit …` records an event and exits without
//! starting the tray app, so the installers need no separate `rpg` binary (#12). All git config
//! and HOME are redirected to a temp dir so the real user's setup is never touched.
use assert_cmd::Command;
use rpg_core::install::{self, GitEnv};
use rpg_core::paths::Paths;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn app(home: &Path) -> Command {
    let mut c = Command::cargo_bin("rpg-app").unwrap();
    c.env("RPG_HOME", home).env_remove("RPG_INTERNAL").timeout(Duration::from_secs(20));
    c
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

#[test]
fn app_binary_emits_claude_event_and_exits() {
    let home = tempfile::tempdir().unwrap();
    app(home.path())
        .args(["emit", "claude-session-start"])
        .write_stdin(r#"{"session_id":"s","cwd":"/x"}"#)
        .assert()
        .success();
    let evs = json_events(&home.path().join("data").join("inbox"));
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert_eq!(evs[0]["kind"], "claude_session_start");
}

#[test]
fn app_binary_emit_never_fails_the_caller() {
    let home = tempfile::tempdir().unwrap();
    app(home.path()).args(["emit", "no-such-event"]).assert().success();
    app(home.path()).env("RPG_INTERNAL", "1").args(["emit", "claude-session-end"]).write_stdin("{}").assert().success();
    assert!(json_events(&home.path().join("data").join("inbox")).is_empty());
}

/// The tray installs shims that call the app itself; a real commit must land in the inbox.
#[test]
fn git_shim_calling_app_binary_records_commit() {
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
    let paths = Paths::from_home(Some(root.join("rpghome"))).unwrap();
    install::install_git_hooks(&paths, &assert_cmd::cargo::cargo_bin("rpg-app"), &genv).unwrap();

    let repo = root.join("r");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let mut c = std::process::Command::new("git");
        c.arg("-C").arg(&repo).args(args).env_remove("RPG_INTERNAL").env_remove("RPG_HOME");
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
    let evs = loop {
        let evs = json_events(&paths.inbox());
        if !evs.is_empty() || start.elapsed() > Duration::from_secs(10) {
            break evs;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(evs.iter().filter(|e| e["kind"] == "commit").count(), 1, "events: {evs:?}");
}
