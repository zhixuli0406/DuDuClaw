//! A2A principals apply to ordinary RPCs, evidence and deleted-task fallbacks.
use super::*;
use super::task_privacy_tests::{fixture, admin, denied, ok, payload, Fixture};

const TASK: &str = "a2a-private-task";

async fn a2a_fixture() -> Fixture {
    let f = fixture().await;
    let mut row = TaskRow::new(TASK.into(), "private A2A title".into(),
        "private A2A description".into(), "normal".into(), "sales".into(),
        format!("a2a:user:{}", f.alice.user_id));
    row.a2a_owner = Some(format!("user:{}", f.alice.user_id));
    let store = f.handler.task_store().await.unwrap();
    store.insert_task(&row).await.unwrap();
    store.append_activity(&ActivityRow {
        id: "a2a-activity".into(), event_type: "task_created".into(), agent_id: "sales".into(),
        task_id: Some(TASK.into()), summary: "private A2A activity".into(),
        timestamp: Utc::now().to_rfc3339(), metadata: None,
    }).await.unwrap();
    f
}

#[tokio::test]
async fn a2a_owner_blocks_bob_and_admin_on_ordinary_task_rpcs() {
    let f = a2a_fixture().await;
    let store = f.handler.task_store().await.unwrap();
    let root = admin(&f);
    for ctx in [&f.bob, &root, &UserContext::admin_fallback()] {
        let p = json!({"task_id": TASK, "body": "intrusion", "title": "overwrite"});
        assert!(denied(&f.handler.handle_tasks_update(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_remove(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_create(json!({"title":"child", "assigned_to":"sales", "parent_task_id":TASK}), ctx).await));
        assert!(denied(&f.handler.handle_tasks_update(json!({"task_id":"open-task", "depends_on":[TASK]}), ctx).await));
        assert!(denied(&f.handler.handle_tasks_comment(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_comments(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_artifacts(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_iterations(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_changes(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_role_turns(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_tasks_timeline(p.clone(), ctx).await));
        assert!(denied(&f.handler.handle_activity_list(p.clone(), ctx).await));
        for method in ["tasks.stop", "tasks.stop_status", "tasks.steer", "tasks.steering"] {
            assert!(denied(&f.handler.handle_responsibilities_rpc(method, p.clone(), ctx).await), "{method}");
        }
        assert!(f.handler.authorize_task_access(&store, ctx, TASK, AccessLevel::Viewer).await.is_err());
        let p = json!({"agent_id": "sales"});
        let list = payload(f.handler.handle_tasks_list(p.clone(), ctx).await);
        let page = payload(f.handler.handle_tasks_list_page(p.clone(), ctx).await);
        let activity = payload(f.handler.handle_activity_list(p.clone(), ctx).await);
        let timeline = payload(f.handler.handle_timeline_list(p, ctx).await);
        for value in [&list, &page, &activity, &timeline] {
            assert!(!value.to_string().contains(TASK));
            assert!(!value.to_string().contains("private A2A"));
        }
        assert_eq!(page["total"], 2);
        assert_eq!(activity["total"], 2);
    }
    assert!(f.handler.authorize_task_access(&store, &f.alice, TASK, AccessLevel::Viewer).await.is_ok());
    assert!(ok(&f.handler.handle_tasks_comment(json!({"task_id":TASK,"body":"owner note"}), &f.alice).await));
    assert_eq!(store.get_task(TASK).await.unwrap().unwrap().title, "private A2A title");
}

#[tokio::test]
async fn a2a_owner_survives_restart_delete_and_inherited_children() {
    let f = a2a_fixture().await;
    let store = TaskStore::open(&f.home).unwrap();
    let mut child = TaskRow::new("a2a-child".into(), "child secret".into(), "".into(),
        "normal".into(), "sales".into(), "sales".into());
    child.parent_task_id = Some(TASK.into());
    store.insert_task(&child).await.unwrap();
    let child = store.get_task("a2a-child").await.unwrap().unwrap();
    assert_eq!(child.a2a_owner.as_deref(), Some(format!("user:{}", f.alice.user_id).as_str()));
    assert!(store.update_task(&child.id, &json!({"parent_task_id":"open-task"})).await.is_err());
    let db = rusqlite::Connection::open(f.home.join("tasks.db")).unwrap();
    assert!(db.execute("UPDATE tasks SET a2a_owner = NULL WHERE id=?1", [TASK]).is_err());
    store.remove_task(TASK).await.unwrap();
    let root = admin(&f);
    for ctx in [&f.bob, &root, &UserContext::admin_fallback()] {
        assert!(denied(&f.handler.handle_activity_list(json!({"task_id": TASK}), ctx).await));
        assert!(!TaskStore::a2a_principal_allows(&f.home, TASK, &format!("user:{}", ctx.user_id)));
        assert!(!TaskStore::a2a_principal_allows(&f.home, &child.id, &format!("user:{}", ctx.user_id)));
    }
    assert!(TaskStore::a2a_principal_allows(&f.home, TASK, &format!("user:{}", f.alice.user_id)));
    let replacement = TaskRow::new(TASK.into(), "replacement".into(), "".into(),
        "normal".into(), "sales".into(), "system".into());
    assert!(store.insert_task(&replacement).await.is_err());
}

#[tokio::test]
async fn a2a_owner_filters_pushes_and_attachment_access() {
    let f = a2a_fixture().await;
    let root = admin(&f);
    for event in ["task.created", "task.updated", "task.removed", "activity.new", "task.comment"] {
        let line = serde_json::to_string(&WsFrame::Event {
            event: event.into(), payload: json!({"id":TASK,"task_id":TASK,"assigned_to":"sales","agent_id":"sales","summary":"secret"}),
            seq: None, state_version: None, boot_id: None,
        }).unwrap();
        for ctx in [&f.bob, &root] {
            assert!(filter_push_event(&f.home, ctx, &line).is_none(), "{event}");
        }
    }
    let path = crate::review_evidence::download::save_private_artifact(&f.home, TASK, "sales",
        b"A2A secret", "report.md", &[format!("user:{}", f.alice.user_id)]).await.unwrap();
    let store = crate::workflow::WorkflowStore::open(&f.home).unwrap();
    store.initialize_evidence().await.unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    for ctx in [&f.bob, &root] {
        assert!(crate::review_evidence::download::authorize_artifact_access(&f.home, &store, ctx, Some("sales"), name).await.is_err());
    }
    assert!(crate::review_evidence::download::authorize_artifact_access(&f.home, &store, &f.alice, Some("sales"), name).await.is_ok());
}

#[tokio::test]
async fn a2a_flow_metrics_filter_before_aggregation_and_recheck_live_access() {
    let f = a2a_fixture().await;
    let root = admin(&f);
    let params = json!({"agent_id":"sales", "principal":format!("user:{}", f.alice.user_id)});
    let baseline = payload(f.handler.handle_tasks_flow_metrics(params.clone(), &f.bob).await);
    let store = f.handler.task_store().await.unwrap();
    let mut goal = TaskRow::new("a2a-metrics-goal".into(), "private goal".into(), String::new(),
        "normal".into(), "sales".into(), format!("a2a:user:{}", f.alice.user_id));
    goal.a2a_owner = Some(format!("user:{}", f.alice.user_id));
    goal.goal_mode = true;
    goal.status = "review".into();
    store.insert_task(&goal).await.unwrap();
    for ctx in [&f.bob, &root, &UserContext::admin_fallback()] {
        let metrics = payload(f.handler.handle_tasks_flow_metrics(params.clone(), ctx).await);
        assert_eq!(metrics, baseline, "caller-supplied principal must not reveal another owner's metrics");
    }
    let owner = payload(f.handler.handle_tasks_flow_metrics(params.clone(), &f.alice).await);
    assert_eq!(owner["review_queue_depth"], 1);
    assert_eq!(owner["agents"][0]["review_queue_depth"], 1);
    f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
    assert!(denied(&f.handler.handle_tasks_flow_metrics(params.clone(), &f.alice).await));
    f.db.set_user_status(&root.user_id, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert!(denied(&f.handler.handle_tasks_flow_metrics(params, &root).await));
}
