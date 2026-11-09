use super::*;

struct Fx {
    home: tempfile::TempDir,
    handler: MethodHandler,
    db: Arc<UserDb>,
    manager: UserContext,
    admin: UserContext,
    employee: String,
    external: String,
    since_micros: i64,
}

#[tokio::test]
async fn receipt_rpc_is_self_only_even_for_admin_and_survives_the_setting_being_off() {
    let f = fixture().await;
    payload(f.handler.handle("users.team_history.get", json!({"session_id":"report"}), &f.manager).await);
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    std::fs::write(f.home.path().join("config.toml"), "[people]\nmanager_team_history=false").unwrap();
    let p = payload(f.handler.handle("users.my_history_views", json!({}), &employee).await);
    assert_eq!(p["views"].as_array().unwrap().len(), 1);
    assert_eq!(p["views"][0]["viewer_display_name"], "主管");
    assert_eq!(p["views"][0]["type"], "webchat");
    let row=p["views"][0].as_object().unwrap();
    assert_eq!(row.len(),4);
    for key in ["details","viewer","report","session","messages","masked","agent_id"] {assert!(!row.contains_key(key));}
    assert!(payload(f.handler.handle("users.my_history_views",json!({}),&f.admin).await)["views"].as_array().unwrap().is_empty());
    assert_eq!(denied(f.handler.handle("users.my_history_views",json!({"user_id":f.employee}),&f.admin).await)["code"],"invalid_params");
}

#[tokio::test]
async fn receipt_rpc_rechecks_a_suspended_account_and_never_hides_a_read_error() {
    let f=fixture().await;
    let employee=f.db.live_user_context(&f.employee).unwrap().unwrap();
    std::fs::write(f.home.path().join("security_audit.jsonl"),"bad json\n").unwrap();
    assert_eq!(denied(f.handler.handle("users.my_history_views",json!({}),&employee).await)["code"],"read_error");
    f.db.set_user_status(&f.employee,duduclaw_auth::UserStatus::Suspended).unwrap();
    let response=f.handler.handle("users.my_history_views",json!({}),&employee).await;
    assert!(matches!(response,WsFrame::Response{ok:false,..}));
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
    fixture_with_manager_id(None).await
}

async fn fixture_with_manager_id(manager_id: Option<&str>) -> Fx {
    let home = crate::fs_safe::private_test_home().unwrap();
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
    let manager = if let Some(manager_id) = manager_id {
        let fixture_id = uuid::Uuid::parse_str(manager_id).unwrap();
        assert_eq!(fixture_id.get_version_num(), 4);
        // No reference to this newly created user exists yet. Keep the
        // deterministic ID local to the one regression journey that opts in.
        let changed = rusqlite::Connection::open(home.path().join("users.db"))
            .unwrap()
            .execute(
                "UPDATE users SET id = ?1 WHERE id = ?2",
                params![manager_id, manager],
            )
            .unwrap();
        assert_eq!(changed, 1);
        manager_id.to_owned()
    } else {
        manager
    };
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
        .set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise))
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
        since_micros: since,
    }
}

#[tokio::test]
async fn manager_reads_report_fulltext_external_masked_and_never_gains_old_readers() {
    manager_reads_body().await;
}

async fn manager_reads_body() {
    let audit_started = Utc::now();
    let f = fixture_with_manager_id(Some("00000912-0000-4000-8000-000000000001")).await;
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
    assert!(!external["messages"].to_string().contains("0912"));
    for secret in [
        "0912-345-678",
        "0912345678",
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
    // Exact metadata rejects content without treating lawful UUID/hash/time
    // digits as PII. The historical CI audit was not retained; this fixture
    // establishes the short-substring false positive, not its historical cause.
    let digests = ["mine", "report", f.external.as_str()]
        .map(|id| crate::team_history::audit::session_digest(f.home.path(), id).unwrap());
    let expected = expected_journey_audit(
        &f.manager.user_id,
        &f.admin.user_id,
        &f.employee,
        f.since_micros,
        f.db.acl_generation().unwrap(),
        &digests,
    );
    let rows: Vec<Value> = audit
        .lines()
        .map(|line| serde_json::from_str(line).expect("audit JSON syntax at /"))
        .collect();
    assert_eq!(rows.len(), expected.len(), "audit row count at /");
    let audit_finished = Utc::now();
    for (ordinal, (row, expected)) in rows.iter().zip(&expected).enumerate() {
        validate_journey_audit_row(row, expected, audit_started, audit_finished)
            .unwrap_or_else(|rule| panic!("audit row {} contract: {rule}", ordinal + 1));
    }
    assert!(
        matches!(
            duduclaw_security::audit_chain::verify_chain(
                &f.home.path().join("security_audit.jsonl")
            )
            .expect("audit chain read at /"),
            duduclaw_security::audit_chain::ChainVerifyResult::Intact { lines_checked: 8 }
        ),
        "audit chain integrity at /_prev_hash"
    );
}

// Each expected row is a closed event-specific schema, including all nested
// values. None of these expected values come from the RPC/audit under test.
fn expected_journey_audit(
    manager: &str,
    admin: &str,
    employee: &str,
    since_micros: i64,
    generation: u64,
    digests: &[String; 3],
) -> Vec<Value> {
    let since = DateTime::from_timestamp_micros(since_micros).unwrap();
    let event = |kind: &str, agent: &str, severity: &str, details: Value| json!({"event_type":kind,"agent_id":agent,"severity":severity,"details":details});
    let mut rows = vec![event(
        "team_history_setting_changed",
        "",
        "warning",
        json!({"actor":"edition_watcher","old":null,"new":true,"since_micros":since_micros}),
    )];
    for (index, class, channel, report, depth, masked) in [
        (0, "own", Value::Null, Value::Null, Value::Null, json!({})),
        (
            1,
            "report_webchat",
            Value::Null,
            json!(employee),
            json!(1),
            json!({}),
        ),
        // The assistant attachment line is withheld in full. The Unicode
        // email regex covers the entire user line, including its phone. The
        // higher-priority phone splits that email into two mask parts; recount
        // counts rendered parts, not distinct PII values. Categories stay closed.
        (
            2,
            "external",
            json!("line"),
            Value::Null,
            Value::Null,
            json!({"PHONE":1,"EMAIL":2}),
        ),
    ] {
        rows.push(event(
            "team_history_viewed",
            "sales",
            "info",
            json!({
                "viewer":manager,"viewer_role":"manager","agent":"sales",
                "session":digests[index],"class":class,"channel":channel,
                "report":report,"via_depth":depth,"messages":2,"masked":masked,
                "ner":false,"since":since,"generation":generation,
            }),
        ));
    }
    rows.push(event(
        "team_history_listed",
        "sales",
        "info",
        json!({
            "viewer":manager,"viewer_role":"manager","agent":"sales",
            "class":null,"report":null,"sessions":1,"since":since,"generation":generation,
        }),
    ));
    // Hidden/missing/inaccessible sessions are indistinguishable. Identical
    // denials for the same viewer/agent/reason are deduplicated for 60 seconds.
    for (viewer, agent) in [(manager, "sales"), (manager, ""), (admin, "")] {
        rows.push(event(
            "team_history_denied",
            agent,
            "warning",
            json!({"viewer":viewer,"agent":agent,"reason":"hidden"}),
        ));
    }
    rows
}

fn validate_journey_audit_row(
    row: &Value,
    expected: &Value,
    started: DateTime<Utc>,
    finished: DateTime<Utc>,
) -> Result<(), &'static str> {
    let envelope = row.as_object().ok_or("object at /")?;
    let keys = [
        "timestamp",
        "event_type",
        "agent_id",
        "severity",
        "details",
        "_prev_hash",
    ];
    if envelope.len() != keys.len() || keys.iter().any(|key| !envelope.contains_key(*key)) {
        return Err("closed envelope at /");
    }
    let timestamp = row["timestamp"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .ok_or("RFC3339 at /timestamp")?;
    if timestamp < started || timestamp > finished {
        return Err("journey time window at /timestamp");
    }
    let previous = row["_prev_hash"]
        .as_str()
        .ok_or("hex string at /_prev_hash")?;
    if previous.len() != 64
        || !previous
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("64 lowercase hex at /_prev_hash");
    }
    if row["event_type"] != expected["event_type"] {
        return Err("exact event at /event_type");
    }
    if row["agent_id"] != expected["agent_id"] {
        return Err("exact fixture agent at /agent_id");
    }
    if row["severity"] != expected["severity"] {
        return Err("exact event severity at /severity");
    }
    // Equality to trusted fixtures checks the complete key set, JSON types,
    // UUIDs, keyed session digest, ACL generation, window and mask counts.
    // It rejects arbitrary strings even when their type/shape looks valid.
    if row["details"] != expected["details"] {
        // Diagnostic-only refinement: the enclosing exact equality remains
        // the acceptance rule. Pointers are trusted static schema names; never
        // print values, unknown keys, or an untrusted event string.
        for pointer in [
            "/details/actor",
            "/details/old",
            "/details/new",
            "/details/since_micros",
            "/details/viewer",
            "/details/viewer_role",
            "/details/agent",
            "/details/session",
            "/details/class",
            "/details/channel",
            "/details/report",
            "/details/via_depth",
            "/details/messages",
            "/details/masked/PHONE",
            "/details/masked/EMAIL",
            "/details/masked",
            "/details/ner",
            "/details/since",
            "/details/generation",
            "/details/sessions",
            "/details/reason",
        ] {
            if row.pointer(pointer) != expected.pointer(pointer) {
                return Err(pointer);
            }
        }
        return Err("closed exact fixture metadata at /details");
    }
    Ok(())
}

