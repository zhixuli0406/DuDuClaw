//! Dedicated MFA HTTP handlers. Enrollment capabilities are not JWTs.
use super::*;
use crate::mfa::{Cipher, enforced, enrollment_payload};
use duduclaw_auth::mfa::FirstFactor;

fn json_response(value: serde_json::Value) -> axum::response::Response {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

fn denied() -> axum::response::Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error":"雙因素驗證失敗或已過期，請重新登入。"})),
    )
        .into_response()
}
fn limited() -> axum::response::Response {
    (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({"error":"嘗試次數過多，請稍後再試。"})),
    )
        .into_response()
}

/// None preserves the exact legacy response, including all field names.
pub(crate) async fn after_first_factor(
    state: &AppState,
    user: &duduclaw_auth::User,
    first: FirstFactor,
) -> Option<axum::response::Response> {
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    let required = match enforced(&state.home_dir, personal) { Ok(v)=>v, Err(_)=>return Some(denied()) };
    first_factor_response(state, user, first, required).await
}

/// The issuance transaction refused a one-factor session. Do not consult the
/// cached policy again: another connection may have enabled the durable policy.
pub(crate) async fn require_after_first_factor(
    state: &AppState, user: &duduclaw_auth::User, first: FirstFactor,
) -> axum::response::Response {
    first_factor_response(state, user, first, true).await.unwrap_or_else(denied)
}

async fn first_factor_response(
    state: &AppState, user: &duduclaw_auth::User, first: FirstFactor, required: bool,
) -> Option<axum::response::Response> {
    let work = state.user_db.clone();
    let user_id = user.id.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let active = work.mfa_active(&user_id)?;
        if !active && !required { return Ok(None); }
        let now = chrono::Utc::now().timestamp();
        let challenge = work.mfa_challenge(&user_id, first, !active, now)?;
        let delivery = if !active { work.mfa_prepare_enrollment(&challenge, true, now)? } else { None };
        // RM-135: a hint only (the dashboard asks for the host setup code);
        // the proof itself re-checks everything.
        let bootstrap = !active && delivery.is_none() && work.mfa_bootstrap_available(&user_id).unwrap_or(false);
        Ok(Some((active, challenge, delivery, bootstrap)))
    }).await;
    let result = match result {
        Ok(Ok(Some((active, challenge, delivery, bootstrap)))) => {
            if let Some(otp) = delivery {
                if state.otp_delivery.deliver(&otp.channel, &otp.channel_user_id,
                    &format!("DuDuClaw 雙因素註冊驗證碼：{}（5 分鐘內有效）", otp.code)).await.is_err() {
                    return Some(denied());
                }
            }
            let mut payload=if active {
                let mut factors=match state.user_db.mfa_factors(&user.id) {Ok(v)=>v,Err(_)=>return Some(denied())};
                factors["mfa_required"]=serde_json::json!(true);
                factors["challenge_id"]=serde_json::json!(challenge);
                factors
            } else {serde_json::json!({"mfa_enrollment_required":true,"enrollment_token":challenge,"challenge_ref":duduclaw_auth::mfa::enrollment_challenge_ref(&challenge),
                "setup_code_required":bootstrap})};
            if matches!(first, FirstFactor::GoogleOidc) {
                // Google cannot prove an independent second factor via email.
                if let Some(factors) = payload.get_mut("factors").and_then(serde_json::Value::as_object_mut) {
                    factors.remove("email");
                }
                if payload["last_used"].as_str() == Some("email") {
                    payload["last_used"] = if payload["factors"]["totp"] == true { serde_json::json!("totp") }
                        else if payload["factors"]["passkey"] == true { serde_json::json!("passkey") }
                        else { serde_json::Value::Null };
                }
                payload["email_available"] = serde_json::json!(false);
                payload["email_unavailable_reason"] = serde_json::json!("請使用其他兩步驟驗證方式。");
            } else {
                let reason=crate::mfa_email::unavailable_reason(&state.home_dir);
                payload["email_available"]=serde_json::json!(reason.is_none());
                payload["email_unavailable_reason"]=serde_json::json!(reason);
            }
            Ok(Some(payload))
        }
        Ok(Ok(None)) => Ok(None),
        _ => Err(()),
    };
    match result {
        Ok(Some(body)) => Some(json_response(body)),
        Ok(None) => None,
        Err(_) => Some(denied()),
    }
}

