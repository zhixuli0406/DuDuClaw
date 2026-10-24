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
