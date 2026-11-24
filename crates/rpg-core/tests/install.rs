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
    /// `GIT_CONFIG_SYSTEM` file; `None` means `GIT_CONFIG_NOSYSTEM=1`.
    system: Option<PathBuf>,
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
        Sandbox { tmp, paths, system: None }
    }

    /// Like `new`, but with a (temp) system config file instead of `GIT_CONFIG_NOSYSTEM`.
    fn with_system(system_config: &str) -> Sandbox {
        let mut sb = Sandbox::new();
        let sys = sb.root().join("systemconfig");
        std::fs::write(&sys, system_config).unwrap();
        sb.system = Some(sys);
        sb
    }

    fn gitconfig(&self) -> PathBuf {
        self.root().join("gitconfig")
    }

    fn append_gitconfig(&self, text: &str) {
        let mut s = std::fs::read_to_string(self.gitconfig()).unwrap();
        s.push_str(text);
        std::fs::write(self.gitconfig(), s).unwrap();
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn vars(&self) -> Vec<(&'static str, PathBuf)> {
        let home = self.root().join("home");
        let system = match &self.system {
            Some(f) => ("GIT_CONFIG_SYSTEM", f.clone()),
            None => ("GIT_CONFIG_NOSYSTEM", PathBuf::from("1")),
        };
        vec![
            ("GIT_CONFIG_GLOBAL", self.gitconfig()),
            system,
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

    /// What git actually uses outside any repo (system + global, following includes).
    fn effective_hooks_path(&self) -> Option<String> {
        let o = self.git(self.root(), &["config", "--includes", "--get", "core.hooksPath"]);
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
    assert!(install::uninstall_git_hooks(&sb.paths, &sb.genv()).unwrap(), "something was removed");
    assert_eq!(sb.global_hooks_path(), None);
}

#[test]
fn uninstall_when_not_installed_is_a_noop() {
    let sb = Sandbox::new();
    assert!(!install::uninstall_git_hooks(&sb.paths, &sb.genv()).unwrap(), "nothing to remove");
    assert_eq!(sb.global_hooks_path(), None);
}

#[test]
fn uninstall_refuses_someone_elses_hookspath() {
    let sb = Sandbox::new();
    let theirs = slash(&sb.root().join("theirs"));
    sb.git_ok(sb.root(), &["config", "--global", "core.hooksPath", &theirs]);
    let e = install::uninstall_git_hooks(&sb.paths, &sb.genv()).unwrap_err();
    assert!(format!("{e:#}").contains("not rpg's"), "{e:#}");
    assert_eq!(sb.global_hooks_path().as_deref(), Some(theirs.as_str()));
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
    assert_eq!(install::HOOK_NAMES.len(), 25);
    for skipped in ["reference-transaction", "post-index-change"] {
        assert!(!install::HOOK_NAMES.contains(&skipped), "{skipped} must not be shimmed (too hot)");
        assert!(!sb.paths.git_hooks().join(skipped).exists());
    }
}

#[test]
fn claude_settings_merge_preserves_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    let stop = serde_json::json!([{ "hooks": [{ "type": "command", "command": "echo stop" }] }]);
    let orig = serde_json::json!({ "model": "opus", "hooks": { "Stop": stop } });
    let orig_text = serde_json::to_string_pretty(&orig).unwrap();
    std::fs::write(&settings, &orig_text).unwrap();
    let bin = tmp.path().join("bin").join("rpg.exe");

    install::install_claude_hooks(&settings, &bin, None).unwrap();
    install::install_claude_hooks(&settings, &bin, None).unwrap();
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
    let backup = std::fs::read_to_string(tmp.path().join("settings.json.rpg-backup")).unwrap();
    assert_eq!(backup, orig_text, "backup must be the pre-install file, even after a second install");

    assert!(install::uninstall_claude_hooks(&settings).unwrap(), "entries removed");
    assert!(!install::uninstall_claude_hooks(&settings).unwrap(), "second uninstall finds nothing");
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
    install::install_claude_hooks(&settings, Path::new("/x/rpg"), None).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0]["command"], "\"/x/rpg\" emit claude-session-end");
}

#[test]
fn claude_install_bakes_rpg_home_into_command() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    install::install_claude_hooks(&settings, Path::new("/x/rpg"), Some(Path::new(r"C:\scratch\rpg home"))).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "\"/x/rpg\" emit claude-session-start --home \"C:/scratch/rpg home\""
    );
    // Characters that cmd.exe or bash would expand inside double quotes are refused.
    for bad in ["a\"b", "a$b", "a`b", "a%b"] {
        assert!(install::install_claude_hooks(&settings, Path::new("/x/rpg"), Some(Path::new(bad))).is_err(), "{bad}");
    }
}

#[test]
fn claude_install_refuses_to_clobber_invalid_json() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    std::fs::write(&settings, "{ not json").unwrap();
    assert!(install::install_claude_hooks(&settings, Path::new("/x/rpg"), None).is_err());
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{ not json");
}

#[test]
fn hookspath_from_global_include_is_chained_and_restored() {
    let sb = Sandbox::new();
    let corp = sb.root().join("corp hooks");
    write_hook(&corp.join("pre-commit"), "exit 1");
    std::fs::write(sb.root().join("inc.cfg"), format!("[core]\n\thooksPath = {}\n", slash(&corp))).unwrap();
    // The include sits first (as org-managed includes usually do), so our value, added to the
    // existing [core] section below it, takes precedence.
    let rest = std::fs::read_to_string(sb.gitconfig()).unwrap();
    std::fs::write(sb.gitconfig(), format!("[include]\n\tpath = inc.cfg\n{rest}")).unwrap();
    assert_eq!(sb.global_hooks_path(), None, "precondition: only visible through the include");
    assert_eq!(sb.effective_hooks_path(), Some(slash(&corp)));

    sb.install(&sb.root().join("nope"));
    assert_ne!(sb.effective_hooks_path(), Some(slash(&corp)));
    let repo = sb.repo("r");
    let o = sb.git(&repo, &["commit", "-q", "-m", "x"]);
    assert!(!o.status.success(), "included corp pre-commit must still block the commit");

    install::uninstall_git_hooks(&sb.paths, &sb.genv()).unwrap();
    assert_eq!(sb.global_hooks_path(), None);
    assert_eq!(sb.effective_hooks_path(), Some(slash(&corp)));
}

