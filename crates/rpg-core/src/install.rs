//! Installs/removes the global git hook shims (`core.hooksPath`) and the Claude Code
//! SessionStart/SessionEnd hooks that call `rpg emit …`.
//!
//! Safety: the shims chain to whatever hook git would have run without us (the previously
//! effective `core.hooksPath` from system/global config, including `[include]`d files, else the
//! repo's own `hooks/` dir), so repo and corporate hooks keep working.
use crate::paths::Paths;
use crate::proc;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SHIM_TEMPLATE: &str = include_str!("../assets/hooks/shim.sh");
/// The effective (system/global, includes followed) `core.hooksPath` before install: the chain target.
const PREV_FILE: &str = "prev_hookspath";
/// The value written directly in the global config file before install: what uninstall restores.
const PREV_GLOBAL_FILE: &str = "prev_hookspath_global";
const CLAUDE_MARKER: &str = "emit claude-session-";
/// Include files nested deeper than this are assumed to set `core.hooksPath` (fail safe).
const MAX_INCLUDE_DEPTH: usize = 8;

/// The standard git hooks we shim, so that a global `core.hooksPath` never silently disables a
/// repo's own hooks.
///
/// Two standard hooks are deliberately left out because they fire far too often for a ~75 ms
/// `sh` start-up each (measured on Windows): `reference-transaction` (several times per commit,
/// fetch or checkout) and `post-index-change` (on most index writes, even `git status`). The
/// cost: a repo-local `.git/hooks/` copy of either one does not run while rpg is installed.
pub const HOOK_NAMES: [&str; 25] = [
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "post-receive",
    "post-update",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
];

/// Shims written by earlier versions, removed on (re-)install and uninstall.
const RETIRED_HOOK_NAMES: [&str; 2] = ["post-index-change", "reference-transaction"];

/// Environment applied to every `git` call the installer makes. `inherit()` uses the real
/// environment; tests add `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`/`HOME` to redirect config.
#[derive(Debug, Clone, Default)]
pub struct GitEnv {
    vars: Vec<(OsString, OsString)>,
}

impl GitEnv {
    pub fn inherit() -> GitEnv {
        GitEnv::default()
    }

    pub fn with(mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> GitEnv {
        self.vars.push((key.as_ref().into(), val.as_ref().into()));
        self
    }

    fn git(&self, args: &[&str]) -> Command {
        let mut c = proc::command("git");
        c.args(args);
        // We may be started from inside a hook; never act on that hook's repository.
        for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_PREFIX"] {
            c.env_remove(k);
        }
        for (k, v) in &self.vars {
            c.env(k, v);
        }
        c
    }

    /// Run git from `cwd` with repository discovery stopped at `cwd`, so only system, global
    /// and included config is visible (never a repo's local config or a matching `includeIf`).
    fn outside_repo(&self, cwd: &Path, args: &[&str]) -> Result<Output> {
        let mut c = self.git(args);
        c.current_dir(cwd);
        c.env("GIT_CEILING_DIRECTORIES", cwd.parent().unwrap_or(cwd));
        c.output().context("running git")
    }

    /// `git config --global <args>` (the global file itself, no includes).
    /// `Ok(None)` when git reports the key as unset.
    fn config(&self, args: &[&str]) -> Result<Option<String>> {
        let mut all = vec!["config", "--global"];
        all.extend_from_slice(args);
        let out = self.git(&all).output().context("running git")?;
        match out.status.code() {
            Some(0) => Ok(Some(trim_nl(&out.stdout))),
            Some(1) if args.contains(&"--get") => Ok(None),
            Some(5) if args.contains(&"--unset-all") => Ok(None),
            _ => bail!("git {}: {}", all.join(" "), String::from_utf8_lossy(&out.stderr).trim()),
        }
    }

    fn set_global(&self, value: Option<&str>) -> Result<()> {
        match value.filter(|v| !v.trim().is_empty()) {
            Some(v) => self.config(&["--replace-all", "core.hooksPath", v.trim()]),
            None => self.config(&["--unset-all", "core.hooksPath"]),
        }
        .map(|_| ())
    }

    /// Resolve a `core.hooksPath`-style value the way git does (`~` expansion).
    fn expand_path(&self, raw: &str) -> Result<String> {
        let kv = format!("rpg.tmp={raw}");
        let out = self.git(&["-c", &kv, "config", "--type=path", "--get", "rpg.tmp"]).output()?;
        if !out.status.success() {
            return Ok(raw.to_string());
        }
        Ok(trim_nl(&out.stdout))
    }
}

