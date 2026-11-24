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
use rpg_core::rules::Rules;
use rpg_core::store::{EventRow, Store};
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

fn event_json(r: &EventRow) -> Value {
    let e = &r.event;
    json!({
        "id": e.id, "kind": e.kind.as_str(), "ts": e.ts, "repo": e.repo,
        "backfill": e.backfill, "xp": r.xp,
    })
}

fn tier_str(t: rpg_core::rules::Tier) -> &'static str {
    use rpg_core::rules::Tier;
    match t {
        Tier::Bronze => "bronze",
        Tier::Silver => "silver",
        Tier::Gold => "gold",
    }
}

async fn character(State(st): State<SiteState>) -> Result<Json<Value>, ApiError> {
    let cfg = st.cfg.read().unwrap_or_else(|p| p.into_inner()).clone();
    let total = st.store.total_xp().map_err(internal)?;
    let (level, into_level, level_span) = xp::progress(total, &cfg.levels);
    let count = |k| st.store.count_live_events(k).map_err(internal);
    let recent: Vec<Value> = st.store.list_events(None, RECENT).map_err(internal)?.iter().map(event_json).collect();
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
    })))
}

async fn achievements(State(st): State<SiteState>) -> Result<Json<Value>, ApiError> {
    let rules = st.rules.read().unwrap_or_else(|p| p.into_inner()).clone();
    let unlocks = st.store.list_unlocks().map_err(internal)?; // newest first
    let mut out = Vec::new();
    for r in &rules.list {
        let count = st.store.unlock_count(&r.id).map_err(internal)?;
        let last = unlocks.iter().find(|u| u.rule_id == r.id);
        let unlocked = count > 0;
        out.push(json!({
            "id": r.id,
            "name": if unlocked { r.name.as_str() } else { MASK },
            "tier": tier_str(r.tier),
            "unlocked": unlocked,
            "count": count,
            "last_at": st.store.last_unlock_at(&r.id).map_err(internal)?,
            "flavor_text_latest": last.map(|u| u.flavor_text.clone().unwrap_or_else(|| r.fallback.clone())),
            "hint": if unlocked { r.hint.clone() } else { Some(r.hint.clone().unwrap_or_else(|| MASK.into())) },
        }));
    }
    // Unlocks whose rule was since removed or renamed stay visible, using the stored row.
    let mut seen = std::collections::HashSet::new();
    for u in &unlocks {
        if rules.list.iter().any(|r| r.id == u.rule_id) || !seen.insert(u.rule_id.as_str()) {
            continue;
        }
        let count = unlocks.iter().filter(|x| x.rule_id == u.rule_id).count();
        out.push(json!({
            "id": u.rule_id, "name": u.name, "tier": u.tier, "unlocked": true, "count": count,
            "last_at": u.ts, "flavor_text_latest": u.flavor_text, "hint": Value::Null,
        }));
    }
    Ok(Json(Value::Array(out)))
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
    Ok(Json(Value::Array(rows.iter().map(event_json).collect())))
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
detector = { type = "first", scope = "ever" }
"#;

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
        let st = state_with(Rules { list: vec![parse_rule(RULE).unwrap()] });
        let v = get_json(&st, "/api/achievements").await;
        let a = &v.as_array().unwrap()[0];
        assert_eq!(a["id"], "first-merge");
        assert_eq!(a["name"], "???");
        assert_eq!(a["unlocked"], false);
        assert_eq!(a["count"], 0);
        assert_eq!(a["hint"], "Merge something.");
        assert!(a["flavor_text_latest"].is_null());
    }

    #[tokio::test]
    async fn unlocked_achievement_shows_name_and_flavor_or_fallback() {
        let st = state_with(Rules { list: vec![parse_rule(RULE).unwrap()] });
        process(&st, &ev("m1", EventKind::PrMerged));
        let v = get_json(&st, "/api/achievements").await;
        let a = &v[0];
        assert_eq!(a["name"], "Merge Maestro");
        assert_eq!(a["unlocked"], true);
        assert_eq!(a["count"], 1);
        assert!(a["last_at"].is_string());
        assert_eq!(a["flavor_text_latest"], "You merged.", "falls back to the rule's fallback");
        let id = st.store.list_unlocks().unwrap()[0].id;
        st.store.set_unlock_flavor(id, "LLM words").unwrap();
        assert_eq!(get_json(&st, "/api/achievements").await[0]["flavor_text_latest"], "LLM words");
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
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["name"], "Old Glory");
        assert_eq!(v[0]["tier"], "gold");
        assert_eq!(v[0]["unlocked"], true);
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
}
