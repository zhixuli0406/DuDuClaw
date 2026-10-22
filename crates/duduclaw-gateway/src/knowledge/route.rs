//! `POST /api/internal/knowledge` — the only way the `knowledge_*` MCP tools
//! reach the knowledge base. Same authentication as the computer-use route
//! (`computer_use_sessions::auth`: loopback peer, signed employee identity,
//! timestamp window, single-use nonce, 120 requests per employee per
//! minute).
//!
//! Body `{op, …, turn_id?}`:
//!
//! | op | body | answer |
//! |---|---|---|
//! | `search` | `query`, `collections?` (slugs, narrowing only), `limit?` (≤ 20) | `hits`, `mode`, `vector_unavailable`, `scan_capped` |
//! | `get_passage` | `chunk_id`, `context?` (0–2) | passage with `before` / `match` / `after` |
//! | `list_collections` | — | the turn's effective collections with counts |
//! | `add_source` | `collection` (slug), one of `path` / `url` / `text`, `title?` | an approval card id (never ingests directly) |
//!
//! Every body may carry `run_id` (the caller's `DUDUCLAW_DISPATCH_RUN_ID`):
//! a cron or dispatched run registered by the gateway is a system turn.
//! Search results inside a live channel turn are numbered in that turn's
//! citation ledger ([`super::ledger`]), shared with the passages injected
//! into the prompt.
//!
//! The tools exist for an employee only while its own side (grants to it or
//! its department, narrowed by `[knowledge] collections`) is non-empty —
//! the same predicate the MCP listing uses. Permissions per call: the employee side (its own or its department's
//! grants, narrowed by its `[knowledge] collections`) ∩ the human side of the
//! turn ([`super::turn_requesters::resolve`]) ∩ the sensitivity filter. The
//! MCP side never reads the database itself, so it cannot skip this.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use duduclaw_auth::db::UserDb;
use duduclaw_knowledge::acl::{AllowedSet, Narrowing, effective_collections};
use duduclaw_knowledge::audit::{AccessEntry, log_access};
use duduclaw_knowledge::requester::{RequesterIdentity, UserDirectory, agent_ctx, human_ctx};
use duduclaw_knowledge::{KbError, schema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::computer_use_sessions::auth::{RateLimiter, ReplayGuard, authenticate, check_peer};

pub const ROUTE: &str = "/api/internal/knowledge";
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Longest query accepted.
pub const MAX_QUERY_CHARS: usize = 1_000;
/// Characters of context per `context` step in `get_passage`.
pub const CONTEXT_STEP_CHARS: usize = 400;

pub struct KnowledgeRouteState {
    pub home: PathBuf,
    pub users: Option<Arc<UserDb>>,
    replay: ReplayGuard,
    rate: RateLimiter,
}

impl KnowledgeRouteState {
    pub fn new(home: PathBuf, users: Option<Arc<UserDb>>) -> Self {
        Self {
            home,
            users,
            replay: ReplayGuard::default(),
            rate: RateLimiter::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Request {
    Search {
        query: String,
        #[serde(default)]
        collections: Option<Vec<String>>,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        turn_id: Option<String>,
        #[serde(default)]
        run_id: Option<String>,
    },
    GetPassage {
        chunk_id: i64,
        #[serde(default)]
        context: Option<u32>,
        #[serde(default)]
        turn_id: Option<String>,
        #[serde(default)]
        run_id: Option<String>,
    },
    ListCollections {
        #[serde(default)]
        turn_id: Option<String>,
        #[serde(default)]
        run_id: Option<String>,
    },
    AddSource {
        collection: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        turn_id: Option<String>,
        #[serde(default)]
        run_id: Option<String>,
    },
}

impl Request {
    pub(crate) fn turn_id(&self) -> Option<&str> {
        match self {
            Request::Search { turn_id, .. }
            | Request::GetPassage { turn_id, .. }
            | Request::ListCollections { turn_id, .. }
            | Request::AddSource { turn_id, .. } => turn_id.as_deref(),
        }
    }

    pub(crate) fn run_id(&self) -> Option<&str> {
        match self {
            Request::Search { run_id, .. }
            | Request::GetPassage { run_id, .. }
            | Request::ListCollections { run_id, .. }
            | Request::AddSource { run_id, .. } => run_id.as_deref(),
        }
    }
}

pub fn router(state: Arc<KnowledgeRouteState>) -> Router {
    Router::new()
        .route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "code": code, "message": message})),
    )
        .into_response()
}

