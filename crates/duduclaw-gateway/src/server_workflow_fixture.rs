//! Isolated browser acceptance fixture, not full gateway boot acceptance.
//! Only compiled into the test binary; no runtime environment escape hatch.
use super::*;
use crate::workflow::pilot_test_factory::Pilot;
use serde_json::json;
use std::{io::Write, path::PathBuf, time::Duration};

/// Run explicitly with `--ignored --exact ... --nocapture` and a verified CLI.
/// The manifest contains only newly created test identities. A sibling `.stop`
/// file shuts down the fixture; an hour is the hard maximum lifetime.
#[tokio::test]
#[ignore = "interactive isolated browser fixture; requires a fresh CLI and explicit output path"]
async fn serve_workflow_browser_fixture() {
    let manifest_path = PathBuf::from(
        std::env::var_os("DUDUCLAW_P1_BROWSER_MANIFEST")
            .expect("set DUDUCLAW_P1_BROWSER_MANIFEST to a new absolute manifest path"),
    );
    assert!(manifest_path.is_absolute());
    let mut manifest_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_path)
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        manifest_file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    let pilot = Pilot::new().await;
    let home = pilot.home.path().to_path_buf();
    let users = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let outsider = users
        .create_user(
            "outsider@test.invalid",
            "Fixture outsider",
            "isolated-outsider-password",
            duduclaw_auth::UserRole::Manager,
        )
        .unwrap();
    // Bind the outsider to the same employee to exercise private audience gates,
    // rather than merely proving a missing employee binding is denied.
    users
        .bind_agent(&outsider.id, "alice", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    let admin = users
        .create_user(
            "admin@test.invalid",
            "Fixture administrator",
            "isolated-admin-password",
            duduclaw_auth::UserRole::Admin,
        )
        .unwrap();
    let jwt = Arc::new(JwtConfig::load_or_generate(&home).unwrap());
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_user_db(users.clone(), jwt.clone()).await;
    handler
        .set_task_store(Arc::new(crate::task_store::TaskStore::open(&home).unwrap()))
        .await;
    handler.install_workflow_fixture(pilot.reopen()).unwrap();
    let (tx, _) = broadcast::channel(128);
    let (event_tx, _) = broadcast::channel(128);
    handler.set_event_tx(event_tx.clone()).await;

    // Seed a real outbound ledger entry so capture traverses the production
    // artifact collector and HTTP download traverses the private-file guard.
    let agent_dir = home.join("agents/alice");
    std::fs::create_dir_all(agent_dir.join("attachments")).unwrap();
    let archived = agent_dir.join("attachments/browser-source.md");
    std::fs::copy(agent_dir.join("source.md"), &archived).unwrap();
    crate::artifacts::record_saved(
        &agent_dir,
        &archived,
        "source.md",
        std::fs::metadata(&archived).unwrap().len(),
        &crate::artifacts::SaveContext {
            origin: crate::artifacts::ArtifactOrigin::Declared,
            task_id: Some(&pilot.draft.source_task),
            round: Some(1),
            channel: Some("dashboard"),
            source_path: Some(std::path::Path::new("source.md")),
        },
    );
    let state = Arc::new(AppState {
        auth: AuthManager::new(None),
        handler,
        tx,
        event_tx,
        user_db: users,
        jwt_config: jwt,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
            home.clone(),
            reqwest::Client::new(),
        )),
        home_dir: home.clone(),
    });
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/login", post(handle_login))
        .route("/api/refresh", post(handle_refresh))
        .route("/api/me", get(handle_me))
        .route("/api/change-password", post(handle_change_password))
        .route("/api/first-run/status", get(handle_first_run_status))
        .route("/api/files", get(handle_files_list))
        .route("/api/files/download", get(handle_files_download))
        .route("/api/files/preview", get(handle_files_preview))
        .route("/health", get(health_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let draft = &pilot.draft;
    let proposal = json!({
        "skill_id":draft.skill_id, "definition":draft.definition,
        "fixtures":draft.fixtures.iter().map(|f| json!({
            "fixture_id":f.fixture_id,"kind":f.kind,"input":f.input,"assertions":f.assertions
        })).collect::<Vec<_>>(),
        "source_data":draft.source_data,"effect_templates":draft.effect_templates,
        "budget":draft.budget,"input_max_age_seconds":draft.input_max_age_seconds,
        "timezone":draft.timezone,"stop_conditions":draft.stop_conditions,
        "routine":draft.routine.as_ref().map(|r|json!({"expression":r.expression,"timezone":r.timezone})),
    });
    let stop_path = manifest_path.with_extension("stop");
    assert!(
        !stop_path.exists(),
        "stale stop file must be removed before launch"
    );
    let manifest = json!({
        "scope":"test router using real JWT/login/WS/MethodHandler/files and WorkflowService with real CLI; fake public HTTP transport; NOT full gateway boot or scheduler-loop acceptance",
        "gateway_url":format!("http://{address}"), "home":home,
        "task_id":draft.source_task,"seed_draft_id":draft.draft_id,
        "owner":{"email":"pilot@test.invalid","password":"isolated-test-password","id":pilot.context.principal_id},
        "outsider":{"email":"outsider@test.invalid","password":"isolated-outsider-password","id":outsider.id},
        "admin":{"email":"admin@test.invalid","password":"isolated-admin-password","id":admin.id},
        "proposal":proposal,"stop_file":stop_path,
        "download_path":"/api/files/download?agent=alice&name=browser-source.md",
        "limits": [
            "Vite serves UI separately and proxies only to gateway_url",
            "No scheduler loop, provider discovery, production channels, OTP delivery, or full gateway boot",
            "Fixture observed_at expires after 600 seconds; restart for a fresh browser journey"
        ]
    });
    serde_json::to_writer_pretty(&mut manifest_file, &manifest).unwrap();
    manifest_file.flush().unwrap();
    println!("P1_BROWSER_READY {}", manifest_path.display());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3600);
        while !stop_path.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap();
    pilot.record_evidence(
        "browser_fixture_transport",
        json!({"manifest":manifest_path}),
    );
}
