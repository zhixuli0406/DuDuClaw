use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const KEY: &str = "odoo-test-secret-never-return";
struct Stub {
    url: String,
    calls: Arc<AtomicUsize>,
    mode: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Stub { fn drop(&mut self) { self.task.abort(); } }
async fn stub() -> Stub {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let mode = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let response_mode = mode.clone();
    let app = axum::Router::new().route("/jsonrpc", axum::routing::post(move |axum::Json(request): axum::Json<Value>| {
        let count = count.clone();
        let mode = response_mode.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            let mode = mode.load(Ordering::SeqCst);
            let reply = if mode == 2 {
                json!({"error":{"data":{"name":"odoo.exceptions.AccessDenied", "message":KEY, "debug":KEY}}})
            } else if request["params"]["method"] == "authenticate" {
                assert_eq!(request["params"]["args"], json!(["test", "member@example.test", KEY, {}]));
                if mode == 4 { json!({"result":false}) } else { json!({"result":7}) }
            } else {
                assert_eq!(request["params"]["method"], "execute_kw");
                assert_eq!(request["params"]["args"], json!(["test",7,KEY,"res.users","read",[[7]],{"fields":["login","name"]}]));
                json!({"result":[{"id":7,"login":if mode == 1 { "different@example.test" } else { "member@example.test" },
                    "name":if mode == 3 { KEY } else { "Member" }}]})
            };
            axum::Json(reply)
        }
    }));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    Stub { url, calls, mode, task }
}
struct Fixture {
    home: tempfile::TempDir, db: Arc<UserDb>, handler: MethodHandler,
    member: UserContext, admin: UserContext, session: String,
}
async fn fixture(stub: &Stub) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    configure(home.path(), stub);
    // Personal connection RPCs use the machine key initialized by gateway setup.
    assert!(crate::config_crypto::encrypt_value("setup", home.path()).is_some());
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let member = db.create_user("m@test.invalid", "Member", "password", UserRole::Employee).unwrap();
    let admin = db.create_user("a@test.invalid", "Admin", "password", UserRole::Admin).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"odoo-member-rpc-tests-secret-32-bytes"))).await;
    Fixture { member: db.live_user_context(&member.id).unwrap().unwrap(),
        admin: db.live_user_context(&admin.id).unwrap().unwrap(), home, db, handler,
        session: uuid::Uuid::new_v4().to_string() }
}
fn configure(home: &Path, stub: &Stub) {
    std::fs::write(home.join("config.toml"), format!("[odoo]\nmember_credentials=true\nurl='{}'\ndb='test'\n", stub.url)).unwrap();
}
async fn rpc(f: &Fixture, ctx: &UserContext, method: &str, args: Value) -> WsFrame {
    let frame = crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), f.session.clone())),
        f.handler.handle_conn(method, args, ctx,
            crate::power_local::RpcConnInfo::from_ws("127.0.0.1:12345".parse().unwrap(), false))).await;
    assert!(!serde_json::to_string(&frame).unwrap().contains(KEY));
    if let Ok(audit) = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")) {
        assert!(!audit.contains(KEY));
    }
    frame
}
fn payload(frame: WsFrame) -> Value {
    match frame { WsFrame::Response { ok: true, payload: Some(p), .. } => p, _ => panic!("expected success") }
}
fn refused(frame: WsFrame) {
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }));
}
async fn connect(f: &Fixture) -> WsFrame {
    rpc(f, &f.member, "me.odoo.connect", json!({"login":"member@example.test", "api_key":KEY})).await
}
#[tokio::test]
async fn odoo_member_connect_status_disconnect_and_admin_management() {
    let s = stub().await;
    let f = fixture(&s).await;
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    payload(connect(&f).await);
    let status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    assert_eq!(status, json!({"member_credentials_enabled":true,"connected":true,"state":"ok","login":"member@example.test","odoo_name":"Member",
        "server":format!("{}/", s.url),"database":"test","verified_at":status["verified_at"],"last_used_at":null}));
    let list = payload(rpc(&f, &f.admin, "odoo.member_credentials.list", json!({})).await);
    assert_eq!(list["credentials"][0]["user_id"], f.member.user_id);
    assert!(!list.to_string().contains("secret"));
    for method in ["odoo.member_credentials.list", "odoo.member_credentials.revoke"] {
        refused(rpc(&f, &f.member, method, json!({"user_id":f.member.user_id})).await);
    }
    payload(rpc(&f, &f.admin, "odoo.member_credentials.revoke", json!({"user_id":f.member.user_id})).await);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    payload(connect(&f).await);
    payload(rpc(&f, &f.member, "me.odoo.disconnect", json!({})).await);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    for event in ["odoo_member_credential_set", "odoo_member_credential_removed", "odoo_member_credential_revoked_by_admin"] {
        assert!(audit.contains(event));
    }
}
#[tokio::test]
async fn odoo_member_rejects_mismatch_denied_false_and_secret_echo() {
    for (mode, reason) in [(1,"login_mismatch"), (2,"access_denied"), (3,"invalid_response"), (4,"access_denied")] {
        let s = stub().await;
        s.mode.store(mode, Ordering::SeqCst);
        let f = fixture(&s).await;
        payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
        refused(connect(&f).await);
        assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
        let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("odoo_member_credential_rejected"));
        assert!(audit.contains(reason));
    }
}
#[tokio::test]
async fn odoo_member_server_change_requires_status_then_explicit_test() {
    let old = stub().await;
    let new = stub().await;
    let f = fixture(&old).await;
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    payload(connect(&f).await);
    let old_status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    configure(f.home.path(), &new);
    refused(rpc(&f, &f.member, "me.odoo.test", json!({})).await);
    assert_eq!(new.calls.load(Ordering::SeqCst), 0);
    let status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    assert_eq!(status["state"], "needs_reverify");
    assert_eq!(old_status["server"], format!("{}/", old.url));
    assert_eq!(status["server"], format!("{}/", new.url));
    assert_ne!(old_status["server"], status["server"]);
    assert_eq!(status["database"], "test");
    assert_eq!(f.db.odoo_credential_status(&f.member.user_id).unwrap().unwrap().state, "needs_reverify");
    payload(rpc(&f, &f.member, "me.odoo.test", json!({})).await);
    assert_eq!(new.calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.db.odoo_credential_status(&f.member.user_id).unwrap().unwrap().state, "ok");
    new.mode.store(2, Ordering::SeqCst);
    refused(rpc(&f, &f.member, "me.odoo.test", json!({})).await);
    assert_eq!(f.db.odoo_credential_status(&f.member.user_id).unwrap().unwrap().state, "invalid");
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("odoo_member_credential_verified"));
    assert!(audit.contains("odoo_member_credential_invalidated"));
}
#[tokio::test]
async fn odoo_member_shared_rate_limit_refuses_sixth_before_network() {
    let s = stub().await;
    let f = fixture(&s).await;
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    payload(connect(&f).await);
    for _ in 0..4 { payload(rpc(&f, &f.member, "me.odoo.test", json!({})).await); }
    let before = s.calls.load(Ordering::SeqCst);
    refused(connect(&f).await);
    assert_eq!(s.calls.load(Ordering::SeqCst), before);
}
#[tokio::test]
async fn odoo_member_browser_live_identity_and_parameter_gates() {
    let s = stub().await;
    let f = fixture(&s).await;
    refused(f.handler.handle("me.odoo.status", json!({}), &f.member).await);
    let mut key_ctx = f.member.clone();
    key_ctx.member_key_id = Some("fake-member-key".into());
    // Direct handler call also refuses even if a future dispatcher changes.
    refused(f.handler.handle_odoo_member_rpc("me.odoo.connect", json!({"login":"member@example.test","api_key":KEY}), &key_ctx).await);
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    for args in [json!({"login":" member@example.test", "api_key":KEY}),
        json!({"login":"member@example.test", "api_key":"bad\nkey"}),
        json!({"login":"member@example.test", "api_key":KEY,"user_id":f.admin.user_id}),
        json!({"login":"a".repeat(255), "api_key":KEY}),
        json!({"login":"member@example.test", "api_key":"x".repeat(513)})] {
        refused(rpc(&f, &f.member, "me.odoo.connect", args).await);
    }
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
    let raw = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    raw.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&f.member.user_id]).unwrap();
    refused(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    raw.execute("UPDATE users SET role='employee' WHERE id=?1", [&f.admin.user_id]).unwrap();
    refused(rpc(&f, &f.admin, "odoo.member_credentials.list", json!({})).await);
}
#[tokio::test]
async fn odoo_member_config_fails_closed() {
    let s = stub().await;
    let f = fixture(&s).await;
    std::fs::write(f.home.path().join("config.toml"), "").unwrap();
    let status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    assert_eq!(status["server"], Value::Null);
    assert_eq!(status["database"], Value::Null);
    refused(connect(&f).await);
    for text in ["broken [", "[odoo]\nmember_credentials=true\nurl=7\ndb='test'"] {
        std::fs::write(f.home.path().join("config.toml"), text).unwrap();
        // Malformed TOML turns the switch off; status still answers. A valid
        // opt-in with an invalid destination retains its configuration error.
        if text == "broken [" {
            assert_eq!(payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await)["member_credentials_enabled"], false);
        } else {
            refused(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
        }
        refused(connect(&f).await);
    }
    std::fs::remove_file(f.home.path().join("config.toml")).unwrap();
    assert_eq!(payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await)["member_credentials_enabled"], false);
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn odoo_member_unreachable_is_audited_without_storing() {
    let s = stub().await;
    let f = fixture(&s).await;
    let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", unavailable.local_addr().unwrap());
    drop(unavailable);
    std::fs::write(f.home.path().join("config.toml"), format!("[odoo]\nmember_credentials=true\nurl='{url}'\ndb='test'\n")).unwrap();
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    refused(connect(&f).await);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("unreachable"));
}

