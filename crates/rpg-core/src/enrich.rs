//! Commit enrichment: how old each touched file was before this commit.
use crate::proc;
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct FileAge {
    pub path: String,
    pub years: f64,
    pub last_author: String,
}

const SECS_PER_YEAR: f64 = 365.25 * 86_400.0;

/// For each of the first `cap` files, find the last commit before `hash` that
/// touched it. Files with no prior history (new files, root commit) are omitted.
pub fn file_ages(repo: &Path, hash: &str, files: &[String], cap: usize) -> Result<Vec<FileAge>> {
    let now = chrono::Utc::now().timestamp();
    let parent = format!("{hash}^");
    let mut out = Vec::new();
    for path in files.iter().take(cap) {
        let res = proc::command("git")
            .arg("--literal-pathspecs")
            .arg("-C")
            .arg(repo)
            .args(["log", "-1", "--format=%ct%x09%an", &parent, "--", path])
            .output()
            .context("running git log")?;
        if !res.status.success() {
            continue; // e.g. root commit has no parent
        }
        let text = String::from_utf8_lossy(&res.stdout);
        let Some((ct, author)) = text.trim().split_once('\t') else { continue };
        let Ok(ct) = ct.parse::<i64>() else { continue };
        out.push(FileAge {
            path: path.clone(),
            years: ((now - ct).max(0)) as f64 / SECS_PER_YEAR,
            last_author: author.to_string(),
        });
    }
    Ok(out)
}