#[test]
fn audit_metadata_accepts_lawful_digits_and_rejects_content_mutations() {
    let instant = DateTime::parse_from_rfc3339("2026-09-12T00:00:00.0912Z")
        .unwrap()
        .with_timezone(&Utc);
    let expected = expected_journey_audit(
        "00000912-0000-4000-8000-000000000001",
        "00000912-0000-4000-8000-000000000002",
        "00000912-0000-4000-8000-000000000003",
        instant.timestamp_micros(),
        10912,
        &[
            "0912abcdef012345".into(),
            "abcdef0912012345".into(),
            "abcdef0123450912".into(),
        ],
    );
    let make_row = |expected: &Value| {
        let mut row = expected.clone();
        row["timestamp"] = json!(instant);
        // Structural positive only: integration separately verifies the real
        // production chain, so no claim this synthetic hash has a valid chain.
        row["_prev_hash"] = json!(format!("0912{}", "0".repeat(60)));
        row
    };
    for expected in &expected {
        let row = make_row(expected);
        assert!(validate_journey_audit_row(&row, expected, instant, instant).is_ok());
        for secret in [
            "0912-345-678",
            "0912345678",
            "a@example.com",
            "電話0912-345-678和a@example.com",
            "收到0912345678，📎DELIVER:/private/person.txt",
            "OLD-CONTENT-SECRET",
            "OLD-TITLE-SECRET",
            "SUMMARY-SECRET",
            "📎DELIVER:/private/person.txt",
            "/private/person.txt",
            "person.txt",
            "line:C0123456789abcdef0123456789abcdef",
        ] {
            for key in ["content", "phone", "email", "attachment_path"] {
                let mut mutated = row.clone();
                mutated[key] = json!(secret);
                assert!(
                    validate_journey_audit_row(&mutated, expected, instant, instant).is_err(),
                    "reject unknown envelope field at /"
                );
                let mut mutated = row.clone();
                mutated["details"][key] = json!(secret);
                assert!(
                    validate_journey_audit_row(&mutated, expected, instant, instant).is_err(),
                    "reject unknown details field at /details"
                );
            }
            // Overwrite every allowed field, including valid-typed metadata
            // and numeric/bool fields, rather than only testing extra keys.
            for key in [
                "timestamp",
                "event_type",
                "agent_id",
                "severity",
                "_prev_hash",
            ] {
                let mut mutated = row.clone();
                mutated[key] = json!(secret);
                assert!(
                    validate_journey_audit_row(&mutated, expected, instant, instant).is_err(),
                    "reject envelope field overwrite at /"
                );
            }
            for key in expected["details"].as_object().unwrap().keys() {
                let mut mutated = row.clone();
                mutated["details"][key] = json!(secret);
                assert!(
                    validate_journey_audit_row(&mutated, expected, instant, instant).is_err(),
                    "reject metadata overwrite at /details"
                );
            }
        }
        // Shape-valid substitutions still fail the fixture's exact contract.
        for (pointer, replacement) in [
            ("/details/generation", json!(10913)),
            ("/details/session", json!("0000000000000000")),
            ("/details/masked", json!({"PHONE":2,"EMAIL":2})),
            ("/details/masked", json!({"PHONE":1,"EMAIL":1})),
            (
                "/details/masked",
                json!({"PHONE":1,"EMAIL":2,"a@example.com":1}),
            ),
            ("/details/masked", json!({"PHONE":"0912345678","EMAIL":2})),
        ] {
            if row.pointer(pointer).is_some() {
                let mut mutated = row.clone();
                *mutated.pointer_mut(pointer).unwrap() = replacement;
                assert!(
                    validate_journey_audit_row(&mutated, expected, instant, instant).is_err(),
                    "reject shape-valid metadata mutation at /details"
                );
            }
        }
        for key in row.as_object().unwrap().keys() {
            let mut mutated = row.clone();
            mutated.as_object_mut().unwrap().remove(key);
            assert!(
                validate_journey_audit_row(&mutated, expected, instant, instant).is_err(),
                "reject missing envelope field at /"
            );
        }
    }
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
    let core = type_bytes(|| f.handler.dispatch_core("approvals.decide", json!({}), &f.manager, crate::power_local::RpcConnInfo::internal()));
    let knowledge = type_bytes(|| f.handler.dispatch_knowledge("approvals.decide", json!({}), &f.manager, crate::power_local::RpcConnInfo::internal()));
    let system = type_bytes(|| f.handler.dispatch_system("approvals.decide", json!({}), &f.manager, crate::power_local::RpcConnInfo::internal()));
    let ops = type_bytes(|| f.handler.dispatch_ops("approvals.decide", json!({}), &f.manager, crate::power_local::RpcConnInfo::internal()));
    let get = constructed_bytes(|| {
        f.handler
            .handle_team_history_get(json!({"session_id":"report"}), &f.manager)
    });
    let list = constructed_bytes(|| {
        f.handler
            .handle_team_history_list(json!({"agent_id":"sales"}), &f.manager)
    });
    let journey = type_bytes(manager_reads_body);
    let sizes = json!({"handle":handle,"handle_conn":conn,"dispatch":dispatch,"dispatch_core":core,"dispatch_knowledge":knowledge,"dispatch_system":system,"dispatch_org":org,"dispatch_ops":ops,"team_get":get,"team_list":list,"long_test":journey});
    eprintln!("RPC_FUTURE_SIZES {sizes}");
    assert!(
        handle <= 4096,
        "RPC entry future grew to {handle} bytes; keep dispatcher boxed"
    );
    assert!(
        conn <= 4096,
        "transport RPC future grew to {conn} bytes; keep dispatcher boxed"
    );
    // Before boxing the dispatch fallthroughs this was 21,264 bytes. The
    // public handle stayed small then, so its guard alone missed the growth.
    assert!(
        dispatch <= 8 * 1024,
        "shared dispatcher future grew to {dispatch} bytes; keep the dispatch layers boxed"
    );
    // Bound each inner layer too: boxing only the public entry hides growth
    // in its children. Measured after boxing: 12,280 / 20,744 / 17,960 /
    // 11,728 / 5,256 bytes. In particular, unboxing core's fallthrough pulls
    // the knowledge layer back above core's 16 KiB limit.
    for (name, size, limit) in [
        ("core", core, 16 * 1024),
        ("knowledge", knowledge, 24 * 1024),
        ("system", system, 20 * 1024),
        ("org", org, 16 * 1024),
        ("ops", ops, 8 * 1024),
    ] {
        assert!(size <= limit, "{name} dispatcher future grew to {size} bytes; box large handler boundaries");
    }
}

