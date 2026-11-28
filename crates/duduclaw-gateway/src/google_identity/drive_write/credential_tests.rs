use super::credential::*;
use crate::google_personal::oauth;
use crate::person_credentials::{self as vault, write_store as store, Secret, Status};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_security::credential_protection::ProtectedKey;
use serde_json::json;
use std::time::Duration;

struct Fixture {
    home: tempfile::TempDir,
    key: ProtectedKey,
    db: UserDb,
    user: String,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        crate::handlers::credential_protection_rpc::protection_for(home.path()).enable(false).unwrap();
        let key = crate::google_identity::protection_gate(home.path()).unwrap();
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("config.toml"), "[integrations]\ngoogle_workspace=true\n").unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("owner@c.test", "Owner", "fixture-password", UserRole::Employee).unwrap();
        db.bind_agent(&user.id, "bot", AccessLevel::Owner).unwrap();
        db.set_google_consent(&user.id, "bot", true, &user.id).unwrap();
        let f = Self { home, key, db, user: user.id };
        f.selector("owner");
        crate::mcp_oauth::upsert_client_config(f.home.path(), crate::mcp_oauth::McpOAuthClientConfig {
            provider_id: "google".into(), client_id: "client-1".into(), client_secret: "client-secret".into(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(), scopes: vec![],
            redirect_uri: "http://127.0.0.1:18889/api/mcp/oauth/callback".into(),
        }).unwrap();
        let mut ordinary = serde_json::to_value(f.metadata("user")).unwrap();
        ordinary.as_object_mut().unwrap().remove("purpose");
        ordinary["granted_scopes"] = json!(["openid", "email"]);
        let epoch = vault::begin_connection(f.home.path(), &f.user).unwrap();
        vault::connect(f.home.path(), &f.key, serde_json::from_value(ordinary).unwrap(), &secret(false), epoch).unwrap();
        f
    }
    fn selector(&self, selector: &str) {
        let fallback = if selector == "delegate:owner" { "none" } else { "home" };
        std::fs::write(self.home.path().join("agents/bot/agent.toml"),
            format!("[google]\nidentity='{selector}'\nfallback='{fallback}'\n")).unwrap();
    }
    fn route(&self) -> IdentityRoute {
        IdentityRoute::Personal { owner_user_id: self.user.clone(), account_email: "linked@c.test".into() }
    }
    fn metadata(&self, kind: &str) -> store::Metadata {
        store::Metadata {
            owner_kind: kind.into(), owner_id: if kind == "home" { "home".into() } else { self.user.clone() },
            purpose: "drive_write".into(), provider: "google".into(),
            credential_id: uuid::Uuid::new_v4().to_string(), revision: 1,
            account_email: "linked@c.test".into(), google_sub: "google-sub-1".into(), hosted_domain: Some("c.test".into()),
            granted_scopes: vec![oauth::DRIVE_WRITE_SCOPE.into(), "openid".into(), "email".into()],
            status: Status::Connected, status_before_suspension: None,
            connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None,
            oauth_client_id: "client-1".into(), google_data_consent: Some(crate::google_data_consent::Evidence::issue(&self.user).unwrap()),
        }
    }
    fn connect(&self, metadata: store::Metadata, expired: bool) {
        let epoch = store::begin_connection(self.home.path(), &metadata.owner_kind, &metadata.owner_id).unwrap();
        store::connect(self.home.path(), &self.key, metadata, &secret(expired), epoch).unwrap();
    }
}
fn secret(expired: bool) -> Secret {
    Secret { access_token: "write-fixture-secret".into(), refresh_token: Some("write-refresh-secret".into()),
        expires_at: Some(if expired { chrono::Utc::now().timestamp() + 120 } else { 4_000_000_000 }) }
}
fn refusal<T>(result: Result<T, crate::google_identity::Refusal>, code: &str) {
    let error = result.err().expect("must refuse");
    assert_eq!(error.code, code);
    assert!(!error.fallback_eligible);
    assert!(crate::google_identity::REFUSAL_CODES.contains(&error.code));
    assert!(!error.message_zh.contains("write-fixture-secret"));
    assert!(!error.message_zh.contains("write-refresh-secret"));
}

