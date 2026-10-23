use super::*;

struct Fx {
    home: tempfile::TempDir,
    handler: MethodHandler,
    db: Arc<UserDb>,
    manager: UserContext,
    admin: UserContext,
    employee: String,
    external: String,
}
fn payload(f: WsFrame) -> Value {
    match f {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected success: {other:?}"),
    }
}
fn denied(f: WsFrame) -> Value {
    match f {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e,
        other => panic!("expected denial: {other:?}"),
    }
}
async fn fixture() -> Fx {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[people]\nmanager_team_history=true",
    )
    .unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let manager = db
        .create_user(
            "manager@test.invalid",
            "主管",
            "isolated-password",
            UserRole::Manager,
        )
        .unwrap()
        .id;
    let employee = db
        .create_user(
            "employee@test.invalid",
            "職員",
            "isolated-password",
            UserRole::Employee,
        )
        .unwrap()
        .id;
    let admin = db
        .create_user(
            "admin@test.invalid",
            "管理員",
            "isolated-password",
            UserRole::Admin,
        )
        .unwrap()
        .id;
    db.set_manager(&employee, Some(&manager)).unwrap();
    db.bind_agent(&employee, "sales", AccessLevel::Operator)
        .unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
        .await;
    handler
        .set_user_db(
            db.clone(),
            Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!")),
        )
        .await;
    let registry = Arc::new(RwLock::new(duduclaw_agent::AgentRegistry::new(
        home.path().join("agents"),
    )));
    let sm =
        Arc::new(crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap());
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let rc = Arc::new(crate::channel_reply::ReplyContext::new(
        registry,
        home.path().into(),
        sm.clone(),
        Arc::new(RwLock::new(HashMap::new())),
        tx,
    ));
    handler.set_reply_ctx(rc).await;
    let since = crate::team_oversight::observe_window(home.path(), &db, "test")
        .unwrap()
        .since_micros
        .unwrap();
    let now = DateTime::from_timestamp_micros(since + 1_000_000)
        .unwrap()
        .to_rfc3339();
    let old = DateTime::from_timestamp_micros(since - 1)
        .unwrap()
        .to_rfc3339();
    let external = "line:C0123456789abcdef0123456789abcdef".to_string();
    {
        let conn = sm.acquire().await;
        for (id, kind, owner) in [
            ("report", "webchat_user", employee.as_str()),
            ("hidden", "webchat_user", "stranger"),
            (external.as_str(), "channel", ""),
        ] {
            conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id,title,summary) VALUES (?1,'sales',?2,?2,?3,?4,'OLD-TITLE-SECRET','SUMMARY-SECRET')",params![id,now,kind,owner]).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES (?1,'user','OLD-CONTENT-SECRET',?2)",params![id,old]).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES (?1,'user','電話0912-345-678和a@example.com',?2)",params![id,now]).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES (?1,'assistant','收到0912345678，📎DELIVER:/private/person.txt',?2)",params![id,now]).unwrap();
        }
    }
    let manager = db.live_user_context(&manager).unwrap().unwrap();
    let admin = db.live_user_context(&admin).unwrap().unwrap();
    Fx {
        home,
        handler,
        db,
        manager,
        admin,
        employee,
        external,
    }
}

#[tokio::test]
async fn manager_reads_report_fulltext_external_masked_and_never_gains_old_readers() {
    manager_reads_body().await;
}