pub(super) async fn legacy_token_allowed(state: &AppState) -> bool {
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    matches!(enforced(&state.home_dir, personal), Ok(false))
}

pub(super) async fn refresh_allowed(state: &AppState, user: &str) -> bool {
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    match enforced(&state.home_dir, personal) {
        Ok(false) => true,
        Ok(true) => state.user_db.mfa_active(user).unwrap_or(false),
        Err(_) => false,
    }
}

/// Do not interrupt work at the policy toggle. JWT sockets under
/// MFA close only after their own access-token expiry (next 30-second tick).
/// Identity read failures pause traffic and use the caller's recovery window.
/// Personal keeps its legacy expiry behavior only when enforcement is off.
pub(super) async fn socket_validity(state: &AppState, ctx: &UserContext, expires: Option<u64>, legacy: bool, heartbeat: bool) -> crate::handlers::live_context::SocketValidity {
    use crate::handlers::live_context::SocketValidity;
    if ctx.member_key_id.is_some() {
        return match state.handler.socket_context(ctx, true).await {
            Ok(_) => SocketValidity::Valid,
            Err(_) => SocketValidity::Revoked,
        };
    }
    if legacy {
        return if legacy_token_allowed(state).await { SocketValidity::Valid } else { SocketValidity::Revoked };
    }
    if expires.is_some()
        && let Err(status) = state.handler.socket_context(ctx, heartbeat).await {
        return status;
    }
    // Expiry policy remains a heartbeat check. Per-frame sends only consult
    // the short identity cache, including for legacy sessions past expiry.
    if !heartbeat { return SocketValidity::Valid; }
    let Some(expires) = expires else { return SocketValidity::Valid; };
    if (chrono::Utc::now().timestamp().max(0) as u64) < expires {
        return SocketValidity::Valid;
    }
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    match enforced(&state.home_dir, personal) {
        Ok(true) => SocketValidity::Revoked,
        Ok(false) if personal => SocketValidity::Valid,
        Ok(false) => match state.user_db.mfa_active(&ctx.user_id) {
            Ok(true) => SocketValidity::Revoked,
            Ok(false) => SocketValidity::Valid,
            Err(_) => SocketValidity::Unavailable,
        },
        Err(_) => SocketValidity::Unavailable,
    }
}

#[cfg(test)]
async fn socket_expired(state: &AppState, ctx: &UserContext, expires: Option<u64>, legacy: bool) -> bool {
    socket_validity(state, ctx, expires, legacy, true).await != crate::handlers::live_context::SocketValidity::Valid
}

/// Only these two short local operations must exclude interleaved socket
/// gates between their commit and handover. Long-running RPCs stay spawned.
pub(super) fn is_session_handover(method: &str) -> bool {
    matches!(method, "mfa.enroll_confirm" | "mfa.disable" | "mfa.email_confirm" | "mfa.email_remove"
        | "mfa.passkey_register_finish" | "mfa.passkey_delete")
}

pub(super) struct MfaRpcResult {
    pub response: WsFrame,
    pub handover: Option<MfaSessionHandover>,
}

/// Private transport metadata: never accepted from a client or serialized.
pub(super) struct MfaSessionHandover {
    ctx: UserContext,
    session: String,
    expires: u64,
}

impl MfaSessionHandover {
    pub(super) fn apply(self, ctx: &mut UserContext,
        session: &mut Option<(String, String)>, expires: &mut Option<u64>) {
        *session = Some((self.ctx.user_id.clone(), self.session));
        *expires = Some(self.expires);
        *ctx = self.ctx;
    }
}