#[test]
fn routes_use_gateway_holder_and_selected_home_path() {
    let f = Fixture::new();
    assert_eq!(resolve_route(f.home.path(), "bot").unwrap(), f.route());
    f.selector(&format!("user:{}", f.user));
    assert_eq!(resolve_route(f.home.path(), "bot").unwrap(), f.route());
    f.selector("delegate:fixed@c.test");
    assert_eq!(resolve_route(f.home.path(), "bot").unwrap(), IdentityRoute::Delegate { email: "fixed@c.test".into() });
    for selector in ["none", "external", "unknown"] {
        f.selector(selector);
        assert_eq!(resolve_route(f.home.path(), "bot").unwrap(), IdentityRoute::Unsupported);
    }
    f.selector("home");
    for (path, section, expected) in [
        ("oauth", "", IdentityRoute::HomeOAuth),
        ("service_account", "[integrations.google_service_account]\nkey_file='sa.json'\nsubject='company@c.test'\n", IdentityRoute::HomeServiceAccount),
        ("apps_script", "[integrations.google_apps_script]\nurl='https://script.google.com/x'\n", IdentityRoute::Unsupported),
        ("unknown", "", IdentityRoute::Unsupported),
        ("none", "", IdentityRoute::Unsupported),
    ] {
        std::fs::write(f.home.path().join("config.toml"), format!(
            "[integrations]\ngoogle_workspace=true\ngoogle_credential_path='{path}'\n{section}"
        )).unwrap();
        assert_eq!(resolve_route(f.home.path(), "bot").unwrap(), expected);
    }
}

#[test]
fn delegate_owner_requires_current_attestation_and_uses_delegation() {
    let f = Fixture::new();
    f.selector("delegate:owner");
    refusal(resolve_route(f.home.path(), "bot"), "attestation_required");
    let admin = f.db.create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
    f.db.ensure_google_employee_nonce("bot").unwrap();
    let employee = crate::google_identity::delegate_owner::incarnation(f.home.path(), "bot").unwrap();
    f.db.attest_google_delegate_owner(&duduclaw_auth::google_delegate_attestation::AttestationRequest {
        agent_id: "bot", employee_id: &hex::encode(employee.0.0), employee_incarnation: employee.1,
        subject: "owner@c.test", user_id: &f.user, acknowledge_self_set: true,
    }, &|email| email == "owner@c.test", &admin.id).unwrap();
    let route = resolve_route(f.home.path(), "bot").unwrap();
    assert_eq!(route, IdentityRoute::DelegateOwner { owner_user_id: f.user.clone(), email: "owner@c.test".into() });
    assert_eq!(readiness(f.home.path(), &route), Readiness::Ready);
    assert!(store::get(f.home.path(), "user", &f.user).unwrap().is_none());
}