async fn manager_reads_body() {
    let f = fixture().await;
    let mine_rc = f.handler.reply_ctx.read().await.clone().unwrap();
    {
        let conn = mine_rc.session_manager.acquire().await;
        conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id) SELECT 'mine',agent_id,last_active,created_at,'webchat_user',?1 FROM sessions WHERE id='report'",[&f.manager.user_id]).unwrap();
        conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) SELECT 'mine',role,content,timestamp FROM session_messages WHERE session_id='report'",[]).unwrap();
    }
    let mine = payload(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"mine"}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(mine["class"], "own");
    assert_eq!(mine["read_only"], true);
    let own = payload(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"report"}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(own["class"], "report_webchat");
    assert_eq!(own["read_only"], true);
    assert!(own.to_string().contains("0912-345-678"));
    assert!(!own.to_string().contains("OLD-CONTENT") && !own.to_string().contains("/private/"));
    let external = payload(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":f.external}),
                &f.manager,
            )
            .await,
    );
    let text = external.to_string();
    for secret in [
        "0912",
        "a@example.com",
        "person.txt",
        "OLD-CONTENT",
        "SUMMARY-SECRET",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
    assert_eq!(external["ner"], false);
    assert_eq!(external["messages"].as_array().unwrap().len(), 2);
    for params in [
        json!({"agent_id":"sales","class":false}),
        json!({"agent_id":"sales","report_user_id":7}),
        json!({"agent_id":"sales","before":"x".repeat(4097)}),
    ] {
        assert_eq!(
            denied(
                f.handler
                    .handle("users.team_history.list", params, &f.manager)
                    .await
            )["code"],
            "invalid_params"
        );
    }
    let list = payload(
        f.handler
            .handle(
                "users.team_history.list",
                json!({"agent_id":"sales","limit":1}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains("OLD-TITLE") && !list.to_string().contains("hidden"));
    assert!(list["next_before"].is_string());
    denied(
        f.handler
            .handle(
                "chat.sessions.history",
                json!({"session_id":"report"}),
                &f.manager,
            )
            .await,
    );
    denied(
        f.handler
            .handle("runs.get", json!({"session_id":"report"}), &f.manager)
            .await,
    );
    let a = denied(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"hidden"}),
                &f.manager,
            )
            .await,
    );
    let b = denied(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"missing"}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(a, b);
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    rc.session_manager.acquire().await.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id) VALUES ('other-private','finance','2026-10-23T00:00:00Z','2026-10-23T00:00:00Z','webchat_user','stranger')",[]).unwrap();
    let inaccessible = denied(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"other-private"}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(inaccessible, b);
    denied(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"report"}),
                &f.admin,
            )
            .await,
    );
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert_eq!(audit.matches("team_history_viewed").count(), 3);
    assert!(!audit.contains(&f.external));
    assert!(!audit.contains("0912") && !audit.contains("a@example.com"));
}

#[test]
fn team_history_rpc_future_sizes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let f = runtime.block_on(Box::pin(fixture()));
    fn constructed_bytes<F: std::future::Future>(make: impl FnOnce() -> F) -> usize {
        let future = make();
        std::mem::size_of_val(&future)
    }
    fn type_bytes<F: std::future::Future>(_: impl FnOnce() -> F) -> usize {
        std::mem::size_of::<F>()
    }
    // Construct one RPC at a time and never poll it. Report the long test's
    // exact type size without constructing its potentially oversized frame.
    let handle = constructed_bytes(|| {
        f.handler.handle(
            "users.team_history.get",
            json!({"session_id":"report"}),
            &f.manager,
        )
    });
    let conn = constructed_bytes(|| {
        f.handler.handle_conn(
            "users.team_history.get",
            json!({"session_id":"report"}),
            &f.manager,
            crate::power_local::RpcConnInfo::internal(),
        )
    });
    let dispatch = constructed_bytes(|| {
        f.handler.dispatch(
            "users.team_history.get",
            json!({"session_id":"report"}),
            &f.manager,
            crate::power_local::RpcConnInfo::internal(),
        )
    });
    let org = constructed_bytes(|| {
        f.handler.dispatch_org(
            "users.team_history.get",
            json!({"session_id":"report"}),
            &f.manager,
            crate::power_local::RpcConnInfo::internal(),
        )
    });
    let get = constructed_bytes(|| {
        f.handler
            .handle_team_history_get(json!({"session_id":"report"}), &f.manager)
    });
    let list = constructed_bytes(|| {
        f.handler
            .handle_team_history_list(json!({"agent_id":"sales"}), &f.manager)
    });
    let journey = type_bytes(manager_reads_body);
    let sizes = json!({"handle":handle,"handle_conn":conn,"dispatch":dispatch,"dispatch_org":org,"team_get":get,"team_list":list,"long_test":journey});
    eprintln!("RPC_FUTURE_SIZES {sizes}");
    assert!(
        handle <= 4096,
        "RPC entry future grew to {handle} bytes; keep dispatcher boxed"
    );
    assert!(
        conn <= 4096,
        "transport RPC future grew to {conn} bytes; keep dispatcher boxed"
    );
}