#[tokio::test]
async fn odoo_member_connect_requires_status_in_the_same_browser_session() {
    let s = stub().await;
    let mut f = fixture(&s).await;
    refused(connect(&f).await);
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    f.session = uuid::Uuid::new_v4().to_string();
    refused(connect(&f).await);
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    payload(connect(&f).await);
    assert_eq!(s.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn odoo_member_connect_refuses_destination_changed_after_status() {
    let old = stub().await;
    let new = stub().await;
    let f = fixture(&old).await;
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    configure(f.home.path(), &new);
    refused(connect(&f).await);
    assert_eq!(old.calls.load(Ordering::SeqCst), 0);
    assert_eq!(new.calls.load(Ordering::SeqCst), 0);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    payload(connect(&f).await);
    assert_eq!(new.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn odoo_member_status_shows_full_destination_and_database() {
    let s = stub().await;
    let f = fixture(&s).await;
    for (url, database, shown) in [
        ("https://odoo.example.test", "first", "https://odoo.example.test:443/"),
        ("https://odoo.example.test:8443/base", "second", "https://odoo.example.test:8443/base"),
        ("https://odoo.example.test:8443/other", "third", "https://odoo.example.test:8443/other"),
    ] {
        std::fs::write(f.home.path().join("config.toml"),
            format!("[odoo]\nmember_credentials=true\nurl='{url}'\ndb='{database}'\n")).unwrap();
        let status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
        assert_eq!(status["server"], shown);
        assert_eq!(status["database"], database);
    }
    for url in ["https://user:secret@odoo.example.test", "https://odoo.example.test/?secret=hidden",
        "https://odoo.example.test/#hidden"] {
        std::fs::write(f.home.path().join("config.toml"),
            format!("[odoo]\nmember_credentials=true\nurl='{url}'\ndb='test'\n")).unwrap();
        refused(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    }
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn odoo_member_missing_machine_key_refuses_connect_before_network() {
    let s = stub().await;
    let f = fixture(&s).await;
    payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    std::fs::remove_file(f.home.path().join(".keyfile")).unwrap();
    refused(connect(&f).await);
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
    assert!(!f.home.path().join(".keyfile").exists());
}


fn stored_credential(f: &Fixture) {
    let server = crate::odoo_identity::server::company_server(f.home.path()).unwrap().unwrap();
    let sealer = crate::odoo_credentials::Sealer::load(f.home.path()).unwrap();
    f.db.odoo_credential_put(&f.member.user_id, "member@example.test", 7, "Member", KEY, &server.fp, &sealer).unwrap();
}

#[tokio::test]
async fn odoo_member_switch_off_refuses_before_unseal_or_network_and_keeps_rows() {
    let s = stub().await;
    let f = fixture(&s).await;
    stored_credential(&f);
    // No machine key: a test that reached unseal would fail for another reason.
    std::fs::remove_file(f.home.path().join(".keyfile")).unwrap();
    let base = format!("[odoo]\nurl='{}'\ndb='test'\n", s.url);
    for setting in ["", "member_credentials=false\n", "member_credentials='true'\n", "member_credentials=1\n"] {
        std::fs::write(f.home.path().join("config.toml"), format!("{base}{setting}")).unwrap();
        let status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
        assert_eq!(status["member_credentials_enabled"], false);
        assert_eq!(status["connected"], true);
        assert_eq!(status["state"], "ok");
        for frame in [connect(&f).await, rpc(&f, &f.member, "me.odoo.test", json!({})).await] {
            assert!(serde_json::to_string(&frame).unwrap().contains("公司尚未開放同仁連接自己的 Odoo 帳號"));
            refused(frame);
        }
        assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_some());
        let list = payload(rpc(&f, &f.admin, "odoo.member_credentials.list", json!({})).await);
        assert_eq!(list["enabled"], false);
        assert_eq!(list["credentials"].as_array().unwrap().len(), 1);
    }
    // Unreadable and invalid TOML also fail closed, including status.
    for raw in [Some("[broken"), None] {
        let path = f.home.path().join("config.toml");
        if let Some(raw) = raw { std::fs::write(&path, raw).unwrap(); }
        else { std::fs::remove_file(&path).unwrap(); std::fs::create_dir(&path).unwrap(); }
        assert_eq!(payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await)["member_credentials_enabled"], false);
        refused(connect(&f).await);
        refused(rpc(&f, &f.member, "me.odoo.test", json!({})).await);
    }
    payload(rpc(&f, &f.member, "me.odoo.disconnect", json!({})).await);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn odoo_member_switch_admin_commit_is_guarded_and_audited() {
    let s = stub().await;
    let f = fixture(&s).await;
    stored_credential(&f);
    let path = f.home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap().replace("member_credentials=true", "member_credentials=true # keep comment");
    std::fs::write(&path, &raw).unwrap();
    for args in [json!({}), json!({"enabled":"false"}), json!({"enabled":false,"user_id":f.admin.user_id})] {
        refused(rpc(&f, &f.admin, "odoo.member_credentials.set_enabled", args).await);
    }
    refused(rpc(&f, &f.member, "odoo.member_credentials.set_enabled", json!({"enabled":false})).await);
    let mut key = f.admin.clone();
    key.member_key_id = Some("member-key-context".into());
    refused(rpc(&f, &key, "odoo.member_credentials.set_enabled", json!({"enabled":false})).await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    let result = payload(rpc(&f, &f.admin, "odoo.member_credentials.set_enabled", json!({"enabled":false})).await);
    assert_eq!(result, json!({"enabled":false}));
    assert!(std::fs::read_to_string(&path).unwrap().contains("# keep comment"));
    let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(doc["odoo"]["member_credentials"].as_bool(), Some(false));
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_some());
    let list = payload(rpc(&f, &f.admin, "odoo.member_credentials.list", json!({})).await);
    assert_eq!(list["enabled"], false);
    assert_eq!(list["credentials"].as_array().unwrap().len(), 1);
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    let event: Value = audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["event_type"] == "odoo_member_credentials_toggled").unwrap();
    assert_eq!(event["details"]["actor_user_id"], f.admin.user_id);
    assert_eq!(event["details"]["enabled"], false);
    assert_eq!(event["details"]["acknowledged"], false);
    assert_eq!(event["details"]["phase"], "before_commit");
    assert!(chrono::DateTime::parse_from_rfc3339(event["timestamp"].as_str().unwrap()).is_ok());
    payload(rpc(&f, &f.admin, "odoo.member_credentials.revoke", json!({"user_id":f.member.user_id})).await);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
    let disabled_config = std::fs::read_to_string(&path).unwrap();
    let audit_before = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    for args in [json!({"enabled":true}), json!({"enabled":true,"acknowledge_risk":false})] {
        let frame = rpc(&f, &f.admin, "odoo.member_credentials.set_enabled", args).await;
        assert!(serde_json::to_string(&frame).unwrap().contains("請先確認你已了解風險"));
        refused(frame);
    }
    for ack in [json!("true"), json!(1), Value::Null] {
        refused(rpc(&f, &f.admin, "odoo.member_credentials.set_enabled",
            json!({"enabled":true,"acknowledge_risk":ack})).await);
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), disabled_config);
    assert_eq!(std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap(), audit_before);
    payload(rpc(&f, &f.admin, "odoo.member_credentials.set_enabled", json!({"enabled":true,"acknowledge_risk":true})).await);
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    let event = audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event_type"] == "odoo_member_credentials_toggled").last().unwrap();
    assert_eq!(event["details"]["enabled"], true);
    assert_eq!(event["details"]["acknowledged"], true);
    assert_eq!(payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await)["member_credentials_enabled"], true);
    // The supplied Admin context cannot override a live demotion.
    let db = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    db.execute("UPDATE users SET role='employee' WHERE id=?1", [&f.admin.user_id]).unwrap();
    refused(rpc(&f, &f.admin, "odoo.member_credentials.set_enabled", json!({"enabled":false})).await);
    assert!(crate::odoo_identity::server::member_credentials_enabled(f.home.path()));
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn odoo_member_switch_audit_failure_leaves_config_unchanged() {
    let s = stub().await;
    let f = fixture(&s).await;
    let path = f.home.path().join("config.toml");
    let original = std::fs::read_to_string(&path).unwrap().replace("member_credentials=true", "member_credentials=false");
    std::fs::write(&path, &original).unwrap();
    let audit = f.home.path().join("security_audit.jsonl");
    // set_user_db may already have emitted an edition-watcher audit record.
    match std::fs::remove_file(&audit) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => panic!("remove fixture audit: {error}"),
    }
    std::fs::create_dir(&audit).unwrap();
    refused(rpc(&f, &f.admin, "odoo.member_credentials.set_enabled", json!({"enabled":true,"acknowledge_risk":true})).await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn odoo_member_epoch_requires_new_key_after_fresh_browser_login() {
    let s = stub().await;
    let f = fixture(&s).await;
    stored_credential(&f);
    let db = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    db.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&f.member.user_id]).unwrap();
    let live = f.db.live_user_context(&f.member.user_id).unwrap().unwrap();
    let status = payload(rpc(&f, &live, "me.odoo.status", json!({})).await);
    assert_eq!(status["state"], "needs_reverify");
    let frame = rpc(&f, &live, "me.odoo.test", json!({})).await;
    assert!(serde_json::to_string(&frame).unwrap().contains("請重新輸入 Odoo 登入名稱和金鑰"));
    refused(frame);
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
    payload(rpc(&f, &live, "me.odoo.connect", json!({"login":"member@example.test","api_key":KEY})).await);
    assert_eq!(f.db.odoo_credential_status(&live.user_id).unwrap().unwrap().state, "ok");
}
