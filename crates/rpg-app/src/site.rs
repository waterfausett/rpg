//! Localhost "character sheet": a JSON API plus the embedded vanilla page in `ui/site/`.
use axum::extract::{Query, Request, State};
use axum::http::HeaderValue;
use axum::middleware::{self, Next};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rpg_core::config::Config;
use rpg_core::event::EventKind;
use chrono::Utc;
use rpg_core::counters;
use rpg_core::rules::{Counter, Rule, Rules, Tier};
use rpg_core::store::{EventRow, Store, UnlockRow};
use rpg_core::xp;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct SiteState {
    pub store: Arc<Store>,
    pub rules: Arc<RwLock<Rules>>,
    pub cfg: Arc<RwLock<Config>>,
    /// The port the site listens on; the only port accepted in `Host`.
    pub port: u16,
}

#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/site/"]
struct Assets;

/// The site only ever listens on loopback.
pub const BIND_ADDR: Ipv4Addr = Ipv4Addr::LOCALHOST;
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;
const RECENT: u32 = 10;
const MASK: &str = "???";

type ApiError = (StatusCode, String);

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub fn router(state: SiteState) -> Router {
    Router::new()
        .route("/api/character", get(character))
        .route("/api/achievements", get(achievements))
        .route("/api/events", get(events))
        .route("/api/quests", get(quests))
        .fallback(static_asset)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Rejects requests whose `Host` is not our own loopback address (DNS-rebinding defense),
/// and marks every response `nosniff`.
async fn guard(State(st): State<SiteState>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let ok = host == format!("127.0.0.1:{}", st.port) || host == format!("localhost:{}", st.port);
    let mut res = if ok { next.run(req).await } else { StatusCode::FORBIDDEN.into_response() };
    res.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    res
}

pub async fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(SocketAddr::from((BIND_ADDR, port))).await
}

pub async fn serve(mut state: SiteState, port: u16) -> std::io::Result<()> {
    state.port = port;
    axum::serve(bind(port).await?, router(state)).await
}

/// `xp` is the event's total; `base_xp`, `quest_xp` and each of `achievements` add up to it.
/// A quest event's ledger row with no achievement tier is the quest's reward (`quest_xp`),
/// never an achievement; quest events also get a readable `label`.
fn event_json(r: &EventRow, rules: &Rules) -> Value {
    let e = &r.event;
    let base = rpg_core::engine::base_label(e.kind);
    let is_quest = e.kind.is_quest();
    let (base_gains, rest): (Vec<_>, Vec<_>) = r.gains.iter().partition(|g| Some(g.reason.as_str()) == base);
    let (quest_gains, ach_gains): (Vec<_>, Vec<_>) = rest.into_iter().partition(|g| is_quest && g.tier.is_none());
    let achievements: Vec<Value> = ach_gains
        .iter()
        .map(|g| {
            let mut a = json!({ "name": g.reason, "xp": g.amount });
            if let Some(t) = &g.tier {
                a["tier"] = json!(t);
            }
            a
        })
        .collect();
    let mut v = json!({
        "id": e.id, "kind": e.kind.as_str(), "ts": e.ts, "repo": e.repo,
        "backfill": e.backfill, "xp": r.xp,
        "base_xp": base_gains.iter().map(|g| g.amount).sum::<u64>(),
        "achievements": achievements,
    });
    if is_quest {
        v["quest_xp"] = json!(quest_gains.iter().map(|g| g.amount).sum::<u64>());
        let qid = e.data["quest_id"].as_str().unwrap_or_default();
        let name = quest_gains
            .iter()
            .find_map(|g| g.reason.strip_prefix("Quest: ").map(str::to_string))
            .or_else(|| rules.quests.iter().find(|q| q.id == qid).map(|q| q.name.clone()))
            .unwrap_or_else(|| qid.to_string());
        let verb = if e.kind == EventKind::QuestCompleted { "complete" } else { "failed" };
        v["label"] = json!(format!("Quest {verb}: {name}"));
    }
    v
}

async fn character(State(st): State<SiteState>) -> Result<Json<Value>, ApiError> {
    let cfg = st.cfg.read().unwrap_or_else(|p| p.into_inner()).clone();
    let total = st.store.total_xp().map_err(internal)?;
    let (level, into_level, level_span) = xp::progress(total, &cfg.levels);
    let count = |k| st.store.count_live_events(k).map_err(internal);
    let rules = st.rules.read().unwrap_or_else(|p| p.into_inner()).clone();
    let recent: Vec<Value> = st.store.list_events(None, RECENT).map_err(internal)?.iter().map(|r| event_json(r, &rules)).collect();
    let skills = skills_json(&st, &cfg)?;
    Ok(Json(json!({
        "level": level, "total_xp": total, "into_level": into_level, "level_span": level_span,
        "totals": {
            "commits": count(EventKind::Commit)?,
            "prs_opened": count(EventKind::PrOpened)?,
            "prs_merged": count(EventKind::PrMerged)?,
            "reviews": count(EventKind::ReviewSubmitted)?,
            "sessions": count(EventKind::ClaudeSessionEnd)?,
        },
        "recent": recent,
        "skills": skills,
    })))
}

/// Skills with any XP, biggest first. A skill's current name wins over the stored one;
/// skills whose file was removed keep showing under their last name.
fn skills_json(st: &SiteState, cfg: &Config) -> Result<Vec<Value>, ApiError> {
    let rules = st.rules.read().unwrap_or_else(|p| p.into_inner());
    let curve = cfg.skill_levels.curve();
    let rows = st.store.skill_totals().map_err(internal)?;
    Ok(rows
        .into_iter()
        .map(|t| {
            let name = rules.skills.iter().find(|s| s.id == t.skill_id).map_or(t.name, |s| s.name.clone());
            let (level, into_level, level_span) = xp::progress(t.xp, &curve);
            json!({
                "id": t.skill_id, "name": name, "xp": t.xp, "level": level,
                "into_level": into_level, "level_span": level_span, "last_at": t.last_at,
            })
        })
        .collect())
}