fn unauthorized() -> Response {
    refuse(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "拒絕存取：這個端點只接受 DuDuClaw gateway 啟動的 MCP 程序在本機呼叫，並需通過員工身分驗證。",
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn handle(
    State(state): State<Arc<KnowledgeRouteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if check_peer(peer).is_err() {
        return unauthorized();
    }
    let Ok(body) = body else {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "請求內容太大或無法讀取（上限 64 KiB）。",
        );
    };
    let caller = match authenticate(&state.home, peer, &headers, &body, unix_now()) {
        Ok(c) => c,
        Err(f) => {
            tracing::debug!(reason = f.as_str(), "knowledge route refused");
            return unauthorized();
        }
    };
    let now = Instant::now();
    if !state.rate.allow(&caller.agent_id, now) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "知識庫查詢太頻繁（每位員工每分鐘最多 120 次），請稍後再試。",
        );
    }
    if !state.replay.record(&caller.nonce, now) {
        return unauthorized();
    }
    let Ok(request) = serde_json::from_slice::<Request>(&body) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "知識庫請求格式不正確。",
        );
    };
    if !super::enabled(&state.home) {
        return refuse(
            StatusCode::FORBIDDEN,
            "knowledge_disabled",
            "文件知識庫已由管理員關閉。",
        );
    }
    if let Request::AddSource { .. } = &request {
        return match super::route_add::serve_add_source(&state.home, &caller.agent_id, request).await {
            Ok(v) => (StatusCode::OK, Json(v)).into_response(),
            Err(Refusal(status, code, msg)) => refuse(status, code, &msg),
        };
    }
    let requester = super::turn_requesters::resolve_with_run(
        &caller.agent_id,
        request.turn_id(),
        request.run_id(),
    );
    let st = state.clone();
    let agent = caller.agent_id.clone();
    let embedder = match &request {
        Request::GetPassage { .. } => None,
        _ => Some(super::embedding::resolve(&state.home).await),
    };
    let result =
        tokio::task::spawn_blocking(move || serve_with(&st, &agent, requester, request, embedder)).await;
    match result {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(Refusal(status, code, msg))) => refuse(status, code, &msg),
        Err(_) => refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            "error",
            "知識庫暫時無法使用，請稍後再試。",
        ),
    }
}

#[derive(Debug)]
pub(crate) struct Refusal(pub StatusCode, pub &'static str, pub String);

impl From<KbError> for Refusal {
    fn from(e: KbError) -> Self {
        match e {
            KbError::PermissionDenied => Refusal(
                StatusCode::FORBIDDEN,
                "permission_denied",
                "permission denied".into(),
            ),
            KbError::Invalid(m) => Refusal(StatusCode::BAD_REQUEST, "bad_request", m),
            other => {
                tracing::error!(error = %other, "knowledge route failed");
                Refusal(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "error",
                    other.public_message(),
                )
            }
        }
    }
}

pub(crate) fn agent_dir(home: &std::path::Path, agent: &str) -> PathBuf {
    crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent))
}

type SharedEmbedder = Option<std::sync::Arc<dyn duduclaw_knowledge::embed::Embedder>>;

/// Keyword-only `serve_with` (tests).
#[cfg(test)]
fn serve(
    st: &KnowledgeRouteState,
    agent: &str,
    requester: RequesterIdentity,
    request: Request,
) -> Result<Value, Refusal> {
    serve_with(st, agent, requester, request, None)
}

