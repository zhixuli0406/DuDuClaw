//! Signed, loopback-only administrator settings access. Request bodies never carry authority.
use std::{net::SocketAddr, path::{Path, PathBuf}, sync::Arc, time::Instant};

use axum::{body::Bytes, extract::{rejection::BytesRejection, ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::post, Json, Router};
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::UserContext;
use duduclaw_core::admin_ai_settings::AdminAiField;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::approval::ConversationAudience;
use crate::computer_use_sessions::auth::{authenticate, check_peer, RateLimiter, ReplayGuard};
use crate::on_behalf_of::admin_turn::{admin_turn, AdminTurn, AdminTurnLevel};

pub const ROUTE: &str = "/api/internal/admin-ai";
const PROOF_LIMIT: usize = 4 * 1024;
const READ_LIMIT: usize = 64 * 1024;

pub struct RouteState {
    home: PathBuf,
    executor: Option<super::execute::Executor>,
    replay: ReplayGuard,
    proof_rate: RateLimiter,
    read_rate: RateLimiter,
}
impl RouteState {
    fn empty(home: PathBuf) -> Self {
        Self { home, executor: None, replay: ReplayGuard::default(), proof_rate: RateLimiter::default(),
            read_rate: RateLimiter::default() }
    }
    pub(crate) fn with_executor(home: PathBuf, executor: super::execute::Executor) -> Self {
        Self { executor: Some(executor), ..Self::empty(home) }
    }
    #[cfg(test)]
    pub fn new(home: PathBuf) -> Self { Self::empty(home) }
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Section { Profile, Model, Limits, Heartbeat, Schedules, Skills }

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Proof { context: String },
    Read { context: String, agent_ids: Option<Vec<String>>, sections: Option<Vec<Section>> },
    Propose { context: String, items: Vec<duduclaw_core::admin_ai_settings::Item>, reason: String },
}

// Only selects the size/rate bucket; never grants authority or accepts op fields.
#[derive(Deserialize)]
struct Selector { op: String }

pub fn router(state: Arc<RouteState>) -> Router {
    Router::new().route(ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(READ_LIMIT)).with_state(state)
}
fn refusal(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"ok": false, "code": code}))).into_response()
}
fn denied(home: &Path, agent: &str, code: &str) -> Response {
    use duduclaw_security::{audit::{AuditEvent, Severity}, credential_protection::sanitize_output_text};
    let agent = duduclaw_core::truncate_chars(&sanitize_output_text(agent), 200).to_owned();
    let reason = duduclaw_core::truncate_chars(&sanitize_output_text(code), 200).to_owned();
    crate::security_autopilot::audit_and_emit(home, &AuditEvent::new(
        "admin_ai_settings_refused", &agent, Severity::Warning,
        json!({"agent": agent, "reason": reason}),
    ));
    refusal(StatusCode::FORBIDDEN, code)
}
async fn handle(
    State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap, body: Result<Bytes, BytesRejection>,
) -> Response {
    if check_peer(peer).is_err() { return refusal(StatusCode::UNAUTHORIZED, "invalid_peer"); }
    let Ok(body) = body else { return refusal(StatusCode::PAYLOAD_TOO_LARGE, "body_limit"); };
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0);
    let Ok(caller) = authenticate(&state.home, peer, &headers, &body, timestamp) else {
        return refusal(StatusCode::UNAUTHORIZED, "invalid_signature");
    };
    if duduclaw_core::is_reserved_agent_id(&caller.agent_id) {
        return refusal(StatusCode::FORBIDDEN, "reserved_agent");
    }
    let selector = serde_json::from_slice::<Selector>(&body).ok();
    let proof = selector.as_ref().is_some_and(|s| s.op == "proof");
    let now = Instant::now();
    let rate = if proof { &state.proof_rate } else { &state.read_rate };
    if !rate.allow(&caller.agent_id, now) { return refusal(StatusCode::TOO_MANY_REQUESTS, "rate_limit"); }
    if !state.replay.record(&caller.nonce, now) { return refusal(StatusCode::UNAUTHORIZED, "replayed_nonce"); }
    if proof && body.len() > PROOF_LIMIT { return refusal(StatusCode::PAYLOAD_TOO_LARGE, "body_limit"); }
    let Ok(request) = serde_json::from_slice::<Request>(&body) else {
        if selector.as_ref().is_some_and(|s| s.op == "propose") {
            let _ = denied(&state.home, &caller.agent_id, "admin_ai_invalid_proposal");
        }
        return refusal(StatusCode::BAD_REQUEST, "invalid_request");
    };
    match request {
        Request::Propose { context, items, reason } => {
            let turn = match admin_turn(&state.home, &caller.agent_id, &context, AdminTurnLevel::Write) {
                Ok(turn) => turn,
                Err(error) => return denied(&state.home, &caller.agent_id, error.code()),
            };
            let proposal = match serde_json::from_value(json!({"items": items, "reason": reason})) {
                Ok(proposal) => proposal,
                Err(_) => return denied(&state.home, &caller.agent_id, "admin_ai_invalid_proposal"),
            };
            let result = super::proposal::propose(&state.home, &turn, &context, proposal, state.executor.clone()).await;
            match result {
                Ok(value) => Json(value).into_response(),
                Err(error) => {
                    let _ = denied(&state.home, &caller.agent_id, error.code);
                    (StatusCode::FORBIDDEN, Json(json!({"ok": false, "code": error.code,
                        "message": error.message}))).into_response()
                }
            }
        }
        Request::Proof { context } => {
            let read = admin_turn(&state.home, &caller.agent_id, &context, AdminTurnLevel::Read).is_ok();
            let write = admin_turn(&state.home, &caller.agent_id, &context, AdminTurnLevel::Write).is_ok();
            // A watcher result is only a hint; even this hint must still be live.
            let read = read && admin_turn(&state.home, &caller.agent_id, &context, AdminTurnLevel::Read).is_ok();
            let write = write && admin_turn(&state.home, &caller.agent_id, &context, AdminTurnLevel::Write).is_ok();
            Json(json!({"ok": true, "read": read, "write": write})).into_response()
        }
        Request::Read { context, agent_ids, sections } => {
            if agent_ids.as_ref().is_some_and(|ids| ids.len() > 200) {
                return refusal(StatusCode::BAD_REQUEST, "agent_limit");
            }
            let turn = match admin_turn(&state.home, &caller.agent_id, &context, AdminTurnLevel::Read) {
                Ok(turn) => turn,
                Err(error) => return denied(&state.home, &caller.agent_id, error.code()),
            };
            let result = read(&state.home, &turn, agent_ids.as_deref(), sections.as_deref()).await;
            let fresh = match admin_turn(&state.home, &caller.agent_id, &context, AdminTurnLevel::Read) {
                Ok(fresh) => fresh,
                Err(error) => return denied(&state.home, &caller.agent_id, error.code()),
            };
            if fresh.user_id != turn.user_id || fresh.session_id != turn.session_id
                || fresh.turn_id != turn.turn_id || fresh.audience != turn.audience {
                return denied(&state.home, &caller.agent_id, "admin_ai_turn_mismatch");
            }
            match result {
                Ok(value) => Json(value).into_response(),
                Err(code) => denied(&state.home, &caller.agent_id, code),
            }
        }
    }
}

