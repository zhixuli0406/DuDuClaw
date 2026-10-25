//! Isolated OAuth proof: a loopback provider and per-test protected-key
//! homes. No Google account, system keychain, or external write is used.
use super::*;
use crate::person_credentials::{self as vault, Status};
use axum::{
    Form, Json, Router,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use duduclaw_auth::{AccessLevel, User, UserDb, UserRole, UserStatus};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

#[derive(Default)]
struct RevokeDelay {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    exchanges: AtomicUsize,
    revoke_requests: AtomicUsize,
    probe_started: tokio::sync::Notify,
    probe_release: tokio::sync::Notify,
    exchange_started: tokio::sync::Notify,
    exchange_release: tokio::sync::Notify,
    block_exchange: AtomicBool,
}
fn delays() -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<RevokeDelay>>> {
    static DELAYS: OnceLock<std::sync::Mutex<std::collections::HashMap<String, Arc<RevokeDelay>>>> =
        OnceLock::new();
    DELAYS.get_or_init(Default::default)
}
fn delay_for(key: &str) -> Option<Arc<RevokeDelay>> {
    delays().lock().unwrap().get(key).cloned()
}

async fn revoke(Form(form): Form<std::collections::HashMap<String, String>>) -> StatusCode {
    if form
        .get("token")
        .is_some_and(|token| token == "accepted-not-completed")
    {
        return StatusCode::ACCEPTED;
    }
    if let Some(delay) = form
        .get("token")
        .and_then(|token| delay_for(token.trim_start_matches("refresh-")))
    {
        delay.revoke_requests.fetch_add(1, Ordering::SeqCst);
        delay.started.notify_one();
        delay.release.notified().await;
    }
    StatusCode::OK
}

fn provider() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        oauth::test_base()
            .set(format!("http://{}", listener.local_addr().unwrap()))
            .unwrap();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let router = Router::new()
                    .route("/oauth2.googleapis.com/token", post(token))
                    .route("/openidconnect.googleapis.com/v1/userinfo", get(info))
                    .route("/oauth2.googleapis.com/revoke", post(revoke));
                axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), router)
                    .await
                    .unwrap();
            });
        });
    });
}

async fn token(
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    if form.get("refresh_token").is_some_and(|v| v == "invalid") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant"})),
        );
    }
    let who = form.get("code").map(String::as_str).unwrap_or("alice");
    if let Some(delay) = delay_for(who) {
        delay.exchanges.fetch_add(1, Ordering::SeqCst);
        if delay.block_exchange.load(Ordering::SeqCst) {
            delay.exchange_started.notify_one();
            delay.exchange_release.notified().await;
        }
    }
    if who == "fail" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant"})),
        );
    }
    if form.get("grant_type").map(String::as_str) == Some("authorization_code") {
        assert!(form.get("code_verifier").is_some_and(|s| s.len() == 43));
        assert!(
            form.get("redirect_uri")
                .unwrap()
                .ends_with(flow::CALLBACK_PATH)
        );
    }
    (
        StatusCode::OK,
        Json(
            json!({"access_token":format!("access-{who}"),"refresh_token":format!("refresh-{who}"),"token_type":"Bearer","expires_in":3600,"scope":"openid email https://www.googleapis.com/auth/gmail.readonly"}),
        ),
    )
}

async fn info(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let who = headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .trim_start_matches("Bearer access-");
    if let Some(delay) = who.strip_prefix("probe-").and_then(delay_for) {
        delay.probe_started.notify_one();
        delay.probe_release.notified().await;
    }
    if who == "broken" {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"fixture"})),
        );
    }
    (
        StatusCode::OK,
        Json(
            json!({"sub":format!("sub-{who}"),"email":format!("{who}@c.test"),"email_verified":true,"hd":"c.test"}),
        ),
    )
}

