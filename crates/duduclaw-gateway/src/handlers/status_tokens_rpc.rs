//! RM-064 status credentials: `me.status_tokens.*` (each staff member's own)
//! and `status_tokens.admin_*` (Admin overview), plus the in-process snapshot
//! behind `GET /api/v1/status/employees` (`crate::external_status`).
//!
//! Routed from `MethodHandler::dispatch` before extension dispatch, like the
//! other gateway-owned credential namespaces, so no plugin ever receives a
//! freshly issued secret. The secret appears in exactly one response — the
//! one to `me.status_tokens.create` — and is never logged.

use super::*;
use duduclaw_auth::status_tokens::{MAX_ACTIVE_STATUS_TOKENS, StatusTokenError};

use crate::external_status::{EmployeeStatus, LatestDone, STATUS_PATH, day_bounds, safe_title};

/// Longest credential id accepted from a request (ids are UUIDs).
const TOKEN_ID_MAX: usize = 64;

/// Explicit role floor for the namespace. Unknown members of either
/// namespace are reserved for Admin and then refused by the handler.
pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "me.status_tokens.list" | "me.status_tokens.create" | "me.status_tokens.revoke" => {
            Some(UserRole::Employee)
        }
        "status_tokens.admin_list" | "status_tokens.admin_revoke" => Some(UserRole::Admin),
        _ if method.split('.').next() == Some("status_tokens")
            || method.split('.').take(2).eq(["me", "status_tokens"]) =>
        {
            Some(UserRole::Admin)
        }
        _ => None,
    }
}

fn coded(code: &str, message: &str) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({ "code": code, "message": message })),
    }
}

fn token_error(e: StatusTokenError) -> WsFrame {
    let message = match e {
        StatusTokenError::InvalidName => "名稱必須是 1 到 60 個字，且不能包含換行。",
        StatusTokenError::LimitReached => {
            "每個帳號最多同時保有 5 組查詢憑證，請先撤銷不再使用的憑證。"
        }
        StatusTokenError::AccountInactive => "目前的帳號無法建立查詢憑證。",
        StatusTokenError::NotFound => "找不到這組查詢憑證。",
        StatusTokenError::Store => "目前無法處理查詢憑證，請稍後再試。",
    };
    coded(e.code(), message)
}

fn disabled_frame() -> WsFrame {
    coded(
        "external_status_disabled",
        "管理員尚未開啟「外部系統查詢 AI 員工狀態」，目前無法建立查詢憑證。",
    )
}

fn token_id_param(params: &Value) -> Option<String> {
    params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.len() <= TOKEN_ID_MAX)
        .map(str::to_owned)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, StatusTokenError> + Send + 'static,
) -> Result<T, StatusTokenError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or(Err(StatusTokenError::Store))
}

impl MethodHandler {
    pub(crate) async fn handle_status_tokens_rpc(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else {
            return token_error(StatusTokenError::Store);
        };
        let enabled = crate::external_status::enabled(&self.home_dir);
        let me = ctx.user_id.clone();
        match method {
            "me.status_tokens.list" => {
                match blocking(move || db.status_tokens_for_user(&me)).await {
                    Ok(tokens) => WsFrame::ok_response(
                        "",
                        json!({
                            "enabled": enabled,
                            "max_active": MAX_ACTIVE_STATUS_TOKENS,
                            "endpoint_path": STATUS_PATH,
                            "tokens": tokens,
                        }),
                    ),
                    Err(e) => token_error(e),
                }
            }
            "me.status_tokens.create" => {
                if !enabled {
                    return disabled_frame();
                }
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match blocking(move || db.status_token_create(&me, &name)).await {
                    Ok(created) => {
                        info!(token_id = %created.info.id, "status credential created");
                        WsFrame::ok_response(
                            "",
                            json!({
                                // Shown once. Only its SHA-256 digest is stored.
                                "secret": created.secret,
                                "token": created.info,
                                "endpoint_path": STATUS_PATH,
                            }),
                        )
                    }
                    Err(e) => token_error(e),
                }
            }
            "me.status_tokens.revoke" => {
                let Some(id) = token_id_param(&params) else {
                    return token_error(StatusTokenError::NotFound);
                };
                let target = id.clone();
                match blocking(move || db.status_token_revoke_own(&me, &target)).await {
                    Ok(()) => WsFrame::ok_response("", json!({ "id": id, "revoked": true })),
                    Err(e) => token_error(e),
                }
            }
            "status_tokens.admin_list" => match blocking(move || db.status_tokens_all()).await {
                Ok(tokens) => WsFrame::ok_response(
                    "",
                    json!({ "enabled": enabled, "endpoint_path": STATUS_PATH, "tokens": tokens }),
                ),
                Err(e) => token_error(e),
            },
            "status_tokens.admin_revoke" => {
                let Some(id) = token_id_param(&params) else {
                    return token_error(StatusTokenError::NotFound);
                };
                let target = id.clone();
                match blocking(move || db.status_token_admin_revoke(&me, &target)).await {
                    Ok(()) => WsFrame::ok_response("", json!({ "id": id, "revoked": true })),
                    Err(e) => token_error(e),
                }
            }
            _ => WsFrame::error_response("", "不支援此操作。"),
        }
    }

