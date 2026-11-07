//! Password-completed login capabilities may request and prove an Email factor.
use super::*;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmailBody {
    challenge_id: Option<String>,
    enrollment_token: Option<String>,
    password: Option<String>,
    code: Option<String>,
}
impl EmailBody {
    fn capability(&self) -> Option<(&str,bool)> {
        match (self.challenge_id.as_deref(),self.enrollment_token.as_deref()) {
            (Some(token),None) if token.len()==64 => Some((token,false)),
            (None,Some(token)) if token.len()==64 => Some((token,true)),
            _=>None,
        }
    }
}
fn denied() -> axum::response::Response {
    (axum::http::StatusCode::UNAUTHORIZED,Json(serde_json::json!({"error":"兩步驟驗證失敗或已過期，請重新登入。"}))).into_response()
}
fn unavailable() -> axum::response::Response {
    (axum::http::StatusCode::SERVICE_UNAVAILABLE,Json(serde_json::json!({"error":crate::mfa_email::UNAVAILABLE}))).into_response()
}
fn response(value:serde_json::Value)->axum::response::Response {
    let mut response=Json(value).into_response();
    response.headers_mut().insert(axum::http::header::CACHE_CONTROL,axum::http::HeaderValue::from_static("no-store"));
    response
}

pub(super) async fn send(State(state):State<Arc<AppState>>,ConnectInfo(addr):ConnectInfo<SocketAddr>,Json(body):Json<EmailBody>)->axum::response::Response {
    if !check_otp_verify_rate_limit(addr.ip()) || !check_login_rate_limit(addr.ip(),"mfa") {return denied();}
    let Some((token,enrollment))=body.capability() else {return denied();};
    let token=token.to_owned();
    let check=state.clone();
    let capability=token.clone();
    let password=body.password.clone();
    let checked=tokio::task::spawn_blocking(move || check.user_db.mfa_email_http_capability(
        &capability,enrollment,password.as_deref(),chrono::Utc::now().timestamp())).await;
    if !matches!(checked,Ok(Ok(()))) {return denied();}
    if !crate::mfa_email::available(&state.home_dir) {return unavailable();}
    let work=state.clone();
    let capability=token.clone();
    let result=tokio::task::spawn_blocking(move ||->Result<(String,String),String> {
        let now=chrono::Utc::now().timestamp();
        if enrollment {work.user_db.mfa_http_enrollment(&capability,body.password.as_deref(),now)?;}
        work.user_db.mfa_email_send(&capability,enrollment,now)
    }).await;
    match result {
        Ok(Ok((email,code)))=>match crate::mfa_email::send_code(&state.home_dir,&state.user_db,&token,&email,&code).await {
            Ok(())=>response(serde_json::json!({"sent":true,"expires_in":600})),
            Err(_)=>unavailable(),
        },
        _=>denied(),
    }
}

pub(super) async fn verify(State(state):State<Arc<AppState>>,ConnectInfo(addr):ConnectInfo<SocketAddr>,Json(body):Json<EmailBody>)->axum::response::Response {
    if !check_otp_verify_rate_limit(addr.ip()) || !check_login_rate_limit(addr.ip(),"mfa") {return denied();}
    let Some((token,enrollment))=body.capability() else {return denied();};
    let token=token.to_owned();
    let check=state.clone();
    let capability=token.clone();
    let password=body.password.clone();
    let checked=tokio::task::spawn_blocking(move || check.user_db.mfa_email_http_capability(
        &capability,enrollment,password.as_deref(),chrono::Utc::now().timestamp())).await;
    if !matches!(checked,Ok(Ok(()))) {return denied();}
    let personal=state.handler.resolve_edition_profile().await.is_personal();
    if crate::mfa::enforced(&state.home_dir,personal).is_err() {return denied();}
    let work=state.clone();
    let result=tokio::task::spawn_blocking(move ||->Result<serde_json::Value,String> {
        let now=chrono::Utc::now().timestamp();
        if enrollment {work.user_db.mfa_http_enrollment(&token,body.password.as_deref(),now)?;}
        work.user_db.mfa_email_verify_and_issue(&token,body.code.as_deref().unwrap_or(""),enrollment,now,|user,access,codes| {
            let (access_token,refresh_token)=work.jwt_config.issue_session_tokens(user,access)?;
            Ok(serde_json::json!({"access_token":access_token,"refresh_token":refresh_token,"user":user,"recovery_codes":codes}))
        })
    }).await;
    match result {
        Ok(Ok(value))=> {
            reset_login_rate_limit(addr.ip(),"mfa");
            if let Some(email)=value["user"]["email"].as_str() {reset_login_rate_limit(addr.ip(),email);}
            if enrollment {
                if let Some(id)=value["user"]["id"].as_str() {
                    crate::mfa_email::notify_change(&state.home_dir,&state.user_db,id,"新增 Email 驗證碼");
                }
            }
            response(value)
        },
        _=>denied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn email_http_requires_exactly_one_capability() {
        let token="a".repeat(64);
        for value in [serde_json::json!({}),serde_json::json!({"challenge_id":token,"enrollment_token":token}),serde_json::json!({"challenge_id":"short"})] {
            assert!(serde_json::from_value::<EmailBody>(value).unwrap().capability().is_none());
        }
        assert!(serde_json::from_value::<EmailBody>(serde_json::json!({"challenge_id":token})).unwrap().capability().is_some());
    }
}