fn wants(sections: Option<&[Section]>, section: Section) -> bool {
    sections.is_none_or(|sections| sections.contains(&section))
}
fn field_section(field: AdminAiField) -> Section {
    use AdminAiField::*;
    match field {
        DisplayName | Icon | Trigger | Status | ReplyLanguage => Section::Profile,
        Model | FallbackModel | Effort => Section::Model,
        TimeoutSecs | MaxTurns | IdleTimeoutSecs => Section::Limits,
        HeartbeatEnabled | HeartbeatCron | HeartbeatInterval | HeartbeatCronTimezone => Section::Heartbeat,
    }
}
pub(super) fn fresh_user(home: &Path, id: &str) -> Result<UserContext, &'static str> {
    crate::decision_notify::open_user_db_read_only(home)
        .map_err(|_| "admin_ai_account_unavailable")?
        .ok_or("admin_ai_account_unavailable")?
        .live_user_context(id).map_err(|_| "admin_ai_account_unavailable")?
        .filter(|ctx| ctx.is_admin() && !ctx.must_change_password)
        .ok_or("admin_ai_account_unavailable")
}
pub(super) fn target_allowed(home: &Path, ctx: &UserContext, id: &str, dir: &Path, table: &toml::Value) -> bool {
    if !duduclaw_core::is_valid_agent_id(id) || duduclaw_core::is_reserved_agent_id(id)
        || crate::ephemeral::is_ephemeral_id(id) || dir != home.join("agents").join(id)
        || table.get(crate::ephemeral::ROLE_MEMBER_SECTION).is_some()
        || ctx.owner_only_denied(id) {
        return false;
    }
    if table.get("agent").and_then(|a| a.get("name")).and_then(toml::Value::as_str) != Some(id) {
        return false;
    }
    // Unknown lifecycle values are not a usable target. Absent means Active.
    if table.get("agent").and_then(|a| a.get("status")).is_some_and(|status|
        !matches!(status.as_str(), Some("active" | "paused" | "archived"))) { return false; }
    // The mirror check also works when the process-wide ACL restriction is
    // not installed. Missing/corrupt ownership is never treated as public.
    let Ok(db) = rusqlite::Connection::open_with_flags(home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return false; };
    match duduclaw_auth::db::personal_agent_owner(&db, id) {
        Ok(None) => true,
        Ok(Some(owner)) => owner == ctx.user_id,
        Err(_) => false,
    }
}
pub(crate) fn effective_text(dir: &Path) -> Result<String, &'static str> {
    // Match the strict runtime reader: an existing resolved artifact that cannot
    // be read must not silently fall back to the employee's source profile.
    if let (Some(home), Some(id)) = (duduclaw_core::preset::agent_home_dir(dir), dir.file_name().and_then(|s| s.to_str())) {
        let resolved = duduclaw_core::preset::agent_resolved_path(&home, id);
        match std::fs::symlink_metadata(&resolved) {
            Ok(_) => return std::fs::read_to_string(resolved).map_err(|_| "admin_ai_settings_unavailable"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err("admin_ai_settings_unavailable"),
        }
    }
    duduclaw_core::agent_toml::read_effective_text(dir).map_err(|_| "admin_ai_settings_unavailable")
}
pub(super) fn effective(dir: &Path) -> Result<toml::Value, &'static str> {
    effective_text(dir)?.parse().map_err(|_| "admin_ai_settings_unavailable")
}
fn fields(table: &toml::Value, sections: Option<&[Section]>) -> Result<Value, &'static str> {
    let mut fields = Map::new();
    for field in AdminAiField::ALL.iter().copied().filter(|field| wants(sections, field_section(*field))) {
        let value = field.toml_path().split('.').try_fold(table, |value, key| value.get(key));
        // Missing settings remain null (inherited/default), never guessed.
        let value = value.map(serde_json::to_value).transpose().map_err(|_| "admin_ai_settings_unavailable")?
            .unwrap_or(Value::Null);
        // A malformed table/array at a leaf must not smuggle arbitrary keys.
        if value.is_object() || value.is_array() { return Err("admin_ai_settings_unavailable"); }
        fields.insert(field.tool_name().into(), json!({"label": field.label("zh-TW"), "value": value}));
    }
    Ok(Value::Object(fields))
}
async fn read(home: &Path, turn: &AdminTurn, ids: Option<&[String]>, sections: Option<&[Section]>) -> Result<Value, &'static str> {
    let ctx = fresh_user(home, &turn.user_id)?;
    let mut registry = AgentRegistry::new(home.join("agents"));
    registry.scan().await.map_err(|_| "admin_ai_registry_unavailable")?;
    let selected: Vec<String> = ids.map(|ids| ids.to_vec()).unwrap_or_else(||
        registry.list().iter().map(|a| a.config.agent.name.clone()).collect());
    let mut agents = Vec::new();
    let mut not_available = Vec::new();
    let private = turn.audience == ConversationAudience::Private;
    let models = if wants(sections, Section::Model) {
        // Same discovery cache and merger as models.list; reads never launch a CLI probe.
        crate::runtime_models::merged_models(&crate::runtime_models::load_cache(home).await.unwrap_or_default())
    } else { Vec::new() };
    let policy = crate::product_policy::load(home).map_err(|_| "admin_ai_policy_unavailable")?;
    let global_limits = if wants(sections, Section::Limits) {
        let config = std::fs::read_to_string(home.join("config.toml"))
            .map_err(|_| "admin_ai_policy_unavailable")?.parse::<toml::Table>()
            .map_err(|_| "admin_ai_policy_unavailable")?;
        let timeout = crate::runtime_config::TimeoutPolicy::from_table(&config).map_err(|_| "admin_ai_policy_unavailable")?;
        let turns = duduclaw_core::turn_limits::TurnPolicy::validate(&config).map_err(|_| "admin_ai_policy_unavailable")?;
        Some(json!({"timeout_max_secs": timeout.max_secs, "turn_limit": turns.limit}))
    } else { None };
    let store = if wants(sections, Section::Schedules) {
        Some(crate::cron_store::CronStore::open(home).map_err(|_| "admin_ai_schedules_unavailable")?)
    } else { None };
    let schedules = match &store {
        Some(store) => store.list_all().await.map_err(|_| "admin_ai_schedules_unavailable")?,
        None => Vec::new(),
    };
    for id in selected {
        if not_available.contains(&id) || agents.iter().any(|a: &Value| a["agent_id"].as_str() == Some(id.as_str())) { continue; }
        let Some(agent) = registry.get(&id) else { not_available.push(id); continue; };
        let table = match effective(&agent.dir) {
            Ok(table) if target_allowed(home, &ctx, &id, &agent.dir, &table) => table,
            _ => { not_available.push(id); continue; }
        };
        let mut row = json!({"agent_id": id, "fields": fields(&table, sections)?});
        if wants(sections, Section::Model) {
            let runtime = crate::runtime_config::load_runtime_settings_strict(&agent.dir)
                .map_err(|_| "admin_ai_settings_unavailable")?;
            let selectable: Vec<Value> = models.iter().filter(|model| {
                let provider = model["provider"].as_str().unwrap_or("");
                policy.require_runtime(provider).is_ok() && policy.require_provider(provider).is_ok()
                    && model["id"].as_str().is_some_and(|id|
                        crate::runtime_config::model_matches_provider(id, runtime.provider))
            }).map(|model| json!({"id": model["id"], "label": model["label"]})).collect();
            row["engine"] = json!(runtime.provider.spec().id);
            row["selectable_models"] = json!(selectable);
        }
        if let Some(limits) = &global_limits { row["global_limits"] = limits.clone(); }
        if let Some(store) = &store {
            let mut rows = Vec::new();
            for schedule in schedules.iter().filter(|s| s.agent_id == id && prompt_only(s)) {
                let bound = store.workflow_binding(&schedule.id).await.map_err(|_| "admin_ai_schedules_unavailable")?.is_some();
                let mut value = json!({"id": schedule.id, "name": schedule.name, "cron": schedule.cron,
                    "timezone": schedule.cron_timezone, "enabled": schedule.enabled, "admin_ai_may_modify": !bound});
                if private { value["task"] = json!(schedule.task); }
                rows.push(value);
            }
            row["schedules"] = json!(rows);
        }
        if wants(sections, Section::Skills) {
            row["skills"] = skills(home, &id, &agent.dir, private).await?;
        }
        agents.push(row);
    }
    // Recheck ownership and target lifecycle after all asynchronous reads.
    let fresh = fresh_user(home, &turn.user_id)?;
    for row in &agents {
        let id = row["agent_id"].as_str().ok_or("admin_ai_settings_unavailable")?;
        let dir = home.join("agents").join(id);
        if !target_allowed(home, &fresh, id, &dir, &effective(&dir)?) {
            return Err("admin_ai_target_changed");
        }
    }
    // Omitted target selection must not disclose even the ids of hidden employees.
    if ids.is_none() { not_available.clear(); }
    Ok(json!({"ok": true, "agents": agents, "not_available": not_available}))
}
pub(super) fn prompt_only(row: &crate::cron_store::CronTaskRow) -> bool {
    row.trigger_kind == "time" && row.condition_script.is_none() && row.watch_command.is_none()
        && row.notify_channel.is_none() && row.notify_chat_id.is_none() && row.notify_thread_id.is_none()
}
pub(super) async fn skills(home: &Path, id: &str, dir: &Path, private: bool) -> Result<Value, &'static str> {
    use duduclaw_agent::{skill_loader, skill_state};
    let global = AgentRegistry::load_skills(&home.join("skills")).await;
    let local = AgentRegistry::load_skills(&dir.join("SKILLS")).await;
    let department = match skill_loader::authoritative_department(home, id) {
        Some(dept) => AgentRegistry::load_skills(&skill_loader::department_skills_dir(home, &dept)).await,
        None => Vec::new(),
    };
    let state = skill_state::read_disabled(home, id);
    if state.error().is_some() { return Err("admin_ai_skills_unavailable"); }
    let rows: Vec<Value> = AgentRegistry::compose_skill_layers(&global, department, local).iter().map(|skill| {
        let mut row = json!({"name": skill.name, "enabled": !state.is_disabled(&skill.name)});
        if private {
            let meta = skill_loader::parse_skill_meta_from_content(&skill.content, &skill.name);
            row["description"] = json!(meta.display_description("zh-TW"));
        }
        row
    }).collect();
    Ok(json!(rows))
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
