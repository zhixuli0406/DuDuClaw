//! Authentication boundary for conversation-scoped attachment downloads.
use super::*;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn chat_attachment_http_requires_header_jwt_and_rejects_query_tokens() {
    let directory = crate::fs_safe::private_test_home().unwrap();
    let home = directory.path().to_path_buf();
    let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let user = user_db.create_user(
        "attachment-admin@example.invalid", "Admin", "isolated-test-password", duduclaw_auth::UserRole::Admin,
    ).unwrap();
    let jwt_config = Arc::new(JwtConfig::new(b"attachment-http-test-secret-32-bytes!!"));
    let token = jwt_config.issue_access_token(&user, &[]).unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_user_db(user_db.clone(), jwt_config.clone()).await;
    #[cfg(unix)]
    let (attachment_id, attachment_path) = {
        let dir = home.join("agents/sales/attachments");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.pdf");
        std::fs::write(&path, b"%PDF-1.7").unwrap();
        let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
        {
            let conn = sessions.acquire().await;
            conn.execute("INSERT INTO sessions(id,agent_id,created_at,last_active) VALUES ('attachment-http','sales',?1,?1)", [chrono::Utc::now().to_rfc3339()]).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('attachment-http','user',?1,?2)",
                rusqlite::params![format!("[📎 Report.pdf (file)]({})", path.display()), chrono::Utc::now().to_rfc3339()]).unwrap();
        }
        let registry = Arc::new(tokio::sync::RwLock::new(duduclaw_agent::AgentRegistry::new(home.join("agents"))));
        let (reply_tx, _) = broadcast::channel(4);
        handler.set_reply_ctx(Arc::new(crate::channel_reply::ReplyContext::new(
            registry, home.clone(), sessions, Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())), reply_tx,
        ))).await;
        let ctx = user_db.live_user_context(&user.id).unwrap().unwrap();
        let response = handler.handle_chat_sessions_history(serde_json::json!({"session_id":"attachment-http"}), &ctx).await;
        let id = match response {
            crate::protocol::WsFrame::Response { ok: true, payload: Some(p), .. } => p["messages"][0]["attachments"][0]["id"].as_str().unwrap().to_owned(),
            other => panic!("history failed: {other:?}"),
        };
        (id, path)
    };
    let (tx, _) = broadcast::channel(4);
    let (event_tx, _) = broadcast::channel(4);
    let state = Arc::new(AppState {
        auth: AuthManager::new(None), handler, tx, event_tx,
        prepared_events: Default::default(), user_db, jwt_config,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
        home_dir: home,
    });
    let app = axum::Router::new()
        .route("/api/chat/attachments", axum::routing::get(handle_chat_attachment))
        .with_state(state);
    #[cfg(unix)]
    {
        let response = app.clone().oneshot(Request::builder()
            .uri(format!("/api/chat/attachments?id={attachment_id}"))
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-length"], "8");
        assert_eq!(response.headers()["content-encoding"], "identity");
        assert_eq!(response.headers()["content-type"], "application/pdf");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        assert_eq!(response.headers()["content-disposition"], "attachment; filename*=UTF-8''report.pdf");
        let bytes = axum::body::to_bytes(response.into_body(), 100).await.unwrap();
        assert_eq!(&bytes[..], b"%PDF-1.7");
    }
    #[cfg(unix)]
    {
        let request = |preview: bool| Request::builder()
            .uri(format!("/api/chat/attachments?id={attachment_id}&preview={preview}"))
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty()).unwrap();
        let first = app.clone().oneshot(request(false)).await.unwrap();
        let second = app.clone().oneshot(request(false)).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(app.clone().oneshot(request(false)).await.unwrap().status(), StatusCode::NOT_FOUND);
        drop(first);
        let replacement = app.clone().oneshot(request(false)).await.unwrap();
        assert_eq!(replacement.status(), StatusCode::OK);
        assert_eq!(axum::body::to_bytes(replacement.into_body(), 100).await.unwrap().len(), 8);
        drop(second);
        let file = std::fs::OpenOptions::new().write(true).open(&attachment_path).unwrap();
        file.set_len(crate::chat_attachments::limits::MAX_PREVIEW + 1).unwrap();
        assert_eq!(app.clone().oneshot(request(true)).await.unwrap().status(), StatusCode::NOT_FOUND);
        file.set_len(crate::chat_attachments::limits::MAX_DOWNLOAD + 1).unwrap();
        assert_eq!(app.clone().oneshot(request(false)).await.unwrap().status(), StatusCode::NOT_FOUND);
    }
    for (uri, auth) in [
        ("/api/chat/attachments?id=opaque".to_string(), None),
        ("/api/chat/attachments?id=opaque".to_string(), Some("Bearer invalid")),
        (format!("/api/chat/attachments?id=opaque&token={token}"), None),
    ] {
        let mut request = Request::builder().uri(uri);
        if let Some(auth) = auth { request = request.header("authorization", auth); }
        let response = app.clone().oneshot(request.body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    // A valid caller still has to supply an issued, currently authorized reference.
    let response = app.oneshot(Request::builder().uri("/api/chat/attachments?id=/etc/passwd&preview=true")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[cfg(unix)]
#[tokio::test]
async fn files_download_and_inline_preview_bind_snapshot_and_enforce_caps() {
    for route in ["download", "preview"] {
        let directory = crate::fs_safe::private_test_home().unwrap();
        let home = directory.path().canonicalize().unwrap();
        let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let user = user_db.create_user("files@test.invalid", "Reader", "isolated-test-password", duduclaw_auth::UserRole::Admin).unwrap();
        let jwt_config = Arc::new(JwtConfig::new(b"files-http-test-secret-32-bytes!!"));
        let token = jwt_config.issue_access_token(&user, &[]).unwrap();
        let handler = MethodHandler::new(home.clone()).await;
        handler.set_user_db(user_db.clone(), jwt_config.clone()).await;
        let path = crate::media::save_webchat_attachment_in_base(&home.join("agents/sales"), b"image bytes", "image.png").await.unwrap();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let (tx, _) = broadcast::channel(4);
        let (event_tx, _) = broadcast::channel(4);
        let state = Arc::new(AppState {
            auth: AuthManager::new(None), handler, tx, event_tx,
            prepared_events: Default::default(), user_db, jwt_config,
            otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(home.clone(), reqwest::Client::new())),
            home_dir: home.clone(),
        });
        let app = axum::Router::new()
            .route("/api/files/download", axum::routing::get(handle_files_download))
            .route("/api/files/preview", axum::routing::get(handle_files_preview))
            .with_state(state);
        let request = || Request::builder().uri(format!("/api/files/{route}?agent=sales&name={name}"))
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty()).unwrap();
        // Recheck the actual opened content even for the inline preview branch.
        std::fs::write(&path, b"other bytes").unwrap();
        assert_eq!(app.clone().oneshot(request()).await.unwrap().status(), StatusCode::FORBIDDEN);
        std::fs::write(&path, b"image bytes").unwrap();
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-length"], "11");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        // Replacing the file while the response is waiting cannot change bytes.
        let outside = home.join("secret.png");
        std::fs::write(&outside, b"secret bytes").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert_eq!(&axum::body::to_bytes(response.into_body(), 100).await.unwrap()[..], b"image bytes");
        assert_eq!(app.clone().oneshot(request()).await.unwrap().status(), StatusCode::FORBIDDEN);
        std::fs::remove_file(&path).unwrap();
        let file = std::fs::File::create(&path).unwrap();
        let cap = if route == "preview" { crate::chat_attachments::limits::MAX_PREVIEW } else { crate::chat_attachments::limits::MAX_DOWNLOAD };
        file.set_len(cap + 1).unwrap();
        assert_eq!(app.oneshot(request()).await.unwrap().status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
