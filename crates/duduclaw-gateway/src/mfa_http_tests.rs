//! In-process handler tests: no socket binding required.
use super::*;
use axum::http::StatusCode;
use duduclaw_auth::UserRole;
use serde_json::{Value, json};

async fn fixture(
    personal: bool,
    enforce: bool,
) -> (tempfile::TempDir, Arc<AppState>, duduclaw_auth::User) {
    fixture_policy(personal, Some(enforce)).await
}

async fn fixture_policy(
    personal: bool,
    policy: Option<bool>,
) -> (tempfile::TempDir, Arc<AppState>, duduclaw_auth::User) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    if let Some(enforce) = policy {
        std::fs::write(home.join("config.toml"), format!("[auth]\nmfa_enforced={enforce}\n")).unwrap();
    } else {
        crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
            &home, "127.0.0.1:18789".parse().unwrap(), personal, None,
        ).unwrap();
    }
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let mut user = if policy.is_none() {
        db.ensure_default_admin().unwrap().unwrap();
        db.get_bootstrap_owner().unwrap().unwrap()
    } else {
        db
        .create_user(
            &format!("mfa-{}@example.test", uuid::Uuid::new_v4()),
            "MFA",
            "correct horse battery",
            UserRole::Admin,
        )
        .unwrap()
    };
    let jwt = Arc::new(JwtConfig::new(b"mfa-http-test-secret-at-least-32-bytes"));
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_user_db(db.clone(), jwt.clone()).await;
    handler
        .set_edition_override(Some(if personal {
            duduclaw_core::EditionProfile::Personal
        } else {
            duduclaw_core::EditionProfile::Enterprise
        }))
        .await;
    let (tx, _) = broadcast::channel(4);
    let (event_tx, _) = broadcast::channel(4);
    let state = Arc::new(AppState {
        auth: AuthManager::new(None),
        handler,
        tx,
        event_tx,
        prepared_events: Default::default(),
        user_db: db,
        jwt_config: jwt,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
            home.clone(),
            reqwest::Client::new(),
        )),
        home_dir: home,
    });
    if policy.is_none() {
        let response = handle_first_run_claim(State(state.clone()),
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12000))),
            Json(FirstRunClaimRequest {
                email: Some("first-admin@example.test".into()),
                password: "correct horse battery".into(),
            })).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        user = state.user_db.get_bootstrap_owner().unwrap().unwrap();
    }
    (dir, state, user)
}
fn peer(n: u8) -> SocketAddr {
    SocketAddr::from(([192, 0, 2, n], 12000))
}
async fn body(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn login(state: &Arc<AppState>, n: u8) -> (StatusCode, Value) {
    body(
        handle_login(
            State(state.clone()),
            ConnectInfo(peer(n)),
            Json(LoginRequest {
                email: state.user_db.list_users().unwrap().into_iter().find(|u| u.display_name == "MFA").unwrap().email,
                password: "correct horse battery".into(),
            }),
        )
        .await
        .into_response(),
    )
    .await
}
fn totp(secret: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut bytes = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in secret.bytes() {
        let v = if c.is_ascii_uppercase() {
            c - b'A'
        } else {
            c - b'2' + 26
        };
        acc = (acc << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
        }
    }
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&bytes).unwrap();
    mac.update(&((chrono::Utc::now().timestamp() / 30) as u64).to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[19] & 15) as usize;
    let value = u32::from_be_bytes(hash[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    format!("{:06}", value % 1_000_000)
}
fn enroll(state: &AppState, user: &duduclaw_auth::User) -> Vec<String> {
    let now = chrono::Utc::now().timestamp();
    let cipher = Cipher(&state.home_dir);
    let token=state.user_db.mfa_password_challenge(&user.id,"correct horse battery","test-session",now).unwrap();
    state.user_db.mfa_prepare_enrollment(&token,false,now).unwrap();
    let secret = state
        .user_db
        .mfa_begin(&user.id, Some(&token), &cipher, now)
        .unwrap();
    state
        .user_db
        .mfa_confirm(&user.id, Some(&token), &totp(&secret), &cipher, now)
        .unwrap()
}
#[tokio::test]
async fn mfa_password_and_channel_otp_return_no_token_in_entire_body() {
    let (_dir, state, user) = fixture(false, false).await;
    let codes = enroll(&state, &user);
    let (status, response) = login(&state, 180).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["factors"]["totp"], true);
    assert_eq!(response["last_used"], "totp");
    assert_eq!(response["mfa_required"], true);
    assert!(!response.to_string().contains("token"));
    let id = response["challenge_id"].as_str().unwrap().to_owned();
    let (_, success) = body(
        verify(
            State(state.clone()),
            ConnectInfo(peer(180)),
            Json(VerifyBody {
                challenge_id: id.clone(),
                code: None,
                recovery_code: Some(codes[0].clone()),
            }),
        )
        .await,
    )
    .await;
    assert!(success["access_token"].is_string());
    assert!(
        state
            .jwt_config
            .verify_access_token(success["access_token"].as_str().unwrap())
            .is_ok()
    );
    assert_eq!(
        body(
            verify(
                State(state.clone()),
                ConnectInfo(peer(180)),
                Json(VerifyBody {
                    challenge_id: id,
                    code: None,
                    recovery_code: Some(codes[1].clone())
                })
            )
            .await
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    state
        .user_db
        .bind_channel_identity(&user.id, "telegram", "12345", true)
        .unwrap();
    let otp = state.user_db.request_otp(&user.email).unwrap().unwrap();
    let (status, response) = body(
        handle_otp_verify(
            State(state.clone()),
            ConnectInfo(peer(181)),
            Json(OtpVerifyBody {
                challenge_id: otp.challenge_id,
                code: otp.code,
            }),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["challenge_id", "email_available", "email_unavailable_reason", "factors", "last_used", "mfa_required"]);
    assert_eq!(response["factors"], json!({"totp":true,"email":false,"passkey":false}));
    assert_eq!(response["last_used"], "totp");
    assert_eq!(response["email_available"], false);
    assert_eq!(response["email_unavailable_reason"], "no_smtp_no_relay");
    assert_eq!(response["mfa_required"], true);
    assert!(!response.to_string().contains("token"));
}
#[tokio::test]
async fn mfa_enforcement_off_preserves_login_response_for_every_edition() {
    for (personal, enforce, n) in [(false, false, 182), (true, false, 183)] {
        let (_dir, state, user) = fixture(personal, enforce).await;
        let (status, response) = login(&state, n).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            response
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["access_token", "refresh_token", "user"]
        );
        assert!(response["access_token"].is_string());
        assert!(response["refresh_token"].is_string());
        assert_eq!(response["user"]["email"], user.email);
        assert!(response["user"].get("credential_epoch").is_none());
        let claims=state.jwt_config.verify_access_token(response["access_token"].as_str().unwrap()).unwrap();
        assert!(serde_json::to_value(claims).unwrap().get("credential_epoch").is_none());
    }
}
#[tokio::test]
async fn mfa_enrollment_capability_cannot_authenticate_ws_me_or_refresh() {
    let (_dir, state, user) = fixture(false, true).await;
    let old = state.jwt_config.issue_session_tokens(&user, &[]).unwrap();
    let (_, response) = login(&state, 184).await;
    assert_eq!(response.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["challenge_ref", "email_available", "email_unavailable_reason", "enrollment_token", "mfa_enrollment_required"]);
    assert_eq!(response["email_available"], false);
    assert_eq!(response["email_unavailable_reason"], "no_smtp_no_relay");
    assert_eq!(response["mfa_enrollment_required"], true);
    assert!(response.get("access_token").is_none() && response.get("refresh_token").is_none());
    let token = response["enrollment_token"].as_str().unwrap().to_owned();
    // This is the exact JWT verifier used by the WS connect frame.
    assert!(authenticate_jwt(&state, &token).is_err());
    assert!(state.jwt_config.verify_refresh_token(&token).is_err());
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    assert_eq!(
        handle_me(State(state.clone()), headers)
            .await
            .into_response()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(authenticate_jwt(&state, &old.0).is_ok());
    assert!(
        !socket_expired(
            &state,
            &authenticate_jwt(&state, &old.0).unwrap(),
            Some(chrono::Utc::now().timestamp() as u64 + 1800), false
        )
        .await
    );
    assert!(socket_expired(&state, &authenticate_jwt(&state, &old.0).unwrap(), Some(1), false).await);
    assert_eq!(
        handle_refresh(
            State(state.clone()),
            ConnectInfo(peer(184)),
            Json(RefreshRequest {
                refresh_token: old.1
            })
        )
        .await
        .into_response()
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(enroll_begin(State(state.clone()),ConnectInfo(peer(184)),Json(EnrollBody {
        enrollment_token:token.clone(),code:None,proof_code:None,password:None,
    })).await.status(),StatusCode::UNAUTHORIZED);
    let proof=state.user_db.mfa_issue_enrollment_code(None,&user.id,&duduclaw_auth::mfa::enrollment_challenge_ref(&token),chrono::Utc::now().timestamp()).unwrap();
    let (_, start) = body(
        enroll_begin(
            State(state.clone()),
            ConnectInfo(peer(184)),
            Json(EnrollBody {
                password: None,
                proof_code: Some(proof),
                enrollment_token: token.clone(),
                code: None,
            }),
        )
        .await,
    )
    .await;
    assert!(!state.user_db.mfa_active(&user.id).unwrap());
    let code = totp(start["secret"].as_str().unwrap());
    let (status, done) = body(
        enroll_confirm(
            State(state.clone()),
            ConnectInfo(peer(184)),
            Json(EnrollBody {
                password: None,
                proof_code: None,
                enrollment_token: token.clone(),
                code: Some(code.clone()),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(done["recovery_codes"].as_array().unwrap().len(), 10);
    assert!(done["access_token"].is_string());
    let access = state.jwt_config.verify_access_token(done["access_token"].as_str().unwrap()).unwrap();
    let refresh = state.jwt_config.verify_refresh_token(done["refresh_token"].as_str().unwrap()).unwrap();
    let epoch = state.user_db.get_user(&user.id).unwrap().unwrap().credential_epoch;
    assert_eq!(access.credential_epoch, epoch);
    assert_eq!(refresh.credential_epoch, epoch);
    assert_eq!(access.jti, refresh.jti);
    assert!(authenticate_jwt(&state, done["access_token"].as_str().unwrap()).is_ok());
    assert!(state.user_db.mfa_active(&user.id).unwrap());
    assert_eq!(
        enroll_confirm(
            State(state.clone()),
            ConnectInfo(peer(184)),
            Json(EnrollBody {
                password: None,
                proof_code: None,
                enrollment_token: token,
                code: Some(code)
            })
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn mfa_unreadable_config_and_secret_deny() {
    let (_dir, state, user) = fixture(false, false).await;
    std::fs::remove_file(state.home_dir.join("config.toml")).unwrap();
    crate::mfa::reload_policy(&state.home_dir);
    assert_eq!(login(&state, 185).await.0, StatusCode::OK);
    std::fs::write(
        state.home_dir.join("config.toml"),
        "[auth]\nmfa_enforced=false",
    )
    .unwrap();
    let codes = enroll(&state, &user);
    let (_, response) = login(&state, 185).await;
    std::fs::remove_file(state.home_dir.join(".keyfile")).unwrap();
    let response = verify(
        State(state.clone()),
        ConnectInfo(peer(185)),
        Json(VerifyBody {
            challenge_id: response["challenge_id"].as_str().unwrap().into(),
            code: None,
            recovery_code: Some(codes[0].clone()),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(state.user_db.mfa_active(&user.id).unwrap());
}
#[tokio::test]
async fn mfa_rpc_permissions_fresh_admin_and_unknown_method_denied() {
    let (_dir, state, user) = fixture(false, false).await;
    enroll(&state, &user);
    let employee = state
        .user_db
        .create_user(
            "employee@example.test",
            "Employee",
            "password-test",
            UserRole::Employee,
        )
        .unwrap();
    let mut forged = UserContext::admin_fallback();
    forged.user_id = employee.id;
    assert!(matches!(
        state
            .handler
            .handle("users.mfa_reset", json!({"user_id":user.id}), &forged)
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(matches!(
        state
            .handler
            .handle("mfa.overview", json!({}), &forged)
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(matches!(
        state
            .handler
            .handle("mfa.unknown", json!({}), &forged)
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(matches!(
        state
            .handler
            .handle(
                "system.update_config",
                json!({"auth.mfa_enforced":true,"expected_state":false}),
                &forged
            )
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(!enforced(&state.home_dir, false).unwrap());
    let mut admin = UserContext::admin_fallback();
    admin.user_id = user.id.clone();
    admin.credential_epoch = state.user_db.get_user(&user.id).unwrap().unwrap().credential_epoch;
    assert!(matches!(
        state
            .handler
            .handle("users.mfa_reset", json!({"user_id":user.id}), &admin)
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(state.user_db.mfa_active(&user.id).unwrap());
    assert!(matches!(
        state
            .handler
            .handle(
                "system.update_config",
                json!({"auth.mfa_enforced":true,"expected_state":false}),
                &admin
            )
            .await,
        crate::protocol::WsFrame::Response { ok: true, .. }
    ));
    assert!(enforced(&state.home_dir, false).unwrap());
}

#[tokio::test]
async fn mfa_old_access_refresh_revoked_but_epoch_zero_compatible() {
    let (_dir,state,user)=fixture(false,false).await;
    let (_,old)=login(&state,186).await;
    let refresh=old["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(state.jwt_config.verify_refresh_token(&refresh).unwrap().credential_epoch,0);
    assert!(authenticate_jwt(&state,old["access_token"].as_str().unwrap()).is_ok());
    assert_eq!(handle_refresh(State(state.clone()),ConnectInfo(peer(186)),Json(RefreshRequest{refresh_token:refresh.clone()})).await.into_response().status(),StatusCode::OK);
    enroll(&state,&user);
    assert!(authenticate_jwt(&state,old["access_token"].as_str().unwrap()).is_err());
    assert_eq!(handle_refresh(State(state.clone()),ConnectInfo(peer(186)),Json(RefreshRequest{refresh_token:refresh})).await.into_response().status(),StatusCode::UNAUTHORIZED);
    state.user_db.mfa_operator_reset(&user.email).unwrap();
    assert!(session(&state,&user.id,1,None).is_err(),"a reset after factor verification cannot mint the new epoch");
}
#[tokio::test]
async fn mfa_legacy_token_only_refused_when_enforced() {
    for (personal,required,allowed) in [(false,false,true),(false,true,false),(true,false,true),(true,true,false)] {
        let (_dir,state,_)=fixture(personal,required).await;
        assert_eq!(legacy_token_allowed(&state).await,allowed);
        let mut conn = crate::power_local::RpcConnInfo::from_ws(peer(213), false);
        conn.legacy_admin_authenticated = true;
        let response = state.handler.handle_conn("system.status", json!({}), &UserContext::admin_fallback(), conn).await;
        assert_eq!(matches!(response, WsFrame::Response { ok: true, .. }), allowed, "{response:?}");
    }
}
#[tokio::test]
async fn mfa_stolen_jwt_cannot_begin_without_password_or_confirm_other_session() {
    let (_dir,state,user)=fixture(false,false).await;
    let ctx=state.user_db.live_user_context(&user.id).unwrap().unwrap();
    let session=uuid::Uuid::new_v4().to_string();
    let call=|method,params|crate::google_personal::DASHBOARD_SESSION.scope(Some((user.id.clone(),session.clone())),state.handler.handle(method,params,&ctx));
    assert!(matches!(call("mfa.enroll_begin",json!({})).await,crate::protocol::WsFrame::Response{ok:false,..}));
    assert!(matches!(call("mfa.enroll_begin",json!({"password":"wrong"})).await,crate::protocol::WsFrame::Response{ok:false,..}));
    let response=call("mfa.enroll_begin",json!({"password":"correct horse battery"})).await;
    let crate::protocol::WsFrame::Response{ok:true,payload:Some(payload),..}=response else { panic!("begin failed: {response:?}") };
    let params=json!({"enrollment_token":payload["enrollment_token"],"code":totp(payload["secret"].as_str().unwrap())});
    assert!(matches!(crate::google_personal::DASHBOARD_SESSION.scope(Some((user.id.clone(),uuid::Uuid::new_v4().to_string())),state.handler.handle("mfa.enroll_confirm",params.clone(),&ctx)).await,crate::protocol::WsFrame::Response{ok:false,..}));
    assert!(matches!(call("mfa.enroll_confirm",params).await,crate::protocol::WsFrame::Response{ok:true,..}));
}

#[tokio::test]
async fn mfa_heartbeat_retires_legacy_and_epoch_but_preserves_default_off() {
    let (_dir,state,user)=fixture(false,false).await;
    let token=state.jwt_config.issue_access_token(&user,&[]).unwrap();
    let ctx=authenticate_jwt(&state,&token).unwrap();
    assert!(!socket_expired(&state,&ctx,Some(1),false).await);
    assert!(!socket_expired(&state,&UserContext::admin_fallback(),None,true).await);
    std::fs::write(state.home_dir.join("config.toml"),"[auth]\nmfa_enforced=true").unwrap();
    crate::mfa::reload_policy(&state.home_dir);
    assert!(socket_expired(&state,&UserContext::admin_fallback(),None,true).await);
    assert!(!socket_expired(&state,&ctx,Some(u64::MAX),false).await);
    assert!(socket_expired(&state,&ctx,Some(1),false).await);
    state.user_db.mfa_operator_reset(&user.email).unwrap();
    assert!(socket_expired(&state,&ctx,Some(u64::MAX),false).await);
    let fresh=state.user_db.get_user(&user.id).unwrap().unwrap();
    let token=state.jwt_config.issue_access_token(&fresh,&[]).unwrap();
    let mut ctx=authenticate_jwt(&state,&token).unwrap();
    ctx.credential_epoch+=1;
    assert!(socket_expired(&state,&ctx,Some(u64::MAX),false).await);
    let db=rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap();
    db.execute("DROP TABLE users",[]).unwrap();
    assert!(socket_expired(&state,&ctx,Some(u64::MAX),false).await);
}

#[tokio::test]
async fn mfa_degraded_is_visible_in_overview_and_system_status() {
    let (_dir,state,user)=fixture(false,false).await;
    let ctx=authenticate_jwt(&state,&state.jwt_config.issue_access_token(&user,&[]).unwrap()).unwrap();
    let normal=state.handler.system_status_payload().await;
    assert!(normal.get("mfa_policy").is_none());
    std::fs::write(state.home_dir.join("config.toml"),"[auth").unwrap();
    crate::mfa::reload_policy(&state.home_dir);
    assert!(!crate::mfa::enforced(&state.home_dir,false).unwrap());
    let status=state.handler.system_status_payload().await;
    assert_eq!(status["mfa_policy"]["degraded"],true);
    let overview=state.handler.handle_mfa_rpc("mfa.overview",json!({}),&ctx).await;
    let value=serde_json::to_value(overview).unwrap();
    assert_eq!(value["payload"]["policy"]["degraded"],true);
    assert!(value["payload"]["users"][0].get("channel_bindings").is_some());
}

#[tokio::test]
async fn mfa_channel_bind_refuses_enforced_self_but_accepts_other_admin() {
    for personal in [false, true] {
        let (_dir,state,user)=fixture(personal,true).await;
        let headers_for=|u:&duduclaw_auth::User| {
            let mut headers=axum::http::HeaderMap::new();
            let token=state.jwt_config.issue_access_token(u,&[]).unwrap();
            headers.insert("authorization",format!("Bearer {token}").parse().unwrap());
            headers
        };
        let binding=||ChannelBindBody {user_id:user.id.clone(),channel:"telegram".into(),channel_user_id:"12345".into()};
        let response=handle_channel_bind(State(state.clone()),headers_for(&user),Json(binding())).await.into_response();
        assert_eq!(response.status(),StatusCode::CONFLICT);
        let other=state.user_db.create_user("other@example.test","Other","password-test",UserRole::Admin).unwrap();
        let response=handle_channel_bind(State(state.clone()),headers_for(&other),Json(binding())).await.into_response();
        assert_eq!(response.status(),StatusCode::OK);
        let now=chrono::Utc::now().timestamp();
        let challenge=state.user_db.mfa_challenge(&user.id,duduclaw_auth::mfa::FirstFactor::Password,true,now).unwrap();
        assert!(state.user_db.mfa_prepare_enrollment(&challenge,true,now).unwrap().is_none());
    }
}

#[tokio::test]
async fn mfa_live_socket_read_failure_pauses_recovers_and_retires_queued_output() {
    use crate::handlers::live_context::{SocketRecovery, SocketValidity};
    let (_dir, state, user) = fixture(false, false).await;
    let token = state.jwt_config.issue_access_token(&user, &[]).unwrap();
    let ctx = authenticate_jwt(&state, &token).unwrap();
    let check = |heartbeat| socket_validity(&state, &ctx, Some(u64::MAX), false, heartbeat);
    let mut recovery = SocketRecovery::default();
    assert!(recovery.observe(check(true).await, true));
    let raw = rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap();
    raw.execute("ALTER TABLE auth_meta RENAME TO saved_auth_meta", []).unwrap();
    assert_eq!(check(true).await, SocketValidity::Unavailable);
    assert!(!recovery.observe(check(true).await, true));
    assert!(!recovery.closed());
    // Admission and sensitive output use the same paused gate, without
    // counting additional messages as failed heartbeats.
    for _ in 0..4 { assert!(!recovery.observe(check(false).await, false)); }
    raw.execute("ALTER TABLE users RENAME TO saved_users", []).unwrap();
    assert!(authenticate_jwt(&state, &token).is_err());
    raw.execute("ALTER TABLE saved_users RENAME TO users", []).unwrap();
    raw.execute("ALTER TABLE saved_auth_meta RENAME TO auth_meta", []).unwrap();
    assert!(recovery.observe(check(true).await, true));
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    tx.send("queued RPC response").await.unwrap();
    tx.send("queued OS event").await.unwrap();
    state.user_db.mfa_operator_reset(&user.email).unwrap();
    assert_eq!(check(true).await, SocketValidity::Revoked);
    let mut sent = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        if recovery.observe(check(false).await, false) { sent.push(frame); }
    }
    assert!(sent.is_empty());
    assert!(recovery.closed());
}

#[tokio::test]
async fn mfa_rpc_enroll_confirm_and_disable_handover_preserves_only_calling_connection() {
    use crate::handlers::live_context::SocketValidity;
    use crate::power_local::RpcConnInfo;
    let (_dir, state, user) = fixture(false, false).await;
    let (access, mut refresh) = state.jwt_config.issue_session_tokens(&user, &[]).unwrap();
    let claims = state.jwt_config.verify_access_token(&access).unwrap();
    let mut ctx = authenticate_jwt(&state, &access).unwrap();
    let mut expires = Some(claims.exp);
    let mut dashboard_session = Some((user.id.clone(), claims.jti.unwrap()));
    // A different login of the same user must never inherit the handover.
    let other_access = state.jwt_config.issue_access_token(&user, &[]).unwrap();
    let other_ctx = authenticate_jwt(&state, &other_access).unwrap();
    let begin = crate::google_personal::DASHBOARD_SESSION.scope(
        dashboard_session.clone(), state.handler.handle("mfa.enroll_begin",
            json!({"password":"correct horse battery"}), &ctx),
    ).await;
    let WsFrame::Response { ok: true, payload: Some(begin), .. } = begin else { panic!("enrollment begin failed") };
    let params = json!({"enrollment_token":begin["enrollment_token"],"code":totp(begin["secret"].as_str().unwrap())});
    let mut codes = Vec::new();
    for (method, expected_epoch, n) in [("mfa.enroll_confirm", 1, 190), ("mfa.disable", 2, 191)] {
        let old_ctx = ctx.clone();
        let old_session = dashboard_session.clone();
        // Prime the old-epoch cache. Handover must replace it even before its
        // two-second expiry; heartbeats force a read after expiry as well.
        assert_eq!(socket_validity(&state, &ctx, expires, false, true).await, SocketValidity::Valid);
        let request = if expected_epoch == 1 { params.clone() } else { json!({"code":codes[0],"password":"correct horse battery"}) };
        let result = crate::mfa::ACCESS_EXPIRY.scope(expires,
            crate::google_personal::DASHBOARD_SESSION.scope(dashboard_session.clone(),
                call_with_handover(&state, method, request, &ctx, RpcConnInfo::internal()),
            ),
        ).await;
        let WsFrame::Response { ok: true, payload: Some(payload), .. } = result.response else { panic!("MFA transition failed") };
        let handover = result.handover.expect("successful transition must carry explicit handover");
        handover.apply(&mut ctx, &mut dashboard_session, &mut expires);
        assert_eq!(ctx.credential_epoch, expected_epoch);
        assert_ne!(dashboard_session, old_session);
        let claims = state.jwt_config.verify_access_token(payload["access_token"].as_str().unwrap()).unwrap();
        assert_eq!(claims.credential_epoch, expected_epoch);
        assert_eq!(dashboard_session.as_ref().unwrap().1, claims.jti.unwrap());
        assert_eq!(expires, Some(claims.exp));
        // The exact forced send gate used by the socket, the ordinary send
        // gate, and the next heartbeat all accept the rebound connection.
        for heartbeat in [true, false, true] {
            assert_eq!(socket_validity(&state, &ctx, expires, false, heartbeat).await, SocketValidity::Valid);
        }
        assert_eq!(socket_validity(&state, &old_ctx, expires, false, true).await, SocketValidity::Revoked);
        assert_eq!(socket_validity(&state, &other_ctx, expires, false, true).await, SocketValidity::Revoked);
        assert_eq!(handle_refresh(State(state.clone()), ConnectInfo(peer(n)),
            Json(RefreshRequest { refresh_token: refresh })).await.into_response().status(), StatusCode::UNAUTHORIZED);
        refresh = payload["refresh_token"].as_str().unwrap().to_owned();
        assert_eq!(handle_refresh(State(state.clone()), ConnectInfo(peer(n)),
            Json(RefreshRequest { refresh_token: refresh.clone() })).await.into_response().status(), StatusCode::OK);
        assert_eq!(state.user_db.mfa_active(&user.id).unwrap(), expected_epoch == 1);
        if expected_epoch == 1 {
            codes = payload["recovery_codes"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect();
            assert_eq!(codes.len(), 10);
        }
    }
}

#[tokio::test]
async fn mfa_rpc_handover_is_success_only_and_cannot_bypass_later_revocation() {
    use crate::handlers::live_context::SocketValidity;
    use crate::power_local::RpcConnInfo;
    let (_dir, state, user) = fixture(false, false).await;
    let codes = enroll(&state, &user);
    let user = state.user_db.get_user(&user.id).unwrap().unwrap();
    let token = state.jwt_config.issue_access_token(&user, &[]).unwrap();
    let mut ctx = authenticate_jwt(&state, &token).unwrap();
    let mut session = None;
    let mut expires = Some(u64::MAX);
    let failed = call_with_handover(&state, "mfa.disable", json!({"code":"invalid","password":"correct horse battery"}), &ctx, RpcConnInfo::internal()).await;
    assert!(matches!(failed.response, WsFrame::Response { ok: false, .. }));
    assert!(failed.handover.is_none());
    assert_eq!(state.user_db.get_user(&user.id).unwrap().unwrap().credential_epoch, ctx.credential_epoch);
    let status = call_with_handover(&state, "mfa.status", json!({}), &ctx, RpcConnInfo::internal()).await;
    assert!(matches!(status.response, WsFrame::Response { ok: true, .. }));
    assert!(status.handover.is_none());
    let result = call_with_handover(&state, "mfa.disable", json!({"code":codes[0],"password":"correct horse battery"}), &ctx, RpcConnInfo::internal()).await;
    // A second revocation after commit must not be overwritten by handover.
    state.user_db.mfa_operator_reset(&user.email).unwrap();
    result.handover.unwrap().apply(&mut ctx, &mut session, &mut expires);
    assert_eq!(socket_validity(&state, &ctx, expires, false, true).await, SocketValidity::Revoked);
}

#[tokio::test]
async fn mfa_policy_switch_rejects_stale_or_missing_expected_state() {
    let (_dir, state, user) = fixture(false, false).await;
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = user.id.clone();
    for expected in [json!(null), json!(true), json!("false")] {
        let response = state.handler.handle("system.update_config",
            json!({"auth.mfa_enforced":true,"expected_state":expected}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: false, .. }));
        assert!(!enforced(&state.home_dir, false).unwrap());
    }
    let response = state.handler.handle("system.update_config",
        json!({"auth.mfa_enforced":true,"expected_state":false}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }));
    let response = state.handler.handle("system.update_config",
        json!({"auth.mfa_enforced":false,"expected_state":false}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: false, .. }));
    assert!(enforced(&state.home_dir, false).unwrap());
}

#[tokio::test]
async fn mfa_disable_requires_fresh_password_without_consuming_factor_on_failure() {
    let (_dir, state, user) = fixture(false, false).await;
    let codes = enroll(&state, &user);
    let user = state.user_db.get_user(&user.id).unwrap().unwrap();
    let token = state.jwt_config.issue_access_token(&user, &[]).unwrap();
    let ctx = authenticate_jwt(&state, &token).unwrap();
    for password in [json!(null), json!("incorrect")] {
        let response = state.handler.handle("mfa.disable",
            json!({"code":codes[0],"password":password}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: false, .. }));
        assert!(state.user_db.mfa_active(&user.id).unwrap());
        assert_eq!(state.user_db.get_user(&user.id).unwrap().unwrap().credential_epoch, user.credential_epoch);
    }
    let response = state.handler.handle("mfa.disable",
        json!({"code":codes[0],"password":"correct horse battery"}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }));
    assert!(!state.user_db.mfa_active(&user.id).unwrap());
}

#[tokio::test]
async fn mfa_disable_password_attempts_share_enrollment_rate_limit() {
    let (_dir, state, user) = fixture(false, false).await;
    let codes = enroll(&state, &user);
    let user = state.user_db.get_user(&user.id).unwrap().unwrap();
    let access = state.jwt_config.issue_access_token(&user, &[]).unwrap();
    let ctx = authenticate_jwt(&state, &access).unwrap();
    for _ in 0..5 {
        let response = state.handler.handle("mfa.disable", json!({"code":codes[0]}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: false, .. }));
    }
    for method in ["mfa.disable", "mfa.enroll_begin"] {
        let response = crate::google_personal::DASHBOARD_SESSION.scope(
            Some((user.id.clone(), uuid::Uuid::new_v4().to_string())),
            state.handler.handle(method, json!({"code":codes[0],"password":"correct horse battery"}), &ctx),
        ).await;
        let WsFrame::Response { ok: false, error: Some(error), .. } = response else { panic!("rate limit was bypassed") };
        assert!(error.as_str().unwrap().contains("嘗試次數過多"));
    }
    assert!(state.user_db.mfa_active(&user.id).unwrap());
}

#[tokio::test]
async fn mfa_personal_self_service_requires_proof_and_admin_policy_disable() {
    let (_dir, state, user) = fixture(true, true).await;
    let access = state.jwt_config.issue_access_token(&user, &[]).unwrap();
    let ctx = authenticate_jwt(&state, &access).unwrap();
    let session = Some((user.id.clone(), uuid::Uuid::new_v4().to_string()));
    let begin = crate::google_personal::DASHBOARD_SESSION.scope(session.clone(),
        state.handler.handle("mfa.enroll_begin", json!({"password":"correct horse battery"}), &ctx)).await;
    let WsFrame::Response { ok: true, payload: Some(begin), .. } = begin else { panic!("begin failed") };
    assert_eq!(begin["proof_required"], true);
    assert!(begin.get("secret").is_none());
    let proof = state.user_db.mfa_issue_enrollment_code(None, &user.id,
        begin["challenge_ref"].as_str().unwrap(), chrono::Utc::now().timestamp()).unwrap();
    let retry = crate::google_personal::DASHBOARD_SESSION.scope(session.clone(),
        state.handler.handle("mfa.enroll_begin", json!({"password":"correct horse battery",
            "enrollment_token":begin["enrollment_token"],"proof_code":proof}), &ctx)).await;
    let WsFrame::Response { ok: true, payload: Some(begin), .. } = retry else { panic!("proof failed: {retry:?}") };
    let confirm = crate::google_personal::DASHBOARD_SESSION.scope(session,
        state.handler.handle("mfa.enroll_confirm", json!({"enrollment_token":begin["enrollment_token"],"code":totp(begin["secret"].as_str().unwrap())}), &ctx)).await;
    let WsFrame::Response { ok: true, payload: Some(confirm), .. } = confirm else { panic!("confirm failed") };
    let ctx = authenticate_jwt(&state, confirm["access_token"].as_str().unwrap()).unwrap();
    let disable = json!({"password":"correct horse battery","code":confirm["recovery_codes"][0]});
    let response = state.handler.handle("mfa.disable", disable.clone(), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: false, .. }));
    assert!(state.user_db.mfa_active(&user.id).unwrap());
    let employee = state.user_db.create_user("employee@test.invalid", "Employee", "password-test", UserRole::Employee).unwrap();
    let mut forged = UserContext::admin_fallback();
    forged.user_id = employee.id;
    let policy = json!({"auth.mfa_enforced":false,"expected_state":true});
    let response = state.handler.handle("system.update_config", policy.clone(), &forged).await;
    assert!(matches!(response, WsFrame::Response { ok: false, .. }));
    assert!(enforced(&state.home_dir, true).unwrap());
    let response = state.handler.handle("system.update_config", policy, &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }), "{response:?}");
    for personal in [true, true, false] {
        crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
            &state.home_dir, "127.0.0.1:18789".parse().unwrap(), personal, None).unwrap();
        crate::mfa::reload_policy(&state.home_dir);
        assert!(!enforced(&state.home_dir, personal).unwrap());
        let conn = rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap();
        assert!(!duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
        assert_eq!(std::fs::read_to_string(state.home_dir.join("config.toml")).unwrap()
            .parse::<toml::Table>().unwrap()["auth"]["mfa_enforced"].as_bool(), Some(false));
    }
    let response = state.handler.handle("mfa.disable", disable, &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }));
    assert!(!state.user_db.mfa_active(&user.id).unwrap());
}

#[tokio::test]
async fn config_raw_mfa_policy_changes_refused_but_unrelated_edits_pass() {
    let (_dir, state, user) = fixture(false, true).await;
    let path = state.home_dir.join("config.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = user.id.clone();
    for content in ["[auth]\nmfa_enforced=false\n", "[auth]\n", "[general]\nlog_level='debug'\n"] {
        let response = state.handler.handle("config.raw.set", json!({"file":"config",
            "base_hash":crate::handlers::config_commit::content_hash(&original), "content":content}), &ctx).await;
        let WsFrame::Response { ok:false, error:Some(error), .. } = response else { panic!("{response:?}") };
        assert!(error.as_str().unwrap().contains("system.update_config"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(enforced(&state.home_dir, false).unwrap());
        let events = duduclaw_security::audit::read_recent_events(&state.home_dir, 100);
        assert!(!events.iter().any(|e| matches!(e.event_type.as_str(),
            "config_raw_edited" | "config_protected_key_changed" | "config_changed")));
        assert!(!std::fs::read_dir(&state.home_dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".bak-")));
    }
    let response = state.handler.handle("config.raw.set", json!({"file":"config",
        "base_hash":crate::handlers::config_commit::content_hash(&original),
        "content":"[auth]\nmfa_enforced=true\n[general]\nlog_level='debug'\n"}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok:true, .. }), "{response:?}");
    assert!(std::fs::read_to_string(&path).unwrap().contains("log_level"));
    assert!(enforced(&state.home_dir, false).unwrap());

    let (_dir, state, user) = fixture(false, false).await;
    let path = state.home_dir.join("config.toml");
    // Adding a previously absent key must also use the dedicated switch.
    std::fs::write(&path, "").unwrap();
    ctx.user_id = user.id;
    let response = state.handler.handle("config.raw.set", json!({"file":"config",
        "base_hash":crate::handlers::config_commit::content_hash(""),
        "content":"[auth]\nmfa_enforced=true\n"}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok:false, .. }), "{response:?}");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "");
}

#[tokio::test]
async fn mfa_nested_batch_config_uses_dedicated_switch() {
    let (_dir, state, user) = fixture(false, false).await;
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = user.id;
    let path = state.home_dir.join("config.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    let response = state.handler.handle("system.update_config",
        json!({"auth":{"mfa_enforced":true},"log_level":"debug"}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok:false, .. }), "{response:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let response = state.handler.handle("system.update_config",
        json!({"auth":{"mfa_enforced":true},"log_level":"debug","expected_state":false}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok:true, .. }), "{response:?}");
    assert!(enforced(&state.home_dir, false).unwrap());
    let events = duduclaw_security::audit::read_recent_events(&state.home_dir, 100);
    assert!(events.iter().any(|e| e.event_type == "config_protected_key_changed"
        && e.details["key"] == "auth.mfa_enforced" && e.details["policy_reconciled"] == true));
}

#[tokio::test]
async fn mfa_policy_rpc_db_failure_obeys_commit_order() {
    for initial in [false, true] {
        let (_dir, state, user) = fixture(false, initial).await;
        let path = state.home_dir.join("config.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        let conn = rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER fail_policy BEFORE INSERT ON mfa_policy_state BEGIN SELECT RAISE(FAIL, 'policy write failed'); END;").unwrap();
        let mut ctx = UserContext::admin_fallback();
        ctx.user_id = user.id.clone();
        let response = state.handler.handle("system.update_config",
            json!({"auth.mfa_enforced":!initial,"expected_state":initial}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: false, .. }), "{response:?}");
        let actual: bool = conn.query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(actual, initial);
        let events = duduclaw_security::audit::read_recent_events(&state.home_dir, 100);
        let policy_events: Vec<_> = events.iter().filter(|e| e.event_type == "config_protected_key_changed"
            && e.details["key"] == "auth.mfa_enforced").collect();
        if initial {
            let WsFrame::Response { error:Some(error), .. } = &response else { panic!("{response:?}") };
            assert!(error.as_str().unwrap().contains("已在 config.toml 生效"));
            assert!(error.as_str().unwrap().contains("同步"));
            assert_eq!(policy_events.len(), 1);
            let event = policy_events[0];
            assert_eq!(event.agent_id, user.id);
            assert_eq!(event.details["before"], true);
            assert_eq!(event.details["after"], false);
            assert_eq!(event.details["config_applied"], true);
            assert_eq!(event.details["reconciling"], true);
            assert_eq!(event.details["policy_reconciled"], false);
        } else {
            assert!(policy_events.is_empty());
        }
        let disk = std::fs::read_to_string(&path).unwrap();
        if initial {
            assert_eq!(disk.parse::<toml::Table>().unwrap()["auth"]["mfa_enforced"].as_bool(), Some(false));
            assert!(enforced(&state.home_dir, false).unwrap());
        } else {
            assert_eq!(disk, original);
        }
    }
}

#[tokio::test]
async fn mfa_fresh_admin_enrolls_with_host_proof_and_can_disable_enforcement() {
    let (_dir, state, user) = fixture_policy(false, None).await;
    assert!(enforced(&state.home_dir, false).unwrap());
    assert!(!legacy_token_allowed(&state).await);
    let (status, challenge) = body(handle_login(State(state.clone()), ConnectInfo(peer(211)),
        Json(LoginRequest { email: user.email.clone(), password: "correct horse battery".into() }))
        .await.into_response()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(challenge["mfa_enrollment_required"], true);
    assert!(challenge.get("access_token").is_none());
    let token = challenge["enrollment_token"].as_str().unwrap().to_string();
    assert_eq!(enroll_begin(State(state.clone()), ConnectInfo(peer(211)), Json(EnrollBody {
        enrollment_token: token.clone(), code: None, proof_code: None, password: None,
    })).await.status(), StatusCode::UNAUTHORIZED);
    let proof = state.user_db.mfa_issue_enrollment_code(None, &user.id,
        challenge["challenge_ref"].as_str().unwrap(), chrono::Utc::now().timestamp()).unwrap();
    let (status, start) = body(enroll_begin(State(state.clone()), ConnectInfo(peer(211)), Json(EnrollBody {
        enrollment_token: token.clone(), code: None, proof_code: Some(proof), password: None,
    })).await).await;
    assert_eq!(status, StatusCode::OK);
    let (status, done) = body(enroll_confirm(State(state.clone()), ConnectInfo(peer(211)), Json(EnrollBody {
        enrollment_token: token, code: Some(totp(start["secret"].as_str().unwrap())), proof_code: None, password: None,
    })).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(done["recovery_codes"].as_array().unwrap().len(), 10);
    let ctx = authenticate_jwt(&state, done["access_token"].as_str().unwrap()).unwrap();
    assert!(state.user_db.mfa_active(&user.id).unwrap());
    let response = state.handler.handle("system.update_config",
        json!({"auth.mfa_enforced":false,"expected_state":true}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }));
    crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
        &state.home_dir, "127.0.0.1:18789".parse().unwrap(), false, None,
    ).unwrap();
    crate::mfa::reload_policy(&state.home_dir);
    assert!(!enforced(&state.home_dir, false).unwrap());
    let conn = rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap();
    assert!(!conn.query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get::<_, bool>(0)).unwrap());
    assert_eq!(std::fs::read_to_string(state.home_dir.join("config.toml")).unwrap()
        .parse::<toml::Table>().unwrap()["auth"]["mfa_enforced"].as_bool(), Some(false));
}

#[tokio::test]
async fn mfa_strict_restore_refuses_local_auto_login_in_every_edition() {
    for personal in [true, false] {
        let (_dir, state, _user) = fixture_policy(personal, None).await;
        // An empty-home restore writes true even for Personal.
        std::fs::write(state.home_dir.join("config.toml"),
            "[auth]\nmfa_enforced=true\n[dashboard]\nlocal_auto_login=true\n").unwrap();
        crate::mfa::reload_policy(&state.home_dir);
        assert!(enforced(&state.home_dir, personal).unwrap());
        assert!(!legacy_token_allowed(&state).await);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-duduclaw-local", "1".parse().unwrap());
        headers.insert("origin", "http://127.0.0.1:18789".parse().unwrap());
        let local = SocketAddr::from(([127, 0, 0, 1], 12000));
        let (status, payload) = body(handle_local_session(State(state.clone()),
            ConnectInfo(local), headers.clone()).await.into_response()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(payload.get("access_token").is_none());
        std::fs::write(state.home_dir.join("config.toml"),
            "[auth]\nmfa_enforced=true\n[dashboard]\nlocal_auto_login=false\n").unwrap();
        assert_eq!(handle_local_session(State(state.clone()), ConnectInfo(local), headers)
            .await.into_response().status(), StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn mfa_personal_policy_requires_login_enrollment_and_expires_sockets() {
    let (_dir, state, user) = fixture(true, true).await;
    let (status, response) = login(&state, 214).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["mfa_enrollment_required"], true);
    assert!(response.get("access_token").is_none());
    assert!(!refresh_allowed(&state, &user.id).await);
    let ctx = state.user_db.live_user_context(&user.id).unwrap().unwrap();
    assert!(socket_expired(&state, &ctx, Some(1), false).await);
    assert!(!socket_expired(&state, &ctx, Some(u64::MAX), false).await);
    let status = state.handler.handle("mfa.status", json!({}), &ctx).await;
    let WsFrame::Response { ok: true, payload: Some(status), .. } = status else { panic!("{status:?}") };
    assert_eq!(status["enforced"], true);
    assert_eq!(status["enabled"], false);
}

#[tokio::test]
async fn mfa_fresh_personal_policy_off_preserves_local_auto_login() {
    let (_dir, state, user) = fixture_policy(true, None).await;
    std::fs::write(state.home_dir.join("config.toml"), "[dashboard]\nlocal_auto_login=true\n").unwrap();
    crate::mfa::reload_policy(&state.home_dir);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-duduclaw-local", "1".parse().unwrap());
    headers.insert("origin", "http://127.0.0.1:18789".parse().unwrap());
    let local = SocketAddr::from(([127, 0, 0, 1], 12000));
    let (status, payload) = body(handle_local_session(State(state.clone()), ConnectInfo(local), headers)
        .await.into_response()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(authenticate_jwt(&state, payload["access_token"].as_str().unwrap()).unwrap().user_id, user.id);
}

#[tokio::test]
async fn mfa_personal_recovery_methods_keep_current_admin_authorization() {
    let (_dir, state, admin) = fixture(true, false).await;
    let admin_codes = enroll(&state, &admin);
    let target = state.user_db.create_user("target@test.invalid", "Target", "correct horse battery", UserRole::Employee).unwrap();
    enroll(&state, &target);
    let admin = state.user_db.get_user(&admin.id).unwrap().unwrap();
    let ctx = authenticate_jwt(&state, &state.jwt_config.issue_access_token(&admin, &[]).unwrap()).unwrap();
    let response = state.handler.handle("system.update_config",
        json!({"auth.mfa_enforced":true,"expected_state":false}), &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }));
    let mut forged = UserContext::admin_fallback();
    forged.user_id = target.id.clone();
    forged.credential_epoch = state.user_db.get_user(&target.id).unwrap().unwrap().credential_epoch;
    let reset = json!({"user_id":target.id,"code":admin_codes[0]});
    let response = state.handler.handle("users.mfa_reset", reset.clone(), &forged).await;
    assert!(matches!(response, WsFrame::Response { ok: false, .. }));
    assert!(state.user_db.mfa_active(&target.id).unwrap());
    let response = state.handler.handle("users.mfa_reset", reset, &ctx).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }), "{response:?}");
    assert!(!state.user_db.mfa_active(&target.id).unwrap());
    let now = chrono::Utc::now().timestamp();
    let token = state.user_db.mfa_challenge(&target.id, duduclaw_auth::mfa::FirstFactor::Password, true, now).unwrap();
    state.user_db.mfa_prepare_enrollment(&token, true, now).unwrap();
    forged.credential_epoch = state.user_db.get_user(&target.id).unwrap().unwrap().credential_epoch;
    let request = json!({"user_id":target.id,"challenge_ref":duduclaw_auth::mfa::enrollment_challenge_ref(&token)});
    let response = state.handler.handle("users.mfa_enrollment_code", request.clone(), &forged).await;
    assert!(matches!(response, WsFrame::Response { ok: false, .. }));
    let response = state.handler.handle("users.mfa_enrollment_code", request, &ctx).await;
    let WsFrame::Response { ok: true, payload: Some(payload), .. } = response else { panic!("{response:?}") };
    state.user_db.mfa_prove_enrollment(&token, payload["enrollment_code"].as_str().unwrap(), now).unwrap();
    state.user_db.mfa_require_independent_proof(&token, now).unwrap();
    assert!(enforced(&state.home_dir, true).unwrap());
}

#[tokio::test]
async fn mfa_login_policy_race_routes_to_enrollment_with_stale_cache() {
    for first in [FirstFactor::Password, FirstFactor::ChannelOtp] {
        let (_dir, state, user) = fixture(true, false).await;
        assert!(after_first_factor(&state, &user, first).await.is_none());
        let mut admin = rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut admin, true, chrono::Utc::now().timestamp_micros()).unwrap();
        // The transaction must override the earlier gate, even before cache reload.
        assert!(!enforced(&state.home_dir, true).unwrap());
        let called = std::cell::Cell::new(false);
        assert!(state.user_db.mfa_issue_if_unenrolled(&user.id, user.credential_epoch, true, |current, access| {
            called.set(true);
            state.jwt_config.issue_session_tokens(current, access)
        }).is_err());
        assert!(!called.get());
        let (status, payload) = body(require_after_first_factor(&state, &user, first).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["mfa_enrollment_required"], true);
        assert!(payload.get("access_token").is_none());
        assert!(payload.get("refresh_token").is_none());
    }
}

#[tokio::test]
async fn mfa_refresh_and_local_session_refuse_durable_policy_despite_stale_cache() {
    let (_dir, state, user) = fixture_policy(true, None).await;
    std::fs::write(state.home_dir.join("config.toml"), "[dashboard]\nlocal_auto_login=true\n").unwrap();
    crate::mfa::reload_policy(&state.home_dir);
    let tokens = state.jwt_config.issue_session_tokens(&user, &[]).unwrap();
    assert!(refresh_allowed(&state, &user.id).await);
    assert!(legacy_token_allowed(&state).await);
    let mut admin = rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap();
    duduclaw_auth::mfa::record_enforcement(&mut admin, true, chrono::Utc::now().timestamp_micros()).unwrap();
    assert!(!enforced(&state.home_dir, true).unwrap());
    let (status, payload) = body(handle_refresh(State(state.clone()), ConnectInfo(peer(230)),
        Json(RefreshRequest { refresh_token: tokens.1 })).await.into_response()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(payload["error"], "請重新登入並完成雙因素驗證註冊。");
    assert!(payload.get("access_token").is_none());
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-duduclaw-local", "1".parse().unwrap());
    headers.insert("origin", "http://127.0.0.1:18789".parse().unwrap());
    let local = SocketAddr::from(([127, 0, 0, 1], 12000));
    let (status, payload) = body(handle_local_session(State(state.clone()), ConnectInfo(local), headers)
        .await.into_response()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(payload["error"], "local auto-login unavailable");
    assert!(payload.get("access_token").is_none());
    assert!(payload.get("refresh_token").is_none());
}

#[tokio::test]
async fn mfa_email_http_unconfigured_is_unavailable_without_sending_or_enabling() {
    let (_dir,state,user)=fixture(false,false).await;
    let token=state.user_db.mfa_challenge(&user.id,duduclaw_auth::mfa::FirstFactor::Password,true,chrono::Utc::now().timestamp()).unwrap();
    state.user_db.mfa_prepare_enrollment(&token,false,chrono::Utc::now().timestamp()).unwrap();
    let payload=serde_json::from_value(json!({"enrollment_token":token})).unwrap();
    let (status,_)=body(super::super::server_mfa_email::send(State(state.clone()),ConnectInfo(peer(230)),Json(payload)).await).await;
    assert_eq!(status,StatusCode::SERVICE_UNAVAILABLE);
    assert!(!state.user_db.mfa_active(&user.id).unwrap());
}

#[tokio::test]
async fn mfa_email_http_login_uses_verified_address_and_consumes_challenge() {
    let (dir,state,user)=fixture(false,false).await;
    std::fs::write(dir.path().join("config.toml"),"[auth]\nmfa_enforced=false\n[channels.email]\nsmtp_host='127.0.0.1'\nsmtp_port=9\nfrom_addr='sender@example.test'\nsmtp_tls='none'\n").unwrap();
    let now=chrono::Utc::now().timestamp();
    let token=state.user_db.mfa_factor_password_challenge(&user.id,"correct horse battery","session",now).unwrap();
    let (_,code)=state.user_db.mfa_email_send(&token,true,now).unwrap();
    state.user_db.mfa_email_verify_and_issue(&token,&code,true,now,|_,_,_|Ok(())).unwrap();
    let token=state.user_db.mfa_challenge(&user.id,duduclaw_auth::mfa::FirstFactor::Password,false,now+60).unwrap();
    let (_,code)=state.user_db.mfa_email_send(&token,false,now+60).unwrap();
    std::fs::write(dir.path().join("config.toml"),"[auth]\nmfa_enforced=false\n[mfa]\nemail_relay=false\n").unwrap();
    assert!(!crate::mfa_email::available(dir.path()));
    let payload=||serde_json::from_value(json!({"challenge_id":token,"code":code})).unwrap();
    let (status,value)=body(super::super::server_mfa_email::verify(State(state.clone()),ConnectInfo(peer(231)),Json(payload())).await).await;
    assert_eq!(status,StatusCode::OK);
    assert!(state.jwt_config.verify_access_token(value["access_token"].as_str().unwrap()).is_ok());
    let (status,_)=body(super::super::server_mfa_email::verify(State(state),ConnectInfo(peer(231)),Json(payload())).await).await;
    assert_eq!(status,StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mfa_enforced_email_user_can_add_totp_without_another_proof() {
    let (_dir,state,user)=fixture(false,true).await;
    let now=chrono::Utc::now().timestamp();
    let token=state.user_db.mfa_factor_password_challenge(&user.id,"correct horse battery","setup",now).unwrap();
    let (_,code)=state.user_db.mfa_email_send(&token,true,now).unwrap();
    state.user_db.mfa_email_verify_and_issue(&token,&code,true,now,|_,_,_|Ok(())).unwrap();
    let user=state.user_db.get_user(&user.id).unwrap().unwrap();
    let access=state.jwt_config.issue_access_token(&user,&[]).unwrap();
    let ctx=authenticate_jwt(&state,&access).unwrap();
    let session=state.jwt_config.verify_access_token(&access).unwrap().jti.unwrap();
    let response=crate::google_personal::DASHBOARD_SESSION.scope(Some((user.id,session)),
        state.handler.handle("mfa.enroll_begin",json!({"password":"correct horse battery"}),&ctx)).await;
    let WsFrame::Response{ok:true,payload:Some(value),..}=response else {panic!("begin failed")};
    assert!(value["secret"].is_string());
    assert!(value.get("proof_required").is_none());
}

#[tokio::test]
async fn mfa_email_http_invalid_capabilities_hide_send_availability() {
    let (dir,state,user)=fixture(false,false).await;
    let now=chrono::Utc::now().timestamp();
    let stepup=state.user_db.mfa_factor_password_challenge(&user.id,"correct horse battery","session",now).unwrap();
    let expired=state.user_db.mfa_challenge(&user.id,duduclaw_auth::mfa::FirstFactor::Password,true,now-3600).unwrap();
    let invalid=[json!({}),json!({"challenge_id":"short"}),json!({"challenge_id":"a".repeat(64)}),
        json!({"enrollment_token":"a".repeat(64)}),json!({"challenge_id":"a".repeat(64),"enrollment_token":"b".repeat(64)}),
        json!({"enrollment_token":stepup}),json!({"enrollment_token":expired})];
    let mut expected=None;
    for smtp in [false,true] {
        let config=if smtp {"[channels.email]\nsmtp_host='127.0.0.1'\nsmtp_port=9\nfrom_addr='sender@example.test'\n"} else {"[mfa]\nemail_relay=false\n"};
        std::fs::write(dir.path().join("config.toml"),config).unwrap();
        assert_eq!(crate::mfa_email::available(dir.path()),smtp);
        for (i,value) in invalid.iter().enumerate() {
            let addr=peer(240+i as u8);
            let payload=serde_json::from_value(value.clone()).unwrap();
            let send=body(super::super::server_mfa_email::send(State(state.clone()),ConnectInfo(addr),Json(payload)).await).await;
            let payload=serde_json::from_value(value.clone()).unwrap();
            let verify=body(super::super::server_mfa_email::verify(State(state.clone()),ConnectInfo(addr),Json(payload)).await).await;
            assert_eq!(send.0,StatusCode::UNAUTHORIZED);
            assert_eq!(verify.0,StatusCode::UNAUTHORIZED);
            assert_eq!(send,verify);
            if let Some(ref expected)=expected {assert_eq!(&send,expected);} else {expected=Some(send);}
        }
    }
}
