//! F6i quarantine cleanup RPC (native Linux isolation v1.81, F6 design r5
//! §3.8, §3.11; owner decisions Q5, N3, S12), child of `isolation_employee_rpc`.
//! Dashboard-only: never an MCP tool, refused for `user_id == "system"`,
//! Enterprise-only (`gates.rs`).
//!
//! | Method | Caller |
//! |---|---|
//! | `isolation.quarantine_purge` | Admin + MFA + fresh migration step-up (CSRF) |
//!
//! `{employee_id, incarnation, confirm_early?: false, retire?: true, csrf_token}`:
//! ≥ 90 days after the tombstone the row is purged inside the call (and with
//! `retire` the F6l retire proposal is staged right after); younger rows need
//! `confirm_early` and file an `early_purge` request, approved or denied with
//! `isolation.employee_restore_decide` by another qualifying Admin (or the
//! sole Admin, audited `self_approved`). Refusals are `{code, message}`.
//! The body lives in `crate::isolation_purge` (blocking pool).
use super::*;
use super::restore::{failure_frame, with_env, Failure};
use crate::isolation_purge::PurgeOptions;
use crate::isolation_restore::Refusal;
use crate::isolation_restore_rpc::RpcError;
use crate::isolation_restore_wiring::LogRule;

pub(super) const QUARANTINE_PURGE: &str = "isolation.quarantine_purge";
pub(super) const METHODS: [&str; 1] = [QUARANTINE_PURGE];

/// 同時釋放主機資源 is checked by default (§3.8).
fn checked_by_default() -> bool { true }

/// `isolation.quarantine_purge`. No field names a requester, an approver, an
/// age or a step-up: the server derives each.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QuarantinePurge {
    pub(super) employee_id: String,
    pub(super) incarnation: u32,
    /// The dialog's confirmation of an early purge (< 90 days).
    #[serde(default)] pub(super) confirm_early: bool,
    /// 同時釋放主機資源.
    #[serde(default = "checked_by_default")] pub(super) retire: bool,
    pub(super) csrf_token: [u8; 32],
}

impl MethodHandler {
    pub(super) async fn handle_quarantine_purge(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        match self.quarantine_purge(method, params, ctx).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(failure) => failure_frame(failure),
        }
    }

    async fn quarantine_purge(&self, method: &str, params: Value, ctx: &UserContext) -> Result<Value, Failure> {
        // Same gates as the restore RPCs, in the same order.
        if !METHODS.contains(&method) || ctx.user_id == "system" { return Err(NOT_ALLOWED.into()); }
        if ctx.requires_password_change() { return Err(REQUESTER_INELIGIBLE.into()); }
        if acl::require_role(ctx, UserRole::Admin).is_err() || self.mfa_require_current_admin(ctx).await.is_err() {
            return Err(ADMIN_REQUIRED.into());
        }
        let session = crate::google_personal::session_for(&ctx.user_id).map_err(|_| ADMIN_REQUIRED)?;
        if !crate::runtime_readiness::is_protected_linux() { return Err(NOT_PROTECTED.into()); }
        let db = self.user_db.read().await.clone().ok_or(USERS_UNAVAILABLE)?;
        let params = if params.is_null() { json!({}) } else { params };
        let input: QuarantinePurge = serde_json::from_value(params).map_err(|_| Failure::Rpc(RpcError::Invalid))?;
        let cap = self.effective_agent_cap().await;
        let (home, user, epoch) = (self.home_dir.clone(), ctx.user_id.clone(), ctx.credential_epoch);
        tokio::task::spawn_blocking(move || -> Result<Value, Failure> {
            let now = now()?;
            // Admin + fresh migration step-up with its session-bound CSRF token.
            let proof = db.migration_confirmation_session(&user, &session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
            authorize_step_up(proof.as_ref(), &user, &input.csrf_token, now).map_err(Failure::Gate)?;
            let verified_at = proof.map(|proof| proof.verified_at);
            if !crate::isolation_restore_wiring::step_up_fresh(verified_at, now) {
                return Err(RpcError::Restore(Refusal::Approval("STEP_UP_REQUIRED")).into());
            }
            // Filing or an in-call purge approves nothing: the rule gets no step-up.
            let rule = LogRule::new(false, now);
            let options = PurgeOptions { confirm_early: input.confirm_early, retire: input.retire };
            Ok(with_env(&home, &user, &rule, cap, now, |env| crate::isolation_purge::quarantine_purge_rpc(&home, env,
                &input.employee_id, input.incarnation, options, &crate::isolation_retire::SpawndRetireRoot))?)
        }).await.map_err(|_| Failure::Gate(SERVICE_UNAVAILABLE))?
    }
}

#[cfg(test)]
#[path = "isolation_employee_purge_tests.rs"]
mod tests;