fn serve_with(
    st: &KnowledgeRouteState,
    agent: &str,
    requester: RequesterIdentity,
    request: Request,
    embedder: SharedEmbedder,
) -> Result<Value, Refusal> {
    let (scope, department) =
        duduclaw_core::knowledge_agent::load_agent_knowledge(&agent_dir(&st.home, agent));
    if scope.is_off() {
        return Err(Refusal(
            StatusCode::FORBIDDEN,
            "knowledge_off",
            "這位 AI 員工沒有啟用文件知識庫。".into(),
        ));
    }
    let conn = schema::open_home(&st.home)?;
    let agent_side = agent_ctx(agent, department.as_deref(), &scope);
    // Same predicate as the MCP listing: no collection on the employee side
    // means the tools do not exist for this employee.
    if duduclaw_knowledge::acl::agent_side_collections(&conn, &agent_side)?.is_empty() {
        return Err(Refusal(
            StatusCode::FORBIDDEN,
            "knowledge_off",
            "這位 AI 員工沒有可用的知識集合（需要先把集合授權給它或它的部門）。".into(),
        ));
    }
    let directory: Box<dyn UserDirectory> = match &st.users {
        Some(db) => Box::new(super::UserDbDirectory(db.clone())),
        None => Box::new(super::NoUsers),
    };
    let human = human_ctx(&requester, directory.as_ref());
    let narrowing = match &request {
        Request::Search {
            collections: Some(c),
            ..
        } => Narrowing {
            only: Some(c.clone()),
        },
        _ => Narrowing::default(),
    };
    let mut entry = AccessEntry {
        agent_id: Some(agent.to_string()),
        requester_kind: requester.kind().to_string(),
        requester_id: requester.audit_id(),
        channel: requester.channel().map(str::to_string),
        turn_id: request
            .turn_id()
            .map(|t| duduclaw_core::truncate_chars(t, 128)),
        outcome: "ok".into(),
        ..Default::default()
    };
    let allowed = match effective_collections(&conn, &human, Some(&agent_side), &narrowing) {
        Ok(a) => a,
        Err(e) => {
            entry.op = "search".into();
            entry.outcome = "error".into();
            let _ = log_access(&conn, &entry);
            return Err(e.into());
        }
    };
    entry.allowed = allowed.ids().to_vec();
    let vectors = Vectors {
        embedder: embedder.unwrap_or_else(|| std::sync::Arc::new(duduclaw_knowledge::embed::NoEmbedder)),
        cache: super::embedding::cache(&st.home),
    };
    let result = run_op(&conn, &allowed, &request, &mut entry, &vectors, agent);
    if result.is_err() && entry.outcome == "ok" {
        entry.outcome = "denied".into();
    }
    if let Err(e) = log_access(&conn, &entry) {
        tracing::warn!(error = %e, "knowledge: access log write failed");
    }
    result
}

/// The vector side of a request: the embedder in effect and the cache.
struct Vectors {
    embedder: std::sync::Arc<dyn duduclaw_knowledge::embed::Embedder>,
    cache: std::sync::Arc<duduclaw_knowledge::embed::vector_cache::VectorCache>,
}

