use crate::event::{Event, EventKind, Source};
use crate::proc;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

const QUERY: &str = "query($n: Int!) { viewer { login pullRequests(first:$n, orderBy:{field:UPDATED_AT, direction:DESC}) { nodes { id number title url createdAt mergedAt additions deletions repository { nameWithOwner } } } contributionsCollection { pullRequestReviewContributions(first:100) { nodes { occurredAt pullRequestReview { id } pullRequest { url repository { nameWithOwner } } } } } } }";

/// Ask GitHub (via the user's `gh` login) for recent PRs and review contributions.
pub fn fetch(limit_prs: u32) -> Result<Value> {
    let out = proc::command("gh")
        .args(["api", "graphql", "-f"])
        .arg(format!("query={QUERY}"))
        .arg("-F")
        .arg(format!("n={limit_prs}"))
        .output()
        .context("failed to run `gh` (is it installed and on PATH?)")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // gh may print GraphQL errors as JSON on stdout even when exiting non-zero.
    let parsed: Option<Value> = serde_json::from_str(&stdout).ok();
    if let Some(v) = &parsed {
        if let Some(errs) = v.get("errors") {
            bail!("github graphql errors: {errs}");
        }
    }
    if !out.status.success() {
        bail!(
            "gh api graphql failed ({}): {} (not logged in? try `gh auth login`)",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parsed.ok_or_else(|| anyhow!("gh returned non-JSON output"))
}

fn ts(v: &Value, key: &str) -> Result<DateTime<Utc>> {
    let s = v[key].as_str().ok_or_else(|| anyhow!("missing {key}"))?;
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

fn gh_event(id: String, kind: EventKind, ts: DateTime<Utc>, repo: Option<String>, backfill: bool, data: Value) -> Event {
    Event { id, kind, ts, source: Source::Github, repo, backfill, data }
}

/// Turn a `fetch` response into events with deterministic ids, sorted by `ts` ascending.
pub fn parse(json: &Value, backfill: bool) -> Result<Vec<Event>> {
    if let Some(errs) = json.get("errors") {
        bail!("github graphql errors: {errs}");
    }
    let viewer = &json["data"]["viewer"];
    if viewer.is_null() {
        bail!("response has no data.viewer");
    }
    let mut events = Vec::new();
    for pr in viewer["pullRequests"]["nodes"].as_array().into_iter().flatten() {
        let id = pr["id"].as_str().ok_or_else(|| anyhow!("PR without id"))?;
        let repo = pr["repository"]["nameWithOwner"].as_str().map(String::from);
        let data = json!({
            "number": pr["number"], "title": pr["title"], "url": pr["url"],
            "additions": pr["additions"], "deletions": pr["deletions"],
        });
        events.push(gh_event(format!("gh:pr_opened:{id}"), EventKind::PrOpened, ts(pr, "createdAt")?, repo.clone(), backfill, data.clone()));
        if pr["mergedAt"].is_string() {
            events.push(gh_event(format!("gh:pr_merged:{id}"), EventKind::PrMerged, ts(pr, "mergedAt")?, repo, backfill, data));
        }
    }
    let reviews = &viewer["contributionsCollection"]["pullRequestReviewContributions"]["nodes"];
    for r in reviews.as_array().into_iter().flatten() {
        let Some(id) = r["pullRequestReview"]["id"].as_str() else { continue };
        let repo = r["pullRequest"]["repository"]["nameWithOwner"].as_str().map(String::from);
        events.push(gh_event(format!("gh:review:{id}"), EventKind::ReviewSubmitted, ts(r, "occurredAt")?, repo, backfill, json!({"url": r["pullRequest"]["url"]})));
    }
    events.sort_by_key(|e| e.ts);
    Ok(events)
}

/// When the one-time backfill completed, from its stored meta value. `None` when the value is
/// not an RFC 3339 timestamp (e.g. written by an older build); the caller replaces it.
pub fn parse_backfill_done(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim()).ok().map(|t| t.with_timezone(&Utc))
}

/// Backfill only sees the most recently updated PRs, so a live poll can bring in older
/// history it missed (an old PR updated later). Anything that happened before the backfill
/// completed is history: mark it backfill so it seeds records only and grants nothing.
pub fn mark_pre_backfill(events: &mut [Event], done: DateTime<Utc>) {
    for e in events.iter_mut().filter(|e| e.ts < done) {
        e.backfill = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, Source};

    const FIXTURE: &str = include_str!("../tests/fixtures/github_viewer.json");

    #[test]
    fn parse_fixture() {
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        let evs = parse(&v, true).unwrap();
        let got: Vec<(&str, EventKind, String)> = evs
            .iter()
            .map(|e| (e.id.as_str(), e.kind, e.ts.to_rfc3339()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("gh:pr_opened:PR_aaa", EventKind::PrOpened, "2026-11-20T10:00:00+00:00".to_string()),
                ("gh:pr_merged:PR_aaa", EventKind::PrMerged, "2026-11-21T09:30:00+00:00".to_string()),
                ("gh:review:PRR_ccc", EventKind::ReviewSubmitted, "2026-11-21T15:00:00+00:00".to_string()),
                ("gh:pr_opened:PR_bbb", EventKind::PrOpened, "2026-11-22T08:00:00+00:00".to_string()),
            ]
        );
        assert!(evs.iter().all(|e| e.backfill && e.source == Source::Github));
        assert_eq!(evs[0].repo.as_deref(), Some("octo/widgets"));
        assert_eq!(evs[0].data["additions"], 120);
        assert_eq!(evs[0].data["deletions"], 15);
        assert_eq!(evs[0].data["number"], 12);
        assert_eq!(evs[2].repo.as_deref(), Some("other/thing"));
        assert_eq!(evs[2].data["url"], "https://github.com/other/thing/pull/7");
        assert!(!parse(&v, false).unwrap()[0].backfill);
    }

    #[test]
    fn live_events_older_than_backfill_are_marked_backfill() {
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        let mut evs = parse(&v, false).unwrap();
        let done = parse_backfill_done("2026-11-21T12:00:00+00:00").unwrap();
        mark_pre_backfill(&mut evs, done);
        let flags: Vec<(&str, bool)> = evs.iter().map(|e| (e.id.as_str(), e.backfill)).collect();
        assert_eq!(
            flags,
            vec![
                ("gh:pr_opened:PR_aaa", true),
                ("gh:pr_merged:PR_aaa", true),
                ("gh:review:PRR_ccc", false),
                ("gh:pr_opened:PR_bbb", false),
            ]
        );
        // Already-backfill events stay backfill.
        let mut all = parse(&v, true).unwrap();
        mark_pre_backfill(&mut all, done);
        assert!(all.iter().all(|e| e.backfill));
    }

    #[test]
    fn backfill_done_parses_only_timestamps() {
        let t = parse_backfill_done("2026-11-21T12:00:00.123456+00:00").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-11-21T12:00:00.123456+00:00");
        assert_eq!(parse_backfill_done(" 2026-11-21T12:00:00Z\n").map(|t| t.timestamp()), Some(t.timestamp()));
        assert_eq!(parse_backfill_done("1"), None);
        assert_eq!(parse_backfill_done(""), None);
    }

    #[test]
    fn graphql_errors_are_surfaced() {
        let v: Value = serde_json::json!({"errors":[{"message":"bad"}]});
        assert!(parse(&v, false).is_err());
    }

    #[test]
    #[ignore]
    fn gh_fetch_smoke() {
        let v = fetch(5).unwrap();
        assert!(v["data"]["viewer"]["login"].is_string());
    }
}
