//! One dashboard gate for a Personal employee's private data (native Linux
//! isolation v1.81, task P1). Protected mode only; see
//! `crate::personal_visibility` for the model.
//!
//! - Agent-scoped reads (and the few writes) of conversations, memory, agent
//!   files and wiki pages are listed in [`agent_scoped`]; `dispatch` runs
//!   [`MethodHandler::personal_data_gate`] on their `agent_id` before any
//!   handler or extension sees the call. `chat.sessions.history` resolves the
//!   employee from the session and calls the same gate from its handler.
//! - The owner passes; an Admin who is not the owner passes only with a
//!   fresh step-up — the migration/isolation dashboard proof
//!   (`UserDb::migration_step_up`, written by `migration.confirmations.step_up`
//!   or `isolation.connector_step_up`; password + TOTP, five minutes, bound to
//!   this session and credential epoch) and its CSRF token in the request
//!   field [`STEP_UP_FIELD`] — and only after an audit row naming the Admin,
//!   the employee, the owner and the data category has been written to
//!   `users.db` (`auth_audit_log`, action [`READ_ACTION`]); a failed audit
//!   write refuses. Every other caller is refused.
//! - Cross-agent listings (`chat.sessions.list`, `search.query` without
//!   `agent_id`) hide Personal employees the caller does not own.
//! - **P1b (work record, [`DataCategory::Work`]).** `activity.list`,
//!   `timeline.list`, `decisions.list`/`dismiss`, `canvas.get`,
//!   `tasks.list`/`list_page`/`flow_metrics` and `tasks.assign` (its
//!   destination) are gated on `agent_id`; the single-task RPCs in
//!   [`task_scoped`] (gets refuse, writes refuse) are gated on the task's
//!   employee, read from `tasks.db` before the handler (unreadable store ⇒
//!   refused). Unscoped lists hide through `TaskReader` (`task_privacy.rs`).
//!   Approval cards of another person's Personal employee are metadata only
//!   ([`personal_metadata_card`]); `approvals.get` with a step-up returns the
//!   full card, audited.
//! - **P1c.** Deciding such a card (`personal_decide_gate.rs`, category
//!   [`DataCategory::Approval`]) and handing work to another person's
//!   Personal employee (`personal_delegation_gate.rs`, run first in
//!   [`MethodHandler::personal_gates_for_params`]); list totals and flow
//!   metrics are computed over the hidden-filtered set; push events are
//!   filtered per subscriber (`personal_push.rs`).
use super::*;
use super::isolation_connectors_rpc::authorize_step_up;
use crate::personal_visibility::{self as visibility, DataCategory, Reader};
use duduclaw_auth::mfa::MigrationConfirmationSession;
use rusqlite::OptionalExtension;

/// Request field carrying the step-up proof's CSRF token (32-byte array).
/// Removed from the params before any handler runs.
pub(super) const STEP_UP_FIELD: &str = "step_up_csrf";
/// `auth_audit_log` action of an audited Admin read.
pub(crate) const READ_ACTION: &str = "isolation.personal_data_read";
/// `auth_audit_log` action of a refused Admin read (best effort).
pub(crate) const REFUSED_ACTION: &str = "isolation.personal_data_read_refused";

pub(super) const STEP_UP_REQUIRED: &str = "這是個人專屬員工的私人資料（對話、記憶、設定檔或 Wiki）。管理員需先以密碼與雙因素驗證確認身分\
    （有效 5 分鐘），並在請求中帶入驗證後的 step_up_csrf；每次讀取都會寫入稽核紀錄。 / This is a Personal employee's private \
    data. An Admin needs a fresh password + TOTP step-up and its step_up_csrf token; every read is audited.";
pub(super) const NOT_ALLOWED: &str = "permission denied";
const UNAVAILABLE: &str = "無法確認這位員工的歸屬，已拒絕讀取。";
const AUDIT_FAILED: &str = "無法寫入稽核紀錄，已拒絕讀取。";

/// Agent-scoped dashboard methods whose `agent_id` names the employee whose
/// private data is read (or, for the few writes, changed).
pub(super) fn agent_scoped(method: &str) -> Option<DataCategory> {
    Some(match method {
        "memory.search" | "memory.browse" | "memory.key_facts" | "memory.decay_overview"
        | "memory.history" | "memory.at" | "memory.get_at" | "memory.graph"
        | "memory.invalidate_origin" | "memory.forget" => DataCategory::Memory,
        "wiki.pages" | "wiki.read" | "wiki.auto_pages" | "wiki.search" | "wiki.lint" | "wiki.stats"
        | "wiki.promote" | "wiki.archive" | "wiki.share" => DataCategory::Wiki,
        "agent_files.list" | "agent_files.get" | "agent_files.runtime_files" | "agent_files.diff"
        | "agent_files.history" | "agent_files.save" | "agent_files.rollback" | "agents.inspect"
        | "contract.get" | "contract.update" => DataCategory::AgentFiles,
        "chat.sessions.list" | "search.query" | "runs.list" => DataCategory::Sessions,
        "activity.list" | "timeline.list" | "decisions.list" | "decisions.dismiss" | "canvas.get"
        | "tasks.list" | "tasks.list_page" | "tasks.flow_metrics" | "tasks.assign" => DataCategory::Work,
        _ => return None,
    })
}