pub(super) struct Fixture {
    _dir: tempfile::TempDir,
    pub(super) home: PathBuf,
    pub(super) db: Arc<UserDb>,
    pub(super) alice: User,
}
impl Fixture {
    pub(super) fn new() -> Self {
        provider();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        duduclaw_core::gateway_instance::acquire(&home).unwrap();
        crate::handlers::credential_protection_rpc::protection_for(&home)
            .enable(false)
            .unwrap();
        std::fs::create_dir_all(home.join("agents/bot")).unwrap();
        std::fs::write(
            home.join("agents/bot/agent.toml"),
            "[agent]\nname='bot'\n[google]\nidentity='owner'\n",
        )
        .unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[integrations]\ngoogle_workspace=true\n",
        )
        .unwrap();
        crate::team_edition::record(&home, duduclaw_core::EditionProfile::Enterprise);
        let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let alice = db
            .create_user(
                "login@c.test",
                "Alice",
                "fixture-password-only",
                UserRole::Employee,
            )
            .unwrap();
        db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
        crate::mcp_oauth::upsert_client_config(
            &home,
            crate::mcp_oauth::McpOAuthClientConfig {
                provider_id: "google".into(),
                client_id: "client-fixture".into(),
                client_secret: "secret-fixture".into(),
                auth_url: oauth::AUTHORIZE.into(),
                token_url: oauth::TOKEN.into(),
                scopes: vec![],
                redirect_uri: "http://127.0.0.1:18789/api/mcp/oauth/callback".into(),
            },
        )
        .unwrap();
        Self {
            _dir: dir,
            home,
            db,
            alice,
        }
    }
    pub(super) async fn connect(&self) {
        let start = flow::start(
            &self.home,
            &self.db,
            &self.alice.id,
            "login-a",
            "http://127.0.0.1:18789",
            &[],
        )
        .await
        .unwrap();
        let state = start["state"].as_str().unwrap();
        pending::receive_code(&self.home, state, "alice").unwrap();
        assert!(
            flow::finish(&self.home, &self.db, &self.alice.id, "login-b", state)
                .await
                .is_err()
        );
        assert!(vault::get(&self.home, &self.alice.id).unwrap().is_none());
        flow::finish(&self.home, &self.db, &self.alice.id, "login-a", state)
            .await
            .unwrap();
        assert!(
            flow::finish(&self.home, &self.db, &self.alice.id, "login-a", state)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn session_bound_connect_records_actual_scopes_and_every_call_rechecks_consent_and_status() {
    let f = Fixture::new();
    f.connect().await;
    let record = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    assert_eq!(record.metadata.account_email, "alice@c.test");
    assert_eq!(record.metadata.granted_scopes.len(), 3);
    let text = std::fs::read_to_string(vault::path(&f.home)).unwrap();
    assert!(!text.contains("access-alice") && !text.contains("refresh-alice"));
    let people = crate::google_identity::requester_gate::UserDbPeople(f.db.clone());
    let requester = RequesterIdentity::DashboardUser {
        user_id: f.alice.id.clone(),
    };
    let effective = duduclaw_core::google_identity::effective_for(&f.home, Some("bot"));
    let call = |tool| access::resolve(&f.home, "bot", &requester, &people, &effective, tool);
    assert_eq!(
        call("gmail_read").await.err().unwrap().code,
        "awaiting_consent"
    );
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id)
        .unwrap();
    assert!(call("gmail_read").await.is_ok());
    assert_eq!(
        call("gmail_create_draft").await.err().unwrap().code,
        "scope_missing"
    );
    f.db.set_google_consent(&f.alice.id, "bot", false, &f.alice.id)
        .unwrap();
    assert_eq!(
        call("gmail_read").await.err().unwrap().code,
        "consent_declined"
    );
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id)
        .unwrap();
    f.db.set_user_status(&f.alice.id, UserStatus::Suspended)
        .unwrap();
    assert_eq!(
        call("gmail_read").await.err().unwrap().code,
        "requester_refused"
    );
    // A suspended requester is rejected before account selection/fallback.
    // A host-owned job still reaches the separate selected-person status gate.
    let host = RequesterIdentity::System { kind: "cron".into(), on_behalf_of: None };
    let inactive = access::resolve(&f.home, "bot", &host, &people, &effective, "gmail_read")
        .await.err().unwrap();
    assert_eq!(inactive.code, "person_inactive");
    assert_eq!(vault::get(&f.home, &f.alice.id).unwrap().unwrap().metadata.status, Status::Suspended);
    f.db.set_user_status(&f.alice.id, UserStatus::Active)
        .unwrap();
    assert!(call("gmail_read").await.is_ok());
    f.db.unbind_agent(&f.alice.id, "bot").unwrap();
    assert!(call("gmail_read").await.is_err());
}

