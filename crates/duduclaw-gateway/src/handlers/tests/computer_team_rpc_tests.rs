use super::*;

struct Fixture {
    home: tempfile::TempDir,
    handler: MethodHandler,
    db: Arc<UserDb>,
    manager: UserContext,
    report: String,
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[people]\nmanager_team_history=true\n",
    )
    .unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let manager = db
        .create_user(
            "manager@test.invalid",
            "Manager",
            "isolated-password",
            UserRole::Manager,
        )
        .unwrap()
        .id;
    let report = db
        .create_user(
            "report@test.invalid",
            "Report",
            "isolated-password",
            UserRole::Employee,
        )
        .unwrap()
        .id;
    db.set_manager(&report, Some(&manager)).unwrap();
    db.bind_agent(&report, "sales", AccessLevel::Operator)
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
    let manager = db.live_user_context(&manager).unwrap().unwrap();
    Fixture {
        home,
        handler,
        db,
        manager,
        report,
    }
}

fn body(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(body),
            ..
        } => body,
        other => panic!("expected success: {other:?}"),
    }
}

fn is_denied(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: false, .. })
}

#[tokio::test]
async fn team_status_uses_reporting_scope_without_expanding_direct_agent_access() {
    let f = fixture().await;
    let status = body(
        f.handler
            .handle(
                "computer_sessions.status",
                json!({"agent_id":"sales"}),
                &f.manager,
            )
            .await,
    );
    assert_eq!(
        status,
        json!({"ok":true,"active":false,"view_mode":"masked","can_control":false})
    );
    for method in [
        "agents.get",
        "computer_sessions.view",
        "computer_sessions.takeover",
        "computer_sessions.hand_back",
        "computer_sessions.resume",
        "computer_profiles.status",
        "computer_profiles.reset",
    ] {
        assert!(
            is_denied(
                &f.handler
                    .handle(
                        method,
                    json!({"agent_id":"sales","name":"sales","confirm":true}),
                        &f.manager
                    )
                    .await
            ),
            "{method}"
        );
    }
}

#[tokio::test]
async fn team_status_rechecks_setting_reports_bindings_and_manager_status() {
    for change in ["off", "report", "viewer", "manager"] {
        let f = fixture().await;
        body(
            f.handler
                .handle(
                    "computer_sessions.status",
                    json!({"agent_id":"sales"}),
                    &f.manager,
                )
                .await,
        );
        match change {
            "off" => std::fs::write(
                f.home.path().join("config.toml"),
                "[people]\nmanager_team_history=false\n",
            )
            .unwrap(),
            "report" => f.db.set_manager(&f.report, None).unwrap(),
            "viewer" => {
                f.db.bind_agent(&f.report, "sales", AccessLevel::Viewer)
                    .unwrap()
            }
            _ => {
                f.db.set_user_status(&f.manager.user_id, duduclaw_auth::UserStatus::Suspended)
                    .unwrap()
            }
        }
        assert!(
            is_denied(
                &f.handler
                    .handle(
                        "computer_sessions.status",
                        json!({"agent_id":"sales"}),
                        &f.manager
                    )
                    .await
            ),
            "{change}"
        );
    }
}

#[tokio::test]
async fn team_disclosure_fails_closed_when_its_durable_audit_is_unavailable() {
    let f = fixture().await;
    crate::team_oversight::observe_window(f.home.path(), &f.db, "test").unwrap();
    std::fs::remove_file(f.home.path().join("security_audit.jsonl")).unwrap();
    std::fs::create_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(is_denied(
        &f.handler
            .handle(
                "computer_sessions.status",
                json!({"agent_id":"sales"}),
                &f.manager
            )
            .await
    ));
}
