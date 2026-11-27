//! Owned write-vault and tokeninfo fixtures; never contact Google.
use std::path::Path;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use crate::person_credentials::{write_store, Secret, Status};
use duduclaw_security::credential_protection::ProtectedKey;

pub(crate) fn install_write_credential(home: &Path, key: &ProtectedKey, kind: &str, owner: &str, client_id: &str) {
    let metadata = write_store::Metadata {
        owner_kind: kind.into(), owner_id: owner.into(), purpose: "drive_write".into(),
        provider: "google".into(), credential_id: uuid::Uuid::new_v4().to_string(), revision: 1,
        account_email: "write@c.test".into(), google_sub: "write-scope-fixture".into(), hosted_domain: None,
        granted_scopes: vec![crate::google_personal::oauth::DRIVE_WRITE_SCOPE.into()],
        status: Status::Connected, status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None,
        oauth_client_id: client_id.into(), google_data_consent: None,
    };
    let epoch = write_store::begin_connection(home, kind, owner).unwrap();
    write_store::connect(home, key, metadata, &Secret {
        access_token: "separate-write-secret".into(), refresh_token: Some("separate-write-refresh".into()),
        expires_at: Some(4_000_000_000),
    }, epoch).unwrap();
}

pub(crate) struct TokeninfoWire {
    pub(crate) url: String,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl TokeninfoWire {
    pub(crate) async fn new(access_token: &str, client_id: &str, scopes: Option<&str>) -> Self {
        let access_token = access_token.to_owned();
        let client_id = client_id.to_owned();
        let scopes = scopes.map(str::to_owned);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let app = axum::Router::new().route("/tokeninfo", axum::routing::get(
            move |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| {
                let access_token = access_token.clone();
                let client_id = client_id.clone();
                let scopes = scopes.clone();
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(query.get("access_token"), Some(&access_token));
                    match scopes {
                        Some(scopes) => (axum::http::StatusCode::OK,
                            axum::Json(serde_json::json!({"aud": client_id, "scope": scopes}))),
                        None => (axum::http::StatusCode::BAD_REQUEST,
                            axum::Json(serde_json::json!({"error": "invalid_token"}))),
                    }
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/tokeninfo", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        Self { url, calls, task }
    }
    pub(crate) fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }
}
impl Drop for TokeninfoWire {
    fn drop(&mut self) { self.task.abort(); }
}