/// Single-task dashboard RPCs (reads and writes) keyed by `task_id`: the
/// gate runs on the task's employee (`assigned_to`), category
/// [`DataCategory::Work`].
pub(super) fn task_scoped(method: &str) -> bool {
    matches!(
        method,
        "tasks.update" | "tasks.remove" | "tasks.assign" | "tasks.archive" | "tasks.unarchive"
            | "tasks.pin" | "tasks.unpin" | "tasks.rename" | "tasks.comment" | "tasks.comments"
            | "tasks.iterations" | "tasks.timeline" | "tasks.changes" | "tasks.artifacts"
            | "tasks.role_turns" | "tasks.goal_decide" | "activity.list"
    )
}

/// The employee a task is assigned to, read from `tasks.db` without
/// opening a writable store. `Ok(None)` = no such task (the handler answers);
/// `Err` = the store could not be read (the gate refuses).
pub(super) fn task_employee(home: &Path, task_id: &str) -> Result<Option<String>, ()> {
    let conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).map_err(|_| ())?;
    conn.query_row("SELECT assigned_to FROM tasks WHERE id=?1", [task_id], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|_| ())
}

/// A non-blank string parameter.
fn param_text(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
}

/// The approval card another person's Personal employee shows to a reader
/// who is not its owner: label, status and timing only. No summary,
/// payload, digest, binding, answer, simulation or conversation link.
pub(super) fn personal_metadata_card(record: &crate::approval::ApprovalRecord) -> Value {
    json!({
        "id": record.id.as_str(),
        "digest": Value::Null,
        "agent_id": record.agent_id,
        "kind": crate::governance::ApprovalKind::parse(&record.action_kind).as_str(),
        "summary": "",
        "payload": {"redacted": true, "personal_employee": true},
        "request_kind": record.request_kind,
        "status": record.status,
        "binding": Value::Null,
        "answer": Value::Null,
        "may_decide": false,
        "workflow_activation": Value::Null,
        "invalidated_reason": Value::Null,
        "created_at": record.created_at,
        "ttl_seconds": record.ttl_seconds,
        "expires_at": record.expires_at_epoch(),
        "simulation": Value::Null,
        "channel": Value::Null,
        "channel_link": Value::Null,
        "restricted": true,
        "personal_employee": true,
    })
}

/// Take the step-up CSRF token out of `params`. A malformed value reads as
/// absent (the gate then refuses an Admin read).
pub(super) fn take_step_up(params: &mut Value) -> Option<[u8; 32]> {
    let raw = params.as_object_mut()?.remove(STEP_UP_FIELD)?;
    serde_json::from_value(raw).ok()
}

/// The read being authorized.
pub(super) struct Target<'a> {
    pub method: &'a str,
    pub category: DataCategory,
    pub agent: &'a str,
}

/// Break-glass core for an Admin who is not `owner`: the step-up proof read
/// for this exact Admin, session and credential epoch must be fresh and the
/// CSRF token must match; then the audit row must be written. Refusals are
/// audited best effort.
pub(super) fn authorize_break_glass(
    db: &UserDb,
    proof: Option<&MigrationConfirmationSession>,
    admin: &str,
    csrf: Option<&[u8; 32]>,
    now: i64,
    target: &Target<'_>,
    owner: &str,
) -> Result<(), &'static str> {
    let detail = json!({
        "admin_user_id": admin,
        "employee": target.agent,
        "owner_user_id": owner,
        "category": target.category.as_str(),
        "method": target.method,
    })
    .to_string();
    let authorized = csrf.is_some_and(|csrf| authorize_step_up(proof, admin, csrf, now).is_ok());
    if !authorized {
        let _ = db.log_action(Some(admin), REFUSED_ACTION, Some(target.agent), Some(detail.as_str()), None);
        return Err(STEP_UP_REQUIRED);
    }
    db.log_action(Some(admin), READ_ACTION, Some(target.agent), Some(detail.as_str()), None)
        .map_err(|_| AUDIT_FAILED)
}