#[test]
fn readiness_requires_connected_write_purpose_exact_scope_and_same_sub() {
    let f = Fixture::new();
    let route = f.route();
    let needs = Readiness::NeedsMemberConsent { user_id: f.user.clone() };
    assert_eq!(readiness(f.home.path(), &route), needs);
    f.connect(f.metadata("user"), false);
    assert_eq!(readiness(f.home.path(), &route), Readiness::Ready);
    let valid = std::fs::read(store::path(f.home.path())).unwrap();
    for (field, value) in [
        ("status", json!("needs_reconnect")), ("purpose", json!("base")),
        ("granted_scopes", json!(["https://www.googleapis.com/auth/drive.readonly"])),
        ("granted_scopes", json!(["https://www.googleapis.com/auth/drive.file"])),
        ("google_sub", json!("another-sub")),
    ] {
        let mut document: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        document["records"][0][field] = value;
        std::fs::write(store::path(f.home.path()), serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(readiness(f.home.path(), &route), needs, "{field}");
    }
    std::fs::write(store::path(f.home.path()), valid).unwrap();
    assert_eq!(readiness(f.home.path(), &IdentityRoute::HomeOAuth), Readiness::NeedsHomeConsent);
    f.connect(f.metadata("home"), false);
    assert_eq!(readiness(f.home.path(), &IdentityRoute::HomeOAuth), Readiness::Ready);
    for route in [IdentityRoute::Delegate { email: "a@c.test".into() }, IdentityRoute::HomeServiceAccount] {
        assert_eq!(readiness(f.home.path(), &route), Readiness::Ready);
    }
    assert_eq!(readiness(f.home.path(), &IdentityRoute::Unsupported), Readiness::Unsupported);
}

#[tokio::test]
async fn fresh_personal_and_home_tokens_stay_in_their_vault_namespace() {
    let f = Fixture::new();
    f.connect(f.metadata("user"), false);
    let token = write_token(f.home.path(), &f.route()).await.unwrap();
    assert_eq!(token.bearer.as_str(), "write-fixture-secret");
    assert_eq!(token.meta.identity_kind, "personal");
    assert_eq!(token.meta.account_email, "linked@c.test");
    assert_eq!(token.meta.credential_revision, Some(1));
    refusal(write_token(f.home.path(), &IdentityRoute::HomeOAuth).await, "drive_write_needs_home_consent");
    f.connect(f.metadata("home"), false);
    let home = write_token(f.home.path(), &IdentityRoute::HomeOAuth).await.unwrap();
    assert_eq!(home.meta.identity_kind, "home_oauth");
    store::disconnect(f.home.path(), "user", &f.user).unwrap();
    refusal(write_token(f.home.path(), &f.route()).await, "drive_write_needs_member_consent");
    refusal(write_token(f.home.path(), &IdentityRoute::Unsupported).await, "drive_write_unsupported_identity");
    refusal(write_token(f.home.path(), &IdentityRoute::Delegate { email: "nobody@c.test".into() }).await, "delegate_unavailable");
}

async fn endpoint(home: &std::path::Path, response: serde_json::Value) ->
    (oauth::mcp_refresh_test::Guard, tokio::task::JoinHandle<()>) {
    let router = axum::Router::new().route("/token", axum::routing::post(move || {
        let response = response.clone();
        async move { axum::Json(response) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guard = oauth::mcp_refresh_test::install(home, &format!("http://{}/token", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
    (guard, server)
}

#[tokio::test]
async fn expired_refresh_requires_verified_drive_scope_and_persists_rotation() {
    let f = Fixture::new();
    let ordinary = std::fs::read(vault::path(f.home.path())).unwrap();
    for (scope, expected) in [
        (None, Some("scope_unverified")),
        (Some("https://www.googleapis.com/auth/drive.file"), Some("drive_write_needs_member_consent")),
        (Some(oauth::DRIVE_WRITE_SCOPE), None),
    ] {
        f.connect(f.metadata("user"), true);
        let mut response = json!({"access_token":"new-write-secret", "expires_in":3600, "token_type":"Bearer"});
        if let Some(scope) = scope { response["scope"] = json!(scope); }
        let (_guard, server) = endpoint(f.home.path(), response).await;
        let result = write_token(f.home.path(), &f.route()).await;
        let stored = store::get(f.home.path(), "user", &f.user).unwrap().unwrap();
        if let Some(code) = expected {
            refusal(result, code);
            assert_eq!(stored.metadata.status, if code == "scope_unverified" { Status::Connected } else { Status::NeedsReconnect });
            assert_eq!(store::unseal(&f.key, &stored).unwrap().access_token, "write-fixture-secret");
        } else {
            let token = result.unwrap();
            assert_eq!(token.bearer.as_str(), "new-write-secret");
            assert_eq!(token.meta.credential_revision, Some(2));
            assert_eq!(stored.metadata.revision, 2);
            let saved = store::unseal(&f.key, &stored).unwrap();
            assert_eq!(saved.access_token, "new-write-secret");
            assert_eq!(saved.refresh_token.as_deref(), Some("write-refresh-secret"));
        }
        server.abort();
    }
    assert_eq!(std::fs::read(vault::path(f.home.path())).unwrap(), ordinary);
}

#[tokio::test]
async fn client_id_mismatch_refuses_even_fresh_tokens() {
    let f = Fixture::new();
    for expired in [false, true] {
        let mut metadata = f.metadata("user");
        metadata.oauth_client_id = "another-client".into();
        f.connect(metadata, expired);
        refusal(write_token(f.home.path(), &f.route()).await, "changed");
    }
}

#[tokio::test]
async fn refresh_cas_refuses_a_concurrent_disconnect() {
    let f = Fixture::new();
    f.connect(f.metadata("user"), true);
    let home = f.home.path().to_path_buf();
    let user = f.user.clone();
    let router = axum::Router::new().route("/token", axum::routing::post(move || {
        store::disconnect(&home, "user", &user).unwrap();
        async { axum::Json(json!({"access_token":"discard-me", "expires_in":3600, "token_type":"Bearer", "scope":oauth::DRIVE_WRITE_SCOPE})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _guard = oauth::mcp_refresh_test::install(f.home.path(), &format!("http://{}/token", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
    refusal(write_token(f.home.path(), &f.route()).await, "changed");
    assert!(store::get(f.home.path(), "user", &f.user).unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn insufficient_permissions_only_invalidates_the_used_revision() {
    let f = Fixture::new();
    for (kind, route) in [("user", f.route()), ("home", IdentityRoute::HomeOAuth)] {
        f.connect(f.metadata(kind), false);
        let token = write_token(f.home.path(), &route).await.unwrap();
        on_insufficient_permissions(f.home.path(), &route, &token.meta).await.unwrap();
        assert_ne!(readiness(f.home.path(), &route), Readiness::Ready);
        f.connect(f.metadata(kind), false);
        refusal(on_insufficient_permissions(f.home.path(), &route, &token.meta).await, "changed");
        assert_eq!(readiness(f.home.path(), &route), Readiness::Ready);
    }
}

#[tokio::test]
async fn home_consent_reconnect_and_disconnect_reject_old_oauth_metadata() {
    let f = crate::google_personal::home_write_tests::Fixture::new().await;
    let route = IdentityRoute::HomeOAuth;
    let start = f.start().await.unwrap();
    f.finish(&start, "write-alice").await.unwrap();
    let old = write_token(f.home(), &route).await.unwrap();
    validate_oauth_token(f.home(), &route, &old.meta).unwrap();
    let reconnect = f.start().await.unwrap();
    f.finish(&reconnect, "write-alice").await.unwrap();
    let new = write_token(f.home(), &route).await.unwrap();
    assert_eq!(old.meta.credential_revision, new.meta.credential_revision);
    refusal(validate_oauth_token(f.home(), &route, &old.meta), "changed");
    validate_oauth_token(f.home(), &route, &new.meta).unwrap();
    f.disconnect().await.unwrap();
    refusal(validate_oauth_token(f.home(), &route, &new.meta), "changed");
}

#[tokio::test]
async fn home_refresh_and_scope_loss_wait_for_authority_reader() {
    use duduclaw_core::authority_coordinator::AuthorityCoordinator;
    for scope in [oauth::DRIVE_WRITE_SCOPE, "https://www.googleapis.com/auth/drive.file"] {
        let f = Fixture::new();
        f.connect(f.metadata("home"), true);
        let (_guard, server) = endpoint(f.home.path(), json!({
            "access_token":"new-write-secret", "expires_in":3600, "token_type":"Bearer", "scope":scope,
        })).await;
        let coordinator = AuthorityCoordinator::new(f.home.path());
        coordinator.initialize().unwrap();
        let epoch = coordinator.epoch().unwrap();
        let reader = coordinator.shared().await.unwrap();
        let before = store::get(f.home.path(), "home", "home").unwrap().unwrap().metadata;
        let refresh = write_token(f.home.path(), &IdentityRoute::HomeOAuth);
        tokio::pin!(refresh);
        assert!(tokio::time::timeout(Duration::from_millis(200), &mut refresh).await.is_err());
        assert_eq!(store::get(f.home.path(), "home", "home").unwrap().unwrap().metadata, before);
        assert_eq!(coordinator.epoch().unwrap(), epoch);
        drop(reader);
        let result = tokio::time::timeout(Duration::from_secs(5), &mut refresh).await.unwrap();
        if scope == oauth::DRIVE_WRITE_SCOPE {
            let token = result.unwrap();
            validate_oauth_token(f.home.path(), &IdentityRoute::HomeOAuth, &token.meta).unwrap();
            assert_eq!(token.meta.credential_revision, Some(2));
        } else {
            refusal(result, "drive_write_needs_member_consent");
            assert_eq!(store::get(f.home.path(), "home", "home").unwrap().unwrap().metadata.status, Status::NeedsReconnect);
        }
        assert_ne!(coordinator.epoch().unwrap(), epoch);
        server.abort();
    }
}

#[tokio::test]
async fn home_insufficient_permissions_waits_for_authority_reader() {
    use duduclaw_core::authority_coordinator::AuthorityCoordinator;
    let f = Fixture::new();
    f.connect(f.metadata("home"), false);
    let route = IdentityRoute::HomeOAuth;
    let token = write_token(f.home.path(), &route).await.unwrap();
    let coordinator = AuthorityCoordinator::new(f.home.path());
    coordinator.initialize().unwrap();
    let epoch = coordinator.epoch().unwrap();
    let reader = coordinator.shared().await.unwrap();
    let invalidate = on_insufficient_permissions(f.home.path(), &route, &token.meta);
    tokio::pin!(invalidate);
    assert!(tokio::time::timeout(Duration::from_millis(200), &mut invalidate).await.is_err());
    validate_oauth_token(f.home.path(), &route, &token.meta).unwrap();
    drop(reader);
    tokio::time::timeout(Duration::from_secs(5), &mut invalidate).await.unwrap().unwrap();
    refusal(validate_oauth_token(f.home.path(), &route, &token.meta), "changed");
    assert_ne!(coordinator.epoch().unwrap(), epoch);
}

#[tokio::test]
async fn home_insufficient_permissions_survives_caller_cancellation() {
    use duduclaw_core::authority_coordinator::AuthorityCoordinator;
    use std::future::Future;
    use std::task::Poll;
    let f = Fixture::new();
    f.connect(f.metadata("home"), false);
    let route = IdentityRoute::HomeOAuth;
    let token = write_token(f.home.path(), &route).await.unwrap();
    let coordinator = AuthorityCoordinator::new(f.home.path());
    coordinator.initialize().unwrap();
    let epoch = coordinator.epoch().unwrap();
    let reader = coordinator.shared().await.unwrap();
    let before = store::get(f.home.path(), "home", "home").unwrap().unwrap().metadata;
    let mut invalidate = Box::pin(on_insufficient_permissions(f.home.path(), &route, &token.meta));
    // The first poll submits the blocking worker, then waits for its lease.
    std::future::poll_fn(|cx| {
        assert!(invalidate.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
    drop(invalidate);
    assert_eq!(store::get(f.home.path(), "home", "home").unwrap().unwrap().metadata, before);
    assert_eq!(coordinator.epoch().unwrap(), epoch);
    drop(reader);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let saved = store::get(f.home.path(), "home", "home").unwrap().unwrap().metadata;
            if saved.status == Status::NeedsReconnect {
                assert_eq!(saved.credential_id, before.credential_id);
                assert_eq!(saved.revision, before.revision + 1);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_ne!(coordinator.epoch().unwrap(), epoch);
}

fn channel(channel: &str, user: Option<&str>) -> RequesterIdentity {
    RequesterIdentity::ChannelUser { channel: channel.into(), chat: "chat".into(), sender: "sender".into(),
        user_id: user.map(str::to_owned), attested_private: false }
}

#[test]
fn requester_matrix_rejects_visitors_nonholders_and_unattended_approval() {
    let routes = [
        IdentityRoute::Personal { owner_user_id: "holder".into(), account_email: "a@c.test".into() },
        IdentityRoute::DelegateOwner { owner_user_id: "holder".into(), email: "a@c.test".into() },
        IdentityRoute::Delegate { email: "a@c.test".into() }, IdentityRoute::HomeOAuth, IdentityRoute::HomeServiceAccount,
    ];
    for (index, route) in routes.iter().enumerate() {
        for approval in [false, true] {
            for member in ["holder", "other"] {
                for who in [RequesterIdentity::DashboardUser { user_id: member.into() }, channel("line", Some(member))] {
                    let result = requester_check(route, &who, approval);
                    if index < 2 && member == "other" { refusal(result, "requester_not_account_holder"); }
                    else { assert_eq!(result.unwrap(), RequesterOutcome::LiveMember); }
                }
            }
            for public in ["widget", "odoolivechat"] {
                for user in [None, Some("holder")] {
                    let result = requester_check(route, &channel(public, user), approval);
                    assert_eq!(result.as_ref().unwrap_err().message_zh, "這個對話無法執行雲端硬碟寫入，請帳號本人登入後提出操作。");
                    refusal(result, "public_visitor");
                }
            }
            for behalf in [None, Some("holder"), Some("other")] {
                let result = requester_check(route, &RequesterIdentity::System {
                    kind: "cron".into(), on_behalf_of: behalf.map(str::to_owned),
                }, approval);
                assert_eq!(RequesterOutcome::notify_unattended(&result), approval);
                if approval { refusal(result, "approval_unavailable_unattended"); }
                else if index < 2 && behalf != Some("holder") { refusal(result, "requester_not_account_holder"); }
                else { assert_eq!(result.unwrap(), RequesterOutcome::Unattended); }
            }
            refusal(requester_check(route, &RequesterIdentity::Unknown, approval), "requester_refused");
            refusal(requester_check(route, &channel("line", None), approval), "requester_refused");
        }
    }
}

#[tokio::test]
async fn unattended_notice_targets_primary_owner_once_per_hour_without_fallback() {
    let f = Fixture::new();
    notify_unattended(f.home.path(), "bot").await;
    notify_unattended(f.home.path(), "bot").await;
    let notices = crate::notify_push::person_notice_test::take(f.home.path());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, f.user);
    notify_unattended(f.home.path(), "missing").await;
    assert!(crate::notify_push::person_notice_test::take(f.home.path()).is_empty());
}

#[tokio::test]
async fn home_oauth_readiness_becomes_ready_after_admin_consent() {
    let f = crate::google_personal::home_write_tests::Fixture::new().await;
    assert_eq!(readiness(f.home(), &IdentityRoute::HomeOAuth), Readiness::NeedsHomeConsent);
    let start = f.start().await.unwrap();
    f.finish(&start, "write-alice").await.unwrap();
    assert_eq!(readiness(f.home(), &IdentityRoute::HomeOAuth), Readiness::Ready);
    let token = write_token(f.home(), &IdentityRoute::HomeOAuth).await.unwrap();
    assert_eq!(token.meta.identity_kind, "home_oauth");
    assert_eq!(token.meta.account_email, "alice@c.test");
    store::disconnect(f.home(), "home", "home").unwrap();
    assert_eq!(readiness(f.home(), &IdentityRoute::HomeOAuth), Readiness::NeedsHomeConsent);
}
