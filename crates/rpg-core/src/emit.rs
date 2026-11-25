use crate::event::{Event, EventKind, Source};
use crate::proc;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::path::Path;

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = proc::command("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Resolve a numstat rename entry to the new path: `a => b` or `dir/{a => b}/f`.
fn new_path(p: &str) -> String {
    if let (Some(open), Some(close)) = (p.find('{'), p.find('}')) {
        if let Some((_, new)) = p[open + 1..close].split_once(" => ") {
            let joined = format!("{}{}{}", &p[..open], new, &p[close + 1..]);
            return joined.replace("//", "/");
        }
    }
    match p.split_once(" => ") {
        Some((_, new)) => new.to_string(),
        None => p.to_string(),
    }
}

/// Extract `owner/name` from a GitHub remote URL (https or scp-style ssh); None for other hosts.
pub fn github_repo_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = ["https://github.com/", "http://github.com/", "git@github.com:", "ssh://git@github.com/"]
        .iter()
        .find_map(|p| url.strip_prefix(p))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then(|| format!("{owner}/{name}"))
}

/// The GitHub `owner/name` of `origin`; None when there is no origin or it is not GitHub.
fn github_repo(repo: &Path) -> Option<String> {
    git(repo, &["remote", "get-url", "origin"]).ok().and_then(|u| github_repo_from_url(&u))
}

fn toplevel(repo: &Path) -> Result<String> {
    git(repo, &["rev-parse", "--show-toplevel"])
}

/// Build a Commit event from `git show --numstat`. Binary files count as 0.
pub fn commit_event(repo: &Path, hash: &str) -> Result<Event> {
    let top = toplevel(repo)?;
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let stat = git(repo, &["-c", "core.quotepath=false", "show", "--numstat", "--format=", hash])?;
    let message = git(repo, &["show", "-s", "--format=%B", hash])?;
    let (mut added, mut removed) = (0u64, 0u64);
    let mut files = Vec::new();
    for line in stat.lines().filter(|l| !l.trim().is_empty()) {
        let mut it = line.splitn(3, '\t');
        let (a, r, path) = match (it.next(), it.next(), it.next()) {
            (Some(a), Some(r), Some(p)) => (a, r, p),
            _ => continue,
        };
        let a = a.parse::<u64>().unwrap_or(0);
        let r = r.parse::<u64>().unwrap_or(0);
        added += a;
        removed += r;
        files.push(json!({ "path": new_path(path), "added": a, "removed": r }));
    }
    Ok(Event::new(
        EventKind::Commit,
        Source::Git,
        Some(top.clone()),
        json!({
            "hash": hash, "branch": branch, "repo_path": top,
            "files": files, "lines_added": added, "lines_removed": removed,
            "message": message, "github_repo": github_repo(repo),
        }),
    ))
}

pub fn push_event(repo: &Path, remote: &str) -> Result<Event> {
    let top = toplevel(repo)?;
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(Event::new(
        EventKind::Push,
        Source::Git,
        Some(top.clone()),
        json!({ "remote": remote, "branch": branch, "repo_path": top, "github_repo": github_repo(repo) }),
    ))
}

/// Build a Claude session event from the hook's stdin JSON.
pub fn claude_event(kind: EventKind, stdin_json: &str) -> Result<Event> {
    if !matches!(kind, EventKind::ClaudeSessionStart | EventKind::ClaudeSessionEnd) {
        bail!("not a claude event kind: {}", kind.as_str());
    }
    let v: serde_json::Value = serde_json::from_str(stdin_json).context("parsing hook stdin")?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let session_id = get("session_id").ok_or_else(|| anyhow!("missing session_id"))?;
    let cwd = get("cwd").unwrap_or_default();
    let repo = (!cwd.is_empty()).then(|| cwd.clone());
    Ok(Event::new(kind, Source::Claude, repo, json!({ "session_id": session_id, "cwd": cwd })))
}
