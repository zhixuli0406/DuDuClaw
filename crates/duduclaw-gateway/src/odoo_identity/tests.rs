//! Route-level tests: real signatures, users.db, gateway sealer and audit files.
//! No Odoo network service is needed to decide an identity.
use super::*;
use std::net::SocketAddr;
use std::sync::Arc;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use duduclaw_auth::{AccessLevel, UserDb, UserRole, UserStatus};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde_json::Value;
use tower::ServiceExt;
use crate::computer_use_sessions::auth;
use route::{OdooIdentityRouteState, ROUTE, router};

const INTERNAL_KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";
const SECRET_A: &str = "odoo-person-a-secret-never-in-audit";
const SECRET_B: &str = "odoo-person-b-secret-never-in-audit";

struct Fixture {
    home: tempfile::TempDir,
    state: Arc<OdooIdentityRouteState>,
    admin: String,
    a: String,
    b: String,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@odoo.test", "Admin", "password-123", UserRole::Admin).unwrap().id;
        let a = db.create_user("a@odoo.test", "A", "password-123", UserRole::Employee).unwrap().id;
        let b = db.create_user("b@odoo.test", "B", "password-123", UserRole::Employee).unwrap().id;
        let state = Arc::new(OdooIdentityRouteState::new(home.path().to_path_buf(), db));
        let f = Self { home, state, admin, a, b };
        f.config("", "https://erp.example.com", &["odoo:read", "odoo:write"]);
        duduclaw_core::ensure_identity_key(f.home.path()).unwrap();
        assert!(crate::config_crypto::encrypt_value("setup", f.home.path()).is_some());
        for agent in ["alice", "bob", "shared", "company", "company-extra"] { f.agent(agent, ""); }
        f
    }
    fn config(&self, access: &str, url: &str, scopes: &[&str]) {
        std::fs::write(self.home.path().join("config.toml"), format!(
            "[odoo]\nmember_credentials=true\nurl='{url}'\ndb='company'\n[odoo_access]\n{access}\n\
             [member_api_keys]\nenabled=true\nallow_mcp_scopes={}\n\
             [mcp_keys.\"{INTERNAL_KEY}\"]\nclient_id='gateway-internal'\nis_external=false\n\
             created_at='2026-11-01T00:00:00Z'\nscopes=['admin']\n",
            serde_json::to_string(scopes).unwrap(),
        )).unwrap();
    }
    fn agent(&self, id: &str, extra: &str) {
        let dir = self.home.path().join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), format!("[agent]\nname='{id}'\n{extra}\n")).unwrap();
    }
    fn avatar(&self, user: &str, agent: &str) {
        self.state.users.bind_agent(user, agent, AccessLevel::Owner).unwrap();
        self.state.users.set_avatar_as(&self.admin, user, Some(agent)).unwrap();
    }
    fn credential(&self, user: &str, secret: &str) {
        let server = server::company_server(self.home.path()).unwrap().unwrap();
        let sealer = crate::odoo_credentials::Sealer::load(self.home.path()).unwrap();
        self.state.users.odoo_credential_put(user, "person", 42, "Person", secret, &server.fp, &sealer).unwrap();
    }
    fn key(&self, user: &str, scopes: &[&str]) -> String {
        let epoch = self.state.users.get_user(user).unwrap().unwrap().credential_epoch;
        self.state.users.member_key_create(user, "Odoo", 30, epoch, false,
            &scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap().secret
    }
    fn signed(&self, agent: &str, body: &Value) -> HeaderMap {
        let key = duduclaw_core::load_identity_key(self.home.path()).unwrap();
        let token = duduclaw_core::current_agent_token(self.home.path(), &key, agent).unwrap();
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let signature = duduclaw_core::internal_request_signature(INTERNAL_KEY.as_bytes(), agent,
            &token, &timestamp, &nonce, body.to_string().as_bytes());
        let mut headers = HeaderMap::new();
        for (name, value) in [(auth::AGENT_ID_HEADER, agent), (auth::TIMESTAMP_HEADER, &timestamp),
            (auth::NONCE_HEADER, &nonce), (auth::SIGNATURE_HEADER, &signature)] {
            headers.insert(name, value.parse().unwrap());
        }
        headers
    }
    async fn request(&self, headers: HeaderMap, body: Value, peer: &str) -> (StatusCode, Value) {
        let mut request = Request::builder().method("POST").uri(ROUTE)
            .extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()))
            .body(Body::from(body.to_string())).unwrap();
        *request.headers_mut() = headers;
        let response = router(self.state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
    async fn employee(&self, agent: &str, body: Value) -> (StatusCode, Value) {
        self.request(self.signed(agent, &body), body, "127.0.0.1:45000").await
    }
    async fn person(&self, key: &str, body: Value) -> (StatusCode, Value) {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {key}").parse().unwrap());
        self.request(headers, body, "127.0.0.1:45000").await
    }
    fn audit(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.home.path().join("tool_calls.jsonl")).unwrap();
        assert!(!text.contains(SECRET_A) && !text.contains(SECRET_B));
        text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }
}