#[test]
fn approval_dispatch_fits_one_mib_stack() {
    std::thread::Builder::new()
        .name("approval-dispatch-1mib".into())
        .stack_size(1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let f = fixture().await;
                    // Exercise every RPC fallthrough used by workflow decisions
                    // with an explicit bound, independent of RUST_MIN_STACK.
                    let response = f.handler.handle(
                        "approvals.decide",
                        json!({"id":"missing-workflow-approval","approve":false}),
                        &f.admin,
                    ).await;
                    assert_eq!(denied(response), json!("approval not found"));
                });
        })
        .unwrap()
        .join()
        .unwrap();
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

#[tokio::test]
async fn restricted_team_metadata_uses_scoped_opaque_ids_and_cursors_even_for_raw_requests() {
    let f=fixture().await;
    let rc=f.handler.reply_ctx.read().await.clone().unwrap();
    let phone_id="whatsapp:+886912345678";
    {
        let c=rc.session_manager.acquire().await;
        c.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id,title,summary) SELECT ?1,agent_id,last_active,created_at,origin_kind,owner_user_id,title,summary FROM sessions WHERE id=?2",params![phone_id,f.external]).unwrap();
        c.execute("INSERT INTO session_messages(session_id,role,content,timestamp) SELECT ?1,role,content,timestamp FROM session_messages WHERE session_id=?2",params![phone_id,f.external]).unwrap();
    }
    let list=payload(f.handler.handle("users.team_history.list",json!({"agent_id":"sales","class":"external","limit":1}),&f.manager).await);
    let handle=list["sessions"][0]["session_id"].as_str().unwrap();
    assert!(crate::history_privacy::handles::is_handle(handle));
    assert!(!list.to_string().contains("886912345678"));
    let cursor=list["next_before"].as_str().unwrap();assert!(cursor.starts_with("view:v1:c:"));
    let get=payload(f.handler.handle("users.team_history.get",json!({"session_id":handle}),&f.manager).await);
    assert_eq!(get["session_id"],handle);assert!(!get.to_string().contains("886912345678"));
    let raw=payload(f.handler.handle("users.team_history.get",json!({"session_id":phone_id}),&f.manager).await);
    assert_eq!(raw["session_id"],handle);assert_eq!(raw["messages"],get["messages"]);
    let next=payload(f.handler.handle("users.team_history.list",json!({"agent_id":"sales","class":"external","limit":1,"before":cursor}),&f.manager).await);
    assert_eq!(next["sessions"].as_array().unwrap().len(),1);assert_ne!(next["sessions"][0]["session_id"],handle);
    assert!(matches!(f.handler.handle("users.team_history.get",json!({"session_id":handle}),&f.admin).await,WsFrame::Response{ok:false,..}));
    let admin=payload(f.handler.handle("chat.sessions.history",json!({"session_id":phone_id}),&f.admin).await);
    assert!(admin.to_string().contains("0912-345-678"));
}

