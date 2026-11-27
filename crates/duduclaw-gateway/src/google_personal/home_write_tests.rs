//! Real consent pipeline with the isolated loopback provider. No live Google.
use super::{flow, home_write, oauth, pending, tests};
use crate::person_credentials::{self as vault, write_store, Status};
use duduclaw_auth::{User, UserRole};
use serde_json::{json, Value};

pub(crate) struct Fixture {
    personal: tests::Fixture,
    admin: User,
}
impl Fixture {
    pub(crate) async fn new() -> Self {
        let personal = tests::Fixture::new();
        let admin = personal.db.create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
        let (state, pending, _endpoint) = tests::bound_home_before_personal_capture(&personal).await;
        crate::mcp_oauth::home_lifecycle::complete_google_home_authorization(
            &personal.home, pending, &state, zeroize::Zeroizing::new("home-alice".into()),
        ).await.unwrap();
        let f = Self { personal, admin };
        f.enable_write();
        f
    }
    pub(crate) fn home(&self) -> &std::path::Path { &self.personal.home }
    fn enable_write(&self) {
        use crate::google_workspace::drive_write_policy as policy;
        let mut enabled = policy::DriveWritePolicy::all_off();
        enabled.create = true;
        let ack = policy::sign(&enabled, &self.admin.id, &self.admin.email, chrono::Utc::now());
        let mut config: toml::Value = std::fs::read_to_string(self.home().join("config.toml")).unwrap().parse().unwrap();
        let table = config.as_table_mut().unwrap().entry("google").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        table.as_table_mut().unwrap().insert("drive_write".into(), policy::render_section(&enabled, &ack));
        std::fs::write(self.home().join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
    }
    pub(crate) async fn start(&self) -> Result<Value, String> { self.start_as(&self.admin.id, true, true).await }
    async fn start_as(&self, user: &str, data: bool, write: bool) -> Result<Value, String> {
        home_write::start(self.home(), &self.personal.db, user, "admin-session",
            "http://127.0.0.1:18889", &[], data, write).await
    }
    pub(crate) async fn finish(&self, start: &Value, code: &str) -> Result<Value, String> {
        let state = start["state"].as_str().unwrap();
        pending::receive_code(self.home(), state, code).unwrap();
        flow::finish_home(self.home(), &self.personal.db, &self.admin.id, "admin-session", state).await
    }
    fn ordinary(&self) -> Vec<u8> { std::fs::read(self.home().join(crate::mcp_oauth::TOKEN_FILE)).unwrap() }
}

#[tokio::test]
async fn home_write_start_requires_admin_oauth_policy_ack_and_existing_connection() {
    let f = Fixture::new().await;
    let ordinary = f.ordinary();
    assert!(f.start_as(&f.personal.alice.id, true, true).await.is_err());
    assert!(home_write::disconnect(f.home(), &f.personal.db, &f.personal.alice.id).is_err());
    for (data, write) in [(false, false), (false, true), (true, false)] {
        assert!(f.start_as(&f.admin.id, data, write).await.is_err());
    }
    let config = std::fs::read_to_string(f.home().join("config.toml")).unwrap();
    for route in ["none", "apps_script", "service_account"] {
        let mut changed: toml::Value = config.parse().unwrap();
        changed["integrations"]["google_credential_path"] = toml::Value::String(route.into());
        std::fs::write(f.home().join("config.toml"), toml::to_string(&changed).unwrap()).unwrap();
        assert!(f.start().await.is_err());
    }
    std::fs::write(f.home().join("config.toml"), "[integrations]\ngoogle_workspace=true\ngoogle_credential_path='oauth'\n").unwrap();
    assert!(f.start().await.unwrap_err().contains("管理員還沒開放"));
    std::fs::write(f.home().join("config.toml"), config).unwrap();
    assert_eq!(f.ordinary(), ordinary);
    crate::mcp_oauth::remove_token(f.home(), "google").unwrap();
    assert!(f.start().await.is_err());
    assert!(!write_store::path(f.home()).exists());
}

#[tokio::test]
async fn home_write_url_isolated_scopes_pkce_login_hint_and_session_binding() {
    let f = Fixture::new().await;
    let ordinary = f.ordinary();
    let start = f.start().await.unwrap();
    let url = url::Url::parse(start["authorization_url"].as_str().unwrap()).unwrap();
    let pairs = url.query_pairs().collect::<std::collections::HashMap<_, _>>();
    let mut scopes: Vec<_> = pairs["scope"].split_ascii_whitespace().collect();
    scopes.sort_unstable();
    let mut expected = vec![oauth::DRIVE_WRITE_SCOPE, "https://www.googleapis.com/auth/documents",
        "https://www.googleapis.com/auth/spreadsheets", "openid", "email"];
    expected.sort_unstable();
    assert_eq!(scopes, expected);
    for (key, value) in [("include_granted_scopes", "false"), ("prompt", "consent"),
        ("access_type", "offline"), ("code_challenge_method", "S256"), ("login_hint", "alice@c.test")] {
        assert_eq!(pairs[key], value);
    }
    assert!(write_store::ever_granted(f.home(), "home", "home").unwrap());
    let state = start["state"].as_str().unwrap();
    pending::receive_code(f.home(), state, "write-alice").unwrap();
    assert!(flow::finish(f.home(), &f.personal.db, &f.admin.id, "another-session", state).await.is_err());
    assert!(flow::finish(f.home(), &f.personal.db, &f.personal.alice.id, "admin-session", state).await.is_err());
    // Existing personal callback remains compatible for an authenticated admin.
    flow::finish(f.home(), &f.personal.db, &f.admin.id, "admin-session", state).await.unwrap();
    assert_eq!(f.ordinary(), ordinary);
}

#[tokio::test]
async fn home_write_finish_refuses_other_account_and_nonexact_scope_without_storing() {
    let f = Fixture::new().await;
    let ordinary = f.ordinary();
    for code in ["write-bob", "drive-alice", "meeting-alice", "write-no-refresh-alice", "write-no-scopes-alice"] {
        let start = f.start().await.unwrap();
        let before = std::fs::read(write_store::path(f.home())).unwrap();
        assert!(f.finish(&start, code).await.is_err());
        assert!(write_store::get(f.home(), "home", "home").unwrap().is_none());
        assert_eq!(std::fs::read(write_store::path(f.home())).unwrap(), before);
        assert_eq!(f.ordinary(), ordinary);
    }
}

#[tokio::test]
async fn home_write_success_only_stores_home_and_disconnect_rejects_stale_epoch() {
    let f = Fixture::new().await;
    let ordinary = f.ordinary();
    let start = f.start().await.unwrap();
    f.finish(&start, "write-alice").await.unwrap();
    let record = write_store::get(f.home(), "home", "home").unwrap().unwrap();
    assert_eq!(record.metadata.owner_kind, "home");
    assert_eq!(record.metadata.owner_id, "home");
    assert_eq!(record.metadata.google_sub, "sub-alice");
    assert_eq!(record.metadata.account_email, "alice@c.test");
    assert_eq!(record.metadata.status, Status::Connected);
    assert!(vault::get(f.home(), &f.admin.id).unwrap().is_none());
    assert!(write_store::get(f.home(), "user", &f.admin.id).unwrap().is_none());
    assert_eq!(f.ordinary(), ordinary);
    let stale = f.start().await.unwrap();
    home_write::disconnect(f.home(), &f.personal.db, &f.admin.id).unwrap();
    let before = std::fs::read(write_store::path(f.home())).unwrap();
    assert!(f.finish(&stale, "write-alice").await.unwrap_err().contains("較新的操作"));
    assert_eq!(std::fs::read(write_store::path(f.home())).unwrap(), before);
    assert!(write_store::get(f.home(), "home", "home").unwrap().is_none());
    assert_eq!(f.ordinary(), ordinary);
    let events = duduclaw_security::audit::read_recent_events(f.home(), 20);
    let details = serde_json::to_string(&events).unwrap();
    assert!(details.contains("company Drive write consent connected: alice@c.test"));
    assert!(details.contains("company Drive write consent disconnected"));
    assert!(!details.contains("access-alice") && !details.contains("refresh-alice"));
}

#[tokio::test]
async fn home_write_finish_rechecks_admin_policy_client_route_and_connection() {
    for changed in ["admin", "policy", "client", "route", "ordinary", "domain"] {
        let f = Fixture::new().await;
        let start = f.start().await.unwrap();
        match changed {
            "admin" => f.personal.db.update_user(&f.admin.id, None, Some(UserRole::Employee), None).unwrap(),
            "client" => {
                let mut client = crate::mcp_oauth::get_client_config(f.home(), "google").unwrap();
                client.client_secret = "changed-secret".into();
                crate::mcp_oauth::upsert_client_config(f.home(), client).unwrap();
            }
            "ordinary" => crate::mcp_oauth::remove_token(f.home(), "google").unwrap(),
            _ => {
                let mut config: toml::Value = std::fs::read_to_string(f.home().join("config.toml")).unwrap().parse().unwrap();
                match changed {
                    "policy" => { config["google"].as_table_mut().unwrap().remove("drive_write"); }
                    "route" => config["integrations"]["google_credential_path"] = toml::Value::String("none".into()),
                    "domain" => {
                        let identity = config["integrations"].as_table_mut().unwrap().entry("google_identity")
                            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                        identity.as_table_mut().unwrap().insert("allowed_domains".into(), toml::Value::Array(vec![toml::Value::String("other.test".into())]));
                    }
                    _ => unreachable!(),
                }
                std::fs::write(f.home().join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
            }
        }
        let ordinary = f.ordinary();
        assert!(f.finish(&start, "write-alice").await.is_err(), "{changed}");
        assert!(write_store::get(f.home(), "home", "home").unwrap().is_none());
        assert_eq!(f.ordinary(), ordinary);
    }
}

#[tokio::test]
async fn home_write_pasted_callback_completes_for_bound_admin_only() {
    let f = Fixture::new().await;
    let start = home_write::start(f.home(), &f.personal.db, &f.admin.id, "admin-session",
        "http://lan:18889", &["http://lan:18889".into()], true, true).await.unwrap();
    assert_eq!(start["completion"], "paste");
    let url = format!("{}?state={}&code=write-alice", start["redirect_uri"].as_str().unwrap(), start["state"].as_str().unwrap());
    assert!(pending::receive_pasted(f.home(), &f.admin.id, "other-session", &url).is_err());
    let state = pending::receive_pasted(f.home(), &f.admin.id, "admin-session", &url).unwrap();
    let result = flow::finish_home(f.home(), &f.personal.db, &f.admin.id, "admin-session", &state).await.unwrap();
    assert_eq!(result["connection"]["owner_kind"], json!("home"));
}

#[tokio::test]
async fn home_write_identity_probe_refreshes_expired_access_only_in_memory() {
    let f = Fixture::new().await;
    let path = f.home().join(crate::mcp_oauth::TOKEN_FILE);
    let mut rows: Value = serde_json::from_slice(&f.ordinary()).unwrap();
    let row = rows.as_array_mut().unwrap().iter_mut().find(|r| r["provider_id"] == "google").unwrap();
    row["expires_at"] = json!("2000-01-01T00:00:00Z");
    std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
    let ordinary = f.ordinary();
    let start = f.start().await.unwrap();
    f.finish(&start, "write-alice").await.unwrap();
    assert_eq!(f.ordinary(), ordinary);
}