/// Run the real authorized dispatcher, then attach an explicit handover only
/// to a successful MFA epoch transition for this caller. The normal send gate
/// still checks the new context, including any intervening reset/suspension.
pub(super) async fn call_with_handover(
    state: &AppState, method: &str, params: serde_json::Value, ctx: &UserContext,
    conn: crate::power_local::RpcConnInfo,
) -> MfaRpcResult {
    let response = state.handler.handle_conn(method, params, ctx, conn).await;
    let handover = if is_session_handover(method) {
        match &response {
            WsFrame::Response { ok: true, payload: Some(payload), .. } => {
                payload.get("access_token").and_then(serde_json::Value::as_str)
                    .and_then(|token| state.jwt_config.verify_access_token(token).ok())
                    .filter(|claims| claims.sub == ctx.user_id
                        && ctx.credential_epoch.checked_add(1) == Some(claims.credential_epoch))
                    .and_then(|claims| {
                        let session = claims.jti.clone()?;
                        let new_ctx = UserContext::from_claims(&claims, ctx.must_change_password).ok()?;
                        let new_ctx = crate::machine_employee::without_restricted_access(
                            state.handler.home_dir(), new_ctx,
                        );
                        Some(MfaSessionHandover { ctx: new_ctx, session, expires: claims.exp })
                    })
            }
            _ => None,
        }
    } else { None };
    MfaRpcResult { response, handover }
}

