//! Real HTTP coverage for live member-key conversation and artifact restrictions.
use super::*;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn request(uri: &str, token: &str) -> Request<axum::body::Body> {
    Request::builder().uri(uri).header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty()).unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn member_key_http_content_scope_and_task_bindings_are_live() {
    let directory = crate::fs_safe::private_test_home().unwrap();
    let home = directory.path().canonicalize().unwrap();
    std::fs::write(home.join("config.toml"),
        "[member_api_keys]\nenabled=true\nrate_per_minute=1000\n[people]\nbound_history_privacy=false\n").unwrap();
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let member = db.create_user("content-member@test.invalid", "Member", "isolated-password", duduclaw_auth::UserRole::Employee).unwrap();
    let admin = db.create_user("content-admin@test.invalid", "Admin", "isolated-password", duduclaw_auth::UserRole::Admin).unwrap();
    db.bind_agent(&member.id, "sales", duduclaw_auth::AccessLevel::Owner).unwrap();
    let unchecked = db.member_key_create(&member.id, "unchecked", 1, member.credential_epoch, false, &[]).unwrap();
    let checked = db.member_key_create(&member.id, "checked", 1, member.credential_epoch, true, &[]).unwrap();
    let jwt_config = Arc::new(JwtConfig::new(b"member-content-http-secret-at-least-32-bytes"));
    let browser = jwt_config.issue_access_token(&member, &[]).unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_user_db(db.clone(), jwt_config.clone()).await;
    let store = handler.workflow_store().await.unwrap();
    store.initialize_evidence().await.unwrap();
    let tasks = crate::task_store::TaskStore::open(&home).unwrap();
    tasks.insert_task(&crate::task_store::TaskRow::new("content-task".into(), "Task".into(),
        "Synthetic task".into(), "normal".into(), "sales".into(), "operator".into())).await.unwrap();
    let base = home.join("agents/sales");
    std::fs::create_dir_all(base.join("attachments")).unwrap();
    // Conversation provenance must win over both explicit bindings and saved archives.
    for (name, owner, channel, task, archived, bound) in [
        ("own.png", Some(member.id.as_str()), Some("webchat"), false, false, false),
        ("own-task.png", Some(member.id.as_str()), Some("webchat"), true, true, true),
        ("other-bound.png", Some("someone-else"), Some("webchat"), true, false, true),
        ("external-archive.png", Some(member.id.as_str()), Some("line"), true, true, false),
        ("owner-only.png", Some(member.id.as_str()), None, true, true, true),
        ("channel-only.png", None, Some("webchat"), true, true, true),
        ("task-archive.png", None, None, true, true, false),
    ] {
        let path = base.join("attachments").join(name);
        std::fs::write(&path, b"synthetic bytes").unwrap();
        let save = crate::artifacts::SaveContext {
            origin: if archived { crate::artifacts::ArtifactOrigin::Declared } else { crate::artifacts::ArtifactOrigin::Uploaded },
            task_id: task.then_some("content-task"), round: None, channel,
            source_path: archived.then_some(std::path::Path::new("/synthetic/source.png")),
        };
        if let Some(owner) = owner {
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(owner.to_owned(), async {
                crate::artifacts::record_saved_bytes(&base, &path, name, b"synthetic bytes", &save).unwrap();
            }).await;
        } else {
            crate::artifacts::record_saved_bytes(&base, &path, name, b"synthetic bytes", &save).unwrap();
        }
        if name == "owner-only.png" {
            // Simulate a legacy partial stamp: an owner without a channel is not own-webchat evidence.
            let authority = rusqlite::Connection::open(home.join("artifact_provenance.db")).unwrap();
            authority.execute("UPDATE artifact_provenance SET record=json_set(record,'$.owner_user_id',?1)
                WHERE agent='sales' AND name=?2", rusqlite::params![member.id, name]).unwrap();
        }
        if bound {
            store.bind_artifact_audience(name, "content-task", "sales", name,
                &[format!("user:{}", member.id)]).await.unwrap();
        }
    }
    for name in ["task-bound.png", "unknown.png"] {
        std::fs::write(base.join("attachments").join(name), b"synthetic bytes").unwrap();
    }
    store.bind_artifact_audience("task-bound", "content-task", "sales", "task-bound.png",
        &[format!("user:{}", member.id)]).await.unwrap();

    let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let mut handles = Vec::new();
    for (sid, kind, owner) in [
        ("own-http", "webchat_user", member.id.as_str()),
        ("other-http", "webchat_user", "someone-else"),
        ("line:C0123456789abcdef0123456789abcdef", "channel", ""),
    ] {
        // Use the same otherwise-readable file to isolate the session-class gate.
        let path = base.join("attachments/own.png");
        let message = {
            let conn = sessions.acquire().await;
            conn.execute("INSERT INTO sessions(id,agent_id,created_at,last_active,origin_kind,owner_user_id)
                VALUES(?1,'sales',?2,?2,?3,?4)", rusqlite::params![sid, chrono::Utc::now().to_rfc3339(), kind, owner]).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES(?1,'user',?2,?3)",
                rusqlite::params![sid, format!("[📎 own.png (file)]({})", path.display()), chrono::Utc::now().to_rfc3339()]).unwrap();
            conn.last_insert_rowid()
        };
        let reference = crate::chat_attachments::Reference { session: sid.into(), message,
            agent: "sales".into(), path: path.to_str().unwrap().into(), name: "own.png".into() };
        handles.push(crate::history_privacy::handles::issue_attachment(&home, &member.id,
            &serde_json::to_string(&reference).unwrap()).unwrap());
    }
    let registry = Arc::new(tokio::sync::RwLock::new(duduclaw_agent::AgentRegistry::new(home.join("agents"))));
    let (reply_tx, _) = broadcast::channel(4);
    handler.set_reply_ctx(Arc::new(crate::channel_reply::ReplyContext::new(
        registry, home.clone(), sessions, Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())), reply_tx,
    ))).await;
    let (tx, _) = broadcast::channel(4);
    let (event_tx, _) = broadcast::channel(4);
    let state = Arc::new(AppState { auth: AuthManager::new(None), handler: Arc::new(handler), tx, event_tx,
        prepared_events: Default::default(), user_db: db.clone(), jwt_config,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
        home_dir: home.clone() });
    let app = axum::Router::new()
        .route("/api/chat/attachments", axum::routing::get(handle_chat_attachment))
        .route("/api/files", axum::routing::get(handle_files_list))
        .route("/api/files/download", axum::routing::get(handle_files_download))
        .route("/api/files/preview", axum::routing::get(handle_files_preview))
        .with_state(state.clone());

    for (token, include) in [(&unchecked.secret, false), (&checked.secret, true)] {
        for (index, handle) in handles.iter().enumerate() {
            for preview in [true] {
                let response = app.clone().oneshot(request(&format!("/api/chat/attachments?id={handle}&preview={preview}"), token)).await.unwrap();
                assert_eq!(response.status(), if !include { StatusCode::FORBIDDEN }
                    else if index == 0 { StatusCode::OK } else { StatusCode::NOT_FOUND });
                if !include {
                    let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
                    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(body["code"], "member_key_conversation_scope");
                }
            }
        }
        for route in ["preview"] {
            for (name, allowed) in [("own.png", include), ("own-task.png", include),
                ("other-bound.png", false), ("external-archive.png", false), ("owner-only.png", false),
                ("channel-only.png", false), ("unknown.png", false), ("task-archive.png", true)] {
                let response = app.clone().oneshot(request(&format!("/api/files/{route}?agent=sales&name={name}"), token)).await.unwrap();
                assert_eq!(response.status(), if allowed { StatusCode::OK } else { StatusCode::FORBIDDEN }, "{route}: {name}, include={include}");
            }
        }
        let response = app.clone().oneshot(request("/api/files?agent=sales", token)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 65536).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let mut names: Vec<_> = body["files"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
        names.sort();
        let mut expected = vec!["task-archive.png", "task-bound.png"];
        if include { expected.extend(["own-task.png", "own.png"]); }
        expected.sort();
        assert_eq!(names, expected);
    }
    // Exercise full downloads as well; keep this fixture within the 120 MiB admission budget.
    assert_eq!(app.clone().oneshot(request(&format!("/api/chat/attachments?id={}", handles[0]), &checked.secret)).await.unwrap().status(), StatusCode::OK);
    assert_eq!(app.clone().oneshot(request("/api/files/download?agent=sales&name=own-task.png", &checked.secret)).await.unwrap().status(), StatusCode::OK);
    // Browser Owner behavior is unchanged, even when the migration privacy flag is off.
    for name in ["other-bound.png", "external-archive.png"] {
        assert_eq!(app.clone().oneshot(request(&format!("/api/files/preview?agent=sales&name={name}"), &browser)).await.unwrap().status(), StatusCode::OK);
    }
    db.member_key_narrow_admin(&admin.id, &checked.info.id, Some(false), None).unwrap();
    for (uri, expected) in [
        (format!("/api/chat/attachments?id={}", handles[0]), StatusCode::FORBIDDEN),
        ("/api/files/download?agent=sales&name=own-task.png".into(), StatusCode::FORBIDDEN),
        ("/api/files/preview?agent=sales&name=own-task.png".into(), StatusCode::FORBIDDEN),
        ("/api/files/preview?agent=sales&name=task-archive.png".into(), StatusCode::OK),
    ] {
        assert_eq!(app.clone().oneshot(request(&uri, &checked.secret)).await.unwrap().status(), expected);
    }
    let response = app.clone().oneshot(request("/api/files?agent=sales", &checked.secret)).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 65536).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["files"].as_array().unwrap().iter().all(|f| f["name"] == "task-archive.png" || f["name"] == "task-bound.png"));
    // A failed live configuration read cannot preserve an earlier grant.
    for config in [Some("invalid ["), None] {
        if let Some(config) = config { std::fs::write(home.join("config.toml"), config).unwrap(); }
        else { std::fs::remove_file(home.join("config.toml")).unwrap(); }
        for uri in [format!("/api/chat/attachments?id={}", handles[0]), "/api/files?agent=sales".into()] {
            assert_eq!(app.clone().oneshot(request(&uri, &checked.secret)).await.unwrap().status(), StatusCode::UNAUTHORIZED);
        }
    }
}