fn read() -> Value { json!({"tool":"odoo_search"}) }

#[tokio::test]
async fn employee_audit_failure_refuses_before_releasing_personal_secret() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.credential(&f.a, SECRET_A);
    let (status, value) = f.employee("alice", read()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["secret"], SECRET_A);
    let audit_path = f.home.path().join("tool_calls.jsonl");
    std::fs::remove_file(&audit_path).unwrap();
    std::fs::create_dir(&audit_path).unwrap();

    let (status, value) = f.employee("alice", read()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
    assert_eq!(value["ok"], false);
    assert!(value.get("secret").is_none());
    assert!(value.get("identity").is_none());
    assert!(!value.to_string().contains(SECRET_A));
    assert!(audit_path.is_dir());
}

fn refused(status: StatusCode, value: &Value, code: &str) {
    assert_eq!(status, StatusCode::FORBIDDEN, "{value}");
    assert_eq!(value["ok"], false);
    assert_eq!(value["code"], code);
    assert!(value.get("secret").is_none());
    assert!(value.get("identity").is_none());
}

#[tokio::test]
async fn five_rows_and_personal_precedence_and_revision_handoff() {
    let f = Fixture::new();
    f.config("company_key_agents=['company']", "https://erp.example.com", &["odoo:read"]);
    f.avatar(&f.a, "alice");
    f.credential(&f.a, SECRET_A);
    f.agent("alice", "[odoo]\nusername='service'\napi_key_enc='cipher'");
    let (status, value) = f.employee("alice", read()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["identity"], "personal");
    assert_eq!(value["user_id"], f.a);
    assert_eq!(value["secret"], SECRET_A);
    assert_eq!(value["permission"], "engine");
    let rev = value["rev"].as_i64().unwrap();
    let (_, cached) = f.employee("alice", json!({"tool":"odoo_read","have_rev":rev})).await;
    assert_eq!(cached["rev"], rev);
    assert_eq!(cached["odoo_uid"], 42);
    assert!(cached.get("secret").is_none());
    assert!(f.state.users.odoo_credential_status(&f.a).unwrap().unwrap().last_used_at.is_some());
    let (_, stale) = f.employee("alice", json!({"tool":"odoo_read","have_rev":rev-1})).await;
    assert_eq!(stale["secret"], SECRET_A);

    f.agent("shared", "[odoo]\nact_as_requester=true");
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register("shared", &turn,
        RequesterIdentity::DashboardUser { user_id: f.a.clone() }).unwrap();
    let (_, requester) = f.employee("shared", json!({"tool":"odoo_search","turn_id":turn})).await;
    assert_eq!(requester["identity"], "personal");
    assert_eq!(requester["user_id"], f.a);

    f.agent("bob", "[odoo]\nusername='service'\npassword_enc='cipher'");
    for (agent, identity) in [("bob", "agent"), ("company", "company")] {
        let (status, value) = f.employee(agent, read()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["identity"], identity);
        assert_eq!(value["permission"], "engine");
        for key in ["secret", "user_id", "login", "rev"] { assert!(value.get(key).is_none()); }
    }
    let (status, value) = f.employee("company-extra", read()).await;
    refused(status, &value, "odoo_not_connected");
    assert_eq!(value["message"], duduclaw_odoo::access_policy::not_connected_message(false));
    let audit = f.audit();
    assert_eq!(audit.len(), 7);
    let last = audit.last().unwrap();
    assert_eq!(last["tool_name"], "odoo_identity");
    assert_eq!(last["agent_id"], "company-extra");
    assert_eq!(last["outcome"], "odoo_not_connected");
    assert!(last["identity"].is_null());
    assert!(!last.to_string().contains("\"company\""));
}

