//! F6 restore RPCs (native Linux isolation v1.81, F6 design r5 §3.11; task
//! F6d), child of `isolation_employee_rpc`. Dashboard-only: never an MCP
//! tool, refused for `user_id == "system"`, Enterprise-only (`gates.rs`).
//!
//! | Method | Caller |
//! |---|---|
//! | `me.isolation.list_restorable` | owner (own Personal rows only) |
//! | `me.isolation.restore_challenge` | owner (own row): the strongest factor's purpose-bound challenge |
//! | `me.isolation.request_restore` / `cancel_restore` | owner + strongest factor (Q2) |
//! | `isolation.employee_restore_decide` / `employee_restore_cancel` | Admin + MFA + step-up |
//! | `isolation.employee_restore_regrant` | Admin + MFA + step-up (one listed item, audited) |
//! | `isolation.employee_retire_propose` | Admin + MFA + step-up (F6l) |
//! | `me.isolation.request_adoption` | owner + strongest factor (Q2), purpose `adopt:<employee_id>:<incarnation>` (F6g) |
//! | `isolation.shared_adoption_request` | Admin + MFA + step-up (F6g, member selection ⊆ snapshot) |
//! | `isolation.employee_restore_request_for_owner` / `isolation.adoption_request_for_owner` | Admin + MFA + fresh step-up; only for an owner with no second factor at the tombstone (F6fa2, owner decision 2026-11-09) |
//!
//! The adoption challenge is `me.isolation.restore_challenge` with
//! `for_adoption: true` (same issuer; the server builds the `adopt:` purpose).
//!
//! Refusals are `{code, message}` objects (code: `isolation_restore_rpc::RpcError::code`).
//! The bodies live in `crate::isolation_restore_rpc` (blocking pool).
use super::*;
use crate::isolation_restore::{CatalogRoutes, RestoreEnv, SpawndRootProbe};
use crate::isolation_restore_rpc::{self as rpc, RpcError};
use crate::isolation_restore_wiring::{LogRule, RegistryMarks};
use duduclaw_auth::mfa::owner_factor::{RestoreChallenge, RestoreProof};
use duduclaw_auth::mfa::{OwnerFactor, OwnerFactorError};

pub(super) const LIST_RESTORABLE: &str = "me.isolation.list_restorable";
pub(super) const RESTORE_CHALLENGE: &str = "me.isolation.restore_challenge";
pub(super) const REQUEST_RESTORE: &str = "me.isolation.request_restore";
pub(super) const CANCEL_RESTORE: &str = "me.isolation.cancel_restore";
pub(super) const DECIDE: &str = "isolation.employee_restore_decide";
pub(super) const CANCEL: &str = "isolation.employee_restore_cancel";
pub(super) const REGRANT: &str = "isolation.employee_restore_regrant";
pub(super) const RETIRE_PROPOSE: &str = "isolation.employee_retire_propose";
/// F6g adoption requests (engine `isolation_adopt::file_adoption`).
pub(super) const REQUEST_ADOPTION: &str = "me.isolation.request_adoption";
pub(super) const SHARED_ADOPTION_REQUEST: &str = "isolation.shared_adoption_request";
/// F6fa2 (owner decision 2026-11-09): an Admin files a Personal restore /
/// adoption for an owner who had no second factor at the tombstone.
pub(super) const RESTORE_FOR_OWNER: &str = "isolation.employee_restore_request_for_owner";
pub(super) const ADOPTION_FOR_OWNER: &str = "isolation.adoption_request_for_owner";
pub(super) const METHODS: [&str; 12] = [LIST_RESTORABLE, RESTORE_CHALLENGE, REQUEST_RESTORE, CANCEL_RESTORE, DECIDE, CANCEL, REGRANT,
    RETIRE_PROPOSE, REQUEST_ADOPTION, SHARED_ADOPTION_REQUEST, RESTORE_FOR_OWNER, ADOPTION_FOR_OWNER];

/// F6k: `isolation.shared_restore_request` / `isolation.shared_restore_options` (`shared::METHODS`).
#[path = "isolation_shared_restore.rs"]
pub(super) mod shared;
/// Every method this handler serves (F6d [`METHODS`] and F6k `shared::METHODS`).
pub(super) fn handles(method: &str) -> bool { METHODS.contains(&method) || shared::METHODS.contains(&method) }

