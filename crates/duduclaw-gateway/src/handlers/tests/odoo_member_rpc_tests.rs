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
    std::fs::write(home.join("config.toml"), format!("[odoo]\nurl='{}'\ndb='test'\n", stub.url)).unwrap();
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
    payload(connect(&f).await);
    let status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    assert_eq!(status, json!({"connected":true,"state":"ok","login":"member@example.test","odoo_name":"Member",
        "server":"127.0.0.1","verified_at":status["verified_at"],"last_used_at":null}));
    let list = payload(rpc(&f, &f.admin, "odoo.member_credentials.list", json!({})).await);
    assert_eq!(list["credentials"][0]["user_id"], f.member.user_id);
    assert!(!list.to_string().contains("secret"));
    for method in ["odoo.member_credentials.list", "odoo.member_credentials.revoke"] {
        refused(rpc(&f, &f.member, method, json!({"user_id":f.member.user_id})).await);
    }
    payload(rpc(&f, &f.admin, "odoo.member_credentials.revoke", json!({"user_id":f.member.user_id})).await);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
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
    payload(connect(&f).await);
    configure(f.home.path(), &new);
    refused(rpc(&f, &f.member, "me.odoo.test", json!({})).await);
    assert_eq!(new.calls.load(Ordering::SeqCst), 0);
    let status = payload(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    assert_eq!(status["state"], "needs_reverify");
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
    refused(connect(&f).await);
    for text in ["broken [", "[odoo]\nurl=7\ndb='test'"] {
        std::fs::write(f.home.path().join("config.toml"), text).unwrap();
        refused(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
        refused(connect(&f).await);
    }
    std::fs::remove_file(f.home.path().join("config.toml")).unwrap();
    refused(rpc(&f, &f.member, "me.odoo.status", json!({})).await);
    assert_eq!(s.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn odoo_member_unreachable_is_audited_without_storing() {
    let s = stub().await;
    let f = fixture(&s).await;
    let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", unavailable.local_addr().unwrap());
    drop(unavailable);
    std::fs::write(f.home.path().join("config.toml"), format!("[odoo]\nurl='{url}'\ndb='test'\n")).unwrap();
    refused(connect(&f).await);
    assert!(f.db.odoo_credential_status(&f.member.user_id).unwrap().is_none());
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("unreachable"));
}
