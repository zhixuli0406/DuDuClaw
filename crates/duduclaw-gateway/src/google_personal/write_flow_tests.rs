use super::{flow, oauth, pending, tests::Fixture};
use crate::person_credentials::{self as vault, write_store, Status};
use serde_json::Value;

fn enable_write(f: &Fixture) {
    use crate::google_workspace::drive_write_policy as policy;
    let mut enabled = policy::DriveWritePolicy::all_off();
    enabled.create = true;
    let ack = policy::sign(&enabled, "admin", "admin@c.test", chrono::Utc::now());
    let mut config: toml::Value = std::fs::read_to_string(f.home.join("config.toml"))
        .unwrap().parse().unwrap();
    config.as_table_mut().unwrap().insert("google".into(), toml::Value::Table(toml::Table::new()));
    config["google"].as_table_mut().unwrap().insert("drive_write".into(), policy::render_section(&enabled, &ack));
    std::fs::write(f.home.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
}

async fn start_write(f: &Fixture, data: bool, write: bool) -> Result<Value, String> {
    flow::start_drive_write(&f.home, &f.db, &f.alice.id, "login-a",
        "http://127.0.0.1:18889", &[], data, write).await
}

async fn finish(f: &Fixture, start: &Value, code: &str) -> Result<Value, String> {
    let state = start["state"].as_str().unwrap();
    pending::receive_code(&f.home, state, code).unwrap();
    flow::finish(&f.home, &f.db, &f.alice.id, "login-a", state).await
}

#[tokio::test]
async fn write_start_requires_policy_acknowledgements_and_connected_ordinary_account() {
    let f = Fixture::new();
    assert!(start_write(&f, true, true).await.unwrap_err().contains("管理員還沒開放"));
    enable_write(&f);
    for (data, write) in [(false, false), (false, true), (true, false)] {
        assert!(start_write(&f, data, write).await.is_err());
    }
    assert!(start_write(&f, true, true).await.is_err());
    assert!(!write_store::path(&f.home).exists());
    f.connect().await;
    let old = vault::get(&f.home, &f.alice.id).unwrap().unwrap().metadata;
    vault::mark_needs_reconnect(&f.home, &old).unwrap();
    assert!(start_write(&f, true, true).await.is_err());
    assert!(!write_store::path(&f.home).exists());
}

#[tokio::test]
async fn write_url_requests_only_five_scopes_without_merging() {
    let f = Fixture::new();
    f.connect().await;
    enable_write(&f);
    let before = std::fs::read(vault::path(&f.home)).unwrap();
    let start = start_write(&f, true, true).await.unwrap();
    let url = url::Url::parse(start["authorization_url"].as_str().unwrap()).unwrap();
    let q = url.query_pairs().collect::<std::collections::HashMap<_, _>>();
    let mut scopes = q["scope"].split_ascii_whitespace().collect::<Vec<_>>();
    scopes.sort_unstable();
    let mut expected = vec![oauth::DRIVE_WRITE_SCOPE, "https://www.googleapis.com/auth/documents",
        "https://www.googleapis.com/auth/spreadsheets", "openid", "email"];
    expected.sort_unstable();
    assert_eq!(scopes, expected);
    for (name, value) in [("include_granted_scopes", "false"), ("access_type", "offline"),
        ("prompt", "consent"), ("code_challenge_method", "S256")] {
        assert_eq!(q[name], value);
    }
    assert_eq!(std::fs::read(vault::path(&f.home)).unwrap(), before);
}

#[tokio::test]
async fn ordinary_finish_rejects_exact_drive_write_scope_without_saving() {
    let f = Fixture::new();
    f.connect().await;
    for (code, rejected) in [("write-alice", true), ("drive-alice", false)] {
        let start = flow::start(&f.home, &f.db, &f.alice.id, "login-a",
            "http://127.0.0.1:18889", &[], true).await.unwrap();
        let before = std::fs::read(vault::path(&f.home)).unwrap();
        let result = finish(&f, &start, code).await;
        if rejected {
            assert!(result.unwrap_err().contains("沒有保存"));
            assert_eq!(std::fs::read(vault::path(&f.home)).unwrap(), before);
        } else {
            result.unwrap();
        }
    }
}

#[tokio::test]
async fn incremental_drive_url_preserves_base_and_chat_send_scopes() {
    let f = Fixture::new();
    f.connect().await;
    let key = crate::google_identity::protection_gate(&f.home).unwrap();
    let old = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    let secret = vault::unseal(&key, &old).unwrap();
    let mut metadata = old.metadata;
    metadata.local_purposes.push("chat_send".into());
    metadata.granted_scopes.extend(crate::google_workspace::CHAT_SEND_SCOPES.iter().map(|s| s.to_string()));
    let epoch = vault::begin_connection(&f.home, &f.alice.id).unwrap();
    vault::connect(&f.home, &key, metadata, &secret, epoch).unwrap();
    let start = flow::start_drive_tools(&f.home, &f.db, &f.alice.id, "login-a",
        "http://127.0.0.1:18889", &[], true).await.unwrap();
    let url = url::Url::parse(start["authorization_url"].as_str().unwrap()).unwrap();
    let q = url.query_pairs().collect::<std::collections::HashMap<_, _>>();
    assert_eq!(q["include_granted_scopes"], "false");
    let scopes = q["scope"].split_ascii_whitespace().collect::<Vec<_>>();
    assert_eq!(scopes, oauth::scopes_for_purposes(&["base", "chat_send", "drive_tools"], false));
    for scope in oauth::requested_scopes(false, false).into_iter()
        .chain(crate::google_workspace::CHAT_SEND_SCOPES.iter().copied())
        .chain([oauth::DRIVE_TOOLS_SCOPE]) {
        assert!(scopes.contains(&scope), "{scope}");
    }
}

#[tokio::test]
async fn write_finish_rejects_other_account_and_nonexact_drive_scopes() {
    let f = Fixture::new();
    f.connect().await;
    enable_write(&f);
    let ordinary = std::fs::read(vault::path(&f.home)).unwrap();
    for (code, message) in [("write-bob", "原本連結"), ("drive-alice", "寫入權限"),
        ("meeting-alice", "寫入權限")] {
        let start = start_write(&f, true, true).await.unwrap();
        let before = std::fs::read(write_store::path(&f.home)).unwrap();
        assert!(finish(&f, &start, code).await.unwrap_err().contains(message));
        assert!(write_store::get(&f.home, "user", &f.alice.id).unwrap().is_none());
        assert_eq!(std::fs::read(write_store::path(&f.home)).unwrap(), before);
        assert_eq!(std::fs::read(vault::path(&f.home)).unwrap(), ordinary);
    }
}

#[tokio::test]
async fn write_finish_stores_separately_and_disconnect_invalidates_pending_epoch() {
    let f = Fixture::new();
    f.connect().await;
    enable_write(&f);
    let ordinary = std::fs::read(vault::path(&f.home)).unwrap();
    let start = start_write(&f, true, true).await.unwrap();
    // Docs/Sheets are optional in the provider's actual grant.
    finish(&f, &start, "write-alice").await.unwrap();
    let m = write_store::get(&f.home, "user", &f.alice.id).unwrap().unwrap().metadata;
    assert_eq!(m.owner_kind, "user");
    assert_eq!(m.owner_id, f.alice.id);
    assert_eq!(m.purpose, "drive_write");
    assert_eq!(m.google_sub, "sub-alice");
    assert_eq!(m.status, Status::Connected);
    assert!(m.granted_scopes.iter().any(|s| s == oauth::DRIVE_WRITE_SCOPE));
    assert_eq!(std::fs::read(vault::path(&f.home)).unwrap(), ordinary);
    let stale = start_write(&f, true, true).await.unwrap();
    flow::write_disconnect(&f.home, &f.db, &f.alice.id).unwrap();
    let before = std::fs::read(write_store::path(&f.home)).unwrap();
    assert!(finish(&f, &stale, "write-alice").await.unwrap_err().contains("較新的操作"));
    assert!(write_store::get(&f.home, "user", &f.alice.id).unwrap().is_none());
    assert_eq!(std::fs::read(write_store::path(&f.home)).unwrap(), before);
    assert_eq!(std::fs::read(vault::path(&f.home)).unwrap(), ordinary);
}

#[tokio::test]
async fn write_finish_rechecks_policy_client_user_and_ordinary_connection() {
    for changed in ["policy", "client", "user", "ordinary"] {
        let f = Fixture::new();
        f.connect().await;
        enable_write(&f);
        let start = start_write(&f, true, true).await.unwrap();
        match changed {
            "policy" => std::fs::write(f.home.join("config.toml"),
                "[integrations]\ngoogle_workspace=true\n").unwrap(),
            "client" => {
                let mut client = crate::mcp_oauth::get_client_config(&f.home, "google").unwrap();
                client.client_secret = "changed-client-secret".into();
                crate::mcp_oauth::upsert_client_config(&f.home, client).unwrap();
            }
            "user" => f.db.set_user_status(&f.alice.id, duduclaw_auth::UserStatus::Suspended).unwrap(),
            "ordinary" => {
                let metadata = vault::get(&f.home, &f.alice.id).unwrap().unwrap().metadata;
                vault::mark_needs_reconnect(&f.home, &metadata).unwrap();
            }
            _ => unreachable!(),
        }
        let ordinary = std::fs::read(vault::path(&f.home)).unwrap();
        let before = std::fs::read(write_store::path(&f.home)).unwrap();
        assert!(finish(&f, &start, "write-alice").await.is_err(), "{changed}");
        assert!(write_store::get(&f.home, "user", &f.alice.id).unwrap().is_none());
        assert_eq!(std::fs::read(write_store::path(&f.home)).unwrap(), before);
        assert_eq!(std::fs::read(vault::path(&f.home)).unwrap(), ordinary);
    }
}


#[tokio::test]
async fn write_connection_refuses_signin_link_mismatch_without_replacing_credentials() {
    let f = Fixture::new();
    f.connect().await;
    enable_write(&f);
    let start = start_write(&f, true, true).await.unwrap();
    finish(&f, &start, "write-alice").await.unwrap();
    // Model a legacy ordinary connection that differs from the sign-in link.
    rusqlite::Connection::open(f.home.join("users.db")).unwrap().execute(
        "INSERT INTO google_signin_links(user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at)
         VALUES(?1,'214001','c.test','login@c.test','self_link',?1,1)",
        [&f.alice.id],
    ).unwrap();
    let start = start_write(&f, true, true).await.unwrap();
    let ordinary_before = std::fs::read(vault::path(&f.home)).unwrap();
    let write_before = std::fs::read(write_store::path(&f.home)).unwrap();
    assert_eq!(finish(&f, &start, "write-alice").await.unwrap_err(), "google_signin_account_mismatch");
    assert_eq!(std::fs::read(vault::path(&f.home)).unwrap(), ordinary_before);
    assert_eq!(std::fs::read(write_store::path(&f.home)).unwrap(), write_before);
}
