//! W4 decisions: durable assignee, no mutation on refusal, cancellation stays open.
use super::*;
use crate::approval::{ApprovalBroker, ApprovalStatus, DecisionContext, ExecutionBinding, RequestKind};
use crate::channel_reply::entry_gate_test_helpers::owned_channel;
use crate::task_store::{TaskRow, TaskStore};
use duduclaw_core::types::RuntimeType;

fn bind(home: &Path, channel: &str) {
    let users = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let admin = users.create_user("decision@test.invalid", "Admin", "Fixture-Password-12345",
        duduclaw_auth::UserRole::Admin).unwrap();
    users.bind_channel_identity(&admin.id, channel, "42", true).unwrap();
}

fn break_worker(home: &Path, kind: &str) {
    let path = home.join("agents/worker/agent.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    match kind {
        "malformed" => std::fs::write(path, "[model").unwrap(),
        "mismatch" => std::fs::write(path, text.replace("provider='codex'", "provider='grok'")).unwrap(),
        "healthy" => {}
        _ => panic!("unknown fixture"),
    }
}

fn assert_refused(result: Result<String, String>, kind: &str) {
    let error = result.unwrap_err();
    assert!(error.contains(if kind == "malformed" { "agent_config_unreadable" }
        else { "model_provider_mismatch" }), "{error}");
}

