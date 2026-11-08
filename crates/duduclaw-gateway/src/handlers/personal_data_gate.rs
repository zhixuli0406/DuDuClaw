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
use super::*;
use super::isolation_connectors_rpc::authorize_step_up;
use crate::personal_visibility::{self as visibility, DataCategory, Reader};
use duduclaw_auth::mfa::MigrationConfirmationSession;

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
const NOT_ALLOWED: &str = "permission denied";
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
        _ => return None,
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

    /// Dispatch-level wrapper for [`agent_scoped`] methods. Calls without an
    /// `agent_id` are left to the handler (cross-agent listings hide
    /// Personal employees there; agent-required methods refuse).
    pub(super) async fn personal_data_gate_for_params(
        &self,
        method: &str,
        category: DataCategory,
        params: &mut Value,
        ctx: &UserContext,
    ) -> Result<(), WsFrame> {
        if !crate::runtime_readiness::is_protected_linux() {
            return Ok(());
        }
        let csrf = take_step_up(params);
        let Some(agent) = params.get("agent_id").and_then(Value::as_str).map(str::trim)
            .filter(|agent| !agent.is_empty()).map(str::to_owned) else {
            return Ok(());
        };
        self.personal_data_gate(ctx, Target { method, category, agent: &agent }, csrf)
            .await
            .map_err(|message| WsFrame::error_response("", message))
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