impl MethodHandler {
    /// The shared gate. A no-op outside native Linux protected mode.
    pub(super) async fn personal_data_gate(
        &self,
        ctx: &UserContext,
        target: Target<'_>,
        csrf: Option<[u8; 32]>,
    ) -> Result<(), &'static str> {
        if !crate::runtime_readiness::is_protected_linux() {
            return Ok(());
        }
        let home = self.home_dir.clone();
        let (user, admin, agent) = (ctx.user_id.clone(), ctx.is_admin(), target.agent.to_owned());
        let reader = tokio::task::spawn_blocking(move || {
            visibility::classify_reader(&home, true, &user, admin, &agent)
        })
        .await
        .map_err(|_| UNAVAILABLE)?
        .map_err(|_| UNAVAILABLE)?;
        let owner = match reader {
            Reader::Unrestricted | Reader::Owner => return Ok(()),
            Reader::Refused => return Err(NOT_ALLOWED),
            Reader::BreakGlass { owner } => owner,
        };
        let db = self.user_db.read().await.clone().ok_or(UNAVAILABLE)?;
        let session = crate::google_personal::session_for(&ctx.user_id).ok();
        let (admin_id, epoch) = (ctx.user_id.clone(), ctx.credential_epoch);
        let (method, category, agent) = (target.method.to_owned(), target.category, target.agent.to_owned());
        tokio::task::spawn_blocking(move || {
            let now = chrono::Utc::now().timestamp();
            let proof = match (&session, now >= 0) {
                (Some(session), true) => db
                    .migration_confirmation_session(&admin_id, session, epoch, now)
                    .ok()
                    .flatten(),
                _ => None,
            };
            let target = Target { method: &method, category, agent: &agent };
            authorize_break_glass(&db, proof.as_ref(), &admin_id, csrf.as_ref(), now, &target, &owner)
        })
        .await
        .map_err(|_| UNAVAILABLE)?
    }

    /// Dispatch-level wrapper for [`agent_scoped`] methods (on `agent_id`)
    /// and [`task_scoped`] methods (on the employee of `task_id`). Calls
    /// without either are left to the handler (cross-agent listings hide
    /// Personal employees there; agent-required methods refuse).
    pub(super) async fn personal_gates_for_params(
        &self,
        method: &str,
        params: &mut Value,
        ctx: &UserContext,
    ) -> Result<(), WsFrame> {
        self.personal_delegation_gate(method, params, ctx).await?;
        let category = agent_scoped(method);
        let by_task = task_scoped(method);
        if (category.is_none() && !by_task) || !crate::runtime_readiness::is_protected_linux() {
            return Ok(());
        }
        let refuse = |message: &str| WsFrame::error_response("", message);
        let csrf = take_step_up(params);
        if let (Some(category), Some(agent)) = (category, param_text(params, "agent_id")) {
            self.personal_data_gate(ctx, Target { method, category, agent: &agent }, csrf)
                .await
                .map_err(refuse)?;
        }
        let Some(task_id) = param_text(params, "task_id").filter(|_| by_task) else {
            return Ok(());
        };
        let home = self.home_dir.clone();
        let employee = tokio::task::spawn_blocking(move || task_employee(&home, &task_id))
            .await
            .map_err(|_| refuse(UNAVAILABLE))?
            .map_err(|()| refuse(UNAVAILABLE))?;
        match employee {
            Some(agent) => self
                .personal_data_gate(ctx, Target { method, category: DataCategory::Work, agent: &agent }, csrf)
                .await
                .map_err(refuse),
            None => Ok(()),
        }
    }

    /// Approval cards (`approvals.list` / `approvals.get`) of these employees
    /// are shown as [`personal_metadata_card`]: Personal employees `ctx` does
    /// not own. `approvals.get` (`by_id`, one row) with a `step_up_csrf`
    /// ([`take_step_up`], taken by the handler) runs the break-glass gate on
    /// that card's employee and, once audited, returns it in full. Empty
    /// outside protected mode.
    pub(super) async fn personal_approval_hidden(
        &self,
        ctx: &UserContext,
        csrf: Option<[u8; 32]>,
        by_id: bool,
        rows: &[crate::approval::ApprovalRecord],
    ) -> Result<std::collections::BTreeSet<String>, WsFrame> {
        if !crate::runtime_readiness::is_protected_linux() {
            return Ok(Default::default());
        }
        let mut hidden = self
            .personal_hidden_agents(ctx)
            .await
            .map_err(|message| WsFrame::error_response("", message))?;
        if let (Some(csrf), true, [record]) = (csrf, by_id, rows)
            && ctx.is_admin()
            && hidden.contains(&record.agent_id)
        {
            let target = Target { method: "approvals.get", category: DataCategory::Work, agent: &record.agent_id };
            self.personal_data_gate(ctx, target, Some(csrf))
                .await
                .map_err(|message| WsFrame::error_response("", message))?;
            hidden.remove(&record.agent_id);
        }
        Ok(hidden)
    }

    /// Personal employees hidden from `ctx` in cross-agent listings. Empty
    /// outside protected mode; unreadable ⇒ `Err` (the caller refuses).
    pub(super) async fn personal_hidden_agents(&self, ctx: &UserContext)
        -> Result<std::collections::BTreeSet<String>, &'static str> {
        if !crate::runtime_readiness::is_protected_linux() {
            return Ok(Default::default());
        }
        let (home, user) = (self.home_dir.clone(), ctx.user_id.clone());
        tokio::task::spawn_blocking(move || visibility::hidden_from(&home, true, &user))
            .await
            .map_err(|_| UNAVAILABLE)?
            .map_err(|_| UNAVAILABLE)
    }
}

#[cfg(all(test, unix))]
#[path = "personal_data_gate_tests.rs"]
mod tests;