#[tokio::test]
async fn unknown_external_requester_needs_explicit_anyone_and_personal_consent_but_public_visitors_never_pass()
 {
    let f = Fixture::new();
    f.connect().await;
    let people = crate::google_identity::requester_gate::UserDbPeople(f.db.clone());
    let default = duduclaw_core::google_identity::effective_for(&f.home, Some("bot"));
    assert_eq!(
        access::resolve(
            &f.home,
            "bot",
            &RequesterIdentity::Unknown,
            &people,
            &default,
            "gmail_read"
        )
        .await
        .err()
        .unwrap()
        .code,
        "requester_refused"
    );
    std::fs::write(
        f.home.join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='anyone'\n",
    )
    .unwrap();
    let anyone = duduclaw_core::google_identity::effective_for(&f.home, Some("bot"));
    assert_eq!(
        access::resolve(
            &f.home,
            "bot",
            &RequesterIdentity::Unknown,
            &people,
            &anyone,
            "gmail_read"
        )
        .await
        .err()
        .unwrap()
        .code,
        "awaiting_consent"
    );
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id)
        .unwrap();
    assert!(
        access::resolve(
            &f.home,
            "bot",
            &RequesterIdentity::Unknown,
            &people,
            &anyone,
            "gmail_read"
        )
        .await
        .is_ok()
    );
    for channel in ["widget", "odoolivechat"] {
        let visitor = RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: "public".into(),
            sender: "visitor".into(),
            user_id: Some(f.alice.id.clone()),
        };
        assert_eq!(
            access::resolve(&f.home, "bot", &visitor, &people, &anyone, "gmail_read")
                .await
                .err()
                .unwrap()
                .code,
            "requester_refused"
        );
    }
}

#[tokio::test]
async fn invalid_grant_transitions_once_and_disconnect_fences_inflight_connection() {
    let f = Fixture::new();
    f.connect().await;
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id)
        .unwrap();
    let key = crate::google_identity::protection_gate(&f.home).unwrap();
    let old = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    let secret = vault::Secret {
        access_token: "expired".into(),
        refresh_token: Some("invalid".into()),
        expires_at: Some(0),
    };
    assert!(vault::finish_refresh(&f.home, &key, &old.metadata, &secret).unwrap());
    let people = crate::google_identity::requester_gate::UserDbPeople(f.db.clone());
    let requester = RequesterIdentity::DashboardUser {
        user_id: f.alice.id.clone(),
    };
    let effective = duduclaw_core::google_identity::effective_for(&f.home, Some("bot"));
    for _ in 0..2 {
        assert_eq!(
            access::resolve(
                &f.home,
                "bot",
                &requester,
                &people,
                &effective,
                "gmail_read"
            )
            .await
            .err()
            .unwrap()
            .code,
            "needs_reconnect"
        );
    }
    assert_eq!(
        vault::get(&f.home, &f.alice.id)
            .unwrap()
            .unwrap()
            .metadata
            .status,
        Status::NeedsReconnect
    );
    assert_eq!(
        f.db.query_audit_log(Some(&f.alice.id), Some("user.google_needs_reconnect"), 10)
            .unwrap()
            .len(),
        1
    );
    let start = flow::start(
        &f.home,
        &f.db,
        &f.alice.id,
        "login-a",
        "http://127.0.0.1:18789",
        &[],
    )
    .await
    .unwrap();
    let state = start["state"].as_str().unwrap();
    pending::receive_code(&f.home, state, "alice").unwrap();
    lifecycle::disconnect(&f.home, &f.db, &f.alice.id, &f.alice.id, false)
        .await
        .unwrap();
    assert!(
        flow::finish(&f.home, &f.db, &f.alice.id, "login-a", state)
            .await
            .is_err()
    );
    assert!(vault::get(&f.home, &f.alice.id).unwrap().is_none());
}