#[tokio::test]
async fn avatar_without_credential_uses_only_explicit_rows_three_to_five() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.credential(&f.b, SECRET_B);
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register("alice", &turn,
        RequesterIdentity::DashboardUser { user_id: f.b.clone() }).unwrap();
    let request = json!({"tool":"odoo_search","turn_id":turn});
    f.agent("alice", "[odoo]\nact_as_requester=true\nusername='service'\napi_key_enc='cipher'");
    let (_, service) = f.employee("alice", request.clone()).await;
    assert_eq!(service["identity"], "agent");
    assert!(service.get("secret").is_none());
    f.agent("alice", "[odoo]\nact_as_requester=true");
    let (status, value) = f.employee("alice", request.clone()).await;
    refused(status, &value, "odoo_not_connected");
    assert_eq!(value["message"], duduclaw_odoo::access_policy::not_connected_message(true));
    f.config("company_key_agents=['alice']", "https://erp.example.com", &["odoo:read"]);
    let (_, company) = f.employee("alice", request).await;
    assert_eq!(company["identity"], "company");
    assert!(company.get("secret").is_none());
}

#[tokio::test]
async fn requester_requires_verified_human_for_this_employee_and_turn() {
    let f = Fixture::new();
    f.agent("shared", "[odoo]\nact_as_requester=true");
    f.credential(&f.a, SECRET_A);
    let channel = |channel: &str, person: Option<String>| RequesterIdentity::ChannelUser {
        channel: channel.into(), chat: "chat".into(), sender: "sender".into(),
        user_id: person, attested_private: false,
    };
    for requester in [RequesterIdentity::Unknown,
        RequesterIdentity::System { kind: "cron".into(), on_behalf_of: None },
        RequesterIdentity::System { kind: "dispatch".into(), on_behalf_of: Some(f.a.clone()) },
        channel("widget", Some(f.a.clone())), channel("odoolivechat", Some(f.a.clone())),
        channel("line", None)] {
        let turn = uuid::Uuid::new_v4().to_string();
        let _guard = crate::knowledge::turn_requesters::register("shared", &turn, requester).unwrap();
        let (status, value) = f.employee("shared", json!({"tool":"odoo_search","turn_id":turn})).await;
        refused(status, &value, "odoo_not_connected");
    }
    for body in [read(), json!({"tool":"odoo_search","turn_id":"unknown-turn"}),
        json!({"tool":"odoo_search","run_id":"unknown-run"})] {
        let (status, value) = f.employee("shared", body).await;
        refused(status, &value, "odoo_not_connected");
    }
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register("shared", &turn, channel("line", Some(f.a.clone()))).unwrap();
    let (_, human) = f.employee("shared", json!({"tool":"odoo_search","turn_id":turn})).await;
    assert_eq!(human["secret"], SECRET_A);
    f.agent("bob", "[odoo]\nact_as_requester=true");
    let (status, value) = f.employee("bob", json!({"tool":"odoo_search","turn_id":turn})).await;
    refused(status, &value, "odoo_not_connected");
    f.agent("shared", "[odoo]\nact_as_requester=false");
    let (status, value) = f.employee("shared", json!({"tool":"odoo_search","turn_id":turn})).await;
    refused(status, &value, "odoo_not_connected");
}

#[tokio::test]
async fn temporary_company_membership_comes_from_host_verified_parent() {
    let f = Fixture::new();
    f.config("company_key_agents=['company']", "https://erp.example.com", &["odoo:read"]);
    let id = "eph-123456789abc";
    let dir = f.home.path().join("agents/.ephemeral").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\nname='temp'\n").unwrap();
    std::fs::write(dir.join("ephemeral.toml"),
        "parent='company'\ntier='cheap'\ncreated_at='2026-11-27T00:00:00Z'\nexpires_at='2026-11-28T00:00:00Z'\n").unwrap();
    std::fs::write(f.home.path().join("org.toml"), format!(
        "schema=1\n[agents.{id}]\nreports_to='company'\ndepartment=''\n")).unwrap();
    let (status, value) = f.employee(id, read()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["identity"], "company");
    assert!(value.get("secret").is_none());
    std::fs::write(f.home.path().join("org.toml"), format!(
        "schema=1\n[agents.{id}]\nreports_to='bob'\ndepartment=''\n")).unwrap();
    let (status, value) = f.employee(id, read()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(value.get("secret").is_none());
}

#[tokio::test]
async fn changed_server_invalidates_personal_and_missing_server_cannot_unseal() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.credential(&f.a, SECRET_A);
    f.config("", "https://new-erp.example.com", &["odoo:read"]);
    let (status, value) = f.employee("alice", read()).await;
    refused(status, &value, "odoo_not_connected");
    assert_eq!(f.state.users.odoo_credential_status(&f.a).unwrap().unwrap().state, "needs_reverify");
    f.config("", "https://erp.example.com", &["odoo:read"]);
    let (status, value) = f.employee("alice", read()).await;
    refused(status, &value, "odoo_not_connected");
    f.credential(&f.a, SECRET_A);
    let path = f.home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut doc: toml::Table = raw.parse().unwrap();
    doc.remove("odoo");
    std::fs::write(path, toml::to_string(&doc).unwrap()).unwrap();
    let (status, value) = f.employee("alice", read()).await;
    refused(status, &value, "odoo_not_connected");
    f.audit();
}

