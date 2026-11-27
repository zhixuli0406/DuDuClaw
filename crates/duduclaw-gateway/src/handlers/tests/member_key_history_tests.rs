//! RM-141K-P1: member keys cannot inherit browser conversation authority.
use super::*;

const OWN: &str = "webchat:own";
const OTHER: &str = "webchat:other";
const EXTERNAL: &str = "line:C0123456789abcdef0123456789abcdef";

struct Fixture {
    home: tempfile::TempDir,
    handler: MethodHandler,
    db: Arc<UserDb>,
    sessions: Arc<crate::session::SessionManager>,
    browser: UserContext,
    key: UserContext,
}

async fn fixture(level: AccessLevel, privacy: bool) -> Fixture {
    let home = crate::fs_safe::private_test_home().unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[people]\nbound_history_privacy={privacy}\n[member_api_keys]\nenabled=true\n"
    )).unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let user = db.create_user("holder@test.invalid", "Holder", "Test-Password-12345", UserRole::Employee).unwrap();
    db.bind_agent(&user.id, "sales", level).unwrap();
    let browser = db.live_user_context(&user.id).unwrap().unwrap();
    let issued = db.member_key_create(&user.id, "history", 1, browser.credential_epoch, true, &[]).unwrap();
    let key = crate::member_keys::authenticate(home.path(), &db, &issued.secret).unwrap();
    assert!(key.member_key_id.is_some());
    let handler = MethodHandler::new(home.path().into()).await;
    handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"member-history-test-secret-32-bytes"))).await;
    let sessions = Arc::new(crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap());
    let registry = Arc::new(RwLock::new(duduclaw_agent::AgentRegistry::new(home.path().join("agents"))));
    let (tx, _) = tokio::sync::broadcast::channel(16);
    handler.set_reply_ctx(Arc::new(crate::channel_reply::ReplyContext::new(
        registry, home.path().into(), sessions.clone(), Arc::new(RwLock::new(HashMap::new())), tx,
    ))).await;
    {
        let conn = sessions.acquire().await;
        for (id, kind, owner, stamp) in [
            (OWN, "webchat_user", user.id.as_str(), "2026-11-01T00:00:00Z"),
            (OTHER, "webchat_user", "another-member", "2026-11-02T00:00:00Z"),
            (EXTERNAL, "channel", "", "2026-11-03T00:00:00Z"),
        ] {
            conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id,title) VALUES (?1,'sales',?2,?2,?3,?4,?1)", params![id,stamp,kind,owner]).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES (?1,'user',?1,?2)", params![id,stamp]).unwrap();
        }
    }
    Fixture { home, handler, db, sessions, browser, key }
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p,
        other => panic!("expected success: {other:?}"),
    }
}

fn denied(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => e,
        other => panic!("expected denial: {other:?}"),
    }
}

async fn archive_all(f: &Fixture) {
    let conn = f.sessions.acquire().await;
    conn.execute("UPDATE sessions SET archived_at='2026-11-04T00:00:00Z',archive_reason='retention'", []).unwrap();
}

async fn assert_key_matrix(level: AccessLevel, privacy: bool) {
    let f = fixture(level, privacy).await;
    let fresh = crate::review_evidence::audience::fresh_dashboard_context(f.home.path(), &f.key).unwrap();
    assert_eq!(fresh.member_key_id, f.key.member_key_id);
    let live = f.handler.bound_history_live(&fresh).unwrap();
    assert_eq!(live.member_key_id, f.key.member_key_id, "live re-read must retain credential restrictions");
    assert_eq!(crate::history_privacy::mode(f.home.path(), &live, "sales").unwrap(), crate::history_privacy::Mode::Filtered);
    assert!(f.handler.bound_history_recheck(&f.key, "sales", crate::history_privacy::Mode::Full).is_err());
    let handle = crate::history_privacy::handles::issue(f.home.path(), &f.key.user_id, EXTERNAL).unwrap();
    let missing = denied(f.handler.handle("chat.sessions.history", json!({"session_id":"missing"}), &f.key).await);
    assert_eq!(missing, "session not found");
    for id in [OTHER, EXTERNAL, handle.as_str()] {
        assert_eq!(denied(f.handler.handle("chat.sessions.history", json!({"session_id":id}), &f.key).await), missing);
    }
    let own = payload(f.handler.handle("chat.sessions.history", json!({"session_id":OWN}), &f.key).await);
    assert_eq!(own["messages"][0]["content"], OWN);
    assert_eq!(own["privacy_mode"], "own");
    for archived in [false, true] {
        if archived { archive_all(&f).await; }
        for limit in [1, 100] {
            let list = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales", "limit":limit, "archived":archived}), &f.key).await);
            let rows = list["sessions"].as_array().unwrap();
            assert_eq!(rows.len(), 1, "external rows must not consume the limit or become masked placeholders");
            assert_eq!(rows[0]["session_id"], OWN);
            assert_ne!(rows[0]["read_only"], true);
        }
    }
    let missing = denied(f.handler.handle("chat.sessions.restore", json!({"session_id":"missing"}), &f.key).await);
    assert_eq!(missing, "session not found");
    for id in [OTHER, EXTERNAL, handle.as_str()] {
        assert_eq!(denied(f.handler.handle("chat.sessions.restore", json!({"session_id":id}), &f.key).await), missing);
        let raw = if id == handle { EXTERNAL } else { id };
        assert!(f.sessions.archive_state(raw).await.unwrap().is_some());
    }
    let restored = payload(f.handler.handle("chat.sessions.restore", json!({"session_id":OWN}), &f.key).await);
    assert_eq!(restored["restored"], true);
    assert!(f.sessions.archive_state(OWN).await.unwrap().is_none());
    assert_eq!(payload(f.handler.handle("chat.sessions.history", json!({"session_id":OWN}), &f.key).await)["messages"][0]["content"], OWN);
    // The external projection itself must refuse even when called directly.
    assert_eq!(f.handler.bound_external_history(&f.key, EXTERNAL, "sales", "line", 10).await.unwrap_err(), "session not found");
    f.db.unbind_agent(&f.key.user_id, "sales").unwrap();
    assert!(f.handler.bound_history_recheck(&f.key, "sales", crate::history_privacy::Mode::Filtered).is_err());
}

