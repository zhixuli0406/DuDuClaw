// Included in member_keys_rpc_tests to share its browser/session fixture.
#[tokio::test]
async fn conversation_methods_require_live_opt_in_and_forget_is_unaffected() {
    let f = fixture().await;
    std::fs::write(f.home.path().join("config.toml"), "[member_api_keys]\nenabled=true\nrate_per_minute=1000").unwrap();
    for include in [false, true] {
        let issued = f.db.member_key_create(&f.member.user_id, "scope", 1, 0, include, &[]).unwrap();
        let ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &issued.secret).unwrap();
        for method in crate::handlers::member_contract::MEMBER_KEY_CONVERSATION_METHODS {
            let result = f.handler.member_key_gate(method, &json!({}), &ctx).await;
            if include { assert!(result.is_ok(), "{method}"); }
            else { assert_eq!(code(result.unwrap_err()), "member_key_conversation_scope", "{method}"); }
        }
        assert!(f.handler.member_key_gate("me.memory.forget", &json!({}), &ctx).await.is_ok());
        if include {
            payload(browser(&f, &f.admin, "member_keys.admin_narrow", json!({"id":issued.info.id,"include_conversations":false})).await);
            assert_eq!(code(f.handler.member_key_gate("me.memory.list", &json!({}), &ctx).await.unwrap_err()), "member_key_conversation_scope");
        }
    }
}

#[tokio::test]
async fn creation_switches_and_narrow_rpc_validation() {
    let f = fixture().await;
    let base = json!({"name":"scope","expires_in_days":30,"password":"test-password"});
    for extra in [json!({"include_conversations":true}), json!({"mcp_scopes":["tasks:read"]}),
        json!({"include_conversations":"true"}), json!({"mcp_scopes":[42]}), json!({"mcp_scopes":null})] {
        let mut args = base.clone(); args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(matches!(browser(&f, &f.member, "me.member_keys.create", args).await, WsFrame::Response { ok:false, .. }));
    }
    std::fs::write(f.home.path().join("config.toml"), "[member_api_keys]\nenabled=true\nallow_conversation_content=true\nallow_mcp_scopes=['tasks:read']").unwrap();
    let mut args = base.clone(); args["mcp_scopes"] = json!(["odoo:read"]);
    assert_eq!(code(browser(&f, &f.member, "me.member_keys.create", args.clone()).await), "member_key_scope_not_allowed");
    args["mcp_scopes"] = json!(["tasks:read", "tasks:read"]); args["include_conversations"] = json!(true);
    let created = payload(browser(&f, &f.member, "me.member_keys.create", args).await);
    assert_eq!(created["key"]["mcp_scopes"], json!(["tasks:read"]));
    assert_eq!(created["key"]["conversations_migrated"], false);
    // Creation switches do not revoke grants on existing keys.
    std::fs::write(f.home.path().join("config.toml"), "[member_api_keys]\nenabled=true").unwrap();
    let ctx = crate::member_keys::authenticate(f.home.path(), &f.db, created["secret"].as_str().unwrap()).unwrap();
    assert!(f.handler.member_key_gate("me.memory.list", &json!({}), &ctx).await.is_ok());
    for (actor, method) in [(&f.member, "me.member_keys.narrow"), (&f.admin, "member_keys.admin_narrow")] {
        payload(browser(&f, actor, method, json!({"id":created["key"]["id"],"include_conversations":false,"mcp_scopes":[]})).await);
        for extra in [json!({"include_conversations":true}), json!({"mcp_scopes":["tasks:read"]})] {
            let mut args = extra; args["id"] = created["key"]["id"].clone();
            assert_eq!(code(browser(&f, actor, method, args).await), "member_key_cannot_widen");
        }
        assert_eq!(code(f.handler.handle(method, json!({"id":created["key"]["id"]}), &ctx).await), "member_key_forbidden");
    }
    assert!(matches!(browser(&f, &f.member, "member_keys.admin_narrow", json!({"id":created["key"]["id"]})).await, WsFrame::Response { ok:false, .. }));
    for (actor, method) in [(&f.member, "me.member_keys.list"), (&f.admin, "member_keys.admin_list")] {
        let listed = payload(browser(&f, actor, method, json!({})).await);
        assert_eq!(listed["keys"][0]["include_conversations"], false);
        assert_eq!(listed["keys"][0]["mcp_scopes"], json!([]));
        assert_eq!(listed["keys"][0]["conversations_migrated"], false);
    }
}