#[tokio::test]
async fn invalid_access_refuses_all_tools_in_both_modes() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.credential(&f.a, SECRET_A);
    let key = f.key(&f.a, &["odoo:read", "odoo:write"]);
    for invalid in ["company_key_agents='alice'", "permission_mode='typo'", "unknown=true"] {
        f.config(invalid, "https://erp.example.com", &["odoo:read", "odoo:write"]);
        for tool in ["odoo_search", "odoo_create", "odoo_write", "odoo_execute", "odoo_unknown"] {
            let body = json!({"tool":tool});
            let (status, value) = f.employee("alice", body.clone()).await;
            refused(status, &value, "odoo_access_invalid");
            let (status, value) = f.person(&key, body).await;
            refused(status, &value, "odoo_access_invalid");
        }
    }
}

#[tokio::test]
async fn signature_identity_and_strict_body_prevent_cross_member_handoff() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.avatar(&f.b, "bob");
    f.credential(&f.a, SECRET_A);
    f.credential(&f.b, SECRET_B);
    for extra in ["user_id", "agent_id", "employee", "identity"] {
        let mut body = read();
        body[extra] = json!(f.b);
        let (status, value) = f.employee("alice", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(value.get("secret").is_none());
    }
    let mut forged = f.signed("alice", &read());
    forged.insert(auth::AGENT_ID_HEADER, "bob".parse().unwrap());
    let (status, value) = f.request(forged, read(), "127.0.0.1:45000").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(value.get("secret").is_none());
    let (_, own) = f.employee("alice", read()).await;
    assert_eq!(own["secret"], SECRET_A);
    assert!(!own.to_string().contains(SECRET_B));
    let headers = f.signed("alice", &read());
    assert_eq!(f.request(headers.clone(), read(), "127.0.0.1:45000").await.0, StatusCode::OK);
    assert_eq!(f.request(headers, read(), "127.0.0.1:45000").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(f.request(f.signed("alice", &read()), read(), "192.0.2.1:45000").await.0, StatusCode::UNAUTHORIZED);
    f.audit();
}

#[tokio::test]
async fn person_key_scopes_never_borrow_server_employee_or_company() {
    let f = Fixture::new();
    f.config("company_key_agents=['company']", "https://erp.example.com", &["odoo:read", "odoo:write"]);
    f.avatar(&f.b, "company");
    f.agent("company", "[odoo]\nusername='service'\napi_key_enc='cipher'");
    f.credential(&f.b, SECRET_B);
    let none = f.key(&f.a, &[]);
    let key = f.key(&f.a, &["odoo:read", "odoo:write"]);
    let (status, value) = f.person(&none, read()).await;
    refused(status, &value, "person_key_scope");
    let (status, value) = f.person(&key, read()).await;
    refused(status, &value, "odoo_not_connected");
    f.credential(&f.a, SECRET_A);
    let mut headers = f.signed("company", &read());
    headers.insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (status, value) = f.request(headers, read(), "127.0.0.1:45000").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["identity"], "personal");
    assert_eq!(value["user_id"], f.a);
    assert_eq!(value["secret"], SECRET_A);
    assert_eq!(value["permission"], "read_only");
    let (_, cached) = f.person(&key, json!({"tool":"odoo_read","have_rev":value["rev"]})).await;
    assert!(cached.get("secret").is_none());
    let (status, value) = f.person(&key, json!({"tool":"odoo_write"})).await;
    refused(status, &value, "odoo_follow_required");
    for field in ["user_id", "turn_id", "run_id", "agent_id"] {
        let mut body = read(); body[field] = json!("forged");
        assert_eq!(f.person(&key, body).await.0, StatusCode::BAD_REQUEST);
    }
    for tool in ["odoo_execute", "odoo_message_send", "odoo_attachment_add", "odoo_search_extra"] {
        assert_eq!(f.person(&key, json!({"tool":tool})).await.0, StatusCode::BAD_REQUEST);
    }
    f.config("", "https://erp.example.com", &[]);
    let (status, value) = f.person(&key, read()).await;
    refused(status, &value, "person_key_scope");
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(!audit.contains(SECRET_A) && !audit.contains(SECRET_B) && !audit.contains(&key));
    assert!(!audit.contains("\"identity\":\"company\""));
    assert!(!f.home.path().join("tool_calls.jsonl").exists());
}