#[tokio::test]
async fn owner_member_key_history_list_restore_only_own_with_privacy_on_or_off() {
    for privacy in [true, false] { assert_key_matrix(AccessLevel::Owner, privacy).await; }
}

#[tokio::test]
async fn viewer_member_key_history_list_restore_only_own_with_privacy_on_or_off() {
    for privacy in [true, false] { assert_key_matrix(AccessLevel::Viewer, privacy).await; }
}

#[tokio::test]
async fn owner_browser_history_list_restore_keep_full_access() {
    let f = fixture(AccessLevel::Owner, true).await;
    assert!(f.browser.member_key_id.is_none());
    for id in [OWN, OTHER, EXTERNAL] {
        assert_eq!(payload(f.handler.handle("chat.sessions.history", json!({"session_id":id}), &f.browser).await)["messages"][0]["content"], id);
    }
    for archived in [false, true] {
        if archived { archive_all(&f).await; }
        let list = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales", "archived":archived}), &f.browser).await);
        let ids: Vec<_> = list["sessions"].as_array().unwrap().iter().map(|r| r["session_id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 3);
        for id in [OWN, OTHER, EXTERNAL] { assert!(ids.contains(&id)); }
    }
    for id in [OWN, OTHER, EXTERNAL] {
        assert_eq!(payload(f.handler.handle("chat.sessions.restore", json!({"session_id":id}), &f.browser).await)["restored"], true);
    }
}

#[tokio::test]
async fn member_key_cannot_use_adjacent_full_conversation_routes() {
    let f = fixture(AccessLevel::Owner, true).await;
    for method in ["runs.list", "runs.get", "runs.steps", "tasks.artifacts", "files.list", "files.download", "search.query", "data_exports.create", "users.team_history.get", "users.team_history.restore"] {
        assert!(super::super::member_contract::member_key_method(method).is_none(), "{method}");
        denied(f.handler.handle(method, json!({"agent_id":"sales", "session_id":EXTERNAL}), &f.key).await);
    }
}


#[tokio::test]
async fn member_key_attachment_handles_cannot_bypass_conversation_scope() {
    let f = fixture(AccessLevel::Owner, false).await;
    for session in [OTHER, EXTERNAL, "missing"] {
        let reference = crate::chat_attachments::Reference {
            session: session.into(), message: 1, agent: "sales".into(),
            path: "not-opened.pdf".into(), name: "not-opened.pdf".into(),
        };
        let handle = crate::history_privacy::handles::issue_attachment(
            f.home.path(), &f.key.user_id, &serde_json::to_string(&reference).unwrap(),
        ).unwrap();
        let error = match f.handler.history_attachment_bytes(&f.key, &handle, 1024).await {
            Err(error) => error,
            Ok(_) => panic!("foreign attachment was readable"),
        };
        assert_eq!(error, "session not found");
    }
}

#[tokio::test]
async fn synthetic_admin_member_key_cannot_take_unscoped_full_list() {
    let f = fixture(AccessLevel::Owner, false).await;
    let mut key = UserContext::admin_fallback();
    key.member_key_id = Some("impossible-admin-key".into());
    denied(f.handler.handle_chat_sessions_list(json!({}), &key).await);
    assert_eq!(crate::history_privacy::mode(f.home.path(),
        &f.handler.bound_history_live(&key).unwrap(), "sales").unwrap(),
        crate::history_privacy::Mode::Filtered);
}