fn session(
    state: &AppState,
    user_id: &str,
    verified_epoch: u64,
    recovery_codes: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    let user = state
        .user_db
        .get_user(user_id)?
        .filter(|u| u.status == duduclaw_auth::UserStatus::Active && duduclaw_auth::mfa::credential_epoch_matches(verified_epoch, u.credential_epoch))
        .ok_or("無法登入此帳號。")?;
    let bindings = state.user_db.get_user_agents(user_id)?;
    let access = bindings
        .iter()
        .map(|b| (b.agent_name.clone(), b.access_level))
        .collect::<Vec<_>>();
    let (access_token, refresh_token) = state.jwt_config.issue_session_tokens(&user, &access)?;
    state.user_db.update_last_login(user_id)?;
    let mut body =
        serde_json::json!({"access_token":access_token,"refresh_token":refresh_token,"user":user});
    if let Some(codes) = recovery_codes {
        body["recovery_codes"] = serde_json::json!(codes);
    }
    Ok(body)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyBody {
    challenge_id: String,
    code: Option<String>,
    recovery_code: Option<String>,
}
pub(super) async fn verify(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<VerifyBody>,
) -> axum::response::Response {
    if !check_otp_verify_rate_limit(addr.ip()) || !check_login_rate_limit(addr.ip(), "mfa") {
        return limited();
    }
    let code = match (body.code, body.recovery_code) {
        (Some(c), None) => (c, false),
        (None, Some(c)) => (c, true),
        _ => return denied(),
    };
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    if enforced(&state.home_dir, personal).is_err() {
        return denied();
    }
    let work = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let (id, epoch) = work.user_db.mfa_verify(
            &body.challenge_id,
            &code.0,
            code.1,
            &Cipher(&work.home_dir),
            chrono::Utc::now().timestamp(),
        )?;
        session(&work, &id, epoch, None)
    })
    .await;
    match result {
        Ok(Ok(body)) => {
            reset_login_rate_limit(addr.ip(), "mfa");
            if let Some(email) = body
                .get("user")
                .and_then(|u| u.get("email"))
                .and_then(serde_json::Value::as_str)
            {
                reset_login_rate_limit(addr.ip(), email);
            }
            json_response(body)
        }
        _ => {
            let _ = state.user_db.log_action(
                None,
                "mfa.verification_failed",
                None,
                None,
                Some(&addr.ip().to_string()),
            );
            denied()
        }
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnrollBody {
    enrollment_token: String,
    code: Option<String>,
    proof_code: Option<String>,
    password: Option<String>,
}

pub(super) async fn enroll_begin(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<EnrollBody>,
) -> axum::response::Response {
    enroll(state, addr, body, false).await
}
pub(super) async fn enroll_confirm(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<EnrollBody>,
) -> axum::response::Response {
    enroll(state, addr, body, true).await
}
async fn enroll(
    state: Arc<AppState>,
    addr: SocketAddr,
    body: EnrollBody,
    confirm: bool,
) -> axum::response::Response {
    if !check_otp_verify_rate_limit(addr.ip()) || !check_login_rate_limit(addr.ip(), "mfa") {
        return limited();
    }
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    if enforced(&state.home_dir, personal).is_err() {
        return denied();
    }
    let work = state.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let now = chrono::Utc::now().timestamp();
        let id = work
            .user_db
            .mfa_enrollment_user(&body.enrollment_token, now)?;
        work.user_db.mfa_http_enrollment(&body.enrollment_token, body.password.as_deref(), now)?;
        work.user_db.mfa_prove_enrollment(&body.enrollment_token, body.proof_code.as_deref().unwrap_or(""), now)?;
        work.user_db.mfa_require_independent_proof(&body.enrollment_token,now)?;
        let cipher = Cipher(&work.home_dir);
        if confirm {
            let epoch = work.user_db.get_user(&id)?.ok_or("無法登入此帳號。")?.credential_epoch;
            let payload = work.user_db.mfa_confirm_and_issue(
                &id,
                Some(&body.enrollment_token),
                body.code.as_deref().unwrap_or(""),
                &cipher,
                now,
                |user, access, codes| crate::mfa::replacement_session(
                    &work.jwt_config, user, access, epoch, codes, true,
                ),
            )?;
            if work.user_db.update_last_login(&id).is_err() {
                tracing::warn!("MFA enrollment succeeded but last-login bookkeeping failed");
            }
            Ok(payload)
        } else {
            let user = work.user_db.get_user(&id)?.ok_or("無法登入此帳號。")?;
            let secret = work
                .user_db
                .mfa_begin(&id, Some(&body.enrollment_token), &cipher, now)?;
            Ok(enrollment_payload(secret, &user.email))
        }
    })
    .await;
    match result {
        Ok(Ok(body)) => {
            if confirm {
                reset_login_rate_limit(addr.ip(), "mfa");
                if let Some(email) = body
                    .get("user")
                    .and_then(|u| u.get("email"))
                    .and_then(serde_json::Value::as_str)
                {
                    reset_login_rate_limit(addr.ip(), email);
                }
                if let Some(id)=body.get("user").and_then(|u|u.get("id")).and_then(serde_json::Value::as_str) {
                    crate::mfa_email::notify_change(&state.home_dir,&state.user_db,id,"新增驗證器");
                }
            }
            json_response(body)
        }
        _ => {
            let _ = state.user_db.log_action(
                None,
                "mfa.verification_failed",
                None,
                None,
                Some(&addr.ip().to_string()),
            );
            denied()
        }
    }
}

/// Security audit event type of every first-Admin bootstrap attempt (RM-135).
pub(super) const BOOTSTRAP_AUDIT_EVENT: &str = "mfa.bootstrap_enrollment";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BootstrapBody {
    enrollment_token: String,
    setup_code: Option<String>,
}

/// Security audit event written once the process-wide cap of bootstrap audit
/// events per window is reached (M4).
pub(super) const BOOTSTRAP_AUDIT_SUPPRESSED_EVENT: &str = "mfa.bootstrap_enrollment_suppressed";
/// Post-validation bootstrap audit events per window, process-wide (M4).
pub(super) const BOOTSTRAP_AUDIT_CAP: u32 = 20;
const BOOTSTRAP_AUDIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3600);
const BOOTSTRAP_WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// What the bootstrap limiter allows for one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuditSlot {
    /// Write the event.
    Write,
    /// Cap just reached: write the one summary event instead.
    Summary,
    /// Over the cap: write nothing.
    Suppressed,
}

