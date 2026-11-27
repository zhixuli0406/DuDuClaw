//! RM-135 / RM-136 in-process HTTP and RPC tests: first-administrator
//! setup always needs the host setup code (a loopback peer without one is
//! refused), unauthenticated bootstrap refusals never reach the security
//! audit, validated ones are capped, and every `users.mfa_reset` attempt —
//! every early gate included — leaves exactly one outbox row or a direct
//! audit event, delivered exactly once.
use super::*;
use axum::http::StatusCode;
use duduclaw_auth::UserRole;
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";
const OWNER: &str = "owner@example.test";

/// A claimed team installation: only the bootstrap owner, policy as given.
async fn state_with_owner(enforce: bool) -> (tempfile::TempDir, Arc<AppState>, duduclaw_auth::User) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    std::fs::write(home.join("config.toml"), format!("[auth]\nmfa_enforced={enforce}\n")).unwrap();
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    db.ensure_default_admin().unwrap().expect("owner created");
    assert!(db.claim_default_admin_with_email(Some(OWNER), PASSWORD).unwrap());
    let owner = db.get_bootstrap_owner().unwrap().unwrap();
    let jwt = Arc::new(JwtConfig::new(b"mfa-bootstrap-test-secret-at-least-32-bytes"));
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_user_db(db.clone(), jwt.clone()).await;
    handler.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    crate::mfa::reload_policy(&home);
    let (tx, _) = broadcast::channel(4);
    let (event_tx, _) = broadcast::channel(4);
    let state = Arc::new(AppState {
        auth: AuthManager::new(None),
        handler: Arc::new(handler),
        tx,
        event_tx,
        prepared_events: Default::default(),
        user_db: db,
        jwt_config: jwt,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
        home_dir: home,
    });
    (dir, state, owner)
}

fn now() -> i64 { chrono::Utc::now().timestamp() }

fn net(n: u8) -> SocketAddr { SocketAddr::from(([198, 51, 100, n], 12000)) }

