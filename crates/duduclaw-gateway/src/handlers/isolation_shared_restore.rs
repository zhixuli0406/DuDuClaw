//! F6k Shared restore RPCs (F6 design r5 §3.9, §3.11), child of the F6d
//! restore handler (`isolation_employee_restore.rs`), which has already run
//! its gates: not `system`, no forced password change, Admin + current MFA,
//! protected Linux mode. Dashboard-only (never an MCP tool), Enterprise-only
//! (`gates.rs`). Refusals are `{code, message}` objects.
//!
//! | Method | Params | Reply |
//! |---|---|---|
//! | `isolation.shared_restore_options` | `{employee_id, incarnation}` | members (snapshot level, `active`), tickable tenant routes, stripped grants, open request |
//! | `isolation.shared_restore_request` | `{employee_id, incarnation, members: [{user_id, access_level}], routes: [{account, generation}], csrf_token}` | `{request_id, state, members, members_dropped, routes, …}` |
//!
//! The request needs a **fresh** migration step-up with its CSRF token in
//! this call; approval is `isolation.employee_restore_decide` (F6c2 rule).
//! The bodies live in `crate::isolation_restore_shared` (blocking pool).
use super::*;
use std::sync::Arc;
use crate::isolation_restore_shared::{self as body, CatalogTenantRoutes};

pub(crate) const SHARED_RESTORE_REQUEST: &str = "isolation.shared_restore_request";
pub(crate) const SHARED_RESTORE_OPTIONS: &str = "isolation.shared_restore_options";
pub(crate) const METHODS: [&str; 2] = [SHARED_RESTORE_REQUEST, SHARED_RESTORE_OPTIONS];

/// `isolation.shared_restore_request`: no field names the requester, an
/// approver, a tenant or an owner (the row and the session decide those).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedRestoreRequest {
    pub(crate) employee_id: String,
    pub(crate) incarnation: u32,
    pub(crate) members: Value,
    pub(crate) routes: Value,
    pub(crate) csrf_token: [u8; 32],
}

/// `isolation.shared_restore_options`: one Shared quarantine row.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedRestoreOptions {
    pub(crate) employee_id: String,
    pub(crate) incarnation: u32,
}

impl MethodHandler {
    /// Dispatch of [`METHODS`]; `session` is the caller's dashboard session.
    pub(super) async fn shared_restore(&self, method: &str, params: Value, ctx: &UserContext, db: Arc<UserDb>,
        session: Option<String>) -> Result<Value, Failure> {
        if !METHODS.contains(&method) || ctx.user_id == "system" { return Err(NOT_ALLOWED.into()); }
        let (home, user, epoch) = (self.home_dir.clone(), ctx.user_id.clone(), ctx.credential_epoch);
        let invalid = |_| Failure::Rpc(RpcError::Invalid);
        match method {
            SHARED_RESTORE_OPTIONS => {
                let input: SharedRestoreOptions = serde_json::from_value(params).map_err(invalid)?;
                run(move || Ok(body::options(&home, &CatalogTenantRoutes(&home), &input.employee_id, input.incarnation, now()?)
                    .map_err(RpcError::from)?)).await
            }
            SHARED_RESTORE_REQUEST => {
                let input: SharedRestoreRequest = serde_json::from_value(params).map_err(invalid)?;
                let (session, cap) = (session.ok_or(ADMIN_REQUIRED)?, self.effective_agent_cap().await);
                run(move || {
                    let now = now()?;
                    // Admin + fresh migration step-up with its session-bound CSRF token.
                    let proof = db.migration_confirmation_session(&user, &session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
                    authorize_step_up(proof.as_ref(), &user, &input.csrf_token, now).map_err(Failure::Gate)?;
                    let verified_at = proof.map(|proof| proof.verified_at).ok_or(Failure::Gate(SESSION_EXPIRED))?;
                    let fresh = crate::isolation_restore_wiring::step_up_fresh(Some(verified_at), now);
                    // Filing never approves: the rule gets no step-up.
                    let rule = LogRule::new(false, now);
                    let routes = CatalogTenantRoutes(&home);
                    Ok(with_env(&home, &user, &rule, cap, now, |env| body::request(&home, env, &routes, &user,
                        &input.employee_id, input.incarnation, &input.members, &input.routes, fresh)).map_err(RpcError::from)?)
                }).await
            }
            _ => Err(NOT_ALLOWED.into()),
        }
    }
}

#[cfg(test)]
#[path = "isolation_shared_restore_tests.rs"]
mod tests;