/// An unlock key belongs to a rule: the rule's own id, or one of its step ids (`id.…`).
fn owns(rule: &Rule, unlock_key: &str) -> bool {
    unlock_key == rule.id || unlock_key.strip_prefix(rule.id.as_str()).is_some_and(|rest| rest.starts_with('.'))
}

/// One achievements entry. `key` maps a step's tier to its unlock key; `name` is shown once
/// any step is unlocked. Ladders list their steps; locked steps past the next one hide `at`.
fn achievement_json(
    r: &Rule,
    id: &str,
    key: impl Fn(Tier) -> String,
    name: &str,
    progress: Option<u64>,
    unlocks: &[UnlockRow],
) -> Value {
    let mut steps = Vec::new();
    let (mut count, mut tier, mut next_shown) = (0, r.steps[0].tier, false);
    for step in &r.steps {
        let k = key(step.tier);
        let rows: Vec<&UnlockRow> = unlocks.iter().filter(|u| u.rule_id == k).collect(); // newest first
        let unlocked = !rows.is_empty();
        count += rows.len();
        let at = if unlocked || !next_shown { json!(step.at) } else { json!(MASK) };
        if unlocked {
            tier = step.tier;
        } else {
            next_shown = true;
        }
        steps.push(json!({ "tier": step.tier.as_str(), "at": at, "unlocked": unlocked, "last_at": rows.first().map(|u| u.ts) }));
    }
    let keys: Vec<String> = r.steps.iter().map(|s| key(s.tier)).collect();
    let latest = unlocks.iter().find(|u| keys.contains(&u.rule_id));
    let unlocked = count > 0;
    let mut v = json!({
        "id": id,
        "name": if unlocked { name } else { MASK },
        "tier": tier.as_str(),
        "unlocked": unlocked,
        "count": count,
        "last_at": latest.map(|u| u.ts),
        "flavor_text_latest": latest.map(|u| u.flavor_text.clone().unwrap_or_else(|| r.fallback.clone())),
        "hint": if unlocked { r.hint.clone() } else { Some(r.hint.clone().unwrap_or_else(|| MASK.into())) },
        "ladder": r.ladder,
        "progress": progress,
        "enabled": r.enabled,
    });
    if r.ladder {
        v["steps"] = Value::Array(steps);
    }
    v
}

async fn achievements(State(st): State<SiteState>) -> Result<Json<Value>, ApiError> {
    let rules = st.rules.read().unwrap_or_else(|p| p.into_inner()).clone();
    let cfg = st.cfg.read().unwrap_or_else(|p| p.into_inner()).clone();
    let tz = crate::runtime::local_offset(Utc::now());
    let unlocks = st.store.list_unlocks().map_err(internal)?; // newest first
    let skills = skills_json(&st, &cfg)?;
    let mut out = Vec::new();
    let mut hidden_remaining = 0;
    for r in &rules.list {
        if r.hidden && !unlocks.iter().any(|u| owns(r, &u.rule_id)) {
            hidden_remaining += 1;
            continue;
        }
        if matches!(r.counter, Counter::SkillLevel) && !skills.is_empty() {
            // One entry per skill with XP; its progress is the skill's level.
            for s in &skills {
                let (sid, sname) = (s["id"].as_str().unwrap_or_default(), s["name"].as_str().unwrap_or_default());
                let key = |t: Tier| format!("{}.{}.{}", r.id, sid, t.as_str());
                let name = format!("{}: {}", r.name, sname);
                out.push(achievement_json(r, &format!("{}.{sid}", r.id), key, &name, s["level"].as_u64(), &unlocks));
            }
        } else {
            let progress = counters::progress(r, &st.store, tz).map_err(internal)?;
            out.push(achievement_json(r, &r.id, |t| r.step_id(t), &r.name, progress, &unlocks));
        }
    }
    // Unlocks whose rule was since removed or renamed stay visible, using the stored row.
    let mut seen = std::collections::HashSet::new();
    for u in &unlocks {
        if rules.list.iter().any(|r| owns(r, &u.rule_id)) || !seen.insert(u.rule_id.as_str()) {
            continue;
        }
        let count = unlocks.iter().filter(|x| x.rule_id == u.rule_id).count();
        out.push(json!({
            "id": u.rule_id, "name": u.name, "tier": u.tier, "unlocked": true, "count": count,
            "last_at": u.ts, "flavor_text_latest": u.flavor_text, "hint": Value::Null,
            "ladder": false, "progress": Value::Null, "enabled": true,
        }));
    }
    Ok(Json(json!({ "achievements": out, "hidden_remaining": hidden_remaining })))
}

#[derive(Deserialize)]
struct EventsQuery {
    kind: Option<String>,
    limit: Option<u32>,
}