/// Process-wide, bounded bookkeeping of first-Administrator bootstrap
/// refusals (M4). Fixed size: three counters, a flag and two instants.
///
/// - Before the password-bound enrollment token is validated, nothing reaches
///   the security audit (its writer re-reads the whole hash chain and fsyncs
///   per record): refusals are only counted and summarized by at most one
///   ordinary `tracing::warn!` per [`BOOTSTRAP_WARN_INTERVAL`].
/// - After validation, at most [`BOOTSTRAP_AUDIT_CAP`] audit events per
///   [`BOOTSTRAP_AUDIT_WINDOW`], then one summary event, then nothing until
///   the window rolls over. A successful proof is always written (it needs
///   a fresh host-issued code, so it cannot be flooded) and counts too.
#[derive(Debug, Default)]
pub(super) struct BootstrapAuditLimiter {
    unauthenticated: u64,
    last_warn: Option<std::time::Instant>,
    window_start: Option<std::time::Instant>,
    audited: u32,
    summarized: bool,
    suppressed: u64,
}

impl BootstrapAuditLimiter {
    pub(super) const fn new() -> Self {
        Self { unauthenticated: 0, last_warn: None, window_start: None, audited: 0, summarized: false, suppressed: 0 }
    }

    /// Count one refusal before token validation. Returns the number of
    /// refusals to report when a warning is due now.
    pub(super) fn unauthenticated_refusal(&mut self, now: std::time::Instant) -> Option<u64> {
        self.unauthenticated = self.unauthenticated.saturating_add(1);
        let due = self.last_warn.is_none_or(|at| now.duration_since(at) >= BOOTSTRAP_WARN_INTERVAL);
        if !due { return None; }
        self.last_warn = Some(now);
        Some(std::mem::take(&mut self.unauthenticated))
    }

    /// Slot for one post-validation event. Returns the events suppressed in
    /// a window that just ended, if any (to be reported in `tracing`).
    pub(super) fn validated_event(&mut self, now: std::time::Instant, success: bool) -> (AuditSlot, Option<u64>) {
        let mut ended = None;
        if self.window_start.is_none_or(|at| now.duration_since(at) >= BOOTSTRAP_AUDIT_WINDOW) {
            if self.suppressed > 0 { ended = Some(self.suppressed); }
            self.window_start = Some(now);
            self.audited = 0;
            self.summarized = false;
            self.suppressed = 0;
        }
        let slot = if self.audited < BOOTSTRAP_AUDIT_CAP {
            self.audited += 1;
            AuditSlot::Write
        } else if success {
            AuditSlot::Write
        } else if !self.summarized {
            self.summarized = true;
            AuditSlot::Summary
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            AuditSlot::Suppressed
        };
        (slot, ended)
    }
}

static BOOTSTRAP_AUDIT_LIMITER: std::sync::Mutex<BootstrapAuditLimiter> = std::sync::Mutex::new(BootstrapAuditLimiter::new());

/// A bootstrap refusal before the enrollment token was validated: no audit
/// write, only the bounded counter and a rate-limited `tracing::warn!`.
fn note_unauthenticated_bootstrap_refusal() {
    let report = BOOTSTRAP_AUDIT_LIMITER.lock().unwrap_or_else(|e| e.into_inner())
        .unauthenticated_refusal(std::time::Instant::now());
    if let Some(refusals) = report {
        tracing::warn!(refusals, "first administrator setup: refused requests without a valid sign-in (not audited individually)");
    }
}