/// The `core.hooksPath` written directly in the global config file, if any.
pub fn global_hooks_path(git: &GitEnv) -> Result<Option<String>> {
    git.config(&["--get", "core.hooksPath"])
}

/// The `core.hooksPath` git uses outside any repo: system and global config, following
/// `[include]`s (last one wins). `cwd` must be a directory that is not inside a repository.
pub fn effective_hooks_path(git: &GitEnv, cwd: &Path) -> Result<Option<String>> {
    let out = git.outside_repo(cwd, &["config", "--includes", "--show-scope", "-z", "--get-all", "core.hooksPath"])?;
    match out.status.code() {
        Some(0) => {}
        Some(1) => return Ok(None),
        _ => bail!("git config --get-all core.hooksPath: {}", String::from_utf8_lossy(&out.stderr).trim()),
    }
    let fields = nul_fields(&out.stdout);
    Ok(fields
        .chunks(2)
        .filter(|kv| kv.len() == 2 && (kv[0] == "system" || kv[0] == "global"))
        .map(|kv| kv[1].clone())
        .next_back())
}

/// If an `[includeIf]` in system/global config (or in a file it includes) can set
/// `core.hooksPath`, describe it. Such a value depends on the repo, so it can't be chained to.
fn conditional_hooks_path(git: &GitEnv, cwd: &Path) -> Result<Option<String>> {
    let out = git.outside_repo(
        cwd,
        &["config", "--includes", "--show-scope", "--show-origin", "-z", "--get-regexp", r"^includeif\..*\.path$"],
    )?;
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    if !out.status.success() {
        bail!("listing includeIf entries: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    for rec in nul_fields(&out.stdout).chunks(3) {
        let [scope, origin, kv] = rec else { continue };
        if scope != "system" && scope != "global" {
            continue;
        }
        let (key, value) = kv.split_once('\n').unwrap_or((kv.as_str(), ""));
        let target = resolve_include(git, origin, value)?;
        if file_may_set_hooks_path(git, cwd, &target, 0)? {
            return Ok(Some(format!("[{key}] -> {}", target.display())));
        }
    }
    Ok(None)
}

/// Does `file` (or anything it includes, conditionally or not) set `core.hooksPath`?
fn file_may_set_hooks_path(git: &GitEnv, cwd: &Path, file: &Path, depth: usize) -> Result<bool> {
    if depth > MAX_INCLUDE_DEPTH {
        return Ok(true);
    }
    if !file.is_file() {
        return Ok(false);
    }
    let file_s = file.to_string_lossy();
    let out = git.outside_repo(
        cwd,
        &["config", "--file", &file_s, "--includes", "--show-origin", "-z", "--get-regexp", r"^(core\.hookspath|includeif\..*\.path)$"],
    )?;
    if out.status.code() == Some(1) {
        return Ok(false);
    }
    if !out.status.success() {
        return Ok(true); // unreadable/unparseable: assume the worst
    }
    for rec in nul_fields(&out.stdout).chunks(2) {
        let [origin, kv] = rec else { continue };
        let (key, value) = kv.split_once('\n').unwrap_or((kv.as_str(), ""));
        if key == "core.hookspath" {
            return Ok(true);
        }
        if file_may_set_hooks_path(git, cwd, &resolve_include(git, origin, value)?, depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// An include path as git resolves it: `~` expanded, relative to the including file's dir.
fn resolve_include(git: &GitEnv, origin: &str, value: &str) -> Result<PathBuf> {
    let p = PathBuf::from(git.expand_path(value)?);
    if p.is_absolute() {
        return Ok(p);
    }
    let base = origin.strip_prefix("file:").map(Path::new).and_then(Path::parent);
    Ok(base.map(|b| b.join(&p)).unwrap_or(p))
}

/// Write shims for every hook into `<data>/git-hooks/` and point the global `core.hooksPath`
/// at it. This always changes the real global git config, even with `RPG_HOME` set; when it is,
/// the shims pass `--home <RPG_HOME>` so their events go to that home. On first install the
/// previously effective value (chain target) and the global file's own value (restored by
/// uninstall) are saved under `<data>/`.
///
/// Refuses, changing nothing, when an `[includeIf]` could set `core.hooksPath`. If our global
/// value turns out to be shadowed (e.g. by an `[include]` placed after it), the global config
/// is put back as it was and an error is returned.
pub fn install_git_hooks(paths: &Paths, rpg_bin: &Path, git: &GitEnv) -> Result<()> {
    let dir = paths.git_hooks();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let cwd = &paths.data;
    let ours = slash(&dir);
    let home = paths.home.as_deref().map(slash).unwrap_or_default();

    if let Some(what) = conditional_hooks_path(git, cwd)? {
        bail!(
            "refusing to install git hooks: core.hooksPath may be set by a conditional include \
             (includeIf {what}); its value depends on the repository, so rpg could not chain to \
             those hooks and they would stop running"
        );
    }

    let global_before = global_hooks_path(git)?;
    let effective = effective_hooks_path(git, cwd)?;
    let is_ours = |v: &Option<String>| v.as_deref().is_some_and(|v| same_dir(&git.expand_path(v).unwrap_or_default(), &dir));
    let prev_file = paths.data.join(PREV_FILE);
    let prev_global_file = paths.data.join(PREV_GLOBAL_FILE);
    let prev_raw = if is_ours(&effective) {
        // Re-install: keep what we saved the first time; never record our own dir as "previous".
        std::fs::read_to_string(&prev_file).unwrap_or_default()
    } else {
        let v = effective.clone().unwrap_or_default();
        std::fs::write(&prev_file, &v).with_context(|| format!("writing {}", prev_file.display()))?;
        std::fs::write(&prev_global_file, global_before.as_deref().unwrap_or_default())
            .with_context(|| format!("writing {}", prev_global_file.display()))?;
        v
    };
    let mut prev =
        if prev_raw.trim().is_empty() { String::new() } else { slash(Path::new(&git.expand_path(prev_raw.trim())?)) };
    if !prev.is_empty() && same_dir(&prev, &dir) {
        prev.clear(); // chaining to ourselves would loop
    }

    for hook in HOOK_NAMES {
        let p = dir.join(hook);
        std::fs::write(&p, render_shim(hook, rpg_bin, &prev, &home)).with_context(|| format!("writing {}", p.display()))?;
        proc::make_executable(&p)?;
    }
    for hook in RETIRED_HOOK_NAMES {
        let _ = std::fs::remove_file(dir.join(hook));
    }
    git.set_global(Some(&ours))?;

    let now = effective_hooks_path(git, cwd)?;
    if !is_ours(&now) {
        git.set_global(global_before.as_deref())?;
        if !is_ours(&effective) {
            let _ = std::fs::remove_file(&prev_file);
            let _ = std::fs::remove_file(&prev_global_file);
        }
        bail!(
            "not installed: rpg's core.hooksPath is shadowed by '{}' set later in your git config \
             (e.g. an [include] after the [core] section); global config restored",
            now.unwrap_or_default()
        );
    }
    Ok(())
}

/// Put the global config file's own `core.hooksPath` back as it was before install (or unset
/// it). Leaves everything alone if the effective value no longer points at our dir.
pub fn uninstall_git_hooks(paths: &Paths, git: &GitEnv) -> Result<()> {
    let dir = paths.git_hooks();
    std::fs::create_dir_all(&paths.data).with_context(|| format!("creating {}", paths.data.display()))?;
    let effective = effective_hooks_path(git, &paths.data)?;
    if !effective.as_deref().is_some_and(|c| same_dir(&git.expand_path(c).unwrap_or_default(), &dir)) {
        bail!(
            "core.hooksPath is {}, not rpg's; leaving it unchanged",
            effective.as_deref().map(|c| format!("'{c}'")).unwrap_or_else(|| "unset".into())
        );
    }
    let prev_file = paths.data.join(PREV_FILE);
    let prev_global_file = paths.data.join(PREV_GLOBAL_FILE);
    let restore = std::fs::read_to_string(&prev_global_file)
        .or_else(|_| std::fs::read_to_string(&prev_file))
        .unwrap_or_default();
    git.set_global(Some(&restore))?;
    let _ = std::fs::remove_file(&prev_file);
    let _ = std::fs::remove_file(&prev_global_file);
    for hook in HOOK_NAMES.iter().chain(&RETIRED_HOOK_NAMES) {
        let _ = std::fs::remove_file(dir.join(hook));
    }
    Ok(())
}

/// The shim for `hook`. `prev` is the previous global hooks dir ("" when none); `home` is the
/// `RPG_HOME` to report into ("" for the default dirs).
pub fn render_shim(hook: &str, rpg_bin: &Path, prev: &str, home: &str) -> String {
    SHIM_TEMPLATE
        .replace("\r\n", "\n")
        .replace("__RPG_BIN__", &sh_quote(&slash(rpg_bin)))
        .replace("__HOOK__", &sh_quote(hook))
        .replace("__PREV__", &sh_quote(prev))
        .replace("__HOME__", &sh_quote(home))
}

/// Claude Code's user settings file, `~/.claude/settings.json`.
pub fn claude_settings_path() -> Result<PathBuf> {
    Ok(proc::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?.join(".claude").join("settings.json"))
}

/// Merge rpg's SessionStart/SessionEnd entries into Claude Code's `settings.json`, after
/// backing it up to `settings.json.rpg-backup`. Idempotent; all other content is preserved.
/// The backup is written only once, so it always holds the file as it was before rpg touched it.
///
/// `home` (the `RPG_HOME` in effect) is passed as `--home "<home>"`, double-quoted so the command
/// works in both cmd.exe and bash; a path with a character either shell would expand inside
/// double quotes is refused.
pub fn install_claude_hooks(settings_path: &Path, rpg_bin: &Path, home: Option<&Path>) -> Result<()> {
    let home_arg = match home {
        Some(h) => {
            let h = slash(h);
            if let Some(c) = h.chars().find(|c| matches!(c, '"' | '$' | '`' | '%')) {
                bail!("RPG_HOME '{h}' contains '{c}', which a hook command cannot quote safely");
            }
            format!(" --home \"{h}\"")
        }
        None => String::new(),
    };
    let mut root = read_settings(settings_path)?;
    let backup = sibling(settings_path, ".rpg-backup");
    if settings_path.exists() && !backup.exists() {
        std::fs::copy(settings_path, &backup).with_context(|| format!("backing up to {}", backup.display()))?;
    }
    remove_rpg_entries(&mut root)?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("`hooks` in {} is not an object", settings_path.display()))?;
    let bin = slash(rpg_bin);
    for (event, arg) in [("SessionStart", "claude-session-start"), ("SessionEnd", "claude-session-end")] {
        let list = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
            .ok_or_else(|| anyhow!("`hooks.{event}` in {} is not an array", settings_path.display()))?;
        list.push(serde_json::json!({
            "hooks": [{ "type": "command", "command": format!("\"{bin}\" emit {arg}{home_arg}") }]
        }));
    }
    write_settings(settings_path, &root)
}

/// Remove only the hook commands containing `emit claude-session-`.
pub fn uninstall_claude_hooks(settings_path: &Path) -> Result<()> {
    if !settings_path.exists() {
        return Ok(());
    }
    let mut root = read_settings(settings_path)?;
    if remove_rpg_entries(&mut root)? {
        write_settings(settings_path, &root)?;
    }
    Ok(())
}

fn read_settings(path: &Path) -> Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))? {
        Value::Object(m) => Ok(m),
        _ => bail!("{} is not a JSON object", path.display()),
    }
}

