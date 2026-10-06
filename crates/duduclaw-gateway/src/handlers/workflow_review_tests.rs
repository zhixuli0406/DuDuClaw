//! F3: accepting a review snapshot (V-M-8, L-3) and the Operator bar on
//! capture / accept (V-M-3).
use super::*;
use crate::review_evidence::ReviewSnapshot;

struct Fx {
    _dir: tempfile::TempDir,
    home: PathBuf,
    handler: MethodHandler,
    operator: UserContext,
    viewer: UserContext,
}

fn ctx_for(user: &duduclaw_auth::User, level: AccessLevel) -> UserContext {
    let mut c = UserContext::admin_fallback();
    c.user_id = user.id.clone();
    c.role = user.role;
    c.agent_access.insert("sales".into(), level);
    c
}

async fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    std::fs::create_dir_all(home.join("agents/sales")).unwrap();
    std::fs::write(home.join("agents/sales/report.md"), "report").unwrap();
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let op = db
        .create_user(
            "op@test.invalid",
            "Op",
            "isolated-test-password",
            UserRole::Manager,
        )
        .unwrap();
    db.bind_agent(&op.id, "sales", AccessLevel::Operator)
        .unwrap();
    let viewer = db
        .create_user(
            "v@test.invalid",
            "V",
            "isolated-test-password",
            UserRole::Manager,
        )
        .unwrap();
    db.bind_agent(&viewer.id, "sales", AccessLevel::Viewer)
        .unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    let tasks = Arc::new(TaskStore::open(&home).unwrap());
    handler.set_task_store(tasks.clone()).await;
    handler
        .workflow_store()
        .await
        .unwrap()
        .initialize_evidence()
        .await
        .unwrap();
    let mut row = TaskRow::new(
        "task".into(),
        "Report".into(),
        String::new(),
        "normal".into(),
        "sales".into(),
        "system".into(),
    );
    row.status = "done".into();
    tasks.insert_task(&row).await.unwrap();
    std::fs::write(
        home.join(crate::task_changes::TASK_CHANGES_FILE),
        format!(
            "{}\n",
            json!({
                "task_id": "task", "agent_id": "sales",
                "path": home.join("agents/sales/report.md"),
                "op": "write", "tool_name": "Write",
                "timestamp": Utc::now().to_rfc3339(),
                "success": true, "source": "native", "round": 1
            })
        ),
    )
    .unwrap();
    Fx {
        _dir: dir,
        home,
        handler,
        operator: ctx_for(&op, AccessLevel::Operator),
        viewer: ctx_for(&viewer, AccessLevel::Viewer),
    }
}

async fn capture(f: &Fx, ctx: &UserContext) -> WsFrame {
    f.handler
        .handle_tasks_review_snapshot(json!({"task_id": "task", "action": "capture"}), ctx)
        .await
}

fn snapshot_of(frame: WsFrame) -> ReviewSnapshot {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => serde_json::from_value(p["snapshot"].clone()).unwrap(),
        other => panic!("capture failed: {other:?}"),
    }
}

async fn accept(f: &Fx, s: &ReviewSnapshot, ctx: &UserContext) -> WsFrame {
    f.handler
        .handle_tasks_review_accept(
            json!({"task_id": "task", "snapshot_id": s.snapshot_id, "snapshot_hash": s.snapshot_hash}),
            ctx,
        )
        .await
}

fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e.as_str().unwrap_or("").into(),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_older_snapshot_is_refused_after_a_newer_capture() {
    let f = fx().await;
    let first = snapshot_of(capture(&f, &f.operator).await);
    // Another reviewer captures while the first page still shows `first`.
    let second = snapshot_of(capture(&f, &f.operator).await);
    assert_ne!(first.snapshot_id, second.snapshot_id);
    let refused = accept(&f, &first, &f.operator).await;
    assert_eq!(error_text(&refused), "review snapshot is not the latest");
    let coded = super::workflow_errors::with_workflow_error_code("tasks.review_accept", refused);
    let WsFrame::Response { error: Some(e), .. } = coded else {
        panic!()
    };
    assert_eq!(e["code"], "snapshot_not_latest");
    assert!(matches!(
        accept(&f, &second, &f.operator).await,
        WsFrame::Response { ok: true, .. }
    ));
}

#[tokio::test]
async fn a_truncated_snapshot_cannot_be_accepted() {
    let f = fx().await;
    let mut s = snapshot_of(capture(&f, &f.operator).await);
    s.snapshot_id = "truncated".into();
    s.gaps
        .push(super::workflow_review_rpc::TRUNCATED_GAP.into());
    s.snapshot_hash = s.compute_hash();
    f.handler
        .workflow_store()
        .await
        .unwrap()
        .save_review_snapshot(&s)
        .await
        .unwrap();
    assert_eq!(
        error_text(&accept(&f, &s, &f.operator).await),
        "review snapshot truncated"
    );
}

#[tokio::test]
async fn capture_and_accept_need_an_operator_binding() {
    let f = fx().await;
    assert_eq!(
        error_text(&capture(&f, &f.viewer).await),
        "permission denied"
    );
    let s = snapshot_of(capture(&f, &f.operator).await);
    assert!(error_text(&accept(&f, &s, &f.viewer).await).contains("permission denied"));
    // Reading stays at Viewer.
    let read = f
        .handler
        .handle_tasks_review_snapshot(json!({"task_id": "task"}), &f.viewer)
        .await;
    assert!(matches!(read, WsFrame::Response { ok: true, .. }));
    let _ = &f.home;
}