#[tokio::test]
async fn directly_bound_operator_reads_own_full_external_masked_and_hides_other_webchat() {
    let f = fixture().await;
    let operator = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let own = payload(f.handler.handle("chat.sessions.history", json!({"session_id":"report"}), &operator).await);
    assert_ne!(own["read_only"], true);
    assert!(own.to_string().contains("0912-345-678"));
    let external = payload(f.handler.handle("chat.sessions.history", json!({"session_id":f.external}), &operator).await);
    assert_eq!(external["read_only"], true);
    assert_eq!(external["privacy_mode"], "masked");
    assert_eq!(external["channel"], "line");
    assert!(!external.to_string().contains("0912-345-678"));
    assert!(!external.to_string().contains("a@example.com"));
    assert!(!external.to_string().contains("/private"));
    assert!(crate::history_privacy::handles::is_handle(external["session_id"].as_str().unwrap()));
    assert_eq!(denied(f.handler.handle("chat.sessions.history", json!({"session_id":"hidden"}), &operator).await), denied(f.handler.handle("chat.sessions.history", json!({"session_id":"missing"}), &operator).await));
    let list = payload(f.handler.handle("chat.sessions.list",json!({"agent_id":"sales"}), &operator).await);
    assert_eq!(list["sessions"].as_array().unwrap().len(),2);
    assert!(!list.to_string().contains("SUMMARY-SECRET"));
    assert!(!list.to_string().contains(&f.external));
    assert_eq!(payload(f.handler.handle("chat.sessions.history",json!({"session_id":external["session_id"]}), &operator).await)["read_only"],true);
    f.db.bind_agent(&f.employee,"sales",AccessLevel::Viewer).unwrap();
    assert_eq!(payload(f.handler.handle("chat.sessions.history", json!({"session_id":f.external}), &operator).await)["read_only"],true);
    f.db.bind_agent(&f.employee,"sales",AccessLevel::Owner).unwrap();
    assert!(payload(f.handler.handle("chat.sessions.history", json!({"session_id":"hidden"}), &operator).await).to_string().contains("0912-345-678"));
    f.db.bind_agent(&f.employee,"sales",AccessLevel::Operator).unwrap();
    std::fs::write(f.home.path().join("config.toml"),"[people]\nbound_history_privacy=false").unwrap();
    assert!(payload(f.handler.handle("chat.sessions.history", json!({"session_id":"hidden"}), &operator).await).to_string().contains("0912-345-678"));
    std::fs::write(f.home.path().join("config.toml"),"[people]\nbound_history_privacy=true").unwrap();
    std::fs::create_dir_all(f.home.path().join("models/privacy-filter")).unwrap();
    denied(f.handler.handle("chat.sessions.history", json!({"session_id":f.external}), &operator).await);
    assert!(payload(f.handler.handle("chat.sessions.history", json!({"session_id":"report"}), &operator).await).to_string().contains("0912-345-678"));
}

#[tokio::test]
async fn bound_search_matches_only_masked_external_text_and_run_routes_hide_identifiers() {
    let f=fixture().await;
    let user=f.db.live_user_context(&f.employee).unwrap().unwrap();
    let hits=payload(f.handler.handle("search.query",json!({"agent_id":"sales","sources":["conversations"],"q":"a@example.com"}),&user).await);
    for hit in hits["hits"].as_array().unwrap() { assert_eq!(hit["jump"]["session_id"],"report"); }
    let all=payload(f.handler.handle("search.query",json!({"agent_id":"sales","sources":["conversations"],"q":"電話"}),&user).await);
    assert!(!all.to_string().contains(&f.external));
    assert!(!all.to_string().contains("hidden"));
    let runs=payload(f.handler.handle("runs.list",json!({"agent_id":"sales"}),&user).await);
    assert!(!runs.to_string().contains(&f.external));
    assert!(!runs.to_string().contains("hidden"));
    for r in runs["runs"].as_array().unwrap() {
        let result=payload(f.handler.handle("runs.get",json!({"run_id":r["id"]}),&user).await);
        if r["channel"]=="line" {
            assert!(!result["events"].to_string().contains("0912"));
            assert!(!result.to_string().contains("a@example.com"));
            assert!(!result.to_string().contains("/private"));
        }
    }
}

#[tokio::test]
async fn bound_full_read_fence_refuses_a_downgrade_before_releasing_content() {
    let f=fixture().await;
    f.db.bind_agent(&f.employee,"sales",AccessLevel::Owner).unwrap();
    let old=f.db.live_user_context(&f.employee).unwrap().unwrap();
    assert!(f.handler.bound_history_recheck(&old,"sales",crate::history_privacy::Mode::Full).is_ok());
    f.db.bind_agent(&f.employee,"sales",AccessLevel::Operator).unwrap();
    assert!(f.handler.bound_history_recheck(&old,"sales",crate::history_privacy::Mode::Full).is_err());
    assert!(f.handler.bound_history_recheck(&old,"sales",crate::history_privacy::Mode::Filtered).is_ok());
}

#[tokio::test]
async fn bound_mask_failures_never_return_private_configuration_lines() {
    let f=fixture().await;
    let reader=f.db.live_user_context(&f.employee).unwrap().unwrap();
    // Strict mode remains enabled when config cannot parse. The masker may
    // diagnose that line internally, but an employee must not receive it.
    std::fs::write(f.home.path().join("config.toml"),"private_api_key = 'SYNTHETIC-CONFIG-SECRET").unwrap();
    let error=denied(f.handler.handle("chat.sessions.history",json!({"session_id":f.external}),&reader).await);
    assert_eq!(error,json!("masking_failed"));
    assert!(!error.to_string().contains("SYNTHETIC-CONFIG-SECRET"));
    let runs=payload(f.handler.handle("runs.list",json!({"agent_id":"sales"}),&reader).await);
    let external=runs["runs"].as_array().unwrap().iter().find(|r|r["channel"]=="line").unwrap();
    let error=denied(f.handler.handle("runs.get",json!({"run_id":external["id"]}),&reader).await);
    assert_eq!(error,json!("masking_failed"));
    let error=denied(f.handler.handle("search.query",json!({"agent_id":"sales","sources":["conversations"],"q":"電話"}),&reader).await);
    assert_eq!(error,json!("masking_failed"));
}

#[tokio::test]
async fn webchat_cancel_marker_never_appears_in_history_rpc() {
    let f = fixture().await;
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    let sessions = &rc.session_manager;
    sessions.compress("report", "Real compression summary survives cancellation").await.unwrap();
    sessions.append_message("report", "user", "Continue the original task", 5).await.unwrap();
    let before = payload(f.handler.handle(
        "chat.sessions.history", json!({"session_id":"report"}), &f.admin,
    ).await);
    sessions.record_webchat_cancel("report", "sales", "cancel-turn-marker", "cancel-msg-marker")
        .await.unwrap();
    let after = payload(f.handler.handle(
        "chat.sessions.history", json!({"session_id":"report"}), &f.admin,
    ).await);
    assert_eq!(after["messages"], before["messages"]);
    assert!(!after.to_string().contains("cancel-turn-marker"));
    assert!(!after.to_string().contains("cancel-msg-marker"));
    assert_eq!(sessions.compression_summary("report").await.unwrap(),
        "Real compression summary survives cancellation");
    let owner = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let own_history = payload(f.handler.handle(
        "chat.sessions.history", json!({"session_id":"report"}), &owner,
    ).await);
    assert_eq!(own_history["messages"], after["messages"]);
    assert_eq!(sessions.acquire().await.query_row(
        "SELECT COUNT(*) FROM webchat_turn_cancellations WHERE session_id='report'",
        [], |row| row.get::<_, i64>(0),
    ).unwrap(), 1, "the control record must exist without becoming content");
}

