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
pub(super) async fn after_first_factor(
    state: &AppState,
    user: &duduclaw_auth::User,
    first: FirstFactor,
) -> Option<axum::response::Response> {
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    let result: Result<Option<serde_json::Value>, String> = (|| {
        let required = enforced(&state.home_dir, personal)?;
        let active = state.user_db.mfa_active(&user.id)?;
        if !active && !required {
            return Ok(None);
        }
        let challenge = state.user_db.mfa_challenge(
            &user.id,
            first,
            !active,
            chrono::Utc::now().timestamp(),
        )?;
        Ok(Some(if active {
            serde_json::json!({"mfa_required":true,"challenge_id":challenge})
        } else {
            serde_json::json!({"mfa_enrollment_required":true,"enrollment_token":challenge})
        }))
    })();
    match result {
        Ok(Some(body)) => Some(json_response(body)),
        Ok(None) => None,
        Err(_) => Some(denied()),
    }
}

pub(super) async fn refresh_allowed(state: &AppState, user: &str) -> bool {
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    match enforced(&state.home_dir, personal) {
        Ok(false) => true,
        Ok(true) => state.user_db.mfa_active(user).unwrap_or(false),
        Err(_) => false,
    }
}

/// Do not interrupt work at the policy toggle. Enterprise JWT sockets under
/// MFA close only after their own access-token expiry (next 30-second tick).
/// Unenrolled accounts with enforcement off and Personal keep their old behavior.
pub(super) async fn socket_expired(state: &AppState, user: &str, expires: Option<u64>) -> bool {
    let Some(expires) = expires else {
        return false;
    };
    if chrono::Utc::now().timestamp() < expires as i64 {
        return false;
    }
    let personal = state.handler.resolve_edition_profile().await.is_personal();
    if personal {
        return false;
    }
    match enforced(&state.home_dir, false) {
        Ok(required) => required || state.user_db.mfa_active(user).unwrap_or(true),
        Err(_) => true,
    }
}

fn session(
    state: &AppState,
    user_id: &str,
    recovery_codes: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    let user = state
        .user_db
        .get_user(user_id)?
        .filter(|u| u.status == duduclaw_auth::UserStatus::Active)
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
        let id = work.user_db.mfa_verify(
            &body.challenge_id,
            &code.0,
            code.1,
            &Cipher(&work.home_dir),
            chrono::Utc::now().timestamp(),
        )?;
        session(&work, &id, None)
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
        let cipher = Cipher(&work.home_dir);
        if confirm {
            let codes = work.user_db.mfa_confirm(
                &id,
                Some(&body.enrollment_token),
                body.code.as_deref().unwrap_or(""),
                &cipher,
                now,
            )?;
            session(&work, &id, Some(codes))
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

#[cfg(test)]
#[path = "mfa_http_tests.rs"]
mod tests;