/// `me.isolation.restore_challenge`: the row the proof will be bound to (a
/// cancel names the row of its request). `for_adoption`: the challenge of a
/// `request_adoption` (purpose `adopt:…`, built by the server).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerChallenge {
    pub(super) employee_id: String, pub(super) incarnation: u32,
    #[serde(default)] pub(super) for_adoption: bool,
}
/// `me.isolation.request_adoption`: the proof fields of [`OwnerRequest`]
/// plus the optional target label (default: the original label).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerAdoption {
    pub(super) employee_id: String, pub(super) incarnation: u32, pub(super) factor: String,
    #[serde(default)] pub(super) target_label: Option<String>,
    #[serde(default)] pub(super) code: String,
    #[serde(default)] pub(super) challenge_id: Option<String>,
    #[serde(default)] pub(super) credential: Option<Value>,
}
/// `isolation.shared_adoption_request`: `members` = `[{user_id, access_level}]`
/// ⊆ the tombstone snapshot at ≤ its level (empty: re-bound later).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SharedAdoption {
    pub(super) employee_id: String, pub(super) incarnation: u32,
    #[serde(default)] pub(super) target_label: Option<String>,
    pub(super) members: Value, pub(super) csrf_token: [u8; 32],
}
/// `isolation.employee_restore_request_for_owner` (F6fa2): the row only; the
/// owner is the row's receipt principal, never a request field.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForOwner { pub(super) employee_id: String, pub(super) incarnation: u32, pub(super) csrf_token: [u8; 32] }
/// `isolation.adoption_request_for_owner` (F6fa2): [`ForOwner`] plus the
/// optional target label (default: the original label).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdoptionForOwner {
    pub(super) employee_id: String, pub(super) incarnation: u32,
    #[serde(default)] pub(super) target_label: Option<String>,
    pub(super) csrf_token: [u8; 32],
}
/// `code`: TOTP or email code; `challenge_id`: passkey / email (from
/// `restore_challenge`); `credential`: the passkey assertion.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerRequest {
    pub(super) employee_id: String, pub(super) incarnation: u32, pub(super) factor: String,
    #[serde(default)] pub(super) code: String,
    #[serde(default)] pub(super) challenge_id: Option<String>,
    #[serde(default)] pub(super) credential: Option<Value>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerCancel {
    pub(super) request_id: String, pub(super) factor: String,
    #[serde(default)] pub(super) code: String,
    #[serde(default)] pub(super) challenge_id: Option<String>,
    #[serde(default)] pub(super) credential: Option<Value>,
}

/// The operator-configured passkey origin (`None`: a passkey owner is
/// refused unavailable, never offered a weaker factor).
fn passkey_origin(home: &Path) -> Option<String> {
    crate::server::server_mfa_passkey::configured_origin(home).ok()
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Decide { pub(super) request_id: String, pub(super) decision: String, pub(super) csrf_token: [u8; 32] }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdminCancel { pub(super) request_id: String, pub(super) csrf_token: [u8; 32] }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Regrant { pub(super) employee_id: String, pub(super) incarnation: u32, pub(super) item: String, pub(super) csrf_token: [u8; 32] }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetirePropose { pub(super) employee_id: String, pub(super) csrf_token: [u8; 32] }

/// A gate refusal (plain message, like the rest of this handler) or an RPC
/// refusal (`{code, message}`).
pub(super) enum Failure { Gate(&'static str), Rpc(RpcError) }
impl From<&'static str> for Failure { fn from(message: &'static str) -> Self { Self::Gate(message) } }
impl From<RpcError> for Failure { fn from(error: RpcError) -> Self { Self::Rpc(error) } }

pub(super) fn failure_frame(failure: Failure) -> WsFrame {
    match failure {
        Failure::Gate(message) => WsFrame::error_response("", message),
        Failure::Rpc(error) => WsFrame::Response { id: String::new(), ok: false, payload: None,
            error: Some(json!({"code": error.code(), "message": error.message()})) },
    }
}

async fn run<T: Send + 'static>(work: impl FnOnce() -> Result<T, Failure> + Send + 'static) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work).await.map_err(|_| Failure::Gate(SERVICE_UNAVAILABLE))?
}