#[tokio::test]
async fn shared_home_and_failed_home_account_probe_never_revoke() {
    let f = Fixture::new();
    f.connect().await;
    for access in ["access-alice", "access-broken"] {
        crate::mcp_oauth::upsert_token(
            &f.home,
            crate::mcp_oauth::McpOAuthToken {
                provider_id: "google".into(),
                access_token: access.into(),
                refresh_token: None,
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                scopes: vec!["openid".into(), "email".into()],
            },
        )
        .unwrap();
        let record = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
        let key = crate::google_identity::protection_gate(&f.home).unwrap();
        let secret = vault::unseal(&key, &record).unwrap();
        // Remove the own row first, exactly as disconnect does.
        vault::disconnect(&f.home, &f.alice.id).unwrap();
        assert!(!lifecycle::safe_revoke(&f.home, &record.metadata, &secret).await);
        let epoch = vault::begin_connection(&f.home, &f.alice.id).unwrap();
        vault::connect(&f.home, &key, record.metadata, &secret, epoch).unwrap();
    }
}

#[tokio::test]
#[cfg(unix)]
async fn delayed_revoke_serializes_same_person_other_person_and_home_exchanges() {
    for kind in ["same_person", "other_person", "home"] {
        let f = Fixture::new();
        let code = format!("race-{}", uuid::Uuid::new_v4());
        let delay = Arc::new(RevokeDelay::default());
        delays().lock().unwrap().insert(code.clone(), delay.clone());
        let start = flow::start(
            &f.home,
            &f.db,
            &f.alice.id,
            "login-a",
            "http://127.0.0.1:18789",
            &[],
        )
        .await
        .unwrap();
        let state = start["state"].as_str().unwrap();
        pending::receive_code(&f.home, state, &code).unwrap();
        flow::finish(&f.home, &f.db, &f.alice.id, "login-a", state)
            .await
            .unwrap();
        let old = vault::get(&f.home, &f.alice.id)
            .unwrap()
            .unwrap()
            .metadata
            .credential_id;
        let revoke_home = f.home.clone();
        let db = f.db.clone();
        let alice = f.alice.id.clone();
        let a = tokio::spawn(async move {
            lifecycle::disconnect(&revoke_home, &db, &alice, &alice, false).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), delay.started.notified())
            .await
            .unwrap();
        assert!(
            vault::get(&f.home, &f.alice.id).unwrap().is_none(),
            "local deletion must precede slow revoke"
        );
        let person = if kind == "other_person" {
            f.db.create_user("bob@c.test", "Bob", "fixture-password", UserRole::Employee)
                .unwrap()
                .id
        } else {
            f.alice.id.clone()
        };
        let home = f.home.clone();
        let db = f.db.clone();
        let code_b = code.clone();
        let person_b = person.clone();
        let ready = Arc::new(tokio::sync::Notify::new());
        let ready_b = ready.clone();
        let b = tokio::spawn(async move {
            ready_b.notify_one();
            if kind == "home" {
                let config = crate::mcp_oauth::McpOAuthConfig {
                    provider_id: "google".into(),
                    client_id: "client-fixture".into(),
                    client_secret: "secret-fixture".into(),
                    auth_url: oauth::AUTHORIZE.into(),
                    token_url: format!(
                        "{}/oauth2.googleapis.com/token",
                        oauth::test_base().get().unwrap()
                    ),
                    scopes: vec!["openid".into()],
                    redirect_uri: "http://127.0.0.1:18789/api/me/google/callback".into(),
                };
                crate::mcp_oauth::exchange_and_store(&home, &config, &code_b, &"x".repeat(43)).await
            } else {
                let start = flow::start(
                    &home,
                    &db,
                    &person_b,
                    "login-b",
                    "http://127.0.0.1:18789",
                    &[],
                )
                .await?;
                let state = start["state"].as_str().unwrap();
                pending::receive_code(&home, state, &code_b)?;
                flow::finish(&home, &db, &person_b, "login-b", state)
                    .await
                    .map(|_| ())
            }
        });
        ready.notified().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !b.is_finished(),
            "{kind} must wait for revoke HTTP completion"
        );
        assert_eq!(
            delay.exchanges.load(Ordering::SeqCst),
            1,
            "{kind} exchanged before revoke finished"
        );
        delay.release.notify_one();
        assert!(a.await.unwrap().unwrap());
        b.await.unwrap().unwrap();
        assert_eq!(delay.exchanges.load(Ordering::SeqCst), 2);
        if kind == "home" {
            assert!(crate::mcp_oauth::get_token(&f.home, "google").is_some());
        } else {
            assert_ne!(
                vault::get(&f.home, &person)
                    .unwrap()
                    .unwrap()
                    .metadata
                    .credential_id,
                old
            );
        }
        delays().lock().unwrap().remove(&code);
    }
}

