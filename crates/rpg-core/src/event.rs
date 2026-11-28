use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Commit,
    /// A commit that concluded a `git merge` which had stopped on conflicts.
    MergeConflictResolved,
    Push,
    PrOpened,
    PrMerged,
    ReviewSubmitted,
    ClaudeSessionStart,
    ClaudeSessionEnd,
    QuestCompleted,
    QuestFailed,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Commit => "commit",
            EventKind::MergeConflictResolved => "merge_conflict_resolved",
            EventKind::Push => "push",
            EventKind::PrOpened => "pr_opened",
            EventKind::PrMerged => "pr_merged",
            EventKind::ReviewSubmitted => "review_submitted",
            EventKind::ClaudeSessionStart => "claude_session_start",
            EventKind::ClaudeSessionEnd => "claude_session_end",
            EventKind::QuestCompleted => "quest_completed",
            EventKind::QuestFailed => "quest_failed",
        }
    }

    /// Internal events the quest engine emits itself.
    pub fn is_quest(self) -> bool {
        matches!(self, EventKind::QuestCompleted | EventKind::QuestFailed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Git,
    Github,
    Claude,
    Quest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub kind: EventKind,
    pub ts: DateTime<Utc>,
    pub source: Source,
    pub repo: Option<String>,
    #[serde(default)]
    pub backfill: bool,
    pub data: serde_json::Value,
}

impl Event {
    /// New event with a fresh ULID id and the current time.
    pub fn new(kind: EventKind, source: Source, repo: Option<String>, data: serde_json::Value) -> Event {
        Event {
            id: ulid::Ulid::new().to_string(),
            kind,
            ts: Utc::now(),
            source,
            repo,
            backfill: false,
            data,
        }
    }

    /// Put `repo` in canonical form (see [`normalize_repo`]) so events from different
    /// sources agree on what "the same repo" is.
    pub fn normalize(&mut self) {
        if let Some(r) = &self.repo {
            self.repo = Some(normalize_repo(r));
        }
    }
}

/// Canonical form of a repo key. Windows paths (`C:\x`, `c:/x`, `\\server\share`) get forward
/// slashes, an upper-case drive letter and no trailing slash, so git (`C:/x`) and Claude Code
/// (`C:\x`) name the same folder the same way. Anything else (`owner/name`, POSIX paths) only
/// loses a trailing slash.
pub fn normalize_repo(repo: &str) -> String {
    let b = repo.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let mut s = if drive || repo.starts_with("\\\\") { repo.replace('\\', "/") } else { repo.to_string() };
    if drive {
        s[..1].make_ascii_uppercase();
    }
    let keep = if drive { 3 } else { 1 }; // `C:/` and `/` are roots.
    while s.len() > keep && s.ends_with('/') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_from_git_and_claude_agree() {
        let git = normalize_repo("C:/Users/me/rpg");
        assert_eq!(git, "C:/Users/me/rpg");
        assert_eq!(normalize_repo(r"C:\Users\me\rpg"), git);
        assert_eq!(normalize_repo(r"c:\Users\me\rpg\"), git);
        assert_eq!(normalize_repo("c:/Users/me/rpg/"), git);
    }

    #[test]
    fn roots_unc_and_non_paths() {
        assert_eq!(normalize_repo(r"c:\"), "C:/");
        assert_eq!(normalize_repo(r"\\server\share\repo"), "//server/share/repo");
        assert_eq!(normalize_repo("octo/widgets"), "octo/widgets");
        assert_eq!(normalize_repo("/home/me/rpg/"), "/home/me/rpg");
        assert_eq!(normalize_repo("/"), "/");
        assert_eq!(normalize_repo(""), "");
    }

    #[test]
    fn normalize_event() {
        let mut e = Event::new(EventKind::Commit, Source::Claude, Some(r"d:\w\x".into()), serde_json::json!({}));
        e.normalize();
        assert_eq!(e.repo.as_deref(), Some("D:/w/x"));
        let mut e = Event::new(EventKind::Commit, Source::Claude, None, serde_json::json!({}));
        e.normalize();
        assert_eq!(e.repo, None);
    }
}
