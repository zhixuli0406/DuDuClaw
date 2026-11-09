//! Dashboard WebAuthn HTTP transport. No cookies authenticate these routes.
use super::*;
use serde_json::{Value, json};
use axum::http::{HeaderMap, StatusCode};
use duduclaw_auth::mfa::passkey::relying_party;

fn response(value: Value) -> axum::response::Response {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"));
    response
}
fn denied() -> axum::response::Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"error":"通行金鑰驗證失敗或已過期，請重新登入。"}))).into_response()
}
fn limited() -> axum::response::Response {
    (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":"嘗試次數過多，請稍後再試。"}))).into_response()
}
/// The operator-configured dashboard origin (passkey relying party). Also
/// the origin of the F6d restore passkey proof, which has no request header.
pub(crate) fn configured_origin(home: &std::path::Path) -> Result<String, String> {
    let text = crate::handlers::config_commit::read_regular_config(&home.join("config.toml")).map_err(|_|"無法讀取登入設定。")?;
    let table: toml::Table = text.parse().map_err(|_|"登入設定格式不正確。")?;
    let base = if let Some(value) = table.get("dashboard").and_then(|d|d.get("public_url")) {
        value.as_str().filter(|s|!s.trim().is_empty()).ok_or("請設定公開控制台網址。")?.to_owned()
    } else {
        let port = table.get("gateway").and_then(|g|g.get("port")).and_then(|p|p.as_integer())
            .filter(|p|(1..=65535).contains(p)).ok_or("請管理員設定公開控制台網址後再使用通行金鑰。")?;
        format!("http://localhost:{port}")
    };
    let url = url::Url::parse(&base).map_err(|_|"公開控制台網址無效。")?;
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("公開控制台網址無效。".into());
    }
    let origin = url.origin().ascii_serialization();
    relying_party(&origin)?;
    Ok(origin)
}
fn checked_origin(home: &std::path::Path, headers: &HeaderMap) -> Result<String,String> {
    if headers.get_all("origin").iter().count()!=1 { return Err("缺少瀏覽器來源。".into()); }
    let configured = configured_origin(home)?;
    let presented = headers.get("origin").and_then(|h|h.to_str().ok()).ok_or("缺少瀏覽器來源。")?;
    let expected = url::Url::parse(&configured).map_err(|_|"公開控制台網址無效。")?;
    let parsed = url::Url::parse(presented).map_err(|_|"瀏覽器來源不符。")?;
    if !duduclaw_core::origin_host_matches(presented, &[expected.host_str().ok_or("公開控制台網址無效。")?])
        || presented != parsed.origin().ascii_serialization() || parsed.origin()!=expected.origin() {
        return Err("請從已設定的公開控制台網址使用通行金鑰。".into());
    }
    Ok(configured)
}
fn account(state: &AppState, headers: &HeaderMap) -> Result<(UserContext,String),String> {
    let token = extract_bearer_token(headers).ok_or("請重新登入。")?;
    let context = authenticate_jwt(state,token)?;
    if context.must_change_password { return Err("請先變更密碼。".into()); }
    let session = state.jwt_config.verify_access_token(token)?.jti.ok_or("請重新登入。")?;
    Ok((context,session))
}
fn issue(jwt: &JwtConfig, user: &duduclaw_auth::User, access: &[(String,duduclaw_auth::AccessLevel)], codes: Vec<String>) -> Result<Value,String> {
    let (access_token,refresh_token)=jwt.issue_session_tokens(user,access)?;
    Ok(json!({"user":user,"access_token":access_token,"refresh_token":refresh_token,"recovery_codes":codes}))
}