#[tokio::test]
#[cfg(unix)]
async fn cancelled_provider_revoke_leaves_new_personal_and_home_grants_closed() {
    let f = Fixture::new();
    let code = format!("race-cancel-{}", uuid::Uuid::new_v4());
    let delay = Arc::new(RevokeDelay::default());
    delays().lock().unwrap().insert(code.clone(), delay.clone());
    let start = flow::start(
        &f.home,
        &f.db,
        &f.alice.id,
        "login-a",
        "http://127.0.0.1:18789",
        &[],
    )
    .await
    .unwrap();
    let state = start["state"].as_str().unwrap();
    pending::receive_code(&f.home, state, &code).unwrap();
    flow::finish(&f.home, &f.db, &f.alice.id, "login-a", state)
        .await
        .unwrap();
    let home = f.home.clone();
    let db = f.db.clone();
    let user = f.alice.id.clone();
    let disconnect =
        tokio::spawn(async move { lifecycle::disconnect(&home, &db, &user, &user, false).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), delay.started.notified())
        .await
        .unwrap();
    assert!(vault::get(&f.home, &f.alice.id).unwrap().is_none());
    disconnect.abort();
    let _ = disconnect.await;
    assert!(
        flow::start(
            &f.home,
            &f.db,
            &f.alice.id,
            "new-login",
            "http://127.0.0.1:18789",
            &[]
        )
        .await
        .is_err()
    );
    let config = crate::mcp_oauth::McpOAuthConfig {
        provider_id: "google".into(),
        client_id: "client-fixture".into(),
        client_secret: "secret-fixture".into(),
        auth_url: oauth::AUTHORIZE.into(),
        token_url: format!(
            "{}/oauth2.googleapis.com/token",
            oauth::test_base().get().unwrap()
        ),
        scopes: vec!["openid".into()],
        redirect_uri: "http://127.0.0.1:18789/api/me/google/callback".into(),
    };
    assert!(
        crate::mcp_oauth::exchange_and_store(&f.home, &config, &code, &"x".repeat(43))
            .await
            .is_err()
    );
    assert_eq!(delay.exchanges.load(Ordering::SeqCst), 1);
    delay.release.notify_one();
    assert!(
        f.home
            .join("locks/google-grant-revoke-uncertain.json")
            .exists()
    );
    delays().lock().unwrap().remove(&code);
}

#[tokio::test]
async fn accepted_202_is_not_a_completed_google_revocation() {
    provider();
    assert!(!oauth::revoke("accepted-not-completed").await);
}

async fn prepare_delayed_case(f: &Fixture) -> (String, Arc<RevokeDelay>) {
    let code = format!("race-{}", uuid::Uuid::new_v4());
    let delay = Arc::new(RevokeDelay::default());
    delays().lock().unwrap().insert(code.clone(), delay.clone());
    let start = flow::start(
        &f.home,
        &f.db,
        &f.alice.id,
        "login-a",
        "http://127.0.0.1:18789",
        &[],
    )
    .await
    .unwrap();
    let state = start["state"].as_str().unwrap();
    pending::receive_code(&f.home, state, &code).unwrap();
    flow::finish(&f.home, &f.db, &f.alice.id, "login-a", state)
        .await
        .unwrap();
    (code, delay)
}

