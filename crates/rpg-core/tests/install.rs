//! Hook installer tests. Every git child process and every installer call is redirected to a
//! temp global config / HOME so the real user's git setup and Claude settings are never touched.
use rpg_core::install::{self, GitEnv};
use rpg_core::paths::Paths;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Sandbox {
    tmp: tempfile::TempDir,
    paths: Paths,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            tmp.path().join("gitconfig"),
            "[user]\n\tname = t\n\temail = t@example.com\n[commit]\n\tgpgsign = false\n\
             [core]\n\tautocrlf = false\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let paths = Paths::from_home(Some(tmp.path().join("rpghome"))).unwrap();
        Sandbox { tmp, paths }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn vars(&self) -> Vec<(&'static str, PathBuf)> {
        let home = self.root().join("home");
        vec![
            ("GIT_CONFIG_GLOBAL", self.root().join("gitconfig")),
            ("GIT_CONFIG_NOSYSTEM", PathBuf::from("1")),
            ("HOME", home.clone()),
            ("USERPROFILE", home),
            ("RPG_HOME", self.root().join("rpghome")),
        ]
    }

    fn genv(&self) -> GitEnv {
        self.vars().into_iter().fold(GitEnv::inherit(), |g, (k, v)| g.with(k, v))
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Output {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir).args(args).env_remove("RPG_INTERNAL");
        for (k, v) in self.vars() {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn git_ok(&self, dir: &Path, args: &[&str]) -> String {
        let o = self.git(dir, args);
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn global_hooks_path(&self) -> Option<String> {
        let o = self.git(self.root(), &["config", "--global", "--get", "core.hooksPath"]);
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn repo(&self, name: &str) -> PathBuf {
        let d = self.root().join(name);
        std::fs::create_dir_all(&d).unwrap();
        self.git_ok(&d, &["init", "-q"]);
        std::fs::write(d.join("a.txt"), "1\n").unwrap();
        self.git_ok(&d, &["add", "."]);
        d
    }

    fn install(&self, rpg_bin: &Path) {
        install::install_git_hooks(&self.paths, rpg_bin, &self.genv()).unwrap();
    }
}

fn write_hook(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn poll_file(p: &Path) -> String {
    let start = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(p) {
            if !s.is_empty() {
                return s;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {}", p.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn failing_repo_precommit_still_blocks_commit() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    write_hook(&repo.join(".git/hooks/pre-commit"), "exit 1");
    sb.install(&sb.root().join("nope"));
    assert!(sb.global_hooks_path().is_some());
    let o = sb.git(&repo, &["commit", "-q", "-m", "x"]);
    assert!(!o.status.success(), "repo pre-commit hook must still block the commit");
}

#[test]
fn commit_succeeds_when_rpg_binary_missing() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    sb.install(&sb.root().join("nope"));
    sb.git_ok(&repo, &["commit", "-q", "-m", "x"]);
    assert_eq!(sb.git_ok(&repo, &["rev-list", "--count", "HEAD"]), "1");
}

#[test]
fn pre_push_passes_stdin_and_exit_code() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    sb.git_ok(&repo, &["commit", "-q", "-m", "x"]);
    let remote = sb.root().join("remote.git");
    sb.git_ok(sb.root(), &["init", "-q", "--bare", &slash(&remote)]);
    let got = sb.root().join("prepush-stdin.txt");
    write_hook(&repo.join(".git/hooks/pre-push"), &format!("cat > '{}'\nexit 1", slash(&got)));
    // A stand-in rpg that records how it was called; proves our emit runs without eating stdin.
    let fake = sb.root().join("fake-rpg");
    let calls = sb.root().join("fake-calls.txt");
    write_hook(&fake, &format!("echo \"$@\" >> '{}'", slash(&calls)));
    sb.install(&fake);

    let o = sb.git(&repo, &["push", "-q", &slash(&remote), "HEAD:refs/heads/main"]);
    assert!(!o.status.success(), "repo pre-push exit 1 must fail the push");
    let stdin = std::fs::read_to_string(&got).unwrap();
    assert!(stdin.contains("refs/heads/main"), "pre-push stdin was: {stdin:?}");
    let call = poll_file(&calls);
    assert!(call.contains("emit push --repo"), "fake rpg called with: {call:?}");
    assert!(call.contains(&format!("--remote {}", slash(&remote))), "fake rpg called with: {call:?}");
}

#[test]
fn uninstall_restores_previous_hookspath() {
    let sb = Sandbox::new();
    let prev = slash(&sb.root().join("myhooks"));
    sb.git_ok(sb.root(), &["config", "--global", "core.hooksPath", &prev]);
    sb.install(&sb.root().join("nope"));
    sb.install(&sb.root().join("nope")); // re-install must not record our own dir as "previous"
    let ours = sb.global_hooks_path().unwrap();
    assert_ne!(ours, prev);
    install::uninstall_git_hooks(&sb.paths, &sb.genv()).unwrap();
    assert_eq!(sb.global_hooks_path().as_deref(), Some(prev.as_str()));
}

#[test]
fn uninstall_unsets_when_nothing_was_set() {
    let sb = Sandbox::new();
    sb.install(&sb.root().join("nope"));
    assert!(sb.global_hooks_path().is_some());
    install::uninstall_git_hooks(&sb.paths, &sb.genv()).unwrap();
    assert_eq!(sb.global_hooks_path(), None);
}

#[test]
fn chains_to_previous_global_hookspath() {
    let sb = Sandbox::new();
    let prev = sb.root().join("myhooks");
    write_hook(&prev.join("pre-commit"), "exit 1");
    sb.git_ok(sb.root(), &["config", "--global", "core.hooksPath", &slash(&prev)]);
    sb.install(&sb.root().join("nope"));
    sb.install(&sb.root().join("nope"));
    let repo = sb.repo("r");
    let o = sb.git(&repo, &["commit", "-q", "-m", "x"]);
    assert!(!o.status.success(), "previous global pre-commit must still block the commit");
}

#[test]
fn push_to_checkout_keeps_default_update_instead() {
    // Git skips its built-in updateInstead logic whenever a push-to-checkout hook exists,
    // so our shim must reproduce it when the repo has no hook of its own.
    let sb = Sandbox::new();
    let target = sb.repo("target");
    sb.git_ok(&target, &["commit", "-q", "-m", "x"]);
    sb.git_ok(&target, &["config", "receive.denyCurrentBranch", "updateInstead"]);
    let clone = sb.root().join("clone");
    sb.git_ok(sb.root(), &["clone", "-q", &slash(&target), &slash(&clone)]);
    sb.install(&sb.root().join("nope"));
    std::fs::write(clone.join("b.txt"), "new\n").unwrap();
    sb.git_ok(&clone, &["add", "."]);
    sb.git_ok(&clone, &["commit", "-q", "-m", "y"]);
    sb.git_ok(&clone, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(std::fs::read_to_string(target.join("b.txt")).unwrap(), "new\n");
}

#[test]
fn shims_cover_every_hook_with_lf_endings() {
    let sb = Sandbox::new();
    sb.install(&sb.root().join("nope"));
    for name in install::HOOK_NAMES {
        let s = std::fs::read_to_string(sb.paths.git_hooks().join(name)).unwrap();
        assert!(s.starts_with("#!/bin/sh\n"), "{name}");
        assert!(!s.contains('\r'), "{name} has CR");
        assert!(!s.contains("__"), "{name} has an unsubstituted placeholder");
    }
    assert_eq!(install::HOOK_NAMES.len(), 27);
}

#[test]
fn claude_settings_merge_preserves_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    let stop = serde_json::json!([{ "hooks": [{ "type": "command", "command": "echo stop" }] }]);
    let orig = serde_json::json!({ "model": "opus", "hooks": { "Stop": stop } });
    std::fs::write(&settings, serde_json::to_string_pretty(&orig).unwrap()).unwrap();
    let bin = tmp.path().join("bin").join("rpg.exe");

    install::install_claude_hooks(&settings, &bin).unwrap();
    install::install_claude_hooks(&settings, &bin).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["model"], "opus");
    assert_eq!(v["hooks"]["Stop"], stop);
    let rpg_cmds = |ev: &str| -> Vec<String> {
        v["hooks"][ev]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .map(|h| h["command"].as_str().unwrap().to_string())
            .filter(|c| c.contains("emit claude-session-"))
            .collect()
    };
    let start = rpg_cmds("SessionStart");
    assert_eq!(start, vec![format!("\"{}\" emit claude-session-start", slash(&bin))]);
    assert_eq!(rpg_cmds("SessionEnd").len(), 1);
    assert_eq!(v["hooks"]["SessionStart"][0]["hooks"][0]["type"], "command");
    let backup: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join("settings.json.rpg-backup")).unwrap())
            .unwrap();
    assert_eq!(backup["model"], "opus");

    install::uninstall_claude_hooks(&settings).unwrap();
    let raw = std::fs::read_to_string(&settings).unwrap();
    assert!(!raw.contains("emit claude-session-"), "{raw}");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["model"], "opus");
    assert_eq!(v["hooks"]["Stop"], stop);
}

#[test]
fn claude_install_creates_missing_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join(".claude").join("settings.json");
    install::install_claude_hooks(&settings, Path::new("/x/rpg")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0]["command"], "\"/x/rpg\" emit claude-session-end");
}

#[test]
fn claude_install_refuses_to_clobber_invalid_json() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    std::fs::write(&settings, "{ not json").unwrap();
    assert!(install::install_claude_hooks(&settings, Path::new("/x/rpg")).is_err());
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{ not json");
}