/// A post-validation bootstrap outcome, under the process-wide cap.
/// `reason` is a closed label; the setup code and tokens never reach here.
pub(super) fn audit_validated_bootstrap(limiter: &std::sync::Mutex<BootstrapAuditLimiter>, home: &std::path::Path,
    outcome: &str, reason: &str) {
    let (slot, ended) = limiter.lock().unwrap_or_else(|e| e.into_inner())
        .validated_event(std::time::Instant::now(), outcome == "allowed");
    if let Some(suppressed) = ended {
        tracing::warn!(suppressed, "first administrator setup: audit events suppressed in the last hour");
    }
    let event = match slot {
        AuditSlot::Write => duduclaw_security::audit::AuditEvent::new(
            BOOTSTRAP_AUDIT_EVENT, "dashboard", duduclaw_security::audit::Severity::Warning,
            serde_json::json!({"method": "setup_code", "outcome": outcome, "reason": reason})),
        AuditSlot::Summary => duduclaw_security::audit::AuditEvent::new(
            BOOTSTRAP_AUDIT_SUPPRESSED_EVENT, "dashboard", duduclaw_security::audit::Severity::Warning,
            serde_json::json!({"cap": BOOTSTRAP_AUDIT_CAP, "window_secs": BOOTSTRAP_AUDIT_WINDOW.as_secs(),
                "note": "further first administrator setup attempts in this window are not audited individually"})),
        AuditSlot::Suppressed => return,
    };
    crate::security_autopilot::audit_and_emit(home, &event);
}

/// POST /api/mfa/enroll/bootstrap (RM-135): prove host access for the first
/// Administrator's first-time enrollment with the one-time setup code the
/// host command `duduclaw ops users mfa-setup-code` printed. There is no
/// code-free path (no "same machine" claim: a reverse proxy on the host makes
/// every peer loopback). On success the ordinary `/api/mfa/enroll/begin` and
/// `/confirm` (or passkey registration) continue without an enrollment code.
/// Same rate limits as enrollment; the code is never logged or echoed.
/// Nothing reaches the security audit until the password-bound enrollment
/// token is validated (M4, [`BootstrapAuditLimiter`]).
pub(super) async fn enroll_bootstrap(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<BootstrapBody>,
) -> axum::response::Response {
    if !check_otp_verify_rate_limit(addr.ip()) || !check_login_rate_limit(addr.ip(), "mfa") {
        note_unauthenticated_bootstrap_refusal();
        return limited();
    }
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    if enforced(&state.home_dir, personal).is_err() {
        note_unauthenticated_bootstrap_refusal();
        return denied();
    }
    let setup = body.setup_code.map(|code| code.trim().to_owned()).filter(|code| !code.is_empty());
    let work = state.clone();
    let token = body.enrollment_token;
    // Ok(None): token not valid (unauthenticated); Ok(Some(result)): validated.
    let result = tokio::task::spawn_blocking(move || -> Option<Result<(), &'static str>> {
        let now = chrono::Utc::now().timestamp();
        work.user_db.mfa_enrollment_user(&token, now).ok()?;
        let Some(code) = setup else { return Some(Err("setup_code_required")) };
        Some(work.user_db.mfa_prove_bootstrap(&token, &code, now).map_err(|_| "proof_refused"))
    }).await;
    match result {
        Ok(Some(Ok(()))) => {
            audit_validated_bootstrap(&BOOTSTRAP_AUDIT_LIMITER, &state.home_dir, "allowed", "allowed");
            json_response(serde_json::json!({"bootstrap": true}))
        }
        Ok(Some(Err(reason))) => {
            audit_validated_bootstrap(&BOOTSTRAP_AUDIT_LIMITER, &state.home_dir, "refused", reason);
            let _ = state.user_db.log_action(None, "mfa.verification_failed", None, None, Some(&addr.ip().to_string()));
            denied()
        }
        Ok(None) | Err(_) => {
            note_unauthenticated_bootstrap_refusal();
            denied()
        }
    }
}

#[cfg(test)]
#[path = "mfa_http_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mfa_bootstrap_http_tests.rs"]
mod bootstrap_tests;
