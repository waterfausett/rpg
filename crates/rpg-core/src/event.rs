use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Commit,
    Push,
    PrOpened,
    PrMerged,
    ReviewSubmitted,
    ClaudeSessionStart,
    ClaudeSessionEnd,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Commit => "commit",
            EventKind::Push => "push",
            EventKind::PrOpened => "pr_opened",
            EventKind::PrMerged => "pr_merged",
            EventKind::ReviewSubmitted => "review_submitted",
            EventKind::ClaudeSessionStart => "claude_session_start",
            EventKind::ClaudeSessionEnd => "claude_session_end",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Git,
    Github,
    Claude,
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
}
