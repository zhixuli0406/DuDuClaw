//! Dashboard-only persistent sessions. Cookie credentials never authenticate RPC or WebSocket.
use super::*;
use axum::http::{HeaderMap, StatusCode, header, HeaderValue};
use axum::response::Response;
use serde_json::{Value, json};

const COOKIE: &str = "duduclaw_remember";
const COOKIE_PATH: &str = "/api/session";
pub(super) fn dashboard_socket_age_valid(has_session: bool, age_secs: u64) -> bool {
    !has_session || duduclaw_auth::remembered_sessions::socket_age_valid(age_secs)
}
fn denied() -> Response { reply(StatusCode::UNAUTHORIZED,json!({"error":"登入已失效，請重新登入。"})) }
fn reply(status:StatusCode,value:Value)->Response {
    let mut response=(status,Json(value)).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL,HeaderValue::from_static("no-store"));
    response
}
fn exact_origins() -> &'static std::sync::RwLock<Vec<String>> {
    static ORIGINS: std::sync::OnceLock<std::sync::RwLock<Vec<String>>> = std::sync::OnceLock::new();
    ORIGINS.get_or_init(|| std::sync::RwLock::new(Vec::new()))
}
static HTTP_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(18889);
pub(super) fn set_origins(raw: &[String], port: Option<u16>) {
    if let Some(port) = port { HTTP_PORT.store(port, std::sync::atomic::Ordering::Relaxed); }
    let mut origins = raw.to_vec();
    if let Ok(env) = std::env::var("DUDUCLAW_ALLOWED_ORIGINS") {
        origins.extend(env.split(',').map(str::to_owned));
    }
    if let Ok(mut target) = exact_origins().write() { *target = origins; }
}
fn canonical_origin(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some()
        || url.path() != "/" || url.query().is_some() || url.fragment().is_some() { return None; }
    Some(url.origin().ascii_serialization())
}
fn csrf_with(headers: &HeaderMap, extra: &[String], port: u16) -> bool {
    if headers.get_all(header::ORIGIN).iter().count() != 1
        || headers.get_all("x-duduclaw-csrf").iter().count() != 1
        || headers.get("x-duduclaw-csrf").and_then(|v|v.to_str().ok()) != Some("1") { return false; }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v|v.to_str().ok()).and_then(canonical_origin) else { return false; };
    ["localhost", "127.0.0.1", "[::1]"].iter().any(|host|
        canonical_origin(&format!("http://{host}:{port}")).as_ref() == Some(&origin))
        || extra.iter().filter_map(|s|canonical_origin(s.trim())).any(|s|s == origin)
}
fn csrf(headers:&HeaderMap)->bool {
    exact_origins().read().map(|origins|csrf_with(headers, &origins,
        HTTP_PORT.load(std::sync::atomic::Ordering::Relaxed))).unwrap_or(false)
}
fn secure(headers:&HeaderMap)->bool {
    headers.get(header::ORIGIN).and_then(|v|v.to_str().ok())
        .and_then(|s|s.split_once("://")).map(|(scheme,_)|scheme)==Some("https")
}
fn cookie(headers:&HeaderMap)->Option<String> {
    let mut found=None;
    for value in headers.get_all(header::COOKIE) {
        for part in value.to_str().ok()?.split(';') {
            let Some((name,value))=part.trim().split_once('=') else {continue};
            if name==COOKIE {
                if found.is_some() || value.len()>160 || !value.bytes().all(|b|b.is_ascii_alphanumeric() || b".-".contains(&b)) {return None;}
                found=Some(value.to_owned());
            }
        }
    }
    found
}
fn with_cookie(mut response:Response,secret:&str,ttl:i64,headers:&HeaderMap)->Response {
    let value=format!("{COOKIE}={secret}; Path={COOKIE_PATH}; HttpOnly; SameSite=Strict; Max-Age={ttl}{}",if secure(headers){"; Secure"}else{""});
    match HeaderValue::from_str(&value) {
        Ok(value)=>{response.headers_mut().append(header::SET_COOKIE,value);response},
        Err(_)=>denied(),
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RememberBody { access_token:String,refresh_token:String }

pub(super) async fn remember(State(state):State<Arc<AppState>>,ConnectInfo(addr):ConnectInfo<SocketAddr>,headers:HeaderMap,Json(body):Json<RememberBody>)->Response {
    if !csrf(&headers) {return reply(StatusCode::FORBIDDEN,json!({"error":"請從已設定的管理介面操作。"}));}
    if check_refresh_rate_limit(addr.ip()).is_err() {return reply(StatusCode::TOO_MANY_REQUESTS,json!({"error":"請稍後再試。"}));}
    let now=chrono::Utc::now().timestamp();
    let access=match state.jwt_config.verify_access_token(&body.access_token) {Ok(c)=>c,Err(_)=>return denied()};
    let refresh=match state.jwt_config.verify_refresh_token(&body.refresh_token) {Ok(c)=>c,Err(_)=>return denied()};
    if access.sub!=refresh.sub || access.jti.is_none() || access.jti!=refresh.jti || access.credential_epoch!=refresh.credential_epoch
        || refresh.iat>now.max(0) as u64 || (now.max(0) as u64)-refresh.iat>300
        || authenticate_jwt(&state,&body.access_token).is_err() || !server_mfa::refresh_allowed(&state,&access.sub).await {return denied();}
    let label=headers.get(header::USER_AGENT).and_then(|v|v.to_str().ok()).unwrap_or("Browser");
    let policy_exempt=state.handler.resolve_edition_profile().await.is_personal();
    let result=state.user_db.remember_create(&access.sub,access.credential_epoch,access.jti.as_deref().unwrap_or(""),label,Some(&addr.ip().to_string()),365,now,policy_exempt,
        |user,bindings,session|state.jwt_config.issue_access_token_in_session(user,bindings,Some(session)));
    match result {
        Ok((token,secret,expires))=>{
            if let Some(previous)=cookie(&headers) {if state.user_db.remember_logout(&previous).is_err(){return denied();}}
            with_cookie(reply(StatusCode::OK,json!({"access_token":token,"remembered":true})),&secret,expires-now,&headers)
        },
        Err(_)=>denied(),
    }
}
pub(super) async fn refresh(State(state):State<Arc<AppState>>,ConnectInfo(addr):ConnectInfo<SocketAddr>,headers:HeaderMap)->Response {
    if !csrf(&headers) {return reply(StatusCode::FORBIDDEN,json!({"error":"請從已設定的管理介面操作。"}));}
    if check_refresh_rate_limit(addr.ip()).is_err(){return reply(StatusCode::TOO_MANY_REQUESTS,json!({"error":"請稍後再試。"}));}
    let Some(secret)=cookie(&headers) else{return denied()};
    let now=chrono::Utc::now().timestamp();
    let policy_exempt=state.handler.resolve_edition_profile().await.is_personal();
    if crate::mfa::enforced(&state.home_dir,policy_exempt).is_err() {
        return with_cookie(denied(),"",0,&headers);
    }
    let result=state.user_db.remember_rotate(&secret,now,policy_exempt,|user,bindings,session| {
        let token=state.jwt_config.issue_access_token_in_session(user,bindings,Some(session))?;
        Ok((token,user.id.clone(),session.to_owned()))
    });
    match result {
        Ok(((token,id,family),next,expires))=>{
            // Keep the existing runtime-policy gate as well as the durable
            // transaction check (e.g. no valid configuration loaded yet).
            if !server_mfa::refresh_allowed(&state,&id).await {
                let _=state.user_db.remember_revoke(&id,Some(&family));
                return with_cookie(denied(),"",0,&headers);
            }
            with_cookie(reply(StatusCode::OK,json!({"access_token":token,"remembered":true})),&next,expires-now,&headers)
        },
        Err(_)=>with_cookie(denied(),"",0,&headers),
    }
}

pub(super) async fn logout(State(state):State<Arc<AppState>>,headers:HeaderMap)->Response {
    if !csrf(&headers) {return reply(StatusCode::FORBIDDEN,json!({"error":"請從已設定的管理介面操作。"}));}
    if let Some(secret)=cookie(&headers) {if state.user_db.remember_logout(&secret).is_err(){return denied();}}
    with_cookie(reply(StatusCode::OK,json!({"ok":true})),"",0,&headers)
}
fn claims(state:&AppState,headers:&HeaderMap)->Option<duduclaw_auth::jwt::Claims> {
    let token=extract_bearer_token(headers)?;
    authenticate_jwt(state,token).ok()?;
    state.jwt_config.verify_access_token(token).ok()
}
pub(super) async fn devices(State(state):State<Arc<AppState>>,headers:HeaderMap)->Response {
    let Some(claims)=claims(&state,&headers) else{return denied()};
    match state.user_db.remember_devices(&claims.sub,chrono::Utc::now().timestamp()) {
        Ok(devices)=>{
            let devices=devices.into_iter().map(|d| {let current=claims.jti.as_deref()==Some(d.id.as_str());let mut d=json!(d);d["current"]=json!(current);d}).collect::<Vec<_>>();
            reply(StatusCode::OK,json!({"devices":devices}))
        },Err(_)=>denied(),
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevokeBody {id:Option<String>,#[serde(default)]all:bool}
pub(super) async fn revoke(State(state):State<Arc<AppState>>,headers:HeaderMap,Json(body):Json<RevokeBody>)->Response {
    let Some(claims)=claims(&state,&headers) else{return denied()};
    if body.all==body.id.is_some(){return reply(StatusCode::BAD_REQUEST,json!({"error":"請選擇要登出的裝置。"}));}
    match state.user_db.remember_revoke(&claims.sub,body.id.as_deref()) {
        Ok(())=>{
            let response=reply(StatusCode::OK,json!({"ok":true}));
            if body.all || body.id==claims.jti {with_cookie(response,"",0,&headers)}else{response}
        },Err(_)=>denied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn account_disable_remove_and_admin_logout_revoke_refresh_access_and_socket_gates() {
        use crate::handlers::live_context::SocketValidity;
        for action in ["disable", "offboard", "delete", "admin_logout", "self_logout"] {
            let (_dir,state,admin)=fixture().await;
            state.handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
            let user=state.user_db.create_user("target@example.test","Target","test-password",duduclaw_auth::UserRole::Employee).unwrap();
            let now=chrono::Utc::now().timestamp();
            let ordinary=state.jwt_config.issue_access_token(&user,&[]).unwrap();
            let (remembered,secret,_)=state.user_db.remember_create(&user.id,0,&uuid::Uuid::new_v4().to_string(),"Browser",None,180,now,false,
                |user,bindings,session|state.jwt_config.issue_access_token_in_session(user,bindings,Some(session))).unwrap();
            let ordinary_ctx=authenticate_jwt(&state,&ordinary).unwrap();
            let remembered_ctx=authenticate_jwt(&state,&remembered).unwrap();
            assert_eq!(server_mfa::socket_validity(&state,&ordinary_ctx,Some((now+3600) as u64),false,true).await,SocketValidity::Valid);
            match action {
                "disable" => state.user_db.set_user_status(&user.id,duduclaw_auth::UserStatus::Suspended).unwrap(),
                "offboard" => { state.user_db.offboard_user(&user.id,None,Some(&admin.id)).unwrap(); },
                "delete" => { rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap().execute("DELETE FROM users WHERE id=?1",[&user.id]).unwrap(); },
                "self_logout" => {
                    let mut headers=HeaderMap::new();
                    headers.insert(header::AUTHORIZATION,HeaderValue::from_str(&format!("Bearer {remembered}")).unwrap());
                    let response=revoke(State(state.clone()),headers,Json(RevokeBody{id:None,all:true})).await;
                    assert_eq!(response.status(),StatusCode::OK);
                    assert_eq!(value(response).await["ok"],true);
                }
                _ => {
                    let mut ctx=UserContext::admin_fallback();ctx.user_id=admin.id.clone();
                    assert!(matches!(state.handler.handle("users.revoke_sessions",json!({"user_id":user.id}),&ctx).await,WsFrame::Response{ok:true,..}));
                }
            }
            for (token,ctx) in [(&ordinary,&ordinary_ctx),(&remembered,&remembered_ctx)] {
                assert!(authenticate_jwt(&state,token).is_err(),"{action}");
                assert_eq!(server_mfa::socket_validity(&state,ctx,Some((now+3600) as u64),false,true).await,SocketValidity::Revoked,"{action}");
            }
            let mut headers=HeaderMap::new();
            headers.insert(header::ORIGIN,HeaderValue::from_static("http://localhost:18889"));
            headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));
            headers.insert(header::COOKIE,HeaderValue::from_str(&format!("{COOKIE}={secret}")).unwrap());
            assert_eq!(refresh(State(state.clone()),ConnectInfo("192.0.2.247:12000".parse().unwrap()),headers).await.status(),StatusCode::UNAUTHORIZED,"{action}");
        }
    }

    #[tokio::test]
    async fn admin_session_rpc_rejects_non_admin_unknown_target_and_invalid_policy() {
        let (_dir,state,admin)=fixture().await;
        state.handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
        let user=state.user_db.create_user("employee@example.test","Employee","test-password",duduclaw_auth::UserRole::Employee).unwrap();
        let token=state.jwt_config.issue_access_token(&user,&[]).unwrap();
        let employee=authenticate_jwt(&state,&token).unwrap();
        for (method,params) in [("sessions.policy",json!({})),("sessions.policy_update",json!({"days":365})),("users.revoke_sessions",json!({"user_id":admin.id}))] {
            assert!(matches!(state.handler.handle(method,params,&employee).await,WsFrame::Response{ok:false,..}));
        }
        let mut ctx=UserContext::admin_fallback();ctx.user_id=admin.id.clone();
        for days in [json!(0),json!(366),json!(-1),json!(1.5),json!("180"),Value::Null] {
            assert!(matches!(state.handler.handle("sessions.policy_update",json!({"days":days}),&ctx).await,WsFrame::Response{ok:false,..}));
        }
        assert!(matches!(state.handler.handle("users.revoke_sessions",json!({"user_id":"missing"}),&ctx).await,WsFrame::Response{ok:false,..}));
        assert!(matches!(state.handler.handle("sessions.policy_update",json!({"days":1}),&ctx).await,WsFrame::Response{ok:true,..}));
        assert_eq!(state.user_db.remember_days(),1);
        assert!(matches!(state.handler.handle("sessions.policy",json!({}),&ctx).await,WsFrame::Response{ok:true,payload:Some(value),..} if value["days"]==1));
        assert!(matches!(state.handler.handle("sessions.unknown",json!({}),&ctx).await,WsFrame::Response{ok:false,..}));
    }

    #[tokio::test]
    async fn http_refresh_uses_new_policy_for_sliding_cookie_and_family_expiry() {
        let (_dir,state,user)=fixture().await;
        let now=chrono::Utc::now().timestamp();
        let (family,secret,_)=state.user_db.remember_create(&user.id,0,&uuid::Uuid::new_v4().to_string(),"Browser",None,180,now-600,false,|_,_,id|Ok(id.to_owned())).unwrap();
        state.user_db.remember_admin_policy(&user.id,0,Some(1)).unwrap();
        let mut headers=HeaderMap::new();
        headers.insert(header::ORIGIN,HeaderValue::from_static("http://localhost:18889"));
        headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));
        headers.insert(header::COOKIE,HeaderValue::from_str(&format!("{COOKIE}={secret}")).unwrap());
        let response=refresh(State(state.clone()),ConnectInfo("192.0.2.248:12000".parse().unwrap()),headers).await;
        assert_eq!(response.status(),StatusCode::OK);
        assert!(response.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=86400"));
        let device=state.user_db.remember_devices(&user.id,now).unwrap().remove(0);
        assert_eq!(device.id,family);
        assert_eq!(device.expires_at,device.last_used_at+86400);
        assert!(device.last_used_at>=now);
    }
    async fn fixture() -> (tempfile::TempDir, Arc<AppState>, duduclaw_auth::User) {
        fixture_with_config("[auth]\nmfa_enforced=false\n").await
    }
    async fn fixture_with_config(config: &str) -> (tempfile::TempDir, Arc<AppState>, duduclaw_auth::User) {
        let dir=tempfile::tempdir().unwrap();
        let home=dir.path().to_path_buf();
        std::fs::write(home.join("config.toml"),config).unwrap();
        let db=Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let user=db.create_user("remember-http@example.test","Remember","test-password",duduclaw_auth::UserRole::Admin).unwrap();
        let jwt=Arc::new(JwtConfig::new(b"remember-http-test-secret-at-least-32-bytes"));
        let handler=MethodHandler::new(home.clone()).await;
        handler.set_user_db(db.clone(),jwt.clone()).await;
        handler.set_edition_override(Some(duduclaw_core::EditionProfile::Personal)).await;
        let (tx,_)=broadcast::channel(4);let (event_tx,_)=broadcast::channel(4);
        let state=Arc::new(AppState{auth:AuthManager::new(None),handler,tx,event_tx,prepared_events:Default::default(),user_db:db,jwt_config:jwt,
            otp_delivery:Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(),reqwest::Client::new())),home_dir:home});
        (dir,state,user)
    }
    async fn value(response:Response)->Value {
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(),1024*1024).await.unwrap()).unwrap()
    }
    #[tokio::test]
    async fn remembered_refresh_without_enterprise_policy_snapshot_does_not_rotate() {
        let (_dir,state,user)=fixture_with_config("[auth]\nmfa_enforced='invalid'\n").await;
        let now=chrono::Utc::now().timestamp();
        let family=uuid::Uuid::new_v4().to_string();
        let (_,secret,_)=state.user_db.remember_create(&user.id,0,&family,"Browser",None,180,now,false,|_,_,_|Ok(())).unwrap();
        state.handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
        assert!(crate::mfa::enforced(&state.home_dir,false).is_err());
        let mut headers=HeaderMap::new();
        headers.insert(header::ORIGIN,HeaderValue::from_static("http://localhost:18889"));
        headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));
        headers.insert(header::COOKIE,HeaderValue::from_str(&format!("{COOKIE}={secret}")).unwrap());
        let response=refresh(State(state.clone()),ConnectInfo("192.0.2.244:12000".parse().unwrap()),headers).await;
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
        assert!(value(response).await.get("access_token").is_none());
        // The original credential still works: refusal happened before rotation.
        assert!(state.user_db.remember_rotate(&secret,now,false,|_,_,_|Ok(())).is_ok());
    }
    #[tokio::test]
    async fn remembered_http_rotates_cookie_rejects_reuse_and_revokes_access() {
        let (_dir,state,user)=fixture().await;
        let (access,refresh_token)=state.jwt_config.issue_session_tokens(&user,&[]).unwrap();
        let mut headers=HeaderMap::new();headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));headers.insert(header::ORIGIN,HeaderValue::from_static("http://localhost:18889"));
        let peer:SocketAddr="192.0.2.241:12000".parse().unwrap();
        let response=remember(State(state.clone()),ConnectInfo(peer),headers.clone(),Json(RememberBody{access_token:access,refresh_token})).await;
        assert_eq!(response.status(),StatusCode::OK);
        let cookie_header=response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
        let body=value(response).await;
        assert_eq!(body["remembered"],true);assert!(body.get("refresh_token").is_none());
        let access=body["access_token"].as_str().unwrap().to_owned();
        assert!(authenticate_jwt(&state,&access).is_ok());
        headers.insert(header::COOKIE,HeaderValue::from_str(&cookie_header).unwrap());
        let response=refresh(State(state.clone()),ConnectInfo(peer),headers.clone()).await;
        assert_eq!(response.status(),StatusCode::OK);
        assert_ne!(response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap(),cookie_header);
        assert_eq!(refresh(State(state.clone()),ConnectInfo(peer),headers).await.status(),StatusCode::UNAUTHORIZED);
        assert!(authenticate_jwt(&state,&access).is_err());
    }
    #[tokio::test]
    async fn remembered_http_rejects_missing_origin_and_mismatched_login_pair() {
        let (_dir,state,user)=fixture().await;
        let (access,refresh_token)=state.jwt_config.issue_session_tokens(&user,&[]).unwrap();
        let peer:SocketAddr="192.0.2.242:12000".parse().unwrap();
        assert_eq!(remember(State(state.clone()),ConnectInfo(peer),HeaderMap::new(),Json(RememberBody{access_token:access.clone(),refresh_token})).await.status(),StatusCode::FORBIDDEN);
        let (_,other)=state.jwt_config.issue_session_tokens(&user,&[]).unwrap();
        let mut headers=HeaderMap::new();headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));headers.insert(header::ORIGIN,HeaderValue::from_static("http://localhost:18889"));
        assert_eq!(remember(State(state.clone()),ConnectInfo(peer),headers,Json(RememberBody{access_token:access,refresh_token:other})).await.status(),StatusCode::UNAUTHORIZED);
        assert!(state.user_db.remember_devices(&user.id,chrono::Utc::now().timestamp()).unwrap().is_empty());
    }
    #[tokio::test]
    async fn remembered_http_rejects_cross_port_and_simple_form_without_revoking() {
        let (_dir,state,user)=fixture().await;
        let now=chrono::Utc::now().timestamp();
        let family=uuid::Uuid::new_v4().to_string();
        let (_,secret,_)=state.user_db.remember_create(&user.id,0,&family,"Browser",None,180,now,false,|_,_,_|Ok(())).unwrap();
        let peer:SocketAddr="192.0.2.243:12000".parse().unwrap();
        for (origin,csrf_header) in [("http://localhost:18890",true),("http://localhost:18890",false),("http://localhost:18889",false),("https://localhost:18889",true)] {
            let mut headers=HeaderMap::new();
            headers.insert(header::ORIGIN,HeaderValue::from_str(origin).unwrap());
            headers.insert(header::COOKIE,HeaderValue::from_str(&format!("{COOKIE}={secret}")).unwrap());
            headers.insert(header::CONTENT_TYPE,HeaderValue::from_static("application/x-www-form-urlencoded"));
            if csrf_header {headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));}
            assert_eq!(refresh(State(state.clone()),ConnectInfo(peer),headers.clone()).await.status(),StatusCode::FORBIDDEN);
            assert_eq!(logout(State(state.clone()),headers).await.status(),StatusCode::FORBIDDEN);
            assert!(state.user_db.remember_session_valid(Some(&family),&user.id,now).unwrap());
        }
        let mut headers=HeaderMap::new();
        headers.insert(header::ORIGIN,HeaderValue::from_static("http://localhost:18889"));
        headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));
        headers.insert(header::COOKIE,HeaderValue::from_str(&format!("{COOKIE}={secret}")).unwrap());
        assert_eq!(logout(State(state.clone()),headers).await.status(),StatusCode::OK);
        assert!(!state.user_db.remember_session_valid(Some(&family),&user.id,now).unwrap());
    }
    #[test]
    fn remembered_origin_matches_scheme_host_and_effective_port_exactly() {
        let allowed=vec!["https://dashboard.example:8443".to_owned()];
        for (origin,expected) in [("https://dashboard.example:8443",true),("http://dashboard.example:8443",false),("https://dashboard.example",false),("https://dashboard.example:8444",false),("https://dashboard.example.evil:8443",false),("https://user@dashboard.example:8443",false),("https://dashboard.example:8443/path",false),("http://localhost:18889",true),("http://localhost:18890",false)] {
            let mut headers=HeaderMap::new();
            headers.insert(header::ORIGIN,HeaderValue::from_str(origin).unwrap());
            headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));
            assert_eq!(csrf_with(&headers,&allowed,18889),expected,"{origin}");
            headers.append("x-duduclaw-csrf",HeaderValue::from_static("1"));
            assert!(!csrf_with(&headers,&allowed,18889));
        }
    }
    #[tokio::test]
    async fn remembered_file_download_gate_rechecks_family_and_epoch() {
        let (_dir,state,user)=fixture().await;
        let now=chrono::Utc::now().timestamp();
        let session=uuid::Uuid::new_v4().to_string();
        let (token,_,_)=state.user_db.remember_create(&user.id,0,&session,"Browser",None,180,now,false,
            |user,access,id|state.jwt_config.issue_access_token_in_session(user,access,Some(id))).unwrap();
        assert!(authorize_file_access(&state,&HeaderMap::new(),Some(&token),None).is_ok());
        state.user_db.remember_revoke(&user.id,Some(&session)).unwrap();
        assert_eq!(authorize_file_access(&state,&HeaderMap::new(),Some(&token),None).unwrap_err().status(),StatusCode::UNAUTHORIZED);
        let ordinary=state.jwt_config.issue_access_token(&user,&[]).unwrap();
        assert!(authorize_file_access(&state,&HeaderMap::new(),Some(&ordinary),None).is_ok());
        state.user_db.mfa_operator_reset(&user.email).unwrap();
        assert_eq!(authorize_file_access(&state,&HeaderMap::new(),Some(&ordinary),None).unwrap_err().status(),StatusCode::UNAUTHORIZED);
    }
    #[test]
    fn remembered_cookie_flags_scope_and_duplicate_rejection() {
        let mut headers=HeaderMap::new();
        headers.insert(header::ORIGIN,HeaderValue::from_static("https://localhost:18789"));
        let response=with_cookie(reply(StatusCode::OK,json!({})),&"a".repeat(64),180*86400,&headers);
        let value=response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(value.contains("HttpOnly; SameSite=Strict; Max-Age=15552000; Secure"));
        assert!(value.contains("Path=/api/session;"));
        headers.insert(header::COOKIE,HeaderValue::from_str(&format!("{COOKIE}={}; {COOKIE}={}","a".repeat(64),"b".repeat(64))).unwrap());
        assert!(cookie(&headers).is_none());
    }
    #[test]
    fn remembered_cookie_requires_explicit_allowed_origin() {
        let mut headers=HeaderMap::new();
        assert!(!csrf(&headers));
        for value in ["null","http://localhost.evil.com","invalid"] {
            headers.insert(header::ORIGIN,HeaderValue::from_str(value).unwrap());assert!(!csrf(&headers));
        }
        headers.insert(header::ORIGIN,HeaderValue::from_static("http://localhost:18889"));
        headers.insert("x-duduclaw-csrf",HeaderValue::from_static("1"));assert!(csrf_with(&headers,&[],18889));
    }
    #[test]
    fn remembered_dashboard_socket_lifetime_bounds_legacy_expiry_exempt_sessions() {
        assert!(dashboard_socket_age_valid(true,86399));
        assert!(!dashboard_socket_age_valid(true,86400));
        assert!(!dashboard_socket_age_valid(true,u64::MAX));
        assert!(dashboard_socket_age_valid(false,86400));
    }

}