#[tokio::test]
async fn webchat_queued_cancel_creates_no_session_and_checks_existing_owner() {
    let f = fixture().await;
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    let sessions = &rc.session_manager;
    sessions.record_webchat_cancel("never-started", "sales", "queued-turn", "queued-msg")
        .await.unwrap();
    assert!(sessions.session_agent("never-started").await.unwrap().is_none());
    assert!(sessions.get_messages("never-started").await.unwrap().is_empty());
    assert!(sessions.record_webchat_cancel("report", "other-agent", "foreign-turn", "foreign-msg")
        .await.is_err());
    assert_eq!(sessions.acquire().await.query_row(
        "SELECT COUNT(*) FROM webchat_turn_cancellations", [], |row| row.get::<_, i64>(0),
    ).unwrap(), 0);
}

#[cfg(unix)]
async fn chat_attachment_fixture_row(f: &Fx, session: &str) -> (i64, String) {
    let dir = f.home.path().join("agents/sales/attachments");
    std::fs::create_dir_all(&dir).unwrap();
    let path = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.employee.clone(), async {
        crate::media::save_webchat_attachment_in_base(
            dir.parent().unwrap(), b"%PDF-1.7\nfixture attachment\n", "報告 (1).pdf",
        ).await.unwrap()
    }).await;
    let text = format!("Please read\n[📎 報告 (1).pdf (file)]({})", path.display());
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    let c = rc.session_manager.acquire().await;
    c.execute(
        "INSERT INTO session_messages(session_id,role,content,timestamp) VALUES (?1,'user',?2,?3)",
        params![session, text, chrono::Utc::now().to_rfc3339()],
    ).unwrap();
    (c.last_insert_rowid(), path.to_string_lossy().into_owned())
}

#[cfg(unix)]
async fn chat_attachment_id(f: &Fx, ctx: &UserContext, session: &str, row_id: i64) -> String {
    let p = payload(f.handler.handle_chat_sessions_history(json!({"session_id":session}), ctx).await);
    let row = p["messages"].as_array().unwrap().iter().find(|row| row["id"] == row_id).unwrap();
    assert!(!row.to_string().contains(f.home.path().to_str().unwrap()), "host paths must not reach the browser");
    assert_eq!(row["attachments"][0]["name"], "報告 (1).pdf");
    assert_eq!(row["attachments"][0]["mime"], "application/pdf");
    assert_eq!(row["attachments"][0]["size"], b"%PDF-1.7\nfixture attachment\n".len());
    row["attachments"][0]["id"].as_str().unwrap().to_string()
}