fn run_op(
    conn: &rusqlite::Connection,
    allowed: &AllowedSet,
    request: &Request,
    entry: &mut AccessEntry,
    vectors: &Vectors,
    agent: &str,
) -> Result<Value, Refusal> {
    match request {
        Request::Search { query, limit, .. } => {
            entry.op = "search".into();
            let query = duduclaw_core::truncate_chars(query.trim(), MAX_QUERY_CHARS);
            entry.query = Some(query.clone());
            if query.is_empty() {
                return Err(Refusal(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "缺少查詢文字（query）。".into(),
                ));
            }
            let limit = limit.unwrap_or(5).clamp(1, 20) as usize;
            let r = duduclaw_knowledge::search::search(
                conn,
                allowed,
                &query,
                limit,
                vectors.embedder.as_ref(),
                &vectors.cache,
            )?;
            if let Some(reason) = &r.vector_unavailable {
                super::embedding::warn_keyword_only(reason);
            }
            entry.mode = Some(r.mode.as_str().to_string());
            // Inside a live channel turn, number hits in the turn's ledger
            // (shared with the injected passages); else list order.
            let numbers = request
                .turn_id()
                .map(|t| super::ledger::number(agent, t, &r.hits, &query))
                .unwrap_or_else(|| vec![None; r.hits.len()]);
            let numbered: Vec<u32> = numbers
                .iter()
                .enumerate()
                .map(|(i, n)| n.unwrap_or(i as u32 + 1))
                .collect();
            entry.results = r
                .hits
                .iter()
                .zip(&numbered)
                .enumerate()
                .map(|(i, (h, n))| json!({"chunk_id": h.chunk_id, "source_id": h.source_id, "n": n, "rank": i + 1}))
                .collect();
            if r.hits.is_empty() {
                entry.outcome = "no_match".into();
            }
            let collections = collection_names(conn, allowed)?;
            let hits: Vec<Value> = r
                .hits
                .iter()
                .zip(&numbered)
                .map(|(h, n)| {
                    let mut v = serde_json::to_value(h).unwrap_or(Value::Null);
                    if let Some(m) = v.as_object_mut() {
                        m.insert("n".into(), json!(n));
                    }
                    v
                })
                .collect();
            Ok(json!({
                "ok": true,
                "no_collections": allowed.is_empty(),
                "hits": hits,
                "mode": r.mode,
                "vector_unavailable": r.vector_unavailable,
                "coverage": r.coverage,
                "scan_capped": r.scan_capped,
                "collections": collections,
            }))
        }
        Request::GetPassage {
            chunk_id, context, ..
        } => {
            entry.op = "get_passage".into();
            let ctx = context.unwrap_or(1).min(2) as usize * CONTEXT_STEP_CHARS;
            let p = duduclaw_knowledge::passage::get_passage(conn, *chunk_id, allowed, ctx)?;
            entry.collection_id = Some(p.collection_id.clone());
            entry.results = vec![json!({"chunk_id": p.chunk_id, "source_id": p.source_id})];
            let mut v = serde_json::to_value(&p).unwrap_or(Value::Null);
            if let Some(m) = v.as_object_mut() {
                m.insert("ok".into(), json!(true));
            }
            Ok(v)
        }
        Request::ListCollections { .. } => {
            entry.op = "list".into();
            let list = duduclaw_knowledge::store::list_by_ids(conn, allowed.ids())?;
            let e = vectors.embedder.as_ref();
            let available = duduclaw_knowledge::embed::is_available(e);
            let excluded = match e.egress() {
                duduclaw_knowledge::embed::Egress::Cloud { .. } => {
                    duduclaw_knowledge::embed::vectors::cloud_excluded_collections(conn)?
                }
                duduclaw_knowledge::embed::Egress::Local => Default::default(),
            };
            let mut items: Vec<Value> = Vec::with_capacity(list.len());
            for c in &list {
                let coverage = if available {
                    let (have, total) = duduclaw_knowledge::embed::vectors::coverage(
                        conn,
                        e.model_id(),
                        Some(std::slice::from_ref(&c.collection.id)),
                        &excluded,
                    )?;
                    if total > 0 { have as f64 / total as f64 } else { 0.0 }
                } else {
                    0.0
                };
                items.push(json!({
                    "slug": c.collection.slug,
                    "name": c.collection.name,
                    "description": c.collection.description,
                    "sensitivity": c.collection.sensitivity,
                    "sources": c.ready_count,
                    "chunks": c.chunk_count,
                    "vector_coverage": coverage,
                }));
            }
            let reason = if available {
                None
            } else {
                match e.embed_query("") {
                    Err(duduclaw_knowledge::embed::EmbedError::Unavailable(r) | duduclaw_knowledge::embed::EmbedError::Failed(r)) => Some(r),
                    Ok(_) => None,
                }
            };
            Ok(json!({
                "ok": true,
                "collections": items,
                "mode": if available { "hybrid" } else { "keyword_only" },
                "vector_unavailable": reason,
            }))
        }
        Request::AddSource { .. } => Err(Refusal(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "知識庫請求格式不正確。".into(),
        )),
    }
}

/// `id → name` of the allowed collections (for result labels).
fn collection_names(conn: &rusqlite::Connection, allowed: &AllowedSet) -> Result<Value, Refusal> {
    let mut map = serde_json::Map::new();
    for id in allowed.ids() {
        if let Some(c) = duduclaw_knowledge::store::get_collection(conn, id)? {
            map.insert(c.id, json!(c.name));
        }
    }
    Ok(Value::Object(map))
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
