//! W5 storage-boundary matrix, reusable by the all-entry acceptance suite.
use super::*;
use crate::channel_reply::entry_gate_test_helpers::{owned_channel, misaligned_worker,
    assert_refused_before_any_side_effect, assert_no_saved_attachments};
use duduclaw_core::types::RuntimeType;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

fn request(plan_first: bool) -> GoalCreateRequest {
    GoalCreateRequest { agent_id: "worker".into(), created_by: "goal:dashboard".into(),
        description: "整理報表".into(), plan_first, ..Default::default() }
}

#[tokio::test(flavor = "current_thread")]
async fn every_creation_rail_refuses_before_planning_admission_or_insert() {
    for broken in [false, true] {
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        if broken { std::fs::write(home.path().join("agents/worker/agent.toml"), "[runtime").unwrap(); }
        let before = crate::model_call_probe::calls();
        let store = TaskStore::open(home.path()).unwrap();
        let code = if broken { "agent_config_unreadable" } else { "model_provider_mismatch" };
        for plan_first in [false, true] {
            // Dashboard calls the plain wrapper; MCP and A2A supply admission.
            let error = create_goal_task(home.path(), &store, request(plan_first)).await.unwrap_err();
            assert!(error.contains(code), "{error}");
            let polled = Arc::new(AtomicBool::new(false));
            let observed = polled.clone();
            let admission = Box::pin(async move { observed.store(true, Ordering::SeqCst); Ok(None) });
            let error = create_goal_task_with_admission(home.path(), &store, request(plan_first), admission).await.unwrap_err();
            assert!(error.contains(code), "{error}");
            assert!(!polled.load(Ordering::SeqCst));
            let error = create_goal_task_for_chat(home.path(), &store, request(plan_first)).await.unwrap_err();
            let GoalCreateError::Entry(refusal) = error else { panic!("typed entry refusal required") };
            for session in ["odoolivechat:test:1", "widget-visitor:test"] {
                assert_eq!(refusal.user_text(session, None, "create"), crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
            }
            assert_refused_before_any_side_effect(&ctx, "webchat:create", &calls, before).await;
            assert_no_saved_attachments(home.path());
        }
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = users.create_user("schedule-entry@test.invalid", "Entry", "Fixture-Password-12345",
            duduclaw_auth::UserRole::Admin).unwrap();
        let creator = crate::assign_schedule::Creator::new(&admin.id, true).unwrap();
        for spec in [crate::assign_schedule::ScheduleSpec::Weekly { days: vec![1], hour: 9, minute: 0 },
            crate::assign_schedule::ScheduleSpec::Once { at: (Utc::now() + chrono::Duration::hours(2)).to_rfc3339() }] {
            let req = crate::assign_schedule::NewSchedule {
                agent_id: "worker".into(), kind: crate::assign_schedule::AssignKind::Goal,
                payload: crate::assign_schedule::AssignPayload { description: "週報".into(), plan_first: true, ..Default::default() },
                spec, timezone: Some("Asia/Taipei".into()), created_via: "dashboard".into(), source: None,
            };
            let error = crate::assign_schedule::manage::create(home.path(), &store, &creator, req, Utc::now()).await.unwrap_err();
            assert!(matches!(error, crate::assign_schedule::CreateError::Entry(_)), "{error:?}");
        }
        let db = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM assign_schedules", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        assert_refused_before_any_side_effect(&ctx, "webchat:create", &calls, before).await;
        assert_no_saved_attachments(home.path());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn healthy_goal_retains_contract_and_admission_behavior() {
    let (home, _ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
    let store = TaskStore::open(home.path()).unwrap();
    let polled = Arc::new(AtomicBool::new(false));
    let observed = polled.clone();
    let admission = Box::pin(async move { observed.store(true, Ordering::SeqCst); Ok(None) });
    let created = create_goal_task_with_admission(home.path(), &store, request(false), admission).await.unwrap();
    assert!(polled.load(Ordering::SeqCst));
    assert_eq!(created.task.assigned_to, "worker");
    assert_eq!(created.task.status, "todo");
    assert_eq!(created.task.acceptance_criteria_baseline.as_deref(), Some("整理報表"));
    assert_eq!(store.list_tasks(None, None, None).await.unwrap().len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn settings_broken_during_admission_still_leave_no_row() {
    let (home, ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
    let store = TaskStore::open(home.path()).unwrap();
    let path = home.path().join("agents/worker/agent.toml");
    let admission = Box::pin(async move { std::fs::write(path, "[model").unwrap(); Ok(None) });
    let before = crate::model_call_probe::calls();
    let error = create_goal_task_with_admission(home.path(), &store, request(false), admission).await.unwrap_err();
    assert!(error.contains("agent_config_unreadable"));
    assert_refused_before_any_side_effect(&ctx, "webchat:admission", &calls, before).await;
}

#[tokio::test(flavor = "current_thread")]
async fn decision_start_uses_durable_assignee_and_denial_remains_available() {
    for broken in [false, true] {
        let (home, ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
        let store = TaskStore::open(home.path()).unwrap();
        let created = create_goal_task(home.path(), &store, request(false)).await.unwrap();
        let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
        // The card's label names healthy main, but the durable task belongs to worker.
        let id = broker.request("main", "goal_kickoff", "Start", serde_json::json!({"task_id":created.task.id}), 600).await.unwrap();
        let record = broker.get(&id).await.unwrap().unwrap();
        let path = home.path().join("agents/worker/agent.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, if broken { "[model".to_string() } else { text.replace("provider='codex'", "provider='grok'") }).unwrap();
        let before = crate::model_call_probe::calls();
        for sid in ["webchat:decision", "widget-visitor:test", "odoolivechat:test:1"] {
            let refused = crate::entry_gate::check_approval_start(home.path(), &record, true, sid).await.unwrap_err();
            if sid != "webchat:decision" { assert_eq!(refused, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE); }
            assert!(crate::entry_gate::check_approval_start(home.path(), &record, false, sid).await.is_ok());
        }
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Pending);
        assert_eq!(store.get_task(&created.task.id).await.unwrap().unwrap().status, "todo");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(crate::model_call_probe::calls(), before);
        assert_no_saved_attachments(&ctx.home_dir);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn task_start_storage_failures_never_expose_internal_errors() {
    for failure in ["missing", "open", "read"] {
        let home = tempfile::tempdir().unwrap();
        match failure {
            "open" => std::fs::create_dir(home.path().join("tasks.db")).unwrap(),
            "read" => {
                let _store = TaskStore::open(home.path()).unwrap();
                rusqlite::Connection::open(home.path().join("tasks.db")).unwrap()
                    .execute_batch("DROP TABLE tasks").unwrap();
            }
            _ => {}
        }
        for sid in ["webchat:operator", "widget-visitor:test", "odoolivechat:test:1"] {
            let text = crate::entry_gate::check_task_start(home.path(), "unknown", sid, "start").await.unwrap_err();
            assert_eq!(text, if sid == "webchat:operator" { "目前無法開始這項工作，請稍後再試。" }
                else { crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE });
        }
        let text = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
            "widget-visitor:account".to_string(),
            crate::entry_gate::check_task_start(home.path(), "unknown", "webchat:account", "start"),
        ).await.unwrap_err();
        assert_eq!(text, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unauthorized_goal_and_schedule_do_not_reveal_broken_settings() {
    let (home, _ctx, _runtime, _calls) = misaligned_worker("").await;
    let store = TaskStore::open(home.path()).unwrap();
    for body in ["[model", "[agent]\nstatus='active'\n[model]\npreferred='gpt-5.4'\n[runtime]\nprovider='grok'\n"] {
        std::fs::write(home.path().join("agents/worker/agent.toml"), body).unwrap();
        let mut req = request(false);
        req.on_behalf_of = Some("user:unknown-account".into());
        let error = create_goal_task_for_chat(home.path(), &store, req).await.unwrap_err();
        assert!(matches!(error, GoalCreateError::Other(_)), "{error:?}");
        let creator = crate::assign_schedule::Creator::new("unknown-account", false).unwrap();
        let req = crate::assign_schedule::NewSchedule {
            agent_id: "worker".into(), kind: crate::assign_schedule::AssignKind::Goal,
            payload: crate::assign_schedule::AssignPayload { description: "work".into(), ..Default::default() },
            spec: crate::assign_schedule::ScheduleSpec::Weekly { days: vec![1], hour: 9, minute: 0 },
            timezone: Some("Asia/Taipei".into()), created_via: "dashboard".into(), source: None,
        };
        assert!(matches!(crate::assign_schedule::manage::create(home.path(), &store, &creator, req, Utc::now()).await,
            Err(crate::assign_schedule::CreateError::Store(_))));
    }
    assert!(store.list_tasks(None, None, None).await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn schedule_creation_preserves_missing_deleted_and_archived_assignee_codes() {
    let home = tempfile::tempdir().unwrap();
    let org = crate::assign_schedule::manage::tests::org(home.path());
    let store = TaskStore::open(home.path()).unwrap();
    let creator = crate::assign_schedule::Creator::new(&org.admin, true).unwrap();
    for status in ["missing", "deleted", "archived"] {
        let mut request = crate::assign_schedule::manage::tests::weekly_goal("work");
        request.agent_id = status.into();
        if status != "missing" {
            let dir = home.path().join("agents").join(status);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("agent.toml"), format!("[agent]\nname='{status}'\nstatus='{status}'\n")).unwrap();
        }
        let error = crate::assign_schedule::manage::create(home.path(), &store, &creator, request, Utc::now()).await.unwrap_err();
        assert_eq!(error.code(), duduclaw_core::agent_seats::TASK_ASSIGNEE_ERROR_CODE);
    }
}