/// The production collaborators of one restore step.
pub(super) fn with_env<T>(home: &Path, actor: &str, rule: &LogRule, cap: usize, now: i64, f: impl FnOnce(&RestoreEnv<'_>) -> T) -> T {
    let routes = CatalogRoutes(home);
    let env = RestoreEnv { root: &SpawndRootProbe, routes: &routes, stores: &RegistryMarks,
        runs: &crate::isolation_quarantine::SpawndRunStopper, rule, agent_cap: cap, actor, now };
    f(&env)
}

/// The production collaborators of one adoption request (F6g): schema-2
/// root, catalog routes, run stopper, F6c2 rule, the license cap read here
/// (L2). The request step never transforms, so no hooks.
pub(super) fn with_adopt_env<T>(home: &Path, actor: &str, cap: usize, now: i64,
    f: impl FnOnce(&crate::isolation_adopt::AdoptEnv<'_>) -> T) -> T {
    let routes = CatalogRoutes(home);
    let env = crate::isolation_adopt::AdoptEnv { root: &crate::isolation_retire::SpawndRetireRoot, routes: &routes,
        runs: &crate::isolation_quarantine::SpawndRunStopper, rule: &duduclaw_auth::admin_approval_rule::EligibilityLogRule,
        hooks: &crate::isolation_adopt::no_hooks, agent_cap: cap, actor, now };
    f(&env)
}

impl MethodHandler {
    pub(super) async fn handle_isolation_restore(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        match self.isolation_restore(method, params, ctx).await {
            Ok(value) => WsFrame::ok_response("", value),
            Err(failure) => failure_frame(failure),
        }
    }

    async fn isolation_restore(&self, method: &str, params: Value, ctx: &UserContext) -> Result<Value, Failure> {
        // Same gates as every other method of this handler.
        if !handles(method) || ctx.user_id == "system" { return Err(NOT_ALLOWED.into()); }
        if ctx.requires_password_change() { return Err(REQUESTER_INELIGIBLE.into()); }
        let admin = method.starts_with("isolation.");
        if admin && (acl::require_role(ctx, UserRole::Admin).is_err() || self.mfa_require_current_admin(ctx).await.is_err()) {
            return Err(ADMIN_REQUIRED.into());
        }
        let session = if admin {
            Some(crate::google_personal::session_for(&ctx.user_id).map_err(|_| ADMIN_REQUIRED)?)
        } else { None };
        if !crate::runtime_readiness::is_protected_linux() { return Err(NOT_PROTECTED.into()); }
        let db = self.user_db.read().await.clone().ok_or(USERS_UNAVAILABLE)?;
        let params = if params.is_null() { json!({}) } else { params };
        let (home, user, epoch) = (self.home_dir.clone(), ctx.user_id.clone(), ctx.credential_epoch);
        let invalid = |_| Failure::Rpc(RpcError::Invalid);
        // Admin + fresh migration step-up with its session-bound CSRF token;
        // returns the proof's `verified_at` (the rule's freshness, F6fa).
        let step_up = move |db: &UserDb, user: &str, session: &str, csrf: &[u8; 32], now: i64| -> Result<u64, Failure> {
            let proof = db.migration_confirmation_session(user, session, epoch, now).map_err(|_| SESSION_EXPIRED)?;
            authorize_step_up(proof.as_ref(), user, csrf, now).map_err(Failure::Gate)?;
            proof.map(|proof| proof.verified_at).ok_or(Failure::Gate(SESSION_EXPIRED))
        };
        match method {
            LIST_RESTORABLE => {
                let _: NoParams = serde_json::from_value(params).map_err(invalid)?;
                run(move || Ok(rpc::list_restorable(&home, &user, now()?)?)).await
            }
            RESTORE_CHALLENGE => {
                let input: OwnerChallenge = serde_json::from_value(params).map_err(invalid)?;
                let (mail_home, mail_db, mail_user) = (home.clone(), db.clone(), user.clone());
                let issued = run(move || {
                    let now = now()?;
                    let origin = passkey_origin(&home);
                    let mail = crate::mfa_email::available(&home);
                    // F6fa: the tombstone's factor baseline of the row the purpose names.
                    let issue = |purpose: &str| -> Result<RestoreChallenge, OwnerFactorError> {
                        let baseline = rpc::factor_baseline(&home, &user, purpose)?;
                        db.restore_challenge(&user, epoch, purpose, &baseline, origin.as_deref(), mail, now)
                    };
                    if input.for_adoption {
                        return Ok(rpc::adoption_challenge(&home, &user, &input.employee_id, input.incarnation, &issue)?);
                    }
                    Ok(rpc::restore_challenge(&home, &user, &input.employee_id, input.incarnation, &issue)?)
                }).await?;
                let ttl = duduclaw_auth::mfa::owner_factor::RESTORE_CHALLENGE_TTL;
                match issued {
                    RestoreChallenge::Totp => Ok(json!({"factor": "totp", "challenge_id": null})),
                    RestoreChallenge::Passkey(mut options) => {
                        options["factor"] = json!("passkey");
                        options["expires_in"] = json!(ttl);
                        Ok(options)
                    }
                    RestoreChallenge::Email { challenge_id, email, code } => {
                        // The email-factor sender; the code never leaves in the reply.
                        if crate::mfa_email::send_code(&mail_home, &mail_db, &challenge_id, &email, &code).await.is_err() {
                            let discard = challenge_id.clone();
                            let _ = tokio::task::spawn_blocking(move || mail_db.restore_challenge_discard(&mail_user, &discard)).await;
                            return Err(RpcError::Factor(OwnerFactorError::Unavailable).into());
                        }
                        Ok(json!({"factor": "email", "challenge_id": challenge_id, "sent": true, "expires_in": ttl}))
                    }
                }
            }
            REQUEST_RESTORE => {
                let input: OwnerRequest = serde_json::from_value(params).map_err(invalid)?;
                let cap = self.effective_agent_cap().await;
                run(move || {
                    let now = now()?;
                    let cipher = crate::mfa::Cipher(&home);
                    let origin = passkey_origin(&home);
                    // F6fa: the tombstone's factor baseline of the row the purpose names.
                    let check = |factor: OwnerFactor, purpose: &str, code: &str| -> Result<OwnerFactor, OwnerFactorError> {
                        let baseline = rpc::factor_baseline(&home, &user, purpose)?;
                        db.restore_owner_factor(&user, epoch, factor, &baseline,
                            &RestoreProof { purpose, code, challenge_id: input.challenge_id.as_deref(), credential: input.credential.as_ref() },
                            &cipher, origin.as_deref(), now)
                    };
                    // The request stage never approves: no step-up for the rule.
                    let rule = LogRule::new(false, now);
                    Ok(with_env(&home, &user, &rule, cap, now, |env| rpc::request_restore(&home, env, &user,
                        &input.employee_id, input.incarnation, &input.factor, &input.code, &check))?)
                }).await
            }
            CANCEL_RESTORE => {
                let input: OwnerCancel = serde_json::from_value(params).map_err(invalid)?;
                run(move || {
                    let now = now()?;
                    let cipher = crate::mfa::Cipher(&home);
                    let origin = passkey_origin(&home);
                    // F6fa: the tombstone's factor baseline of the row the purpose names.
                    let check = |factor: OwnerFactor, purpose: &str, code: &str| -> Result<OwnerFactor, OwnerFactorError> {
                        let baseline = rpc::factor_baseline(&home, &user, purpose)?;
                        db.restore_owner_factor(&user, epoch, factor, &baseline,
                            &RestoreProof { purpose, code, challenge_id: input.challenge_id.as_deref(), credential: input.credential.as_ref() },
                            &cipher, origin.as_deref(), now)
                    };
                    Ok(rpc::cancel_own(&home, &user, &input.request_id, &input.factor, &input.code, &check, now)?)
                }).await
            }
            DECIDE => {
                let input: Decide = serde_json::from_value(params).map_err(invalid)?;
                let approve = match input.decision.as_str() { "approve" => true, "deny" => false, _ => return Err(RpcError::Invalid.into()) };
                let (session, cap) = (session.ok_or(ADMIN_REQUIRED)?, self.effective_agent_cap().await);
                let scan_home = home.clone();
                let done = run(move || {
                    let now = now()?;
                    let verified_at = step_up(db.as_ref(), &user, &session, &input.csrf_token, now)?;
                    // The verified step-up of this very call (S1), its
                    // freshness computed from the proof (F6fa), never assumed.
                    let rule = LogRule::new(crate::isolation_restore_wiring::step_up_fresh(Some(verified_at), now), now);
                    let notices = || rule.notice_addresses();
                    Ok(with_env(&home, &user, &rule, cap, now, |env| rpc::decide(&home, env, &user, &input.request_id, approve, &notices))?)
                }).await?;
                if approve {
                    // The restored tree is back under agents/.
                    if let Err(error) = self.registry.write().await.scan().await {
                        warn!(error = %error, home = %scan_home.display(), "employee restore: registry rescan failed");
                    }
                }
                Ok(done)
            }
            CANCEL => {
                let input: AdminCancel = serde_json::from_value(params).map_err(invalid)?;
                let session = session.ok_or(ADMIN_REQUIRED)?;
                run(move || {
                    let now = now()?;
                    step_up(db.as_ref(), &user, &session, &input.csrf_token, now)?;
                    Ok(rpc::admin_cancel(&home, &user, &input.request_id, now)?)
                }).await
            }
            REGRANT => {
                let input: Regrant = serde_json::from_value(params).map_err(invalid)?;
                let session = session.ok_or(ADMIN_REQUIRED)?;
                run(move || {
                    let now = now()?;
                    step_up(db.as_ref(), &user, &session, &input.csrf_token, now)?;
                    Ok(rpc::regrant(&home, &user, &input.employee_id, input.incarnation, &input.item)?)
                }).await
            }
            RETIRE_PROPOSE => {
                let input: RetirePropose = serde_json::from_value(params).map_err(invalid)?;
                let session = session.ok_or(ADMIN_REQUIRED)?;
                run(move || {
                    let now = now()?;
                    step_up(db.as_ref(), &user, &session, &input.csrf_token, now)?;
                    Ok(rpc::retire_propose(&home, &user, &input.employee_id, now)?)
                }).await
            }
            REQUEST_ADOPTION => {
                let input: OwnerAdoption = serde_json::from_value(params).map_err(invalid)?;
                let cap = self.effective_agent_cap().await;
                run(move || {
                    let now = now()?;
                    let cipher = crate::mfa::Cipher(&home);
                    let origin = passkey_origin(&home);
                    // F6fa: the tombstone's factor baseline of the row the purpose names.
                    let check = |factor: OwnerFactor, purpose: &str, code: &str| -> Result<OwnerFactor, OwnerFactorError> {
                        let baseline = rpc::factor_baseline(&home, &user, purpose)?;
                        db.restore_owner_factor(&user, epoch, factor, &baseline,
                            &RestoreProof { purpose, code, challenge_id: input.challenge_id.as_deref(), credential: input.credential.as_ref() },
                            &cipher, origin.as_deref(), now)
                    };
                    Ok(with_adopt_env(&home, &user, cap, now, |env| rpc::request_adoption(&home, env, &user, &input.employee_id,
                        input.incarnation, input.target_label.as_deref(), &input.factor, &input.code, &check))?)
                }).await
            }
            SHARED_ADOPTION_REQUEST => {
                let input: SharedAdoption = serde_json::from_value(params).map_err(invalid)?;
                let (session, cap) = (session.ok_or(ADMIN_REQUIRED)?, self.effective_agent_cap().await);
                run(move || {
                    let now = now()?;
                    step_up(db.as_ref(), &user, &session, &input.csrf_token, now)?;
                    Ok(with_adopt_env(&home, &user, cap, now, |env| rpc::shared_adoption_request(&home, env, &user,
                        &input.employee_id, input.incarnation, input.target_label.as_deref(), input.members))?)
                }).await
            }
            RESTORE_FOR_OWNER => {
                let input: ForOwner = serde_json::from_value(params).map_err(invalid)?;
                let (session, cap) = (session.ok_or(ADMIN_REQUIRED)?, self.effective_agent_cap().await);
                run(move || {
                    let now = now()?;
                    let verified_at = step_up(db.as_ref(), &user, &session, &input.csrf_token, now)?;
                    // F6fa2: the filer's step-up freshness, computed from the proof.
                    let fresh = crate::isolation_restore_wiring::step_up_fresh(Some(verified_at), now);
                    // Filing never approves: the rule gets no step-up.
                    let rule = LogRule::new(false, now);
                    Ok(with_env(&home, &user, &rule, cap, now, |env| rpc::request_restore_for_owner(&home, env, &user,
                        &input.employee_id, input.incarnation, fresh))?)
                }).await
            }
            ADOPTION_FOR_OWNER => {
                let input: AdoptionForOwner = serde_json::from_value(params).map_err(invalid)?;
                let (session, cap) = (session.ok_or(ADMIN_REQUIRED)?, self.effective_agent_cap().await);
                run(move || {
                    let now = now()?;
                    let verified_at = step_up(db.as_ref(), &user, &session, &input.csrf_token, now)?;
                    let fresh = crate::isolation_restore_wiring::step_up_fresh(Some(verified_at), now);
                    Ok(with_adopt_env(&home, &user, cap, now, |env| rpc::adoption_request_for_owner(&home, env, &user,
                        &input.employee_id, input.incarnation, input.target_label.as_deref(), fresh))?)
                }).await
            }
            // F6k Shared restore (`isolation_shared_restore.rs`).
            m if shared::METHODS.contains(&m) => self.shared_restore(m, params, ctx, db, session).await,
            _ => Err(NOT_ALLOWED.into()),
        }
    }
}

#[cfg(test)]
#[path = "isolation_employee_restore_tests.rs"]
mod tests;