#[tokio::test]
async fn follow_ack_and_employee_override_and_person_write_scope() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.credential(&f.a, SECRET_A);
    let read_key = f.key(&f.a, &["odoo:read"]);
    let write_key = f.key(&f.a, &["odoo:read", "odoo:write"]);
    let write_only = f.key(&f.a, &["odoo:write"]);
    f.config("permission_mode='follow_odoo_account'", "https://erp.example.com", &["odoo:read", "odoo:write"]);
    let (status, value) = f.person(&write_key, json!({"tool":"odoo_create"})).await;
    refused(status, &value, "odoo_follow_required");
    let follow = "permission_mode='follow_odoo_account'\npermission_mode_ack={by='admin',at='2026-11-27T00:00:00Z'}";
    f.config(follow, "https://erp.example.com", &["odoo:read", "odoo:write"]);
    for (override_, mode) in [("inherit", "follow"), ("engine", "engine"), ("read_only", "read_only")] {
        f.agent("alice", &format!("[odoo]\npermission_override='{override_}'"));
        let (_, value) = f.employee("alice", read()).await;
        assert_eq!(value["permission"], mode);
    }
    assert_eq!(f.person(&read_key, read()).await.1["permission"], "follow");
    for tool in ["odoo_create", "odoo_write", "odoo_activity_create", "odoo_note_post"] {
        let (status, value) = f.person(&write_key, json!({"tool":tool})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["permission"], "follow");
        assert_eq!(value["user_id"], f.a);
        let (status, value) = f.person(&read_key, json!({"tool":tool})).await;
        refused(status, &value, "person_key_scope");
        let (status, value) = f.person(&write_only, json!({"tool":tool})).await;
        refused(status, &value, "person_key_scope");
    }
}

#[tokio::test]
async fn inactive_member_and_failed_security_audit_never_release_a_secret() {
    let f = Fixture::new();
    f.credential(&f.a, SECRET_A);
    let key = f.key(&f.a, &["odoo:read"]);
    // A directory in place of the audit file makes the append fail.
    std::fs::create_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    let (status, value) = f.person(&key, read()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(value.get("secret").is_none());
    std::fs::remove_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    f.state.users.set_user_status(&f.a, UserStatus::Suspended).unwrap();
    let (status, value) = f.person(&key, read()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(value.get("secret").is_none());
}


#[tokio::test]
async fn member_switch_off_skips_personal_without_deleting_and_on_restores_it() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.credential(&f.a, SECRET_A);
    let key = f.key(&f.a, &["odoo:read", "odoo:write"]);
    let path = f.home.path().join("config.toml");
    let enabled = std::fs::read_to_string(&path).unwrap();
    for setting in ["", "member_credentials=false", "member_credentials='true'"] {
        std::fs::write(&path, enabled.replace("member_credentials=true", setting)).unwrap();
        let (status, value) = f.employee("alice", read()).await;
        refused(status, &value, "odoo_not_connected");
        for request in [read(), json!({"tool":"odoo_write"})] {
            let (status, value) = f.person(&key, request).await;
            refused(status, &value, "odoo_member_credentials_off");
        }
        assert_eq!(f.state.users.odoo_credential_status(&f.a).unwrap().unwrap().state, "ok");
        // Row three is still eligible, but carries no personal secret.
        f.agent("alice", "[odoo]\nusername='service'\napi_key_enc='cipher'");
        let (status, value) = f.employee("alice", read()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["identity"], "agent");
        assert!(value.get("secret").is_none());
        f.agent("alice", "");
    }
    std::fs::write(&path, enabled).unwrap();
    let (status, value) = f.employee("alice", read()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["secret"], SECRET_A);
    let (status, value) = f.person(&key, read()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["secret"], SECRET_A);
}

#[tokio::test]
async fn password_epoch_change_stops_personal_handoff_even_with_new_person_key() {
    let f = Fixture::new();
    f.avatar(&f.a, "alice");
    f.credential(&f.a, SECRET_A);
    assert_eq!(f.employee("alice", read()).await.1["secret"], SECRET_A);
    let db = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    db.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&f.a]).unwrap();
    let (status, value) = f.employee("alice", read()).await;
    refused(status, &value, "odoo_not_connected");
    let new_key = f.key(&f.a, &["odoo:read"]);
    let (status, value) = f.person(&new_key, read()).await;
    refused(status, &value, "odoo_not_connected");
    assert_eq!(f.state.users.odoo_credential_status(&f.a).unwrap().unwrap().state, "needs_reverify");
    f.credential(&f.a, SECRET_B);
    let (status, value) = f.employee("alice", read()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["secret"], SECRET_B);
}