#[cfg(unix)]
#[tokio::test]
async fn chat_attachment_full_history_admin_and_own_operator_can_read_legacy_file() {
    use std::io::Read;
    let f = fixture().await;
    let (row, path) = chat_attachment_fixture_row(&f, "report").await;
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    for ctx in [&f.admin, &employee] {
        let id = chat_attachment_id(&f, ctx, "report", row).await;
        assert_ne!(id, path);
        assert!(!id.contains("report.pdf"));
        let (mut file, _) = f.handler.open_history_attachment(ctx, &id).await.unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"%PDF-1.7\nfixture attachment\n");
        assert!(f.handler.open_history_attachment(ctx, &path).await.is_err());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn chat_attachment_handle_is_user_bound_and_revocation_is_live() {
    let f = fixture().await;
    let (row, _) = chat_attachment_fixture_row(&f, "report").await;
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let id = chat_attachment_id(&f, &employee, "report", row).await;
    assert!(f.handler.open_history_attachment(&f.manager, &id).await.is_err());
    assert!(f.handler.open_history_attachment(&f.admin, &id).await.is_err());
    f.db.unbind_agent(&employee.user_id, "sales").unwrap();
    assert!(f.handler.open_history_attachment(&employee, &id).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn chat_attachment_hidden_or_undone_message_invalidates_existing_handle() {
    let f = fixture().await;
    let (row, _) = chat_attachment_fixture_row(&f, "report").await;
    let id = chat_attachment_id(&f, &f.admin, "report", row).await;
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    rc.session_manager.acquire().await.execute("UPDATE session_messages SET hidden=1 WHERE id=?1", [row]).unwrap();
    assert!(f.handler.open_history_attachment(&f.admin, &id).await.is_err());
    rc.session_manager.acquire().await.execute("UPDATE session_messages SET hidden=0,undone_at='2026-11-05T00:00:00Z' WHERE id=?1", [row]).unwrap();
    assert!(f.handler.open_history_attachment(&f.admin, &id).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn chat_attachment_conversation_ownership_change_invalidates_existing_handle() {
    let f = fixture().await;
    let (row, _) = chat_attachment_fixture_row(&f, "report").await;
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let id = chat_attachment_id(&f, &employee, "report", row).await;
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    rc.session_manager.acquire().await.execute("UPDATE sessions SET owner_user_id='stranger' WHERE id='report'", []).unwrap();
    assert!(f.handler.open_history_attachment(&employee, &id).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn chat_attachment_external_masked_history_does_not_expose_files() {
    let f = fixture().await;
    let (row, path) = chat_attachment_fixture_row(&f, &f.external).await;
    let admin_id = chat_attachment_id(&f, &f.admin, &f.external, row).await;
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let p = payload(f.handler.handle_chat_sessions_history(json!({"session_id":f.external}), &employee).await);
    assert_eq!(p["privacy_mode"], "masked");
    assert!(!p.to_string().contains(&path));
    assert!(!p.to_string().contains("報告 (1).pdf"));
    for message in p["messages"].as_array().unwrap() {
        assert!(message.get("attachments").is_none_or(|a| a.as_array().is_some_and(Vec::is_empty)));
    }
    assert!(f.handler.open_history_attachment(&employee, &admin_id).await.is_err());
    assert!(f.handler.open_history_attachment(&employee, &path).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn chat_attachment_unattributed_legacy_file_is_admin_only_not_a_typed_path_grant() {
    let f = fixture().await;
    let (row, path) = chat_attachment_fixture_row(&f, "report").await;
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let owner_id = chat_attachment_id(&f, &employee, "report", row).await;
    std::fs::remove_file(f.home.path().join("artifacts.jsonl")).unwrap();
    assert!(f.handler.open_history_attachment(&employee, &owner_id).await.is_ok(), "audit retention does not revoke a durable owner binding");
    // A genuinely legacy transcript has a path but no durable content/owner binding.
    let authority = rusqlite::Connection::open(f.home.path().join("artifact_provenance.db")).unwrap();
    let name = std::path::Path::new(&path).file_name().unwrap().to_str().unwrap();
    assert_eq!(authority.execute("DELETE FROM artifact_provenance WHERE agent='sales' AND name=?1", [name]).unwrap(), 1);
    assert!(f.handler.open_history_attachment(&employee, &owner_id).await.is_err());
    let id = chat_attachment_id(&f, &f.admin, "report", row).await;
    assert!(f.handler.open_history_attachment(&f.admin, &id).await.is_ok());
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let p = payload(f.handler.handle_chat_sessions_history(json!({"session_id":"report"}), &employee).await);
    let row = p["messages"].as_array().unwrap().iter().find(|r| r["id"] == row).unwrap();
    assert!(row.get("attachments").is_none_or(|a| a.as_array().is_some_and(Vec::is_empty)));
    assert!(!row.to_string().contains(f.home.path().to_str().unwrap()));
    assert!(f.handler.open_history_attachment(&employee, &id).await.is_err());
}

// ── RM-173: retention settings, archived history and restore ──────────────

async fn rm173_archive(f: &Fx, id: &str) {
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    rc.session_manager
        .acquire()
        .await
        .execute(
            "UPDATE sessions SET archived_at=?2 WHERE id=?1",
            params![id, Utc::now().to_rfc3339()],
        )
        .unwrap();
}

async fn rm173_is_archived(f: &Fx, id: &str) -> bool {
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    rc.session_manager.archived_at(id).await.unwrap().is_some()
}

fn rm173_audit(f: &Fx) -> String {
    std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap_or_default()
}

#[tokio::test]
async fn rm173_team_history_includes_archived_sessions_flagged() {
    let f = fixture().await;
    rm173_archive(&f, "report").await;
    let list = payload(f.handler.handle("users.team_history.list", json!({"agent_id":"sales"}), &f.manager).await);
    let sessions = list["sessions"].as_array().unwrap();
    let report = sessions.iter().find(|s| s["class"] == "report_webchat").unwrap();
    assert_eq!(report["archived"], true);
    let external = sessions.iter().find(|s| s["class"] == "external").unwrap();
    assert_eq!(external["archived"], false);
    let get = payload(f.handler.handle("users.team_history.get", json!({"session_id":report["session_id"]}), &f.manager).await);
    assert_eq!(get["archived"], true);
    assert!(get.to_string().contains("0912-345-678"));
    // The activation window and origin scope still apply to archived rows.
    assert!(!get.to_string().contains("OLD-CONTENT"));
    rm173_archive(&f, "hidden").await;
    denied(f.handler.handle("users.team_history.get", json!({"session_id":"hidden"}), &f.manager).await);
}

#[tokio::test]
async fn rm173_team_history_restore_authorization() {
    let f = fixture().await;
    for id in ["report", "hidden", f.external.as_str()] {
        rm173_archive(&f, id).await;
    }
    // Staff (not a manager) is refused like an unreadable session.
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    denied(f.handler.handle("users.team_history.restore", json!({"session_id":"report"}), &employee).await);
    assert!(rm173_is_archived(&f, "report").await);
    // A manager without team-history access to the session is refused.
    assert_eq!(denied(f.handler.handle("users.team_history.restore", json!({"session_id":"hidden"}), &f.manager).await)["code"], "not_found");
    assert!(rm173_is_archived(&f, "hidden").await);
    // A manager who can read it through team history may restore it.
    let handle = payload(f.handler.handle("users.team_history.list", json!({"agent_id":"sales","class":"report_webchat"}), &f.manager).await)["sessions"][0]["session_id"].clone();
    let restored = payload(f.handler.handle("users.team_history.restore", json!({"session_id":handle}), &f.manager).await);
    assert_eq!(restored["restored"], true);
    assert!(!rm173_is_archived(&f, "report").await);
    let again = payload(f.handler.handle("users.team_history.restore", json!({"session_id":handle}), &f.manager).await);
    assert_eq!(again["restored"], false, "idempotent");
    // Admin may restore any existing session.
    assert_eq!(payload(f.handler.handle("users.team_history.restore", json!({"session_id":f.external}), &f.admin).await)["restored"], true);
    assert!(!rm173_is_archived(&f, &f.external).await);
    assert_eq!(denied(f.handler.handle("users.team_history.restore", json!({"session_id":"missing"}), &f.admin).await)["code"], "not_found");
    let audit = rm173_audit(&f);
    assert_eq!(audit.matches("team_history_session_restored").count(), 2);
    assert!(!audit.contains(&f.external), "audit carries a digest, not the raw id");
}

#[tokio::test]
async fn rm173_org_settings_read_and_write_retention() {
    let f = fixture().await;
    let initial = payload(f.handler.handle("users.org_settings.get", json!({}), &f.admin).await);
    assert_eq!(initial["archive_after_days"], 30);
    assert_eq!(initial["purge_after_days"], 0);
    denied(f.handler.handle("users.org_settings.update", json!({"purge_after_days":30}), &f.manager).await);
    for bad in [json!({"purge_after_days":3651}), json!({"archive_after_days":-1}), json!({"archive_after_days":"7"}), json!({})] {
        assert_eq!(denied(f.handler.handle("users.org_settings.update", bad.clone(), &f.admin).await)["code"], "invalid_params", "{bad}");
    }
    let updated = payload(f.handler.handle("users.org_settings.update", json!({"archive_after_days":7,"purge_after_days":90}), &f.admin).await);
    assert_eq!((updated["archive_after_days"].clone(), updated["purge_after_days"].clone()), (json!(7), json!(90)));
    assert_eq!(updated["manager_team_history"], true, "other settings untouched");
    let retention = crate::session_retention::load(f.home.path());
    assert_eq!((retention.archive_after_days, retention.purge_after_days), (7, 90));
    let audit = rm173_audit(&f);
    assert!(audit.contains("sessions.purge_after_days") && audit.contains("config_protected_key_changed"));
}

#[tokio::test]
async fn rm173_backdating_team_history_since_requires_step_up_and_is_audited() {
    let f = fixture().await;
    let since = DateTime::from_timestamp_micros(f.since_micros).unwrap();
    let earlier = (since - chrono::Duration::days(3)).to_rfc3339();
    async fn current(f: &Fx) -> Value {
        payload(f.handler.handle("users.org_settings.get", json!({}), &f.admin).await)["team_history_since"].clone()
    }
    let before = current(&f).await;
    // Earlier without / with a wrong password: refused, nothing changes.
    assert_eq!(denied(f.handler.handle("users.org_settings.update", json!({"team_history_since":earlier}), &f.admin).await)["code"], "step_up_required");
    assert_eq!(denied(f.handler.handle("users.org_settings.update", json!({"team_history_since":earlier,"password":"wrong-password"}), &f.admin).await)["code"], "step_up_failed");
    assert_eq!(current(&f).await, before);
    assert!(rm173_audit(&f).contains("team_history_since_step_up_failed"));
    assert!(!rm173_audit(&f).contains("team_history_since_changed"));
    // Not with a manager, never in the future or before the floor.
    denied(f.handler.handle("users.org_settings.update", json!({"team_history_since":earlier,"password":"isolated-password"}), &f.manager).await);
    for bad in ["2019-12-31", "2999-01-01T00:00:00Z", "yesterday"] {
        assert_eq!(denied(f.handler.handle("users.org_settings.update", json!({"team_history_since":bad}), &f.admin).await)["code"], "invalid_params", "{bad}");
    }
    // With the admin's own current password: moved earlier, audited old/new.
    let moved = payload(f.handler.handle("users.org_settings.update", json!({"team_history_since":earlier,"password":"isolated-password"}), &f.admin).await);
    let moved_since = DateTime::parse_from_rfc3339(moved["team_history_since"].as_str().unwrap()).unwrap();
    assert_eq!(moved_since, DateTime::parse_from_rfc3339(&earlier).unwrap());
    let audit = rm173_audit(&f);
    let line = audit.lines().find(|l| l.contains("team_history_since_changed")).unwrap();
    assert!(line.contains("\"direction\":\"earlier\"") && line.contains(&f.since_micros.to_string()), "{line}");
    assert!(!audit.contains("isolated-password"), "the password is never audited");
    // Managers now see messages from the new, earlier window.
    let report = payload(f.handler.handle("users.team_history.get", json!({"session_id":"report"}), &f.manager).await);
    assert!(report.to_string().contains("OLD-CONTENT-SECRET"));
    // Moving it later again needs no step-up.
    let later = since.to_rfc3339();
    payload(f.handler.handle("users.org_settings.update", json!({"team_history_since":later}), &f.admin).await);
    assert_eq!(rm173_audit(&f).matches("team_history_since_changed").count(), 2);
    // Cannot be combined with the on/off switch.
    assert_eq!(denied(f.handler.handle("users.org_settings.update", json!({"team_history_since":later,"manager_team_history":true}), &f.admin).await)["code"], "invalid_params");
}

#[tokio::test]
async fn rm173_sidebar_lists_own_archived_and_restores_only_own() {
    let f = fixture().await;
    for id in ["report", "hidden", f.external.as_str()] {
        rm173_archive(&f, id).await;
    }
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    // Default listing is unchanged: archived conversations are not in it.
    let live = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales"}), &employee).await);
    assert!(live["sessions"].as_array().unwrap().is_empty());
    // `archived: true`: same privacy filter (own WebChat + masked external), never others'.
    let archived = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales","archived":true}), &employee).await);
    let rows = archived["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r["archived"] == true));
    assert!(!archived.to_string().contains("hidden") && !archived.to_string().contains(&f.external));
    let external = rows.iter().find(|r| r["read_only"] == true).unwrap()["session_id"].clone();
    denied(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales","archived":"yes"}), &employee).await);
    // A masked read-only conversation and someone else's conversation cannot be restored.
    denied(f.handler.handle("chat.sessions.restore", json!({"session_id":external}), &employee).await);
    denied(f.handler.handle("chat.sessions.restore", json!({"session_id":"hidden"}), &employee).await);
    assert!(rm173_is_archived(&f, &f.external).await && rm173_is_archived(&f, "hidden").await);
    // Their own conversation can.
    let restored = payload(f.handler.handle("chat.sessions.restore", json!({"session_id":"report"}), &employee).await);
    assert_eq!(restored["restored"], true);
    assert!(!rm173_is_archived(&f, "report").await);
    let live = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales"}), &employee).await);
    assert_eq!(live["sessions"][0]["session_id"], "report");
    assert!(rm173_audit(&f).contains("chat_session_restored"));
    // A manager with no binding to the agent cannot restore through the sidebar.
    rm173_archive(&f, "report").await;
    denied(f.handler.handle("chat.sessions.restore", json!({"session_id":"report"}), &f.manager).await);
    assert!(rm173_is_archived(&f, "report").await);
    // Full-history readers (Admin) list and restore with the same gate.
    let admin = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales","archived":true}), &f.admin).await);
    assert_eq!(admin["sessions"].as_array().unwrap().len(), 3);
    assert!(admin["sessions"].as_array().unwrap().iter().all(|r| r["archive_reason"] == "retention"));
    assert_eq!(payload(f.handler.handle("chat.sessions.restore", json!({"session_id":"hidden"}), &f.admin).await)["restored"], true);
}

/// F1/F2: a conversation its user cleared (`/new`, channel reset) is never
/// team history — not listed, not readable, not restorable by a manager —
/// and the reused channel id starts a genuinely fresh conversation.
#[tokio::test]
async fn rm173_user_cleared_conversations_never_reach_managers() {
    let f = fixture().await;
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    let sm = &rc.session_manager;
    // Baseline: the manager sees the report's WebChat and the customer chat.
    let before = payload(f.handler.handle("users.team_history.list", json!({"agent_id":"sales"}), &f.manager).await);
    assert_eq!(before["sessions"].as_array().unwrap().len(), 2);
    // The employee clears their WebChat; the customer sends `/new` on LINE.
    let report = sm.clear_for_user("report").await.unwrap().unwrap();
    let external = sm.clear_for_user(&f.external).await.unwrap().unwrap();

    let list = payload(f.handler.handle("users.team_history.list", json!({"agent_id":"sales"}), &f.manager).await);
    assert!(list["sessions"].as_array().unwrap().is_empty(), "{list}");
    let missing = denied(f.handler.handle("users.team_history.get", json!({"session_id":"missing"}), &f.manager).await);
    for id in [report.as_str(), external.as_str(), "report", f.external.as_str()] {
        let refused = denied(f.handler.handle("users.team_history.get", json!({"session_id":id}), &f.manager).await);
        assert_eq!(refused, missing, "{id}: same shape as a missing session");
        let refused = denied(f.handler.handle("users.team_history.restore", json!({"session_id":id}), &f.manager).await);
        assert_eq!(refused["code"], "not_found", "{id}");
    }
    assert!(rm173_is_archived(&f, &report).await && rm173_is_archived(&f, &external).await);

    // The customer's next message starts a fresh conversation on the same id:
    // the manager sees only that, never the pre-clear content.
    sm.get_or_create_with_origin(
        &f.external,
        "sales",
        &crate::session_origin::SessionOrigin::of_kind(crate::session_origin::OriginKind::Channel),
    )
    .await
    .unwrap();
    sm.append_message(&f.external, "user", "NEW-TOPIC-AFTER-CLEAR", 1).await.unwrap();
    let context = sm.get_messages(&f.external).await.unwrap();
    assert_eq!(context.len(), 1, "the agent context holds no pre-clear message");
    let list = payload(f.handler.handle("users.team_history.list", json!({"agent_id":"sales"}), &f.manager).await);
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
    let fresh = payload(f.handler.handle("users.team_history.get", json!({"session_id":list["sessions"][0]["session_id"]}), &f.manager).await);
    assert_eq!(fresh["messages"].as_array().unwrap().len(), 1, "{fresh}");
    assert!(!fresh.to_string().contains("收到"), "{fresh}");

    // The employee's own sidebar shows their cleared WebChat, labelled — but
    // not the customer's clear.
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let mine = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales","archived":true}), &employee).await);
    let rows = mine["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{mine}");
    assert_eq!(rows[0]["session_id"], report.as_str());
    assert_eq!(rows[0]["archive_reason"], "user");

    // An Admin may restore it (audited with the reason).
    let restored = payload(f.handler.handle("users.team_history.restore", json!({"session_id":report}), &f.admin).await);
    assert_eq!(restored["restored"], true);
    assert!(!rm173_is_archived(&f, &report).await);
    let audit = rm173_audit(&f);
    let line = audit.lines().find(|l| l.contains("team_history_session_restored")).unwrap();
    assert!(line.contains("\"archive_reason\":\"user\"") && line.contains("\"via\":\"admin\""), "{line}");
}

/// N2: a restore never launders a user clear. Full mode gives every Full-mode
/// viewer `Class::Own`, so the sidebar restore of a user clear is limited to a
/// live Admin or the conversation's own WebChat owner, and the sticky
/// `user_cleared_at` keeps a restored clear out of team history for good.
#[tokio::test]
async fn rm173_restore_never_launders_a_user_clear() {
    let f = fixture().await;
    let rc = f.handler.reply_ctx.read().await.clone().unwrap();
    let sm = &rc.session_manager;
    let owner_id = f
        .db
        .create_user("owner@test.invalid", "店長", "isolated-password", UserRole::Employee)
        .unwrap()
        .id;
    f.db.bind_agent(&owner_id, "sales", AccessLevel::Owner).unwrap();
    let owner = f.db.live_user_context(&owner_id).unwrap().unwrap();
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    let customer = sm.clear_for_user(&f.external).await.unwrap().unwrap();
    let report = sm.clear_for_user("report").await.unwrap().unwrap();

    // The agent's Owner reads in Full mode (it lists both clears) …
    let listed = payload(f.handler.handle("chat.sessions.list", json!({"agent_id":"sales","archived":true}), &owner).await);
    assert!(listed.to_string().contains(&customer), "{listed}");
    // … but can restore neither the customer's clear nor the employee's.
    for id in [customer.as_str(), report.as_str()] {
        denied(f.handler.handle("chat.sessions.restore", json!({"session_id":id}), &owner).await);
        assert!(rm173_is_archived(&f, id).await, "{id}");
    }
    // The employee restores their own cleared WebChat.
    let mine = payload(f.handler.handle("chat.sessions.restore", json!({"session_id":report}), &employee).await);
    assert_eq!(mine["restored"], true);
    assert!(!rm173_is_archived(&f, &report).await);
    // An Admin restores the customer's (audited as a user clear).
    let admin = payload(f.handler.handle("users.team_history.restore", json!({"session_id":customer}), &f.admin).await);
    assert_eq!(admin["restored"], true);
    assert!(rm173_audit(&f).contains("\"user_cleared\":true"));

    // Restored — live again — yet never team history, and the mark stays.
    let list = payload(f.handler.handle("users.team_history.list", json!({"agent_id":"sales"}), &f.manager).await);
    assert!(list["sessions"].as_array().unwrap().is_empty(), "{list}");
    let missing = denied(f.handler.handle("users.team_history.get", json!({"session_id":"missing"}), &f.manager).await);
    for id in [customer.as_str(), report.as_str()] {
        assert!(sm.user_cleared_at(id).await.unwrap().is_some(), "{id}");
        let refused = denied(f.handler.handle("users.team_history.get", json!({"session_id":id}), &f.manager).await);
        assert_eq!(refused, missing, "{id}");
    }

    // Re-archived later (retention, no `user` reason): still the user's own.
    rm173_archive(&f, &customer).await;
    denied(f.handler.handle("chat.sessions.restore", json!({"session_id":customer}), &owner).await);
    let refused = denied(f.handler.handle("users.team_history.restore", json!({"session_id":customer}), &f.manager).await);
    assert_eq!(refused["code"], "not_found");
    assert!(rm173_is_archived(&f, &customer).await);
}

/// F3/F4: the boundary never moves for the shared-token context; it is never
/// combined with retention in one request; a failed step-up is also logged
/// in the user DB.
#[tokio::test]
async fn rm173_org_settings_refuses_partial_and_system_boundary_changes() {
    let f = fixture().await;
    let since = DateTime::from_timestamp_micros(f.since_micros).unwrap();
    // A valid (not future) boundary, so only the new refusals can trigger.
    let later = since.to_rfc3339();
    let earlier = (since - chrono::Duration::days(1)).to_rfc3339();
    let before = payload(f.handler.handle("users.org_settings.get", json!({}), &f.admin).await);
    for combined in [
        json!({"team_history_since":later,"archive_after_days":7}),
        json!({"team_history_since":later,"purge_after_days":90}),
    ] {
        let refused = denied(f.handler.handle("users.org_settings.update", combined.clone(), &f.admin).await);
        assert_eq!(refused["code"], "invalid_params", "{combined}");
        assert!(refused["message"].as_str().unwrap().contains("retention"), "{refused}");
    }
    assert_eq!(payload(f.handler.handle("users.org_settings.get", json!({}), &f.admin).await), before, "nothing written");
    let system = UserContext::admin_fallback();
    denied(f.handler.handle("users.org_settings.update", json!({"team_history_since":later}), &system).await);
    assert_eq!(payload(f.handler.handle("users.org_settings.get", json!({}), &f.admin).await), before);
    // A failed step-up reaches the user-DB audit log as well as the JSONL.
    assert_eq!(denied(f.handler.handle("users.org_settings.update", json!({"team_history_since":earlier,"password":"wrong-password"}), &f.admin).await)["code"], "step_up_failed");
    let logged: i64 = rusqlite::Connection::open(f.home.path().join("users.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM auth_audit_log WHERE action='team_history.since_step_up_failed' AND user_id=?1",
            [&f.admin.user_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(logged, 1);
    // The ordinary later move still works for a live Admin.
    payload(f.handler.handle("users.org_settings.update", json!({"team_history_since":later}), &f.admin).await);
}