    /// Counts for the external status endpoint, for exactly `agents` (the
    /// caller's live bindings) **as `reader` may see them**: employees that
    /// no longer exist or are archived are dropped, the rest sorted by id.
    ///
    /// The same rules as the dashboard apply to every count and to the
    /// latest title, against an identity re-read here:
    /// - tasks host-stamped for another A2A principal are excluded in SQL
    ///   (`a2a_owner IS NULL OR a2a_owner = user:<reader>`);
    /// - a task whose TaskPacket audience does not admit the reader is not
    ///   counted (`dashboard_may_read_task`);
    /// - `awaiting_confirmation` adds only the pending approvals
    ///   `ApprovalVisibility::may_read` lets the reader see.
    ///
    /// Any unreadable source — identity, task store, approvals, a packet
    /// audience — is an error (the request is refused), never a zero.
    pub(crate) async fn external_status_snapshot(
        &self,
        reader: &UserContext,
        agents: &[String],
    ) -> Result<Vec<EmployeeStatus>, String> {
        let mut listed: Vec<(String, String)> = {
            let reg = self.registry.read().await;
            agents
                .iter()
                .filter(|id| {
                    reg.get(id)
                        .is_some_and(|a| a.config.agent.status.is_listable(false))
                })
                .map(|id| (id.clone(), crate::trigger_route::display_name(&reg, id)))
                .collect()
        };
        listed.sort();
        listed.dedup_by(|a, b| a.0 == b.0);
        if listed.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = listed.iter().map(|(id, _)| id.clone()).collect();

        let store = self
            .task_store
            .read()
            .await
            .clone()
            .ok_or_else(|| "task store not initialized".to_string())?;
        let principal = crate::trigger_route::assign::principal_for_user(&reader.user_id)
            .ok_or_else(|| "status reader has no principal".to_string())?;
        let (today, window) = day_bounds(Utc::now());
        let rows = store
            .status_rows_for_principal(&ids, today, window, &principal)
            .await?;

        let broker = crate::approval::ApprovalBroker::open(&self.home_dir)?;
        let pending: Vec<crate::approval::ApprovalRecord> = broker
            .list_pending(None)
            .await?
            .into_iter()
            .filter(|record| ids.contains(&record.agent_id))
            .collect();

        // Identity, packet audiences, approval facts and title masking all
        // read files or SQLite: off the async workers, in one place.
        let home = self.home_dir.clone();
        let session = reader.clone();
        let wanted = ids.clone();
        let checked = tokio::task::spawn_blocking(move || {
            external_status_visibility(&home, &session, &wanted, &rows, &pending)
        })
        .await
        .map_err(|_| "status visibility check failed".to_string())??;

        Ok(listed
            .into_iter()
            .filter(|(agent_id, _)| checked.visible_agents.contains(agent_id))
            .map(|(agent_id, display_name)| {
                let c = checked.counts.get(&agent_id).cloned().unwrap_or_default();
                let pending = checked.approvals.get(&agent_id).copied().unwrap_or(0);
                let title = checked.titles.get(&agent_id).cloned().flatten();
                EmployeeStatus {
                    display_name,
                    in_progress: c.in_progress,
                    awaiting_confirmation: c.awaiting_tasks.saturating_add(pending),
                    done_today: c.done_today,
                    done_7d: c.done_7d,
                    latest_done: c
                        .latest_done
                        .map(|(_, completed_at)| LatestDone { title, completed_at }),
                    agent_id,
                }
            })
            .collect())
    }
}

/// What one reader may see of the candidate rows and pending approvals.
struct ExternalStatusVisibility {
    visible_agents: std::collections::HashSet<String>,
    counts: HashMap<String, crate::task_store::AgentStatusCounts>,
    approvals: HashMap<String, i64>,
    /// Masked latest title per employee; `None` ⇒ omitted.
    titles: HashMap<String, Option<String>>,
}

/// Blocking half of [`MethodHandler::external_status_snapshot`]. The reader
/// is re-read from the identity store (the same refresh the dashboard's
/// approval list uses); a failed read refuses.
fn external_status_visibility(
    home: &Path,
    session: &UserContext,
    agents: &[String],
    rows: &[crate::task_store::StatusTaskRow],
    pending: &[crate::approval::ApprovalRecord],
) -> Result<ExternalStatusVisibility, String> {
    use crate::review_evidence::audience::{TaskAudience, dashboard_may_read_task, task_audience};

    let (fresh, department) = super::approval_visibility::fresh_reader(home, session)
        .ok_or_else(|| "status reader identity unavailable".to_string())?;
    if fresh.user_id != session.user_id {
        return Err("status reader identity changed".into());
    }
    let visible_agents: std::collections::HashSet<String> = agents
        .iter()
        .filter(|agent| fresh.has_agent_access(agent, AccessLevel::Viewer))
        .cloned()
        .collect();

    let counts = crate::task_store::AgentStatusCounts::tally(rows, |row| {
        if !visible_agents.contains(&row.agent) {
            return Ok(false);
        }
        // An id that cannot name a packet directory is refused to every
        // non-admin by the dashboard without reading anything: hidden here,
        // not an ACL read failure.
        if !duduclaw_core::is_valid_agent_id(&row.id) {
            return Ok(false);
        }
        match task_audience(home, &row.id) {
            // Fail closed for everyone, Admin included: this leaves the
            // building, and nobody can tell what the packets would allow.
            TaskAudience::Unreadable => Err("task audience unreadable".into()),
            audience => Ok(dashboard_may_read_task(&fresh, &audience)),
        }
    })?;

    let mut approvals: HashMap<String, i64> = HashMap::new();
    for record in pending {
        if !visible_agents.contains(&record.agent_id) {
            continue;
        }
        let visibility =
            super::approval_visibility::ApprovalVisibility::load(home, record, &fresh);
        if visibility.may_read(record, &fresh, department.as_deref()) {
            *approvals.entry(record.agent_id.clone()).or_insert(0) += 1;
        }
    }

    let titles = counts
        .iter()
        .filter_map(|(agent, c)| {
            c.latest_done
                .as_ref()
                .map(|(raw, _)| (agent.clone(), safe_title(home, raw)))
        })
        .collect();

    Ok(ExternalStatusVisibility { visible_agents, counts, approvals, titles })
}