#[test]
fn hookspath_from_system_config_is_chained() {
    let corp = tempfile::tempdir().unwrap();
    write_hook(&corp.path().join("pre-commit"), "exit 1");
    let sb = Sandbox::with_system(&format!("[core]\n\thooksPath = {}\n", slash(corp.path())));
    sb.install(&sb.root().join("nope"));
    let repo = sb.repo("r");
    let o = sb.git(&repo, &["commit", "-q", "-m", "x"]);
    assert!(!o.status.success(), "system pre-commit must still block the commit");
    install::uninstall_git_hooks(&sb.paths, &sb.genv()).unwrap();
    assert_eq!(sb.global_hooks_path(), None, "system value must not be copied into global");
    assert_eq!(sb.effective_hooks_path(), Some(slash(corp.path())));
}

#[test]
fn conditional_include_setting_hookspath_makes_install_refuse() {
    let sb = Sandbox::new();
    std::fs::write(sb.root().join("work.cfg"), "[core]\n\thooksPath = C:/corp/hooks\n").unwrap();
    sb.append_gitconfig("[includeIf \"gitdir:~/work/\"]\n\tpath = work.cfg\n");
    let before = std::fs::read(sb.gitconfig()).unwrap();
    let err = install::install_git_hooks(&sb.paths, &sb.root().join("nope"), &sb.genv()).unwrap_err();
    assert!(format!("{err:#}").contains("includeIf"), "{err:#}");
    assert_eq!(std::fs::read(sb.gitconfig()).unwrap(), before, "global config must be untouched");
}

#[test]
fn conditional_include_without_hookspath_is_fine() {
    let sb = Sandbox::new();
    std::fs::write(sb.root().join("work.cfg"), "[user]\n\temail = w@example.com\n").unwrap();
    sb.append_gitconfig("[includeIf \"gitdir:~/work/\"]\n\tpath = work.cfg\n");
    sb.install(&sb.root().join("nope"));
    assert!(sb.global_hooks_path().is_some());
}

#[test]
fn install_rolls_back_when_a_later_include_shadows_ours() {
    let sb = Sandbox::new();
    std::fs::write(sb.root().join("inc.cfg"), "[core]\n\thooksPath = C:/corp/hooks\n").unwrap();
    sb.append_gitconfig("[include]\n\tpath = inc.cfg\n"); // after [core]: our value can't win
    let err = install::install_git_hooks(&sb.paths, &sb.root().join("nope"), &sb.genv()).unwrap_err();
    assert!(format!("{err:#}").contains("shadow"), "{err:#}");
    assert_eq!(sb.global_hooks_path(), None);
    assert_eq!(sb.effective_hooks_path().as_deref(), Some("C:/corp/hooks"));
}

#[test]
fn linked_worktree_chains_to_main_repo_hook() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    sb.git_ok(&repo, &["commit", "-q", "-m", "x"]);
    let wt = sb.root().join("wt");
    sb.git_ok(&repo, &["worktree", "add", "-q", &slash(&wt)]);
    write_hook(&repo.join(".git/hooks/pre-commit"), "exit 1");
    sb.install(&sb.root().join("nope"));
    std::fs::write(wt.join("b.txt"), "b\n").unwrap();
    sb.git_ok(&wt, &["add", "."]);
    let o = sb.git(&wt, &["commit", "-q", "-m", "y"]);
    assert!(!o.status.success(), "main repo pre-commit must run for a linked worktree commit");
}

#[test]
fn rebase_emits_no_commit_events() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    sb.git_ok(&repo, &["commit", "-q", "-m", "base"]);
    sb.git_ok(&repo, &["checkout", "-q", "-b", "side"]);
    for f in ["s1", "s2"] {
        std::fs::write(repo.join(f), "s\n").unwrap();
        sb.git_ok(&repo, &["add", "."]);
        sb.git_ok(&repo, &["commit", "-q", "-m", f]);
    }
    sb.git_ok(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("m1"), "m\n").unwrap();
    sb.git_ok(&repo, &["add", "."]);
    sb.git_ok(&repo, &["commit", "-q", "-m", "m1"]);
    sb.git_ok(&repo, &["checkout", "-q", "side"]);

    let calls = sb.root().join("fake-calls.txt");
    let fake = sb.root().join("fake-rpg");
    write_hook(&fake, &format!("echo \"$1 $2\" >> '{}'", slash(&calls)));
    sb.install(&fake);
    sb.git_ok(&repo, &["rebase", "-q", "main"]);
    assert_eq!(sb.git_ok(&repo, &["rev-list", "--count", "main..side"]), "2");
    std::thread::sleep(Duration::from_millis(500));

    std::fs::write(repo.join("after"), "a\n").unwrap();
    sb.git_ok(&repo, &["add", "."]);
    sb.git_ok(&repo, &["commit", "-q", "-m", "after"]);
    poll_file(&calls);
    std::thread::sleep(Duration::from_millis(300));
    let got = std::fs::read_to_string(&calls).unwrap();
    let commits = got.lines().filter(|l| *l == "emit commit").count();
    assert_eq!(commits, 1, "calls: {got:?}");
}