#[tokio::test]
async fn checked_member_key_memory_is_limited_to_own_avatar() {
    let f = fixture().await;
    f.db.bind_agent(&f.member.user_id, "shared", AccessLevel::Owner).unwrap();
    f.db.bind_agent(&f.member.user_id, "avatar", AccessLevel::Owner).unwrap();
    f.db.set_avatar_as(&f.admin.user_id, &f.member.user_id, Some("avatar")).unwrap();
    let key = f.db.member_key_create(&f.member.user_id, "memory", 1, 0, true, &[]).unwrap();
    let ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &key.secret).unwrap();
    for method in ["memory.search", "memory.browse", "memory.key_facts", "memory.history"] {
        let denied = f.handler.dispatch_knowledge(method, json!({"agent_id":"shared","query":"test"}), &ctx,
            crate::power_local::RpcConnInfo::default()).await;
        assert!(matches!(denied, WsFrame::Response { ok:false, error:Some(ref e), .. }
            if e.to_string().contains("這位 AI 員工的記憶包含其他人的對話")));
    }
    assert!(f.handler.member_key_memory_gate(&ctx, "avatar").await.is_ok());
    let result = f.handler.dispatch_knowledge("memory.search", json!({"agent_id":"avatar","query":"test"}), &ctx,
        crate::power_local::RpcConnInfo::default()).await;
    assert_eq!(payload(result)["entries"], json!([]));
    assert!(f.handler.member_key_memory_gate(&f.member, "shared").await.is_ok());
    f.db.set_avatar_as(&f.admin.user_id, &f.member.user_id, None).unwrap();
    assert!(f.handler.member_key_memory_gate(&ctx, "avatar").await.is_err());
}

#[tokio::test]
async fn activity_text_requires_own_conversation_provenance_and_live_opt_in() {
    let f = fixture().await;
    f.db.bind_agent(&f.member.user_id, "sales", AccessLevel::Owner).unwrap();
    let sessions = crate::session::SessionManager::new(&f.home.path().join("sessions.db")).unwrap();
    {
        let conn = sessions.acquire().await;
        for (id, kind, owner) in [("own", "webchat_user", f.member.user_id.as_str()),
            ("other", "webchat_user", "someone-else"), ("line:external", "channel", "")] {
            conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id)
                VALUES(?1,'sales',?2,?2,?3,?4)", params![id, Utc::now().to_rfc3339(), kind, owner]).unwrap();
        }
    }
    let rows: Vec<ActivityRow> = [Some("own"), Some("other"), Some("line:external"), None].into_iter()
        .enumerate().map(|(i, session)| ActivityRow {
            id: i.to_string(), event_type: "agent_reply".into(), agent_id: "sales".into(), task_id: None,
            summary: "private conversation excerpt".into(), timestamp: Utc::now().to_rfc3339(),
            metadata: session.map(|id| json!({"session_id":id}).to_string()),
        }).collect();
    let issued = f.db.member_key_create(&f.member.user_id, "activity", 1, 0, true, &[]).unwrap();
    let ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &issued.secret).unwrap();
    let store = Arc::new(crate::task_store::TaskStore::open(f.home.path()).unwrap());
    let task = TaskRow::new("activity-task".into(), "Task".into(), "".into(), "normal".into(), "sales".into(), "system".into());
    store.insert_task(&task).await.unwrap();
    for row in &rows {
        let mut stored = row.clone();
        stored.task_id = Some(task.id.clone());
        store.append_activity(&stored).await.unwrap();
    }
    f.handler.set_task_store(store).await;
    for args in [json!({"agent_id":"sales"}), json!({"task_id":task.id})] {
        let body = payload(f.handler.handle_activity_list(args, &ctx).await);
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 4);
        for event in events {
            assert_eq!(event["summary"].as_str().unwrap().is_empty(), event["id"] != "0");
            if event["id"] != "0" { assert!(event["metadata"].is_null()); }
        }
    }
    let projected = f.handler.member_key_activity_rows(&ctx, rows.clone()).await.unwrap();
    assert!(!projected[0].summary.is_empty());
    assert!(projected.iter().skip(1).all(|row| row.summary.is_empty() && row.metadata.is_none()));
    let browser_rows = f.handler.member_key_activity_rows(&f.member, rows.clone()).await.unwrap();
    assert!(browser_rows.iter().all(|row| !row.summary.is_empty()));
    f.db.member_key_narrow_admin(&f.admin.user_id, &issued.info.id, Some(false), None).unwrap();
    assert!(f.handler.member_key_activity_rows(&ctx, rows.clone()).await.unwrap().iter()
        .all(|row| row.summary.is_empty() && row.metadata.is_none()));
    f.db.member_key_revoke_own(&f.member.user_id, &issued.info.id).unwrap();
    assert!(f.handler.member_key_activity_rows(&ctx, rows).await.is_err());
}