pub(super) async fn list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> axum::response::Response {
    let Ok((ctx,_))=account(&state,&headers) else { return denied(); };
    let available=checked_origin(&state.home_dir,&headers);
    match state.user_db.mfa_passkeys(&ctx.user_id) {
        Ok(passkeys)=>response(json!({"passkeys":passkeys,"available":available.is_ok(),"unavailable_reason":available.err()})),
        Err(_)=>denied(),
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RegisterBegin {
    password: Option<String>, name: String, enrollment_token: Option<String>, proof_code: Option<String>,
}
pub(super) async fn register_begin(State(state): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(body): Json<RegisterBegin>) -> axum::response::Response {
    if !check_login_rate_limit(addr.ip(),"mfa-passkey") { return limited(); }
    let Ok(origin)=checked_origin(&state.home_dir,&headers) else { return denied(); };
    let personal=state.handler.resolve_edition_profile().await.is_personal();
    if crate::mfa::enforced(&state.home_dir,personal).is_err() { return denied(); }
    let account = if headers.contains_key("authorization") {
        match account(&state,&headers) { Ok(a)=>Some(a),Err(_)=>return denied() }
    } else { None };
    let work=state.clone();
    let result=tokio::task::spawn_blocking(move || -> Result<Value,String> {
        let state=work;
        let now=chrono::Utc::now().timestamp();
        let (user,token,session)=if let Some((ctx,session))=account {
            let token=if let Some(token)=body.enrollment_token {
                let user=state.user_db.get_user(&ctx.user_id)?.ok_or("帳號不存在。")?;
                let verified=state.user_db.verify_password(&user.email,body.password.as_deref().unwrap_or(""))?;
                if verified.id!=ctx.user_id || verified.credential_epoch!=ctx.credential_epoch { return Err("請重新登入。".into()); }
                state.user_db.mfa_check_session(&token,&ctx.user_id,&session,now)?;
                state.user_db.mfa_prove_enrollment(&token,body.proof_code.as_deref().unwrap_or(""),now)?;
                token
            } else {
                let token=state.user_db.mfa_password_challenge(&ctx.user_id,body.password.as_deref().unwrap_or(""),&session,now)?;
                let required=crate::mfa::enforced(&state.home_dir,personal)? && !state.user_db.mfa_active(&ctx.user_id)?;
                let delivery=state.user_db.mfa_prepare_enrollment(&token,required,now)?;
                if required {
                    return Ok(json!({"enrollment_token":token,"challenge_ref":duduclaw_auth::mfa::enrollment_challenge_ref(&token),"proof_required":true,
                        "_delivery":delivery.map(|o|json!({"channel":o.channel,"target":o.channel_user_id,"code":o.code}))}));
                }
                token
            };
            (ctx.user_id,token,Some(session))
        } else {
            let token=body.enrollment_token.ok_or("缺少註冊憑證。")?;
            state.user_db.mfa_http_enrollment(&token,body.password.as_deref(),now)?;
            state.user_db.mfa_prove_enrollment(&token,body.proof_code.as_deref().unwrap_or(""),now)?;
            (state.user_db.mfa_enrollment_user(&token,now)?,token,None)
        };
        state.user_db.mfa_passkey_register_begin(&user,&token,session.as_deref(),&origin,&body.name,now,personal)
    }).await;
    match result {
        Ok(Ok(mut value))=>{
            if let Some(delivery)=value.as_object_mut().and_then(|v|v.remove("_delivery")).filter(|v|!v.is_null()) {
                if state.otp_delivery.deliver(delivery["channel"].as_str().unwrap_or(""),delivery["target"].as_str().unwrap_or(""),
                    &format!("DuDuClaw 兩步驟註冊驗證碼：{}（5 分鐘內有效）",delivery["code"].as_str().unwrap_or(""))).await.is_err() { return denied(); }
            }
            response(value)
        }, _=>denied(),
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RegisterFinish { ceremony_id: String, enrollment_token: String, credential: Value }
pub(super) async fn register_finish(State(state): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(body): Json<RegisterFinish>) -> axum::response::Response {
    if !check_login_rate_limit(addr.ip(),"mfa-passkey") { return limited(); }
    let Ok(origin)=checked_origin(&state.home_dir,&headers) else { return denied(); };
    let personal=state.handler.resolve_edition_profile().await.is_personal();
    if crate::mfa::enforced(&state.home_dir,personal).is_err() { return denied(); }
    let account=if headers.contains_key("authorization") {
        match account(&state,&headers) { Ok(a)=>Some(a),Err(_)=>return denied() }
    } else { None };
    let work=state.clone();
    let result=tokio::task::spawn_blocking(move || -> Result<Value,String> {
        let now=chrono::Utc::now().timestamp();
        let user=work.user_db.mfa_enrollment_user(&body.enrollment_token,now)?;
        let session=if let Some((ctx,session))=account {
            if ctx.user_id!=user { return Err("帳號不符。".into()); }
            Some(session)
        } else {
            // Begin already proved the channel-first password. Auth checks the
            // server-held ceremony and requires a public (session-less) proof.
            None
        };
        work.user_db.mfa_passkey_register_finish(&body.ceremony_id,&body.enrollment_token,session.as_deref(),&origin,body.credential,now,personal,
            |user,access,codes|issue(&work.jwt_config,user,access,codes))
    }).await;
    match result {
        Ok(Ok(value))=>{
            if let Some(user)=value["user"]["id"].as_str() {
                crate::mfa_email::notify_change(&state.home_dir,&state.user_db,user,"新增通行金鑰");
            }
            response(value)
        }, _=>denied(),
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginBegin { challenge_id: String }
pub(super) async fn login_begin(State(state): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(body): Json<LoginBegin>) -> axum::response::Response {
    if !check_login_rate_limit(addr.ip(),"mfa-passkey") { return limited(); }
    let Ok(origin)=checked_origin(&state.home_dir,&headers) else { return denied(); };
    let personal=state.handler.resolve_edition_profile().await.is_personal();
    if crate::mfa::enforced(&state.home_dir,personal).is_err() { return denied(); }
    let result=tokio::task::spawn_blocking(move || state.user_db.mfa_passkey_login_begin(&body.challenge_id,&origin,chrono::Utc::now().timestamp())).await;
    match result { Ok(Ok(value))=>response(value), _=>denied() }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginFinish { challenge_id: String, ceremony_id: String, credential: Value }
pub(super) async fn login_finish(State(state): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(body): Json<LoginFinish>) -> axum::response::Response {
    if !check_login_rate_limit(addr.ip(),"mfa-passkey") { return limited(); }
    let Ok(origin)=checked_origin(&state.home_dir,&headers) else { return denied(); };
    let personal=state.handler.resolve_edition_profile().await.is_personal();
    if crate::mfa::enforced(&state.home_dir,personal).is_err() { return denied(); }
    let result=tokio::task::spawn_blocking(move || state.user_db.mfa_passkey_login_finish(&body.ceremony_id,&body.challenge_id,&origin,body.credential,chrono::Utc::now().timestamp(),
        |user,access,codes|issue(&state.jwt_config,user,access,codes))).await;
    match result { Ok(Ok(value))=>{ reset_login_rate_limit(addr.ip(),"mfa-passkey"); response(value) }, _=>denied() }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Delete { id: String, password: String }
pub(super) async fn delete(State(state): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(body): Json<Delete>) -> axum::response::Response {
    if !check_login_rate_limit(addr.ip(),"mfa-passkey") { return limited(); }
    if checked_origin(&state.home_dir,&headers).is_err() { return denied(); }
    let Ok((ctx,session))=account(&state,&headers) else { return denied(); };
    let personal=state.handler.resolve_edition_profile().await.is_personal();
    if crate::mfa::enforced(&state.home_dir,personal).is_err() { return denied(); }
    let work=state.clone();
    let result=tokio::task::spawn_blocking(move || -> Result<Value,String> {
        let now=chrono::Utc::now().timestamp();
        let token=work.user_db.mfa_factor_password_challenge(&ctx.user_id,&body.password,&session,now)?;
        work.user_db.mfa_passkey_delete(&ctx.user_id,&body.id,&token,&session,now,personal,
            |user,access,codes|issue(&work.jwt_config,user,access,codes))
    }).await;
    match result {
        Ok(Ok(value))=>{
            if let Some(user)=value["user"]["id"].as_str() { crate::mfa_email::notify_change(&state.home_dir,&state.user_db,user,"移除通行金鑰"); }
            response(value)
        }, _=>denied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passkey_http_origin_uses_operator_configuration_exactly() {
        let home=tempfile::tempdir().unwrap();
        let mut headers=HeaderMap::new();
        std::fs::write(home.path().join("config.toml"),"[dashboard]\npublic_url='https://dashboard.example.com/app'\n").unwrap();
        assert!(checked_origin(home.path(),&headers).is_err());
        for value in ["null","https://dashboard.example.com.evil.com","http://dashboard.example.com","https://dashboard.example.com:444","https://user@dashboard.example.com"] {
            headers.insert("origin",value.parse().unwrap());
            assert!(checked_origin(home.path(),&headers).is_err(),"{value}");
        }
        headers.insert("origin","https://dashboard.example.com".parse().unwrap());
        assert_eq!(checked_origin(home.path(),&headers).unwrap(),"https://dashboard.example.com");
        std::fs::write(home.path().join("config.toml"),"[dashboard]\npublic_url='http://192.168.0.1:18889'\n").unwrap();
        assert!(configured_origin(home.path()).is_err());
        std::fs::write(home.path().join("config.toml"),"[gateway]\nport=18889\n").unwrap();
        assert_eq!(configured_origin(home.path()).unwrap(),"http://localhost:18889");
        std::fs::write(home.path().join("config.toml"),"[dashboard]\npublic_url=false\n[gateway]\nport=18889\n").unwrap();
        assert!(configured_origin(home.path()).is_err());
    }
}