#[tokio::test]
#[cfg(unix)]
async fn home_account_probe_and_revoke_share_one_lease_before_new_connect() {
    let f = Fixture::new();
    let (code, delay) = prepare_delayed_case(&f).await;
    crate::mcp_oauth::upsert_token(
        &f.home,
        crate::mcp_oauth::McpOAuthToken {
            provider_id: "google".into(),
            access_token: format!("access-probe-{code}"),
            refresh_token: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            scopes: vec!["openid".into()],
        },
    )
    .unwrap();
    let home = f.home.clone();
    let db = f.db.clone();
    let user = f.alice.id.clone();
    let a =
        tokio::spawn(async move { lifecycle::disconnect(&home, &db, &user, &user, false).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        delay.probe_started.notified(),
    )
    .await
    .unwrap();
    assert!(vault::get(&f.home, &f.alice.id).unwrap().is_none());
    let home = f.home.clone();
    let db = f.db.clone();
    let user = f.alice.id.clone();
    let b = tokio::spawn(async move {
        flow::start(
            &home,
            &db,
            &user,
            "next-login",
            "http://127.0.0.1:18789",
            &[],
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    assert!(!b.is_finished());
    delay.probe_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), delay.started.notified())
        .await
        .unwrap();
    assert!(
        !b.is_finished(),
        "lease must continue from account probe through revoke HTTP"
    );
    delay.release.notify_one();
    assert!(a.await.unwrap().unwrap());
    b.await.unwrap().unwrap();
    delays().lock().unwrap().remove(&code);
}

#[tokio::test]
#[cfg(unix)]
async fn revoke_waits_for_inflight_other_person_exchange_then_preserves_its_shared_grant() {
    let f = Fixture::new();
    let (code, delay) = prepare_delayed_case(&f).await;
    let bob =
        f.db.create_user("bob@c.test", "Bob", "fixture-password", UserRole::Employee)
            .unwrap();
    let start = flow::start(
        &f.home,
        &f.db,
        &bob.id,
        "bob-login",
        "http://127.0.0.1:18789",
        &[],
    )
    .await
    .unwrap();
    let state = start["state"].as_str().unwrap().to_owned();
    pending::receive_code(&f.home, &state, &code).unwrap();
    delay.block_exchange.store(true, Ordering::SeqCst);
    let home = f.home.clone();
    let db = f.db.clone();
    let bob_id = bob.id.clone();
    let b =
        tokio::spawn(async move { flow::finish(&home, &db, &bob_id, "bob-login", &state).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        delay.exchange_started.notified(),
    )
    .await
    .unwrap();
    let home = f.home.clone();
    let db = f.db.clone();
    let user = f.alice.id.clone();
    let a =
        tokio::spawn(async move { lifecycle::disconnect(&home, &db, &user, &user, false).await });
    tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    assert!(vault::get(&f.home, &f.alice.id).unwrap().is_none());
    assert!(!a.is_finished());
    delay.exchange_release.notify_one();
    b.await.unwrap().unwrap();
    assert!(
        !a.await.unwrap().unwrap(),
        "a published shared grant must prevent provider revoke"
    );
    assert!(vault::get(&f.home, &bob.id).unwrap().is_some());
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            delay.started.notified()
        )
        .await
        .is_err()
    );
    delays().lock().unwrap().remove(&code);
}

#[tokio::test]
#[cfg(unix)]
async fn parent_directory_sync_failure_never_sends_provider_revoke() {
    let f = Fixture::new();
    let (code, delay) = prepare_delayed_case(&f).await;
    grants::fail_home_sync(&f.home, true);
    assert!(
        !lifecycle::disconnect(&f.home, &f.db, &f.alice.id, &f.alice.id, false)
            .await
            .unwrap()
    );
    grants::fail_home_sync(&f.home, false);
    assert!(vault::get(&f.home, &f.alice.id).unwrap().is_none());
    assert_eq!(delay.revoke_requests.load(Ordering::SeqCst), 0);
    assert!(
        f.home
            .join("locks/google-grant-revoke-uncertain.json")
            .exists()
    );
    assert!(grants::acquire(&f.home).await.is_err());
    delays().lock().unwrap().remove(&code);
}
