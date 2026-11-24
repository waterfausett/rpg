//! Installs/removes the global git hook shims (`core.hooksPath`) and the Claude Code
//! SessionStart/SessionEnd hooks that call `rpg emit …`.
//!
//! Safety: the shims chain to whatever hook git would have run without us (the previous global
//! `core.hooksPath`, else the repo's own `hooks/` dir), so repo hooks keep working.
use crate::paths::Paths;
use crate::proc;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

const SHIM_TEMPLATE: &str = include_str!("../assets/hooks/shim.sh");
const PREV_FILE: &str = "prev_hookspath";
const CLAUDE_MARKER: &str = "emit claude-session-";

/// Every standard git hook name. A shim is written for each so that setting a global
/// `core.hooksPath` never silently disables a repo's own hooks.
pub const HOOK_NAMES: [&str; 27] = [
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
    "post-index-change",
    "reference-transaction",
];

/// Environment applied to every `git` call the installer makes. `inherit()` uses the real
/// environment; tests add `GIT_CONFIG_GLOBAL`/`HOME` to redirect the "global" config.
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
        for (k, v) in &self.vars {
            c.env(k, v);
        }
        c
    }

    /// `git config --global <args>`. `Ok(None)` when git reports the key as unset (exit 1).
    fn config(&self, args: &[&str]) -> Result<Option<String>> {
        let mut all = vec!["config", "--global"];
        all.extend_from_slice(args);
        let out = self.git(&all).output().context("running git")?;
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim_end_matches(['\r', '\n']).to_string())),
            Some(1) | Some(5) if args.iter().any(|a| *a == "--get" || *a == "--unset") => Ok(None),
            _ => bail!("git {}: {}", all.join(" "), String::from_utf8_lossy(&out.stderr).trim()),
        }
    }

    /// Resolve a `core.hooksPath`-style value the way git does (`~` expansion).
    fn expand_path(&self, raw: &str) -> Result<String> {
        let kv = format!("rpg.tmp={raw}");
        let out = self.git(&["-c", &kv, "config", "--type=path", "--get", "rpg.tmp"]).output()?;
        if !out.status.success() {
            return Ok(raw.to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches(['\r', '\n']).to_string())
    }
}

/// The global `core.hooksPath`, if set.
pub fn global_hooks_path(git: &GitEnv) -> Result<Option<String>> {
    git.config(&["--get", "core.hooksPath"])
}

/// Write shims for every hook into `<data>/git-hooks/` and point the global `core.hooksPath`
/// at it. The previous value is saved to `<data>/prev_hookspath` (only on first install).
pub fn install_git_hooks(paths: &Paths, rpg_bin: &Path, git: &GitEnv) -> Result<()> {
    let dir = paths.git_hooks();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let ours = slash(&dir);
    let prev_file = paths.data.join(PREV_FILE);

    let current = global_hooks_path(git)?;
    let current_is_ours = current.as_deref().is_some_and(|c| same_dir(&git.expand_path(c).unwrap_or_default(), &dir));
    let prev_raw = if current_is_ours {
        // Re-install: keep what we saved the first time; never record our own dir as "previous".
        std::fs::read_to_string(&prev_file).unwrap_or_default()
    } else {
        let v = current.unwrap_or_default();
        std::fs::write(&prev_file, &v).with_context(|| format!("writing {}", prev_file.display()))?;
        v
    };
    let mut prev = if prev_raw.trim().is_empty() { String::new() } else { slash(Path::new(&git.expand_path(prev_raw.trim())?)) };
    if !prev.is_empty() && same_dir(&prev, &dir) {
        prev.clear(); // chaining to ourselves would loop
    }

    for hook in HOOK_NAMES {
        let p = dir.join(hook);
        std::fs::write(&p, render_shim(hook, rpg_bin, &prev)).with_context(|| format!("writing {}", p.display()))?;
        proc::make_executable(&p)?;
    }
    git.config(&["core.hooksPath", &ours])?;
    Ok(())
}

/// Restore the saved global `core.hooksPath` (or unset it). Leaves the setting alone if it no
/// longer points at our dir (the user changed it since).
pub fn uninstall_git_hooks(paths: &Paths, git: &GitEnv) -> Result<()> {
    let dir = paths.git_hooks();
    let prev_file = paths.data.join(PREV_FILE);
    let current = global_hooks_path(git)?;
    let current_is_ours = current.as_deref().is_some_and(|c| same_dir(&git.expand_path(c).unwrap_or_default(), &dir));
    if !current_is_ours {
        bail!(
            "global core.hooksPath is {}, not rpg's; leaving it unchanged",
            current.as_deref().map(|c| format!("'{c}'")).unwrap_or_else(|| "unset".into())
        );
    }
    let prev = std::fs::read_to_string(&prev_file).unwrap_or_default();
    let prev = prev.trim();
    if prev.is_empty() {
        git.config(&["--unset", "core.hooksPath"])?;
    } else {
        git.config(&["core.hooksPath", prev])?;
    }
    let _ = std::fs::remove_file(&prev_file);
    for hook in HOOK_NAMES {
        let _ = std::fs::remove_file(dir.join(hook));
    }
    Ok(())
}

/// The shim for `hook`. `prev` is the previous global hooks dir ("" when none).
pub fn render_shim(hook: &str, rpg_bin: &Path, prev: &str) -> String {
    SHIM_TEMPLATE
        .replace("\r\n", "\n")
        .replace("__RPG_BIN__", &sh_quote(&slash(rpg_bin)))
        .replace("__HOOK__", &sh_quote(hook))
        .replace("__PREV__", &sh_quote(prev))
}

/// Claude Code's user settings file, `~/.claude/settings.json`.
pub fn claude_settings_path() -> Result<PathBuf> {
    Ok(proc::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?.join(".claude").join("settings.json"))
}

/// Merge rpg's SessionStart/SessionEnd entries into Claude Code's `settings.json`, after
/// backing it up to `settings.json.rpg-backup`. Idempotent; all other content is preserved.
pub fn install_claude_hooks(settings_path: &Path, rpg_bin: &Path) -> Result<()> {
    let mut root = read_settings(settings_path)?;
    if settings_path.exists() {
        let backup = sibling(settings_path, ".rpg-backup");
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
            "hooks": [{ "type": "command", "command": format!("\"{bin}\" emit {arg}") }]
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