async fn events(State(st): State<SiteState>, Query(q): Query<EventsQuery>) -> Result<Json<Value>, ApiError> {
    let kind = match q.kind.as_deref().filter(|k| !k.is_empty() && *k != "all") {
        None => None,
        Some(k) => Some(
            serde_json::from_value::<EventKind>(Value::String(k.to_string()))
                .map_err(|_| (StatusCode::BAD_REQUEST, format!("unknown kind: {k}")))?,
        ),
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let rows = st.store.list_events(kind, limit).map_err(internal)?;
    let rules = st.rules.read().unwrap_or_else(|p| p.into_inner()).clone();
    Ok(Json(Value::Array(rows.iter().map(|r| event_json(r, &rules)).collect())))
}

/// Quest log lines kept on the sheet.
const QUEST_LOG: u32 = 50;

/// Chain, counts, the newest resolved quests and the board of every loaded quest.
/// `enabled` is the config setting; the tray's per-session toggle is not visible here.
async fn quests(State(st): State<SiteState>) -> Result<Json<Value>, ApiError> {
    let rules = st.rules.read().unwrap_or_else(|p| p.into_inner()).clone();
    let cfg = st.cfg.read().unwrap_or_else(|p| p.into_inner()).clone();
    let ch = rpg_core::quests::state::chain(&st.store).map_err(internal)?;
    let counts = st.store.quest_counts().map_err(internal)?;
    let (completed, failed) = counts.iter().fold((0, 0), |(c, f), (_, a, b)| (c + a, f + b));
    let mult = (rpg_core::quests::state::multiplier(ch.n, &cfg.quests) * 1000.0).round() / 1000.0;
    let log: Vec<Value> = st
        .store
        .list_quests(QUEST_LOG)
        .map_err(internal)?
        .iter()
        .map(|q| {
            let done = q.status == "completed";
            let data = q.result_event_id.as_deref().and_then(|id| st.store.event_data(id).ok().flatten());
            let reward = done.then(|| {
                data.as_ref().and_then(|d| d["xp"].as_u64()).unwrap_or_else(|| rpg_core::quests::state::reward(q.xp, q.multiplier))
            });
            let lost = (!done).then(|| data.as_ref().and_then(|d| d["chain_lost"].as_u64()).unwrap_or(q.chain_at_start as u64));
            json!({
                "quest_id": q.quest_id, "name": q.name, "tier": q.tier, "status": q.status,
                "reward": reward, "chain_lost": lost, "ts": q.resolved_at,
            })
        })
        .collect();
    let board: Vec<Value> = rules
        .quests
        .iter()
        .map(|q| {
            let completions = counts.iter().find(|(id, _, _)| *id == q.id).map_or(0, |(_, c, _)| *c);
            json!({
                "id": q.id, "name": q.name, "tier": q.tier.as_str(), "hint": q.hint, "chance": q.chance,
                "minutes": q.minutes, "completions": completions, "enabled": q.enabled,
            })
        })
        .collect();
    Ok(Json(json!({
        "enabled": cfg.quests.enabled, "chain": ch.n, "multiplier": mult, "best_chain": ch.best,
        "completed": completed, "failed": failed, "log": log, "board": board,
    })))
}

async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(f) = Assets::get(path) else { return StatusCode::NOT_FOUND.into_response() };
    let mime = f.metadata.mimetype().to_string();
    let mut res = ([(header::CONTENT_TYPE, mime.clone())], f.data.into_owned()).into_response();
    if mime.starts_with("text/html") {
        res.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'"));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::Utc;
    use http_body_util::BodyExt;
    use rpg_core::engine;
    use rpg_core::event::{Event, Source};
    use rpg_core::rules::parse_rule;
    use tower::ServiceExt;

    const TEST_HOST: &str = "127.0.0.1:47474";

    fn state_with(rules: Rules) -> SiteState {
        SiteState {
            store: Arc::new(Store::open_in_memory().unwrap()),
            rules: Arc::new(RwLock::new(rules)),
            cfg: Arc::new(RwLock::new(Config::default())),
            port: 47474,
        }
    }

    async fn get_json(state: &SiteState, uri: &str) -> Value {
        let res = router(state.clone()).oneshot(Request::get(uri).header(header::HOST, TEST_HOST).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{uri}");
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn ev(id: &str, kind: EventKind) -> Event {
        Event {
            id: id.into(),
            kind,
            ts: Utc::now(),
            source: Source::Github,
            repo: Some("r".into()),
            backfill: false,
            data: json!({}),
        }
    }

    fn process(st: &SiteState, e: &Event) {
        let cfg = st.cfg.read().unwrap().clone();
        let rules = st.rules.read().unwrap().clone();
        engine::process(&st.store, &rules, &cfg, e, &[], chrono::FixedOffset::east_opt(0).unwrap()).unwrap();
    }

    const RULE: &str = r#"
id = "first-merge"
name = "Merge Maestro"
tier = "bronze"
fallback = "You merged."
hint = "Merge something."
trigger = ["pr_merged"]
counter = { type = "first", scope = "ever" }
"#;

    #[tokio::test]
    async fn character_lists_skills_biggest_first() {
        let rules = Rules {
            skills: vec![
            rpg_core::skills::parse_skill("id = \"rust\"
name = \"Rust\"
globs = [\"*.rs\"]
xp = 150
").unwrap(),
            rpg_core::skills::parse_skill("id = \"docs\"
name = \"Scribe\"
globs = [\"*.md\"]
").unwrap(),
            ],
            ..Default::default()
        };
        let st = state_with(rules);
        let mut c = ev("c1", EventKind::Commit);
        c.data = json!({ "files": [{ "path": "src/main.rs" }, { "path": "README.md" }] });
        process(&st, &c);
        st.store.add_skill_xp("old", "gone", "Old Skill", 5, Utc::now()).unwrap();
        let v = get_json(&st, "/api/character").await;
        let s = v["skills"].as_array().unwrap();
        let names: Vec<&str> = s.iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Rust", "Scribe", "Old Skill"]);
        assert_eq!((s[0]["xp"].as_u64(), s[0]["level"].as_u64()), (Some(150), Some(2)));
        assert_eq!((s[0]["into_level"].as_u64(), s[0]["level_span"].as_u64()), (Some(50), Some(183)));
        assert_eq!(v["total_xp"], 10, "skills do not add to character exp");
    }

    #[tokio::test]
    async fn character_endpoint_shape() {
        let st = state_with(Rules::default());
        process(&st, &ev("m1", EventKind::PrMerged));
        let v = get_json(&st, "/api/character").await;
        assert_eq!(v["level"], 1, "100 exp is still level 1 (level 2 at 1000)");
        assert_eq!(v["total_xp"], 100);
        assert_eq!(v["into_level"], 100);
        assert_eq!(v["level_span"], 1000);
        assert_eq!(v["totals"]["prs_merged"], 1);
        assert_eq!(v["totals"]["commits"], 0);
        assert_eq!(v["recent"].as_array().unwrap().len(), 1);
        assert_eq!(v["recent"][0]["xp"], 100);
        assert_eq!(v["recent"][0]["kind"], "pr_merged");
        assert_eq!(v["recent"][0]["base_xp"], 100);
        assert_eq!(v["recent"][0]["achievements"], json!([]));
    }

    #[tokio::test]
    async fn event_xp_split_into_base_and_achievements() {
        let st = state_with(Rules { list: vec![parse_rule(RULE).unwrap()], ..Default::default() });
        process(&st, &ev("m1", EventKind::PrMerged));
        let v = get_json(&st, "/api/events").await;
        assert_eq!(v[0]["xp"], 350, "100 base + 250 bronze");
        assert_eq!(v[0]["base_xp"], 100);
        assert_eq!(v[0]["achievements"], json!([{ "name": "Merge Maestro", "xp": 250, "tier": "bronze" }]));
    }

    #[tokio::test]
    async fn backfill_events_do_not_inflate_totals() {
        let st = state_with(Rules::default());
        let mut old = ev("old", EventKind::PrMerged);
        old.backfill = true;
        st.store.insert_event(&old).unwrap();
        let v = get_json(&st, "/api/character").await;
        assert_eq!(v["totals"]["prs_merged"], 0);
        assert_eq!(v["level"], 1);
    }

    #[tokio::test]
    async fn locked_achievements_masked() {
        let st = state_with(Rules { list: vec![parse_rule(RULE).unwrap()], ..Default::default() });
        let v = get_json(&st, "/api/achievements").await;
        assert_eq!(v["hidden_remaining"], 0);
        let a = &v["achievements"].as_array().unwrap()[0];
        assert_eq!(a["id"], "first-merge");
        assert_eq!(a["name"], "???");
        assert_eq!(a["unlocked"], false);
        assert_eq!(a["count"], 0);
        assert_eq!(a["hint"], "Merge something.");
        assert!(a["flavor_text_latest"].is_null());
        assert_eq!((a["ladder"].clone(), a["enabled"].clone()), (json!(false), json!(true)));
        assert!(a["progress"].is_null(), "moment counters have no progress");
        assert!(a.get("steps").is_none(), "steps are for ladders only");
    }

    #[tokio::test]
    async fn unlocked_achievement_shows_name_and_flavor_or_fallback() {
        let st = state_with(Rules { list: vec![parse_rule(RULE).unwrap()], ..Default::default() });
        process(&st, &ev("m1", EventKind::PrMerged));
        let v = get_json(&st, "/api/achievements").await;
        let a = &v["achievements"][0];
        assert_eq!(a["name"], "Merge Maestro");
        assert_eq!(a["unlocked"], true);
        assert_eq!(a["count"], 1);
        assert!(a["last_at"].is_string());
        assert_eq!(a["flavor_text_latest"], "You merged.", "falls back to the rule's fallback");
        let id = st.store.list_unlocks().unwrap()[0].id;
        st.store.set_unlock_flavor(id, "LLM words").unwrap();
        assert_eq!(get_json(&st, "/api/achievements").await["achievements"][0]["flavor_text_latest"], "LLM words");
    }

    #[tokio::test]
    async fn unlock_for_removed_rule_still_listed() {
        let st = state_with(Rules::default());
        st.store
            .insert_unlock(&rpg_core::store::NewUnlock {
                rule_id: "gone".into(),
                name: "Old Glory".into(),
                tier: "gold".into(),
                xp: 1000,
                context: json!({}),
                ts: Utc::now(),
            })
            .unwrap();
        let v = get_json(&st, "/api/achievements").await;
        let v = &v["achievements"];
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["name"], "Old Glory");
        assert_eq!(v[0]["tier"], "gold");
        assert_eq!(v[0]["unlocked"], true);
    }

    const LADDER: &str = r#"
id = "committed"
name = "Committed"
trigger = ["commit"]
counter = { type = "lifetime" }
tiers = [
  { tier = "bronze", at = 1 },
  { tier = "silver", at = 3 },
  { tier = "gold", at = 10 },
  { tier = "platinum", at = 100 },
]
fallback = "You commit."
"#;

    fn unlock(st: &SiteState, rule_id: &str, tier: &str) {
        st.store
            .insert_unlock(&rpg_core::store::NewUnlock {
                rule_id: rule_id.into(),
                name: rule_id.into(),
                tier: tier.into(),
                xp: 0,
                context: json!({}),
                ts: Utc::now(),
            })
            .unwrap();
    }

    fn ids(v: &Value) -> Vec<String> {
        v["achievements"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect()
    }

    #[tokio::test]
    async fn ladder_entry_has_steps_and_progress() {
        let st = state_with(Rules { list: vec![parse_rule(LADDER).unwrap()], ..Default::default() });
        let v = get_json(&st, "/api/achievements").await;
        let a = &v["achievements"][0];
        assert_eq!((a["name"].clone(), a["unlocked"].clone(), a["progress"].clone()), (json!("???"), json!(false), json!(0)));
        assert_eq!(a["tier"], "bronze", "nothing unlocked: the first step");
        assert_eq!(a["steps"][0]["at"], 1);
        assert_eq!(a["steps"][1]["at"], "???");
        process(&st, &ev("c1", EventKind::Commit));
        process(&st, &ev("c2", EventKind::Commit));
        let v = get_json(&st, "/api/achievements").await;
        let a = &v["achievements"][0];
        assert_eq!(a["id"], "committed");
        assert_eq!(a["name"], "Committed");
        assert_eq!(a["ladder"], true);
        assert_eq!(a["enabled"], true);
        assert_eq!(a["unlocked"], true);
        assert_eq!(a["tier"], "bronze", "the highest unlocked step");
        assert_eq!(a["count"], 1);
        assert_eq!(a["progress"], 2);
        assert!(a["last_at"].is_string());
        assert_eq!(a["flavor_text_latest"], "You commit.");
        let steps = a["steps"].as_array().unwrap();
        let tiers: Vec<&str> = steps.iter().map(|s| s["tier"].as_str().unwrap()).collect();
        assert_eq!(tiers, vec!["bronze", "silver", "gold", "platinum"]);
        let ats: Vec<Value> = steps.iter().map(|s| s["at"].clone()).collect();
        assert_eq!(ats, vec![json!(1), json!(3), json!("???"), json!("???")], "only the next locked step shows its threshold");
        let unlocked: Vec<bool> = steps.iter().map(|s| s["unlocked"].as_bool().unwrap()).collect();
        assert_eq!(unlocked, vec![true, false, false, false]);
        assert!(steps[0]["last_at"].is_string());
        assert!(steps[1]["last_at"].is_null());
    }

    #[tokio::test]
    async fn step_unlocks_are_not_orphans() {
        let st = state_with(Rules { list: vec![parse_rule(LADDER).unwrap()], ..Default::default() });
        process(&st, &ev("c1", EventKind::Commit));
        unlock(&st, "committed.silver", "silver");
        unlock(&st, "committedx", "gold");
        let v = get_json(&st, "/api/achievements").await;
        assert_eq!(ids(&v), vec!["committed", "committedx"], "a shared prefix without the dot is still an orphan");
        assert_eq!(v["achievements"][0]["tier"], "silver");
        assert_eq!(v["achievements"][0]["count"], 2);
    }

    #[tokio::test]
    async fn disabled_rule_shown_off() {
        let off = format!("{RULE}enabled = false
");
        let st = state_with(Rules { list: vec![parse_rule(&off).unwrap()], ..Default::default() });
        let v = get_json(&st, "/api/achievements").await;
        let a = &v["achievements"].as_array().unwrap()[0];
        assert_eq!((a["id"].clone(), a["enabled"].clone()), (json!("first-merge"), json!(false)));
    }

    #[tokio::test]
    async fn hidden_locked_rule_omitted_and_counted() {
        let hidden = RULE.replace(r#"id = "first-merge""#, "id = \"secret\"\nhidden = true");
        let st = state_with(Rules {
            list: vec![parse_rule(RULE).unwrap(), parse_rule(&hidden).unwrap(), parse_rule(&format!("{LADDER}hidden = true\n")).unwrap()],
            ..Default::default()
        });
        let v = get_json(&st, "/api/achievements").await;
        assert_eq!(ids(&v), vec!["first-merge"]);
        assert_eq!(v["hidden_remaining"], 2);
        process(&st, &ev("m1", EventKind::PrMerged));
        process(&st, &ev("c1", EventKind::Commit));
        let v = get_json(&st, "/api/achievements").await;
        assert_eq!(ids(&v), vec!["first-merge", "secret", "committed"]);
        assert_eq!(v["hidden_remaining"], 0);
        assert_eq!(v["achievements"][1]["name"], "Merge Maestro");
    }

    #[tokio::test]
    async fn skill_ladder_one_entry_per_skill() {
        let rule = parse_rule(
            r#"
id = "skill_master"
name = "Skill Master"
trigger = ["commit"]
counter = { type = "skill_level" }
tiers = [
  { tier = "bronze", at = 2 },
  { tier = "silver", at = 5 },
  { tier = "gold", at = 9 },
]
fallback = "A skill, levelled."
"#,
        )
        .unwrap();
        let skill = |s: &str| rpg_core::skills::parse_skill(s).unwrap();
        let rules = Rules {
            list: vec![rule],
            skills: vec![
                skill("id = \"rust\"\nname = \"Rust\"\nglobs = [\"*.rs\"]\nxp = 150\n"),
                skill("id = \"docs\"\nname = \"Scribe\"\nglobs = [\"*.md\"]\n"),
                skill("id = \"idle\"\nname = \"Idle\"\nglobs = [\"*.zz\"]\n"),
            ],
            ..Default::default()
        };
        let st = state_with(rules);
        let mut c = ev("c1", EventKind::Commit);
        c.data = json!({ "files": [{ "path": "src/main.rs" }, { "path": "README.md" }] });
        process(&st, &c);
        let v = get_json(&st, "/api/achievements").await;
        assert_eq!(ids(&v), vec!["skill_master.rust", "skill_master.docs"], "skills with xp, biggest first; no orphans");
        let (rust, docs) = (&v["achievements"][0], &v["achievements"][1]);
        assert_eq!(rust["name"], "Skill Master: Rust");
        assert_eq!((rust["progress"].clone(), rust["unlocked"].clone(), rust["tier"].clone()), (json!(2), json!(true), json!("bronze")));
        assert_eq!((rust["ladder"].clone(), rust["count"].clone()), (json!(true), json!(1)));
        let steps = rust["steps"].as_array().unwrap();
        assert_eq!(steps[0]["unlocked"], true);
        assert_eq!((steps[1]["at"].clone(), steps[2]["at"].clone()), (json!(5), json!("???")));
        assert_eq!(docs["name"], "???");
        assert_eq!((docs["progress"].clone(), docs["unlocked"].clone(), docs["count"].clone()), (json!(1), json!(false), json!(0)));
        assert_eq!(docs["steps"][0]["at"], 2);
    }

    #[tokio::test]
    async fn events_limit_capped_at_500() {
        let st = state_with(Rules::default());
        for i in 0..510 {
            st.store.insert_event(&ev(&format!("e{i:04}"), EventKind::Commit)).unwrap();
        }
        let v = get_json(&st, "/api/events?limit=100000").await;
        assert_eq!(v.as_array().unwrap().len(), 500);
        let v = get_json(&st, "/api/events?kind=push").await;
        assert_eq!(v.as_array().unwrap().len(), 0);
        let v = get_json(&st, "/api/events?kind=commit&limit=3").await;
        assert_eq!(v.as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn events_rejects_unknown_kind() {
        let st = state_with(Rules::default());
        let res = router(st).oneshot(Request::get("/api/events?kind=bogus").header(header::HOST, TEST_HOST).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn serves_embedded_page_and_assets() {
        let st = state_with(Rules::default());
        for (uri, ct) in [("/", "text/html"), ("/site.css", "text/css"), ("/site.js", "javascript")] {
            let res = router(st.clone()).oneshot(Request::get(uri).header(header::HOST, TEST_HOST).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri}");
            let got = res.headers()[header::CONTENT_TYPE].to_str().unwrap().to_string();
            assert!(got.contains(ct), "{uri}: {got}");
        }
        let res = router(st).oneshot(Request::get("/nope.txt").header(header::HOST, TEST_HOST).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn binds_localhost_only() {
        let l = bind(0).await.unwrap();
        assert_eq!(l.local_addr().unwrap().ip(), std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    async fn status_with_host(st: &SiteState, host: Option<&str>) -> (StatusCode, axum::http::HeaderMap) {
        let mut b = Request::get("/api/character");
        if let Some(h) = host {
            b = b.header(header::HOST, h);
        }
        let res = router(st.clone()).oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
        (res.status(), res.headers().clone())
    }

    #[tokio::test]
    async fn host_header_is_validated() {
        let st = state_with(Rules::default());
        for ok in ["127.0.0.1:47474", "localhost:47474"] {
            assert_eq!(status_with_host(&st, Some(ok)).await.0, StatusCode::OK, "{ok}");
        }
        for bad in ["evil.example:47474", "127.0.0.1:1", "127.0.0.1", "localhost.evil.example:47474"] {
            assert_eq!(status_with_host(&st, Some(bad)).await.0, StatusCode::FORBIDDEN, "{bad}");
        }
        assert_eq!(status_with_host(&st, None).await.0, StatusCode::FORBIDDEN, "missing Host");
    }

    #[tokio::test]
    async fn security_headers_set() {
        let st = state_with(Rules::default());
        let (_, h) = status_with_host(&st, Some(TEST_HOST)).await;
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let (_, h) = status_with_host(&st, Some("evil.example:47474")).await;
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "also on 403");
        let res = router(st).oneshot(Request::get("/").header(header::HOST, TEST_HOST).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_SECURITY_POLICY], "default-src 'self'");
    }

    #[tokio::test]
    async fn event_json_omits_raw_data() {
        let st = state_with(Rules::default());
        let mut e = ev("x", EventKind::Commit);
        e.data = json!({ "secret": "diff text" });
        st.store.insert_event(&e).unwrap();
        let v = get_json(&st, "/api/events").await;
        assert!(v[0].get("data").is_none());
        assert!(v[0].get("source").is_none());
        assert_eq!(v[0]["id"], "x");
    }

    const QUEST_A: &str = r#"
id = "ship_it"
name = "Ship It"
tier = "gold"
trigger = ["commit"]
chance = 0.25
minutes = 15
steps = [{ kind = "push" }]
hint = "Push it."
fallback = "Shipped."
"#;

    fn quest_rules(extra: &str) -> Rules {
        let b = QUEST_A.replace("id = \"ship_it\"", "id = \"lazy\"").replace("Ship It", "Lazy One").replace("Push it.", "Rest.");
        Rules { quests: vec![rpg_core::quests::parse_quest(QUEST_A).unwrap(), rpg_core::quests::parse_quest(&format!("{b}{extra}")).unwrap()], ..Default::default() }
    }

    /// Store one resolved attempt the way the engine does: row plus its result event.
    fn resolved_quest(st: &SiteState, quest_id: &str, name: &str, ok: bool, at: &str) -> String {
        let ts: chrono::DateTime<Utc> = at.parse().unwrap();
        let id = st
            .store
            .insert_quest(&rpg_core::store::NewQuest {
                quest_id: quest_id.into(),
                name: name.into(),
                tier: "gold".into(),
                trigger_event_id: "t".into(),
                binding: json!({}),
                steps: json!([{ "kind": "push", "need": 1 }]),
                accepted_at: ts,
                deadline: ts,
                chain_at_start: 2,
                multiplier: 1.5,
                xp: 500,
            })
            .unwrap();
        let (kind, data) = if ok {
            (EventKind::QuestCompleted, json!({ "quest_id": quest_id, "tier": "gold", "chain": 3, "multiplier": 1.5, "xp": 750 }))
        } else {
            (EventKind::QuestFailed, json!({ "quest_id": quest_id, "tier": "gold", "chain_lost": 2 }))
        };
        let mut e = Event::new(kind, Source::Quest, None, data);
        e.ts = ts;
        st.store.insert_event(&e).unwrap();
        st.store.resolve_quest(id, if ok { "completed" } else { "failed" }, Some(&e.id), ts).unwrap();
        if ok {
            st.store.add_xp(&e.id, 750, &format!("Quest: {name}"), None, ts).unwrap();
        }
        e.id
    }

    #[tokio::test]
    async fn quests_endpoint_shape() {
        let st = state_with(quest_rules(""));
        resolved_quest(&st, "ship_it", "Ship It", true, "2026-01-01T10:00:00Z");
        resolved_quest(&st, "ship_it", "Ship It", false, "2026-01-02T10:00:00Z");
        st.store.meta_set(rpg_core::quests::state::QUEST_CHAIN, "3").unwrap();
        st.store.meta_set(rpg_core::quests::state::QUEST_BEST_CHAIN, "5").unwrap();
        let v = get_json(&st, "/api/quests").await;
        assert_eq!(v["enabled"], true);
        assert_eq!((v["chain"].clone(), v["best_chain"].clone()), (json!(3), json!(5)));
        assert_eq!((v["completed"].clone(), v["failed"].clone()), (json!(1), json!(1)));
        assert_eq!(v["multiplier"], 2.5, "default chain_step 0.5: 1 + 0.5 x 3");
        let log = v["log"].as_array().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!((log[0]["status"].clone(), log[1]["status"].clone()), (json!("failed"), json!("completed")), "newest first");
        assert_eq!((log[0]["chain_lost"].clone(), log[0]["reward"].clone()), (json!(2), Value::Null));
        assert_eq!((log[1]["reward"].clone(), log[1]["chain_lost"].clone()), (json!(750), Value::Null));
        assert_eq!((log[1]["quest_id"].clone(), log[1]["name"].clone(), log[1]["tier"].clone()), (json!("ship_it"), json!("Ship It"), json!("gold")));
        assert!(log[0]["ts"].is_string());
        let board = v["board"].as_array().unwrap();
        assert_eq!(board.len(), 2, "every loaded quest");
        assert_eq!(board[0]["id"], "ship_it");
        assert_eq!((board[0]["completions"].clone(), board[1]["completions"].clone()), (json!(1), json!(0)));
        assert_eq!((board[0]["name"].clone(), board[0]["tier"].clone(), board[0]["hint"].clone()), (json!("Ship It"), json!("gold"), json!("Push it.")));
        assert_eq!((board[0]["chance"].clone(), board[0]["minutes"].clone(), board[0]["enabled"].clone()), (json!(0.25), json!(15), json!(true)));
    }

    #[tokio::test]
    async fn quests_disabled_in_config_reports_off() {
        let st = state_with(quest_rules(""));
        st.cfg.write().unwrap().quests.enabled = false;
        assert_eq!(get_json(&st, "/api/quests").await["enabled"], false);
    }

    #[tokio::test]
    async fn quest_log_falls_back_to_row_without_result_event() {
        let st = state_with(quest_rules(""));
        let ts: chrono::DateTime<Utc> = "2026-01-01T10:00:00Z".parse().unwrap();
        for (ok, qid) in [(true, "a"), (false, "b")] {
            let id = st
                .store
                .insert_quest(&rpg_core::store::NewQuest {
                    quest_id: qid.into(), name: qid.into(), tier: "gold".into(), trigger_event_id: "t".into(), binding: json!({}),
                    steps: json!([]), accepted_at: ts, deadline: ts, chain_at_start: 2, multiplier: 1.5, xp: 501,
                })
                .unwrap();
            st.store.resolve_quest(id, if ok { "completed" } else { "failed" }, None, ts + chrono::Duration::minutes(ok as i64)).unwrap();
        }
        let v = get_json(&st, "/api/quests").await;
        assert_eq!((v["log"][0]["reward"].clone(), v["log"][0]["chain_lost"].clone()), (json!(751), Value::Null), "xp x multiplier, floored");
        assert_eq!((v["log"][1]["reward"].clone(), v["log"][1]["chain_lost"].clone()), (Value::Null, json!(2)), "chain_at_start");
    }

    #[tokio::test]
    async fn quest_reward_not_an_achievement_in_history() {
        let st = state_with(quest_rules(""));
        resolved_quest(&st, "ship_it", "Ship It", true, "2026-01-01T10:00:00Z");
        resolved_quest(&st, "ship_it", "Ship It", false, "2026-01-02T10:00:00Z");
        let v = get_json(&st, "/api/events").await;
        let (failed, done) = (&v[0], &v[1]);
        assert_eq!(done["kind"], "quest_completed");
        assert_eq!(done["quest_xp"], 750);
        assert_eq!(done["base_xp"], 0);
        assert_eq!(done["achievements"], json!([]));
        assert_eq!(done["xp"], 750);
        assert_eq!(done["label"], "Quest complete: Ship It");
        assert_eq!(failed["label"], "Quest failed: Ship It");
        assert_eq!(failed["quest_xp"], 0);
        let plain = get_json(&st, "/api/character").await;
        assert_eq!(plain["total_xp"], 750);
    }

    #[tokio::test]
    async fn achievement_on_quest_event_stays_an_achievement() {
        let rule = parse_rule(&RULE.replace("pr_merged", "quest_completed").replace("first-merge", "quest-ach")).unwrap();
        let st = state_with(Rules { list: vec![rule], ..quest_rules("") });
        let eid = resolved_quest(&st, "ship_it", "Ship It", true, "2026-01-01T10:00:00Z");
        let u = st
            .store
            .insert_unlock(&rpg_core::store::NewUnlock { rule_id: "quest-ach".into(), name: "Quester".into(), tier: "bronze".into(), xp: 250, context: json!({}), ts: Utc::now() })
            .unwrap();
        st.store.add_xp(&eid, 250, "Quester", Some(u), Utc::now()).unwrap();
        let v = get_json(&st, "/api/events").await;
        assert_eq!(v[0]["quest_xp"], 750);
        assert_eq!(v[0]["achievements"], json!([{ "name": "Quester", "xp": 250, "tier": "bronze" }]));
    }

    #[tokio::test]
    async fn disabled_quest_shown_off() {
        let st = state_with(quest_rules("enabled = false
"));
        let v = get_json(&st, "/api/quests").await;
        assert_eq!((v["board"][0]["enabled"].clone(), v["board"][1]["enabled"].clone()), (json!(true), json!(false)));
    }

    #[test]
    fn quests_section_hidden_when_off_and_styled_per_tier() {
        let html = include_str!("../../../ui/site/index.html");
        assert!(html.contains("id=\"quests\""), "index.html lacks the quests section");
        let js = include_str!("../../../ui/site/site.js");
        assert!(js.contains("/api/quests") && js.contains("enabled"), "site.js doesn't load quests");
        assert!(SITE_CSS.contains(".qlog"), "site.css lacks quest log styles");
    }

    const OVERLAY_CSS: &str = include_str!("../../../ui/overlay/overlay.css");
    const SITE_CSS: &str = include_str!("../../../ui/site/site.css");

    /// The value of the first `--<name>:` declaration in `css`.
    fn css_var<'a>(css: &'a str, name: &str) -> Option<&'a str> {
        let pat = format!("--{name}:");
        let rest = &css[css.find(&pat)? + pat.len()..];
        Some(rest[..rest.find(';')?].trim())
    }

    #[test]
    fn sheet_has_crest_header_and_no_inline_styles() {
        let html = include_str!("../../../ui/site/index.html");
        for needle in ["class=\"crest\"", "id=\"xptotal\"", "class=\"pill\""] {
            assert!(html.contains(needle), "index.html lacks {needle}");
        }
        assert!(!html.contains("style="), "inline style attributes are blocked by the CSP");
    }

    #[test]
    fn event_log_uses_chips_for_every_kind() {
        let html = include_str!("../../../ui/site/index.html");
        assert!(!html.contains("<select"), "the kind dropdown should be chips now");
        for kind in ["", "commit", "push", "pr_opened", "pr_merged", "review_submitted", "claude_session_start", "claude_session_end"] {
            assert!(html.contains(&format!("data-kind=\"{kind}\"")), "no chip for {kind:?}");
        }
    }

    #[test]
    fn event_rows_fit_large_exp() {
        // "+12,345 exp" must be formatted and must widen the exp column for every row, not spill.
        let js = include_str!("../../../ui/site/site.js");
        assert!(js.contains("\"+\" + num(base) + \" exp\""), "base exp is not formatted");
        assert!(js.contains("\" +\" + num(a.xp)"), "achievement exp is not formatted");
        assert!(SITE_CSS.contains("grid-template-columns: subgrid"), "event rows don't share column widths");
    }

    #[test]
    fn tier_palette_matches_between_overlay_and_site() {
        for tier in ["bronze", "silver", "gold", "platinum", "legendary", "celestial"] {
            let (o, s) = (css_var(OVERLAY_CSS, tier), css_var(SITE_CSS, tier));
            assert!(o.is_some() && s.is_some(), "--{tier} missing: overlay {o:?}, site {s:?}");
            assert_eq!(o, s, "--{tier} differs between overlay.css and site.css");
        }
        assert_eq!(css_var(OVERLAY_CSS, "silver"), Some("#c0c8d0"));
    }

    #[test]
    fn banner_styles_every_tier_and_reduced_motion() {
        for tier in ["Bronze", "Silver", "Gold", "Platinum", "Legendary", "Celestial"] {
            let sel = format!(".ach-b[data-tier=\"{tier}\"]");
            assert!(OVERLAY_CSS.contains(&sel), "overlay.css has no {sel} rule");
        }
        assert!(OVERLAY_CSS.contains("prefers-reduced-motion"), "banner ignores prefers-reduced-motion");
    }

    #[test]
    fn unlocked_cards_style_every_tier() {
        for tier in ["bronze", "silver", "gold", "platinum", "legendary", "celestial"] {
            let sel = format!(".ach.t-{tier}");
            assert!(SITE_CSS.contains(&sel), "site.css has no {sel} rule");
        }
        assert!(include_str!("../../../ui/site/site.js").contains("\"unlocked t-\" + a.tier"), "site.js doesn't tag unlocked cards with their tier");
    }

    /// The `<name> <timing>` animation entry in `css` after `anchor`, e.g. `tier-flash 1s ease-in-out .45s`.
    fn anim_after<'a>(css: &'a str, anchor: &str, name: &str) -> Option<&'a str> {
        let rule = &css[css.find(anchor)?..];
        let rule = &rule[..rule.find('}')?];
        let at = rule.find(&format!("{name} "))? + name.len() + 1;
        let rest = &rule[at..];
        Some(rest[..rest.find([',', ';']).unwrap_or(rest.len())].trim())
    }

    #[test]
    fn platinum_and_up_banner_flash_and_shimmer_run_together() {
        let flash = anim_after(OVERLAY_CSS, ".ach-b.in:is([data-tier=\"Platinum\"], [data-tier=\"Legendary\"], [data-tier=\"Celestial\"])", "tier-flash");
        let sweep = anim_after(OVERLAY_CSS, ".ach-b:is([data-tier=\"Platinum\"], [data-tier=\"Legendary\"], [data-tier=\"Celestial\"]).in::after", "shimmer");
        assert!(flash.is_some(), "no Platinum-and-up tier-flash");
        assert_eq!(flash, sweep, "flash and shimmer timings differ");
    }

    #[test]
    fn platinum_and_up_toast_pill_shimmers() {
        let sweep = anim_after(OVERLAY_CSS, ".xp.ach:is([data-tier=\"Platinum\"], [data-tier=\"Legendary\"], [data-tier=\"Celestial\"])::after", "shimmer");
        assert!(sweep.is_some(), "Platinum-and-up toast pill has no shimmer");
    }

    #[test]
    fn top_tiers_have_their_extra_flourishes() {
        for needle in [".ach-b.in[data-tier=\"Legendary\"]", ".ach-b.in[data-tier=\"Celestial\"]", "tier-pulse", ".ach-pulse", "tier-bloom", "name-drift", "twinkle"] {
            assert!(OVERLAY_CSS.contains(needle), "overlay.css lacks {needle}");
        }
        assert!(SITE_CSS.contains("card-sweep"), "site.css lacks the card sweep");
        let reduced = &OVERLAY_CSS[OVERLAY_CSS.find("prefers-reduced-motion").unwrap()..];
        assert!(reduced.contains("[data-tier=\"Celestial\"]::before { animation: none; }"), "reduced motion leaves the stars twinkling");
        assert!(SITE_CSS.contains("prefers-reduced-motion"), "site.css ignores reduced motion");
    }

    #[test]
    fn banner_reward_line_leaves_the_tier_to_its_color() {
        let js = include_str!("../../../ui/overlay/overlay.js");
        assert!(js.contains("set(\"reward\", b.reward)"), "overlay.js should show the reward without the tier name");
    }
}
