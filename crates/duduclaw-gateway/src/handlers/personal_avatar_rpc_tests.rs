//! D4 session/memory RPC regression coverage. Protected gates take an explicit
//! flag so these tests never mutate the process isolation flag or global ACL.
use super::*;
use crate::personal_visibility::tests::{home, assign_avatar, PERSONAL};

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(body), .. } => body,
        other => panic!("expected success: {other:?}"),
    }
}

#[tokio::test]
async fn d4_avatar_memory_and_session_rpc_reads_without_proof_are_audited() {
    let h = home();
    assign_avatar(&h);
    let handler = MethodHandler::new(h.path().into()).await;
    let db = Arc::new(UserDb::new(&h.path().join("users.db")).unwrap());
    handler.set_user_db(db, Arc::new(JwtConfig::new(b"d4-test-secret-at-least-32-bytes!!"))).await;
    let admin = h.db.live_user_context(&h.admin).unwrap().unwrap();
    let sessions = Arc::new(crate::session::SessionManager::new(&h.path().join("sessions.db")).unwrap());
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    handler.set_reply_ctx(Arc::new(crate::channel_reply::ReplyContext::new(
        Arc::new(RwLock::new(duduclaw_agent::AgentRegistry::new(h.path().join("agents")))),
        h.path().into(), sessions.clone(), Arc::new(RwLock::new(HashMap::new())), tx,
    ))).await;
    {
        let conn = sessions.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id,title)
            VALUES('avatar-session',?1,?2,?2,'webchat_user',?3,'Avatar conversation')",
            rusqlite::params![PERSONAL, now, h.alice]).unwrap();
        conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp)
            VALUES('avatar-session','user','avatar conversation secret',?1)", [&now]).unwrap();
    }
    let path = h.path().join("agents").join(PERSONAL).join("memory.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let engine = SqliteMemoryEngine::new(&path).unwrap();
    engine.store(PERSONAL, duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(), agent_id: PERSONAL.into(), content: "avatar memory secret".into(),
        timestamp: Utc::now(), tags: vec![], embedding: None, layer: Default::default(), importance: 5.0,
        access_count: 0, last_accessed: None, source_event: "conversation_summary".into(),
    }).await.unwrap();

    visibility::with_break_glass_scope(async {
        handler.personal_data_gate_under(&admin, Target {
            method: "memory.browse", category: DataCategory::Memory, agent: PERSONAL,
        }, None, true).await.unwrap();
        let body = payload(handler.handle_memory_browse(json!({"agent_id": PERSONAL})).await);
        assert!(body["entries"].to_string().contains("avatar memory secret"));
        assert!(visibility::avatar_read_granted(h.path(), &h.admin, PERSONAL));
    }).await;
    visibility::with_break_glass_scope(async {
        handler.personal_data_gate_under(&admin, Target {
            method: "chat.sessions.history", category: DataCategory::Sessions, agent: PERSONAL,
        }, None, true).await.unwrap();
        let body = payload(handler.handle_chat_sessions_history(json!({"session_id": "avatar-session"}), &admin).await);
        assert!(body["messages"].to_string().contains("avatar conversation secret"));
    }).await;
    visibility::with_break_glass_scope(async {
        handler.personal_avatar_list_reads(&admin, "chat.sessions.list", true).await.unwrap();
        assert!(visibility::avatar_read_granted(h.path(), &h.admin, PERSONAL));
        let body = payload(handler.handle_chat_sessions_list(json!({}), &admin).await);
        assert!(body["sessions"].to_string().contains("avatar-session"));
    }).await;
    visibility::with_break_glass_scope(async {
        handler.personal_data_gate_under(&admin, Target {
            method: "search.query", category: DataCategory::Sessions, agent: PERSONAL,
        }, None, true).await.unwrap();
        let wiki = h.path().join("agents").join(PERSONAL).join("wiki");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::write(wiki.join("private.md"), "# avatar\navatar wiki must remain private").unwrap();
        let body = payload(handler.handle_search_query(json!({"agent_id": PERSONAL, "q": "avatar"}), &admin).await);
        assert!(body["hits"].to_string().contains("avatar conversation secret"));
        assert!(!body["hits"].to_string().contains("avatar wiki must remain private"));
    }).await;
    let rows = h.db.query_audit_log(Some(&h.admin), Some("isolation.avatar_data_read"), 50).unwrap();
    for method in ["memory.browse", "chat.sessions.history", "chat.sessions.list", "search.query"] {
        assert!(rows.iter().any(|row| {
            let detail: Value = serde_json::from_str(row.detail.as_deref().unwrap()).unwrap();
            detail["method"] == method && detail["member_user_id"] == h.alice
        }), "missing audit for {method}");
    }
}

#[test]
fn d4_push_rechecks_avatar_and_audits_each_allowed_frame() {
    use crate::handlers::{personal_push, push_filter::PreparedPush};
    let h = home();
    assign_avatar(&h);
    for event in ["chat.sessions.updated", "memory.changed"] {
        let frame = PreparedPush { line: json!({"event": event, "payload": {"agent_id": PERSONAL}}).to_string(),
            task: None, approval: None };
        assert!(!personal_push::withheld(h.path(), true, &h.admin, &frame));
        assert!(personal_push::withheld(h.path(), true, &h.bob, &frame));
        h.db.set_avatar_as(&h.admin, &h.alice, None).unwrap();
        assert!(personal_push::withheld(h.path(), true, &h.admin, &frame));
        assign_avatar(&h);
    }
    let work = PreparedPush { line: json!({"event": "canvas.updated", "payload": {"agent_id": PERSONAL}}).to_string(),
        task: None, approval: None };
    assert!(personal_push::withheld(h.path(), true, &h.admin, &work));
    let rows = h.db.query_audit_log(Some(&h.admin), Some("isolation.avatar_data_read"), 50).unwrap();
    assert_eq!(rows.len(), 2);
    let frame = PreparedPush { line: json!({"event": "memory.changed", "payload": {"agent_id": PERSONAL}}).to_string(),
        task: None, approval: None };
    rusqlite::Connection::open(h.path().join("users.db")).unwrap()
        .execute("ALTER TABLE users RENAME COLUMN avatar_agent_id TO unreadable_avatar", []).unwrap();
    assert!(personal_push::withheld(h.path(), true, &h.admin, &frame));
}
