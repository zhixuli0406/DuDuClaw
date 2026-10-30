//! In-process handler tests: no socket binding required.
use super::*;
use axum::http::StatusCode;
use duduclaw_auth::UserRole;
use serde_json::{Value, json};

async fn fixture(
    personal: bool,
    enforce: bool,
) -> (tempfile::TempDir, Arc<AppState>, duduclaw_auth::User) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    std::fs::write(
        home.join("config.toml"),
        format!("[auth]\nmfa_enforced={enforce}\n"),
    )
    .unwrap();
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let user = db
        .create_user(
            "mfa@example.test",
            "MFA",
            "correct horse battery",
            UserRole::Admin,
        )
        .unwrap();
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
        user_db: db,
        jwt_config: jwt,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
            home.clone(),
            reqwest::Client::new(),
        )),
        home_dir: home,
    });
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
                email: "mfa@example.test".into(),
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
    let secret = state
        .user_db
        .mfa_begin(&user.id, None, &cipher, now)
        .unwrap();
    state
        .user_db
        .mfa_confirm(&user.id, None, &totp(&secret), &cipher, now)
        .unwrap()
}
#[tokio::test]
async fn mfa_password_and_channel_otp_return_no_token_in_entire_body() {
    let (_dir, state, user) = fixture(false, false).await;
    let codes = enroll(&state, &user);
    let (status, response) = login(&state, 180).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.as_object().unwrap().len(), 2);
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
    assert_eq!(response.as_object().unwrap().len(), 2);
    assert_eq!(response["mfa_required"], true);
    assert!(!response.to_string().contains("token"));
}
#[tokio::test]
async fn mfa_enforcement_off_preserves_login_response_and_personal_ignores_on() {
    for (personal, enforce, n) in [(false, false, 182), (true, true, 183)] {
        let (_dir, state, _) = fixture(personal, enforce).await;
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
        assert_eq!(response["user"]["email"], "mfa@example.test");
    }
}
#[tokio::test]
async fn mfa_enrollment_capability_cannot_authenticate_ws_me_or_refresh() {
    let (_dir, state, user) = fixture(false, true).await;
    let old = state.jwt_config.issue_session_tokens(&user, &[]).unwrap();
    let (_, response) = login(&state, 184).await;
    assert_eq!(response.as_object().unwrap().len(), 2);
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
            &user.id,
            Some(chrono::Utc::now().timestamp() as u64 + 1800)
        )
        .await
    );
    assert!(socket_expired(&state, &user.id, Some(1)).await);
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
    let (_, start) = body(
        enroll_begin(
            State(state.clone()),
            ConnectInfo(peer(184)),
            Json(EnrollBody {
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
    assert!(state.user_db.mfa_active(&user.id).unwrap());
    assert_eq!(
        enroll_confirm(
            State(state.clone()),
            ConnectInfo(peer(184)),
            Json(EnrollBody {
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
    assert_eq!(login(&state, 185).await.0, StatusCode::UNAUTHORIZED);
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
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
                json!({"auth.mfa_enforced":true}),
                &forged
            )
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(!enforced(&state.home_dir, false).unwrap());
    let mut admin = UserContext::admin_fallback();
    admin.user_id = user.id.clone();
    assert!(matches!(
        state
            .handler
            .handle("users.mfa_reset", json!({"user_id":user.id}), &admin)
            .await,
        crate::protocol::WsFrame::Response { ok: true, .. }
    ));
    assert!(!state.user_db.mfa_active(&user.id).unwrap());
    assert!(matches!(
        state
            .handler
            .handle(
                "system.update_config",
                json!({"auth.mfa_enforced":true}),
                &admin
            )
            .await,
        crate::protocol::WsFrame::Response { ok: true, .. }
    ));
    assert!(enforced(&state.home_dir, false).unwrap());
}