async fn task(store: &TaskStore, id: &str, status: &str) {
    let mut row = TaskRow::new(id.into(), "報表".into(), "整理報表".into(), "medium".into(),
        "worker".into(), "system".into());
    row.goal_mode = true;
    row.status = status.into();
    store.insert_task(&row).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn needs_human_retry_checks_assignee_but_done_and_abort_do_not() {
    for kind in ["mismatch", "malformed", "healthy"] {
        let (home, _ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
        bind(home.path(), "telegram");
        let store = TaskStore::open(home.path()).unwrap();
        for id in ["retry", "done", "abort"] { task(&store, id, "needs_human").await; }
        break_worker(home.path(), kind);
        let before = crate::model_call_probe::calls();
        let result = crate::goal_notify::apply_needs_human(home.path(), "telegram", "42", "retry", DecisionAct::Retry).await;
        if kind == "healthy" { assert!(result.is_ok(), "{result:?}"); }
        else { assert_refused(result, kind); }
        assert_eq!(store.get_task("retry").await.unwrap().unwrap().status,
            if kind == "healthy" { "pending" } else { "needs_human" });
        for (id, act, status) in [("done", DecisionAct::Done, "done"), ("abort", DecisionAct::Abort, "cancelled")] {
            assert!(crate::goal_notify::apply_needs_human(home.path(), "telegram", "42", id, act).await.is_ok());
            assert_eq!(store.get_task(id).await.unwrap().unwrap().status, status);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(crate::model_call_probe::calls(), before);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn kickoff_and_legacy_approval_check_task_before_recording_and_allow_denial() {
    for kind in ["mismatch", "malformed", "healthy"] {
        let (home, _ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
        bind(home.path(), "line");
        let store = TaskStore::open(home.path()).unwrap();
        task(&store, "work", "todo").await;
        let broker = ApprovalBroker::open(home.path()).unwrap();
        break_worker(home.path(), kind);
        let before = crate::model_call_probe::calls();
        for kickoff in [false, true] {
            let id = broker.request("main", if kickoff { "goal_kickoff" } else { "mcp_call" },
                "開始報表", serde_json::json!({"task_id":"work"}), 600).await.unwrap();
            let result = if kickoff {
                crate::goal_notify::apply_kickoff(home.path(), "line", "42", id.as_str(), true).await
            } else {
                crate::approval_notify::apply_decision_in_session(home.path(), "line", "42", id.as_str(), true, None).await
            };
            if kind == "healthy" { assert!(result.is_ok(), "{result:?}"); }
            else {
                assert_refused(result, kind);
                assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
            }
            // A fresh pending request proves decline works even with bad settings.
            let denied = broker.request("main", if kickoff { "goal_kickoff" } else { "mcp_call" },
                "取消報表", serde_json::json!({"task_id":"work"}), 600).await.unwrap();
            let result = if kickoff {
                crate::goal_notify::apply_kickoff(home.path(), "line", "42", denied.as_str(), false).await
            } else {
                crate::approval_notify::apply_decision_in_session(home.path(), "line", "42", denied.as_str(), false, None).await
            };
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(broker.get(&denied).await.unwrap().unwrap().status, ApprovalStatus::Denied);
            assert_eq!(store.get_task("work").await.unwrap().unwrap().status, "todo");
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(crate::model_call_probe::calls(), before);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn bound_continue_checks_durable_assignee_and_cancel_remains_available() {
    for kind in ["mismatch", "malformed", "healthy"] {
        let (home, _ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
        bind(home.path(), "slack");
        let store = TaskStore::open(home.path()).unwrap();
        task(&store, "work", "in_progress").await;
        let snapshot = store.authority_snapshot("work").await.unwrap().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let context = DecisionContext { channel: "slack".into(), account_id: "bot".into(),
            principal_id: "42".into(), conversation_id: "slack:group:room".into() };
        let payload = serde_json::json!({"action":"continue"});
        let binding = ExecutionBinding {
            schema_version: 1, run_id: uuid::Uuid::new_v4().to_string(), run_origin_kind: "ingress".into(),
            actor_principal: "main".into(), conversation_audience: Default::default(),
            decision_context: context.clone(), task_id: Some("work".into()),
            task_revision: Some(snapshot.revision), task_snapshot_hash: Some(snapshot.hash),
            payload_hash: crate::approval::payload_hash(&payload),
            policy_revision: crate::approval::policy_revision(home.path(), "main").unwrap(),
            cwd: None, environment_hash: "fixture".into(), file_hashes: Default::default(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
            resume_handler: "live_only_v1".into(), resume_version: 1,
        };
        let id = broker.request_bound(RequestKind::Approval, "main", "繼續", payload.clone(), binding.clone()).await.unwrap();
        let cancel = broker.request_bound(RequestKind::Approval, "main", "取消", payload, binding).await.unwrap();
        break_worker(home.path(), kind);
        let before = crate::model_call_probe::calls();
        let result = apply_bound_request(home.path(), &context, id.as_str(), Some(true), None).await;
        if kind == "healthy" { assert!(result.is_ok(), "{result:?}"); }
        else {
            assert_refused(result, kind);
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
        }
        assert!(apply_bound_request(home.path(), &context, cancel.as_str(), Some(false), None).await.is_ok());
        assert_eq!(broker.get(&cancel).await.unwrap().unwrap().status, ApprovalStatus::Denied);
        assert_eq!(store.get_task("work").await.unwrap().unwrap().status, "in_progress");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(crate::model_call_probe::calls(), before);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn webchat_positive_legacy_decision_still_requires_a_session() {
    let (home, _ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    bind(home.path(), "webchat");
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let id = broker.request("main", "mcp_call", "同意", serde_json::json!({}), 600).await.unwrap();
    assert!(crate::approval_notify::apply_decision_in_session(
        home.path(), "webchat", "42", id.as_str(), true, None,
    ).await.is_err());
    assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
}

#[test]
fn channel_buttons_reach_shared_decision_handlers() {
    for source in [include_str!("telegram.rs"), include_str!("line.rs"),
        include_str!("discord.rs"), include_str!("slack.rs"), include_str!("googlechat.rs")] {
        assert!(source.contains("decision_notify::route_verified_bound_press("));
    }
    assert!(include_str!("line.rs").contains("decision_notify::route_bound_press("));
    assert!(include_str!("miniapp.rs").contains("decision_notify::route_press("));
    let source = include_str!("decision_notify.rs");
    for handler in ["goal_notify::apply_needs_human(", "goal_notify::apply_kickoff(",
        "approval_notify::apply_decision(", "apply_bound_request("] {
        assert!(source.contains(handler), "{handler}");
    }
}