fn write_settings(path: &Path, root: &Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = sibling(path, ".rpg-tmp");
    let mut text = serde_json::to_string_pretty(root)?;
    text.push('\n');
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Strip rpg's commands from SessionStart/SessionEnd, dropping groups/lists left empty.
/// Returns whether anything was removed.
fn remove_rpg_entries(root: &mut Map<String, Value>) -> Result<bool> {
    let Some(hooks) = root.get_mut("hooks") else { return Ok(false) };
    let hooks = hooks.as_object_mut().ok_or_else(|| anyhow!("`hooks` is not an object"))?;
    let mut changed = false;
    for event in ["SessionStart", "SessionEnd"] {
        let Some(list) = hooks.get_mut(event) else { continue };
        let list = list.as_array_mut().ok_or_else(|| anyhow!("`hooks.{event}` is not an array"))?;
        list.retain_mut(|group| {
            let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            let before = inner.len();
            inner.retain(|h| !h.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(CLAUDE_MARKER)));
            let removed = inner.len() != before;
            changed |= removed;
            !(removed && inner.is_empty())
        });
        if list.is_empty() {
            hooks.remove(event);
        }
    }
    if changed && hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(changed)
}

fn trim_nl(out: &[u8]) -> String {
    String::from_utf8_lossy(out).trim_end_matches(['\r', '\n']).to_string()
}

/// Split `git config -z` output into its NUL-terminated fields.
fn nul_fields(out: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(out);
    let mut v: Vec<String> = s.split('\0').map(str::to_string).collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// Path as a string with forward slashes (and no Windows `\\?\` prefix).
fn slash(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").map(str::to_string).unwrap_or(s)
}

/// Single-quote for POSIX sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn same_dir(a: &str, b: &Path) -> bool {
    let a = Path::new(a);
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => {
            let norm = |p: &Path| slash(p).trim_end_matches('/').to_lowercase();
            norm(a) == norm(b)
        }
    }
}
