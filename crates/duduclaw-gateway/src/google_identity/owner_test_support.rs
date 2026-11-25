use super::*;
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use std::sync::Arc;

pub(crate) const AGENT: &str = "owner-test-bot";
pub(crate) const INTERNAL: &str = "ddc_dev_11111111111111111111111111111111";
pub(crate) struct Fixture {
    pub home: tempfile::TempDir,
    pub db: Arc<UserDb>,
    pub alice: String,
    pub bob: String,
    pub admin: String,
    pub state: Arc<execute::ExecuteState>,
    pub turn: String,
    _turn: crate::knowledge::turn_requesters::RequesterGuard,
}
impl Fixture {
    pub fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join(AGENT);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), format!("[agent]\nname='{AGENT}'\n[google]\nidentity='delegate:owner'\nrequesters='owner_only'\n")).unwrap();
        std::fs::write(home.path().join("config.toml"), format!("[integrations]\ngoogle_workspace=true\n[integrations.google_identity]\nallowed_subject_domains=['c.test']\n[mcp_keys.{INTERNAL}]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n", chrono::Utc::now().to_rfc3339())).unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin).unwrap().id;
        let alice = db.create_user("alice@c.test", "Alice", "fixture-password", UserRole::Employee).unwrap().id;
        let bob = db.create_user("bob@c.test", "Bob", "fixture-password", UserRole::Employee).unwrap().id;
        db.bind_agent(&alice, AGENT, AccessLevel::Owner).unwrap();
        db.bind_agent(&bob, AGENT, AccessLevel::Owner).unwrap();
        db.set_google_primary_owner(AGENT, &alice, &admin).unwrap();
        db.ensure_google_employee_nonce(AGENT).unwrap();
        let key = delegation_key::path(home.path());
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        // Snapshot tests never open this as credential material.
        std::fs::write(key, b"sealed-fixture-not-a-key").unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let state = Arc::new(execute::ExecuteState::new(home.path().to_owned(), Some(db.clone())));
        let turn = uuid::Uuid::new_v4().to_string();
        let guard = crate::knowledge::turn_requesters::register(AGENT, &turn,
            duduclaw_knowledge::requester::RequesterIdentity::System { kind:"test".into(), on_behalf_of:None }).unwrap();
        let f = Self { home, db, alice, bob, admin, state, turn, _turn:guard };
        f.attest();
        f
    }
    pub fn attest(&self) {
        let owner = self.db.google_owner(AGENT).unwrap();
        let employee = delegate_owner::incarnation(self.home.path(), AGENT).unwrap();
        self.db.attest_google_delegate_owner(&duduclaw_auth::google_delegate_attestation::AttestationRequest {
            agent_id:AGENT, employee_id:&hex::encode(employee.0.0), employee_incarnation:employee.1,
            subject:&owner.email, user_id:&owner.id, acknowledge_self_set:true,
        }, &|email| email == owner.email, &self.admin).unwrap();
    }
    pub fn snapshot(&self) -> owner_snapshot::OwnerSnapshot {
        owner_snapshot::OwnerSnapshot::optional(self.home.path(), AGENT).unwrap().unwrap()
    }
    pub fn signed(&self, body: &[u8]) -> axum::http::HeaderMap {
        use crate::computer_use_sessions::auth;
        let key = duduclaw_core::load_identity_key(self.home.path()).unwrap();
        let token = duduclaw_core::mint_identity_token(&key, AGENT);
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let timestamp = crate::google_personal::authority::now().to_string();
        let signature = duduclaw_core::internal_request_signature(INTERNAL.as_bytes(), AGENT, &token, &timestamp, &nonce, body);
        let mut headers = axum::http::HeaderMap::new();
        for (k,v) in [(auth::AGENT_ID_HEADER, AGENT), (auth::TIMESTAMP_HEADER, &timestamp),
            (auth::NONCE_HEADER, &nonce), (auth::SIGNATURE_HEADER, &signature)] {
            headers.insert(k, v.parse().unwrap());
        }
        headers
    }
    pub async fn post(&self, path: &str, body: Vec<u8>, headers: axum::http::HeaderMap) -> (u16, serde_json::Value) {
        use tower::ServiceExt;
        let token = Arc::new(route::GoogleTokenRouteState::new(self.home.path().to_owned(), Some(self.db.clone())));
        let router = execute::router(self.state.clone()).merge(route::router(token));
        let mut request = axum::http::Request::builder().method("POST").uri(path)
            .header("content-type", "application/json").body(axum::body::Body::from(body)).unwrap();
        request.headers_mut().extend(headers);
        request.extensions_mut().insert(axum::extract::ConnectInfo("127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap()));
        let response = router.oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), execute::MAX_RESPONSE).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