#[tokio::test]
async fn revocation_audit_failure_and_partial_ner_install_never_release_content() {
    let f = fixture().await;
    std::fs::create_dir_all(f.home.path().join("models/privacy-filter")).unwrap();
    assert_eq!(
        denied(
            f.handler
                .handle(
                    "users.team_history.get",
                    json!({"session_id":f.external}),
                    &f.manager
                )
                .await
        )["code"],
        "masking_failed"
    );
    std::fs::remove_dir(f.home.path().join("models/privacy-filter")).unwrap();
    std::fs::remove_file(f.home.path().join("security_audit.jsonl")).unwrap();
    std::fs::create_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    assert_eq!(
        denied(
            f.handler
                .handle(
                    "users.team_history.get",
                    json!({"session_id":"report"}),
                    &f.manager
                )
                .await
        )["code"],
        "audit_unavailable"
    );
    std::fs::remove_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    f.db.set_manager(&f.employee, None).unwrap();
    denied(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"report"}),
                &f.manager,
            )
            .await,
    );
}

#[tokio::test]
async fn admin_switch_advances_window_and_manager_cannot_write_it() {
    let f = fixture().await;
    denied(
        f.handler
            .handle(
                "users.org_settings.update",
                json!({"manager_team_history":false}),
                &f.manager,
            )
            .await,
    );
    let first = payload(
        f.handler
            .handle("users.org_settings.get", json!({}), &f.admin)
            .await,
    );
    assert_eq!(
        payload(
            f.handler
                .handle(
                    "users.org_settings.update",
                    json!({"manager_team_history":false}),
                    &f.admin
                )
                .await
        )["manager_team_history"],
        false
    );
    assert_eq!(
        denied(
            f.handler
                .handle(
                    "users.team_history.list",
                    json!({"agent_id":"sales"}),
                    &f.manager
                )
                .await
        )["code"],
        "team_history_off"
    );
    let next = payload(
        f.handler
            .handle(
                "users.org_settings.update",
                json!({"manager_team_history":true}),
                &f.admin,
            )
            .await,
    );
    assert_ne!(first["since"], next["since"]);
    denied(
        f.handler
            .handle(
                "users.org_settings.update",
                json!({"manager_team_history":"yes"}),
                &f.admin,
            )
            .await,
    );
}

#[tokio::test]
async fn get_burst_limit_is_enforced_before_the_twenty_first_disclosure() {
    let f = fixture().await;
    for _ in 0..20 {
        payload(
            f.handler
                .handle(
                    "users.team_history.get",
                    json!({"session_id":"report"}),
                    &f.manager,
                )
                .await,
        );
    }
    assert_eq!(
        denied(
            f.handler
                .handle(
                    "users.team_history.get",
                    json!({"session_id":"report"}),
                    &f.manager
                )
                .await
        )["code"],
        "rate_limited"
    );
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert_eq!(audit.matches("team_history_viewed").count(), 20);
    let missing = denied(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":"missing"}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(missing["code"], "rate_limited");
}

#[tokio::test]
async fn counts_and_audit_summary_describe_only_returned_message_parts() {
    let f = fixture().await;
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    let time = (Utc::now() + chrono::Duration::seconds(2)).to_rfc3339();
    rc.session_manager.acquire().await.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES (?1,'assistant','聯絡b@example.com',?2)",params![f.external,time]).unwrap();
    let result = payload(
        f.handler
            .handle(
                "users.team_history.get",
                json!({"session_id":f.external,"limit":1}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(result["messages"].as_array().unwrap().len(), 1);
    assert!(result["masked_counts"].get("PHONE").is_none());
    assert_eq!(result["masked_counts"]["EMAIL"], 1);
    let unified = payload(
        f.handler
            .handle(
                "audit.unified_log",
                json!({"sources":["security"]}),
                &f.admin,
            )
            .await,
    );
    let rows = unified["events"].as_array().unwrap();
    let viewed = rows
        .iter()
        .find(|r| r["event_type"] == "team_history_viewed")
        .unwrap();
    assert!(viewed["summary"].as_str().unwrap().contains("主管檢視了"));
    assert!(viewed["summary"].as_str().unwrap().contains("遮蔽1處"));
}