/// `::ffff:127.0.0.n` — a loopback peer as a dual-stack listener reports it
/// (its own rate-limit key, so tests do not share the 127.0.0.1 budget).
fn mapped_loopback(n: u16) -> SocketAddr {
    SocketAddr::from((std::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, n), 12000))
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn login_from(state: &Arc<AppState>, peer: SocketAddr) -> Value {
    let response = handle_login(State(state.clone()), ConnectInfo(peer),
        Json(LoginRequest { email: OWNER.into(), password: PASSWORD.into() })).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

async fn bootstrap(state: &Arc<AppState>, peer: SocketAddr, token: &str, setup: Option<&str>) -> StatusCode {
    enroll_bootstrap(State(state.clone()), ConnectInfo(peer),
        Json(BootstrapBody { enrollment_token: token.into(), setup_code: setup.map(str::to_owned) })).await.status()
}

fn totp(secret: &str) -> String {
    use hmac::{Hmac, Mac};
    let (mut bytes, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in secret.bytes() {
        let v = if c.is_ascii_uppercase() { c - b'A' } else { c - b'2' + 26 };
        acc = (acc << 5) | v as u32;
        bits += 5;
        if bits >= 8 { bits -= 8; bytes.push((acc >> bits) as u8); }
    }
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&bytes).unwrap();
    mac.update(&((now() / 30) as u64).to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[19] & 15) as usize;
    let value = u32::from_be_bytes(hash[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    format!("{:06}", value % 1_000_000)
}

fn give_factor(state: &AppState, user: &str) {
    rusqlite::Connection::open(state.home_dir.join("users.db")).unwrap()
        .execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'enc',0)", [user]).unwrap();
}

fn audit_text(home: &std::path::Path) -> String {
    std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default()
}

#[tokio::test]
async fn bootstrap_loopback_peer_without_a_setup_code_is_refused() {
    let (_dir, state, owner) = state_with_owner(true).await;
    let login = login_from(&state, mapped_loopback(2)).await;
    assert_eq!(login["mfa_enrollment_required"], true);
    assert_eq!(login["setup_code_required"], true);
    assert!(login.get("bootstrap_available").is_none(), "no hint of a code-free path");
    let token = login["enrollment_token"].as_str().unwrap().to_owned();
    // A loopback peer (what a same-host reverse proxy also looks like), no
    // code or an empty one: refused by the proof itself.
    assert_eq!(bootstrap(&state, mapped_loopback(2), &token, None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(bootstrap(&state, mapped_loopback(2), &token, Some("   ")).await, StatusCode::UNAUTHORIZED);
    // Plain IPv4 / IPv6 loopback too (their rate-limit budget is shared with
    // other tests, so a 429 is also a refusal here).
    for peer in [SocketAddr::from(([127, 0, 0, 1], 1)), SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 1))] {
        let status = bootstrap(&state, peer, &token, None).await;
        assert!(matches!(status, StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS), "{peer}: {status}");
    }
    assert!(state.user_db.mfa_require_independent_proof(&token, now()).is_err());
    assert!(!state.user_db.mfa_active(&owner.id).unwrap());
}

#[tokio::test]
async fn bootstrap_setup_code_completes_first_enrollment() {
    let (_dir, state, owner) = state_with_owner(true).await;
    // What `duduclaw ops users mfa-setup-code` does on the host.
    let code = state.user_db.mfa_issue_bootstrap_token("test", now()).unwrap().expect("code issued");
    let login = login_from(&state, net(41)).await;
    let token = login["enrollment_token"].as_str().unwrap().to_owned();
    assert_eq!(bootstrap(&state, net(41), &token, Some(code.as_str())).await, StatusCode::OK);
    // The ordinary enrollment continues without an enrollment code.
    let begin = enroll_begin(State(state.clone()), ConnectInfo(net(41)),
        Json(EnrollBody { enrollment_token: token.clone(), code: None, proof_code: None, password: None })).await;
    assert_eq!(begin.status(), StatusCode::OK);
    let secret = json_body(begin).await["secret"].as_str().unwrap().to_owned();
    let confirm = enroll_confirm(State(state.clone()), ConnectInfo(net(41)),
        Json(EnrollBody { enrollment_token: token.clone(), code: Some(totp(&secret)), proof_code: None, password: None })).await;
    assert_eq!(confirm.status(), StatusCode::OK);
    assert!(json_body(confirm).await["access_token"].is_string());
    assert!(state.user_db.mfa_active(&owner.id).unwrap());
    // Once per installation.
    assert!(state.user_db.mfa_bootstrap_closed().unwrap());
    assert!(state.user_db.mfa_issue_bootstrap_token("test", now()).is_err());
    let audit = audit_text(&state.home_dir);
    assert!(audit.contains(BOOTSTRAP_AUDIT_EVENT));
    assert!(!audit.contains(&token), "the enrollment capability never reaches the audit log");
    assert!(!audit.contains(&code), "the setup code never reaches the audit log");
}

#[tokio::test]
async fn bootstrap_refused_once_an_administrator_has_a_factor() {
    let (_dir, state, _owner) = state_with_owner(true).await;
    let other = state.user_db.create_user("other@example.test", "Other", PASSWORD, UserRole::Admin).unwrap();
    let code = state.user_db.mfa_issue_bootstrap_token("test", now()).unwrap().expect("code issued");
    give_factor(&state, &other.id);
    let login = login_from(&state, net(42)).await;
    assert_eq!(login["setup_code_required"], false);
    let token = login["enrollment_token"].as_str().unwrap().to_owned();
    assert_eq!(bootstrap(&state, net(42), &token, Some(code.as_str())).await, StatusCode::UNAUTHORIZED);
    assert!(state.user_db.mfa_require_independent_proof(&token, now()).is_err());
}

#[tokio::test]
async fn bootstrap_setup_code_works_from_the_network_exactly_once() {
    let (_dir, state, _owner) = state_with_owner(true).await;
    let code = state.user_db.mfa_issue_bootstrap_token("test", now()).unwrap().expect("code issued");
    let token = login_from(&state, net(43)).await["enrollment_token"].as_str().unwrap().to_owned();
    let wrong = if code.starts_with('0') { "1".repeat(32) } else { "0".repeat(32) };
    assert_eq!(bootstrap(&state, net(43), &token, Some(wrong.as_str())).await, StatusCode::UNAUTHORIZED);
    assert_eq!(bootstrap(&state, net(43), &token, Some(code.as_str())).await, StatusCode::OK);
    state.user_db.mfa_require_independent_proof(&token, now()).unwrap();
    let again = login_from(&state, net(44)).await["enrollment_token"].as_str().unwrap().to_owned();
    assert_eq!(bootstrap(&state, net(44), &again, Some(code.as_str())).await, StatusCode::UNAUTHORIZED);
    assert!(!audit_text(&state.home_dir).contains(&code), "the setup code never reaches the audit log");
}

#[tokio::test]
async fn bootstrap_unauthenticated_refusals_never_write_the_security_audit() {
    let (_dir, state, _owner) = state_with_owner(true).await;
    let code = state.user_db.mfa_issue_bootstrap_token("test", now()).unwrap().expect("code issued");
    let before = std::fs::read(state.home_dir.join("security_audit.jsonl")).ok();
    // A distributed flood: no valid password-bound enrollment token, with or
    // without a (even the right) setup code, malformed or well-formed tokens.
    for n in 0..30u8 {
        let token = if n % 2 == 0 { "f".repeat(64) } else { format!("bogus-{n}") };
        let setup = if n % 3 == 0 { None } else { Some(code.as_str()) };
        let status = bootstrap(&state, net(150 + n), &token, setup).await;
        assert!(matches!(status, StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS), "{status}");
    }
    let after = std::fs::read(state.home_dir.join("security_audit.jsonl")).ok();
    assert_eq!(before, after, "no security audit write before the enrollment token is validated");
    // The outstanding code was not touched by any of them.
    let token = login_from(&state, net(45)).await["enrollment_token"].as_str().unwrap().to_owned();
    assert_eq!(bootstrap(&state, net(45), &token, Some(code.as_str())).await, StatusCode::OK);
}

#[test]
fn bootstrap_unauthenticated_warning_is_at_most_once_per_minute() {
    let mut limiter = BootstrapAuditLimiter::new();
    let start = std::time::Instant::now();
    assert_eq!(limiter.unauthenticated_refusal(start), Some(1));
    for s in 1..60 {
        assert_eq!(limiter.unauthenticated_refusal(start + std::time::Duration::from_secs(s)), None);
    }
    assert_eq!(limiter.unauthenticated_refusal(start + std::time::Duration::from_secs(60)), Some(60));
}

#[test]
fn bootstrap_validated_audit_is_capped_per_hour_with_one_summary() {
    let mut limiter = BootstrapAuditLimiter::new();
    let start = std::time::Instant::now();
    let slots: Vec<_> = (0..30).map(|_| limiter.validated_event(start, false).0).collect();
    assert!(slots[..BOOTSTRAP_AUDIT_CAP as usize].iter().all(|s| *s == AuditSlot::Write));
    assert_eq!(slots[BOOTSTRAP_AUDIT_CAP as usize], AuditSlot::Summary);
    assert!(slots[BOOTSTRAP_AUDIT_CAP as usize + 1..].iter().all(|s| *s == AuditSlot::Suppressed));
    // A success is always written.
    assert_eq!(limiter.validated_event(start, true).0, AuditSlot::Write);
    // Next window: writes again and reports what the last one suppressed.
    let (slot, ended) = limiter.validated_event(start + std::time::Duration::from_secs(3600), false);
    assert_eq!((slot, ended), (AuditSlot::Write, Some(9)));

    // End to end on a real audit file.
    let dir = tempfile::tempdir().unwrap();
    let limiter = std::sync::Mutex::new(BootstrapAuditLimiter::new());
    for _ in 0..30 { audit_validated_bootstrap(&limiter, dir.path(), "refused", "proof_refused"); }
    let events = duduclaw_security::audit::read_recent_events(dir.path(), 100);
    assert_eq!(events.iter().filter(|e| e.event_type == BOOTSTRAP_AUDIT_EVENT).count(), BOOTSTRAP_AUDIT_CAP as usize);
    assert_eq!(events.iter().filter(|e| e.event_type == BOOTSTRAP_AUDIT_SUPPRESSED_EVENT).count(), 1);
    assert_eq!(events.len(), BOOTSTRAP_AUDIT_CAP as usize + 1);
}

fn enroll_totp(state: &AppState, user: &str) -> Vec<String> {
    let now = now();
    let cipher = crate::mfa::Cipher(&state.home_dir);
    let token = state.user_db.mfa_password_challenge(user, PASSWORD, "test-session", now).unwrap();
    state.user_db.mfa_prepare_enrollment(&token, false, now).unwrap();
    let secret = state.user_db.mfa_begin(user, Some(&token), &cipher, now).unwrap();
    state.user_db.mfa_confirm(user, Some(&token), &totp(&secret), &cipher, now).unwrap()
}

/// `users.mfa_reset` over the real dispatcher, inside the dashboard session
/// the WS handshake would establish.
async fn reset_rpc(state: &Arc<AppState>, ctx: &UserContext, session: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION
        .scope(Some((ctx.user_id.clone(), session.to_owned())), state.handler.handle("users.mfa_reset", params, ctx))
        .await
}

fn error_code(frame: &WsFrame) -> Option<String> {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => error.get("code").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

fn reset_events(home: &std::path::Path) -> Vec<duduclaw_security::audit::AuditEvent> {
    duduclaw_security::audit::read_recent_events(home, 1000).into_iter()
        .filter(|e| e.event_type == crate::handlers::RESET_AUDIT_EVENT_FOR_TESTS).collect()
}

fn reasons(home: &std::path::Path) -> Vec<String> {
    reset_events(home).iter().map(|e| e.details["reason"].as_str().unwrap_or_default().to_owned()).collect()
}

async fn admin_ctx(state: &Arc<AppState>, id: &str) -> UserContext {
    let user = state.user_db.get_user(id).unwrap().unwrap();
    authenticate_jwt(state, &state.jwt_config.issue_access_token(&user, &[]).unwrap()).unwrap()
}

#[tokio::test]
async fn admin_reset_rpc_needs_a_second_factor_and_audits_every_attempt() {
    let (_dir, state, owner) = state_with_owner(false).await;
    let codes = enroll_totp(&state, &owner.id);
    let other = state.user_db.create_user("other-admin@example.test", "Other", PASSWORD, UserRole::Admin).unwrap();
    enroll_totp(&state, &other.id);
    let staff = state.user_db.create_user("staff@example.test", "Staff", PASSWORD, UserRole::Employee).unwrap();
    enroll_totp(&state, &staff.id);
    let ctx = admin_ctx(&state, &owner.id).await;
    let session = uuid::Uuid::new_v4().to_string();
    // The password alone never resets an Administrator, whatever the dashboard sends.
    let frame = reset_rpc(&state, &ctx, &session, json!({"user_id": other.id, "password": PASSWORD})).await;
    assert_eq!(error_code(&frame).as_deref(), Some("MFA_RESET_SECOND_FACTOR_REQUIRED"));
    // A fresh factor, but the eligibility log is younger than 7 days.
    let frame = reset_rpc(&state, &ctx, &session, json!({"user_id": other.id, "code": codes[0]})).await;
    assert_eq!(error_code(&frame).as_deref(), Some("MFA_RESET_COOLING"));
    assert!(state.user_db.mfa_active(&other.id).unwrap());
    // Staff keep the password confirmation.
    let frame = reset_rpc(&state, &ctx, &session, json!({"user_id": staff.id, "password": PASSWORD})).await;
    assert!(matches!(frame, WsFrame::Response { ok: true, .. }), "{frame:?}");
    assert!(!state.user_db.mfa_active(&staff.id).unwrap());
    let events = reset_events(&state.home_dir);
    assert_eq!(reasons(&state.home_dir), ["second_factor_required", "cooling_window", "allowed"]);
    assert!(events.iter().all(|e| e.details["actor_user_id"] == json!(owner.id)));
    // One distinct attempt id per attempt; every row delivered and deleted.
    let ids: std::collections::BTreeSet<_> = events.iter().map(|e| e.details["attempt_id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(ids.len(), 3);
    assert!(crate::handlers::reset_outbox_pending_for_tests(&state.user_db).is_empty());
    let text = audit_text(&state.home_dir);
    assert!(!text.contains(PASSWORD) && !text.contains(&codes[0]), "no secret in the audit log");
}

#[tokio::test]
async fn admin_reset_outbox_row_survives_an_audit_failure_and_is_delivered_once() {
    let (_dir, state, _owner) = state_with_owner(false).await;
    // Its own actor, so the per-actor rate limit is not shared with other tests.
    let actor = state.user_db.create_user("outbox-admin@example.test", "Outbox", PASSWORD, UserRole::Admin).unwrap();
    enroll_totp(&state, &actor.id);
    let staff = state.user_db.create_user("staff@example.test", "Staff", PASSWORD, UserRole::Employee).unwrap();
    enroll_totp(&state, &staff.id);
    let ctx = admin_ctx(&state, &actor.id).await;
    let session = uuid::Uuid::new_v4().to_string();
    // The audit file cannot be written (a directory in its place).
    let audit_path = state.home_dir.join("security_audit.jsonl");
    let saved = std::fs::read(&audit_path).ok();
    let _ = std::fs::remove_file(&audit_path);
    std::fs::create_dir(&audit_path).unwrap();
    let frame = reset_rpc(&state, &ctx, &session, json!({"user_id": staff.id, "password": PASSWORD})).await;
    assert!(matches!(frame, WsFrame::Response { ok: true, .. }), "{frame:?}");
    assert!(!state.user_db.mfa_active(&staff.id).unwrap());
    // The reset committed with its outcome row; the failed drain kept it.
    let pending = crate::handlers::reset_outbox_pending_for_tests(&state.user_db);
    assert_eq!(pending.len(), 1);
    assert_eq!((pending[0].outcome.as_str(), pending[0].reason.as_str()), ("allowed", "allowed"));
    let attempt_id = pending[0].attempt_id.clone();
    // Still failing: nothing delivered, nothing lost.
    assert_eq!(crate::handlers::drain_reset_audit_outbox(&state.home_dir, &state.user_db, true), 0);
    assert_eq!(crate::handlers::reset_outbox_pending_for_tests(&state.user_db).len(), 1);
    // The audit file is back: delivered exactly once, then the drain is a no-op.
    std::fs::remove_dir(&audit_path).unwrap();
    if let Some(bytes) = saved { std::fs::write(&audit_path, bytes).unwrap(); }
    assert_eq!(crate::handlers::drain_reset_audit_outbox(&state.home_dir, &state.user_db, true), 1);
    assert_eq!(crate::handlers::drain_reset_audit_outbox(&state.home_dir, &state.user_db, true), 0);
    let delivered: Vec<_> = reset_events(&state.home_dir).into_iter()
        .filter(|e| e.details["attempt_id"] == json!(attempt_id)).collect();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].details["outcome"], "allowed");
    assert!(crate::handlers::reset_outbox_pending_for_tests(&state.user_db).is_empty());
}

#[tokio::test]
async fn admin_reset_every_early_gate_is_recorded() {
    let (_dir, state, _owner) = state_with_owner(false).await;
    // Its own actor, so the per-actor rate limit is not shared with other tests.
    let actor = state.user_db.create_user("gate-admin@example.test", "Gate", PASSWORD, UserRole::Admin).unwrap();
    enroll_totp(&state, &actor.id);
    let staff = state.user_db.create_user("gate-staff@example.test", "Staff", PASSWORD, UserRole::Employee).unwrap();
    let ctx = admin_ctx(&state, &actor.id).await;
    // Not an Administrator (role gate before the MFA handler).
    let staff_ctx = admin_ctx(&state, &staff.id).await;
    let frame = state.handler.handle("users.mfa_reset", json!({"user_id": actor.id, "code": "123456"}), &staff_ctx).await;
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }));
    // Missing target.
    let frame = state.handler.handle("users.mfa_reset", json!({"code": "123456"}), &ctx).await;
    assert_eq!(error_code(&frame).as_deref(), Some("MFA_RESET_INVALID"));
    // Password path without a dashboard session (session lookup failure).
    let frame = state.handler.handle("users.mfa_reset", json!({"user_id": staff.id, "password": PASSWORD}), &ctx).await;
    assert_eq!(error_code(&frame).as_deref(), Some("MFA_RESET_INVALID"));
    // Rate limit (per actor).
    let mut limited = None;
    for _ in 0..8 {
        let frame = state.handler.handle("users.mfa_reset", json!({"user_id": staff.id, "code": "wrong"}), &ctx).await;
        if error_code(&frame).as_deref() == Some("MFA_RESET_RATE_LIMITED") { limited = Some(frame); break; }
    }
    assert!(limited.is_some(), "the per-actor rate limit applies");
    let got = reasons(&state.home_dir);
    for expected in ["actor_not_admin", "invalid_request", "rate_limited"] {
        assert!(got.iter().any(|r| r == expected), "{expected} missing from {got:?}");
    }
    assert_eq!(got.iter().filter(|r| *r == "invalid_request").count(), 2);
    assert!(crate::handlers::reset_outbox_pending_for_tests(&state.user_db).is_empty());

    // Policy store unreadable (no valid policy snapshot for this home): an outbox row.
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(UserDb::new(&dir.path().join("users.db")).unwrap());
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), state.jwt_config.clone()).await;
    let frame = handler.handle_mfa_rpc("users.mfa_reset", json!({"user_id": "someone", "code": "123456"}), &ctx).await;
    assert_eq!(error_code(&frame).as_deref(), Some("MFA_RESET_UNAVAILABLE"));
    assert_eq!(reasons(dir.path()), ["unavailable"]);
    assert!(crate::handlers::reset_outbox_pending_for_tests(&db).is_empty());

    // users.db unavailable: no outbox, the event goes straight to the audit.
    let dir = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let frame = handler.handle_mfa_rpc("users.mfa_reset", json!({"user_id": "someone", "code": "123456"}), &ctx).await;
    assert_eq!(error_code(&frame).as_deref(), Some("MFA_RESET_UNAVAILABLE"));
    let events = reset_events(dir.path());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].details["reason"], "unavailable");
    assert!(events[0].details["attempt_id"].as_str().is_some_and(|id| id.len() == 32));
}
