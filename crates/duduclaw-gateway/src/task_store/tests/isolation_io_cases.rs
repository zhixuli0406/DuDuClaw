//! Actual TaskStore mutex wait regression for isolated legacy MCP writes.
use super::*;
use std::{future::Future, sync::{Arc, atomic::{AtomicBool, Ordering}}, task::Poll};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, LiveRequestVerifier, Principal, RequestContext, VerificationError},
    ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::io_authority;

struct Live(Arc<AtomicBool>);
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
        if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
    }
}
fn context(active: Arc<AtomicBool>) -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
        gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Live(active)))
}

#[tokio::test]
async fn revoked_isolated_task_writes_waiting_on_real_store_mutex_leave_database_unchanged() {
    for operation in 0..9 {
        let (store, _home) = temp_store();
        let original = pending_task("target");
        store.insert_task(&original).await.unwrap();
        let before = serde_json::to_value(store.get_task("target").await.unwrap()).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let held = store.conn.lock().await;
        let request = io_authority::scope(context(active.clone()), async {
            // Simulate dispatch/admission succeeding before a later lock await.
            io_authority::verify_before_io().map_err(|e| e.to_string())?;
            match operation {
                0 => store.update_task("target", &serde_json::json!({"title":"changed"})).await.map(|_| ()),
                1 => store.insert_task(&pending_task("new")).await,
                2 => store.atomic_claim("target", "alice", "2026-11-05T00:00:00Z", "2026-11-05T01:00:00Z").await.map(|_| ()),
                3 => store.remove_task("target").await.map(|_| ()),
                4 => store.complete_task("target", "done", "alice").await.map(|_| ()),
                5 => store.claim_task("target", "alice", "2026-11-05T00:00:00Z").await.map(|_| ()),
                6 => store.renew_lease("target", "alice", "2026-11-05T01:00:00Z", "2026-11-05T00:00:00Z").await.map(|_| ()),
                7 => store.set_goal_state_json("target", Some("{}" )).await,
                _ => store.append_activity(&ActivityRow { id: "event".into(), event_type: "test".into(),
                    agent_id: "alice".into(), task_id: Some("target".into()), summary: "must not write".into(),
                    timestamp: "2026-11-05T00:00:00Z".into(), metadata: None }).await,
            }
        });
        tokio::pin!(request);
        std::future::poll_fn(|cx| {
            assert!(request.as_mut().poll(cx).is_pending(), "operation must really wait on the store mutex");
            Poll::Ready(())
        }).await;
        active.store(false, Ordering::SeqCst);
        drop(held);
        assert!(request.await.unwrap_err().contains("revoked"), "operation {operation}");
        let after = serde_json::to_value(store.get_task("target").await.unwrap()).unwrap();
        assert_eq!(after, before, "operation {operation} wrote after revocation");
        assert!(store.get_task("new").await.unwrap().is_none());
        assert_eq!(store.list_activity(None, None, 10, 0).await.unwrap().1, 0);
    }
}

#[tokio::test]
async fn live_isolated_and_host_task_mutations_still_persist() {
    let (store, _home) = temp_store();
    store.insert_task(&pending_task("target")).await.unwrap();
    io_authority::scope(context(Arc::new(AtomicBool::new(true))), async {
        store.update_task("target", &serde_json::json!({"title":"isolated update"})).await
    }).await.unwrap();
    assert_eq!(store.get_task("target").await.unwrap().unwrap().title, "isolated update");
    store.update_task("target", &serde_json::json!({"title":"host update"})).await.unwrap();
    assert_eq!(store.get_task("target").await.unwrap().unwrap().title, "host update");
}

#[tokio::test]
async fn goals_and_plans_recheck_after_real_mutex_wait() {
    for operation in 0..4 {
        let (store, _home) = temp_store();
        store.insert_goal(&goal("goal", "original", None)).await.unwrap();
        store.insert_plan(&plan("plan", "alice")).await.unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let held = store.conn.lock().await;
        let request = io_authority::scope(context(active.clone()), async {
            match operation {
                0 => store.insert_goal(&goal("new", "new", None)).await,
                1 => store.update_goal("goal", &serde_json::json!({"title": "changed"})).await.map(|_| ()),
                2 => store.insert_plan(&plan("new", "alice")).await,
                _ => store.remove_plan("plan").await.map(|_| ()),
            }
        });
        tokio::pin!(request);
        std::future::poll_fn(|cx| {
            assert!(request.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        active.store(false, Ordering::SeqCst);
        drop(held);
        assert!(request.await.unwrap_err().contains("revoked"));
        assert_eq!(store.get_goal("goal").await.unwrap().unwrap().title, "original");
        assert!(store.get_goal("new").await.unwrap().is_none());
        assert!(store.get_plan("new").await.unwrap().is_none());
        assert!(store.get_plan("plan").await.unwrap().is_some());
    }
}

#[tokio::test]
async fn live_write_commit_refusal_rolls_back_sqlite_changes() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE changes (id INTEGER)").unwrap();
    let active = Arc::new(AtomicBool::new(true));
    io_authority::scope(context(active.clone()), async {
        let tx = super::super::begin_live_write(&mut conn).unwrap();
        tx.execute("INSERT INTO changes VALUES (1)", []).unwrap();
        active.store(false, Ordering::SeqCst);
        assert!(super::super::commit_live_write(tx).unwrap_err().contains("revoked"));
    }).await;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM changes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn live_goal_plan_and_step_transactions_persist() {
    let (store, _home) = temp_store();
    io_authority::scope(context(Arc::new(AtomicBool::new(true))), async {
        store.insert_goal(&goal("goal", "original", None)).await.unwrap();
        store.update_goal("goal", &serde_json::json!({"title":"updated"})).await.unwrap();
        store.insert_plan(&plan("plan", "alice")).await.unwrap();
        store.update_plan("plan", &serde_json::json!({"title":"updated"})).await.unwrap();
        store.add_plan_step("plan", "one", "first", "agent", "alice", None).await.unwrap();
        store.add_plan_step("plan", "two", "second", "agent", "alice", None).await.unwrap();
        store.update_plan_step("one", &serde_json::json!({"status":"doing"})).await.unwrap();
        assert!(store.move_plan_step("plan", "two", 0).await.unwrap());
        assert_eq!(store.list_plan_steps("plan").await.unwrap()[0].id, "two");
        assert!(store.remove_plan_step("one").await.unwrap().is_some());
        assert!(store.remove_plan("plan").await.unwrap());
    }).await;
    assert_eq!(store.get_goal("goal").await.unwrap().unwrap().title, "updated");
    assert!(store.get_plan("plan").await.unwrap().is_none());
}

#[tokio::test]
async fn round3_service_writes_recheck_after_real_mutex_wait() {
    for operation in 0..12 {
        let (store, _home) = temp_store();
        let mut row = goal_review_task("target");
        row.source_channel = Some("telegram".into());
        row.source_chat_id = Some("chat".into());
        store.insert_task(&row).await.unwrap();
        let before = serde_json::to_value(store.get_task("target").await.unwrap()).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let held = store.conn.lock().await;
        let request = io_authority::scope(context(active.clone()), async {
            match operation {
                0 => store.accept_review("target", "accepted").await.map(|_| ()),
                1 => store.reject_review("target", "redo", 3).await.map(|_| ()),
                2 => store.mark_needs_human("target", "review").await.map(|_| ()),
                3 => store.mark_needs_human_sealing_round("target", "review", crate::pause_reason::PauseReason::Unknown, None).await.map(|_| ()),
                4 => store.cancel_task("target", "cancel").await.map(|_| ()),
                5 => store.clear_plan_pending("target").await,
                6 => store.claim_conversation_tasks("telegram", "chat", "person").await.map(|_| ()),
                7 => store.record_iteration_dispatch("target", 1, "2026-11-05T00:00:00Z").await,
                8 => store.record_iteration_evaluator_verdict("target", "blocked").await,
                9 => store.rewrite_result_summary("target", "before", "after").await.map(|_| ()),
                10 => store.stamp_iteration_pause("target", "infra").await,
                _ => store.discard_steering_of_finished_tasks(chrono::Utc::now()).await.map(|_| ()),
            }
        });
        tokio::pin!(request);
        std::future::poll_fn(|cx| {
            assert!(request.as_mut().poll(cx).is_pending(), "operation {operation} must wait on the store mutex");
            Poll::Ready(())
        }).await;
        active.store(false, Ordering::SeqCst);
        drop(held);
        assert!(request.await.unwrap_err().contains("revoked"), "operation {operation}");
        assert_eq!(serde_json::to_value(store.get_task("target").await.unwrap()).unwrap(), before);
        assert!(store.list_iterations("target").await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn finite_goals_do_not_expand_to_new_ungranted_or_reassigned_tasks() {
    let (store, _home) = temp_store();
    for id in ["goal-a", "goal-b", "goal-new"] {
        store.insert_goal(&goal(id, id, None)).await.unwrap();
    }
    for (id, owner, goal_id) in [("a", "alice", "goal-a"), ("b", "bob", "goal-b"), ("new", "alice", "goal-new")] {
        let mut row = pending_task(id);
        row.assigned_to = owner.into(); row.goal_id = Some(goal_id.into());
        store.insert_task(&row).await.unwrap();
    }
    let grants = vec!["a".into(), "b".into()];
    let rows = store.list_goals_isolated("alice", &grants, None).await.unwrap();
    assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["goal-a"]);
    assert!(store.list_goals_isolated("alice", &grants, Some("done")).await.unwrap().is_empty());
    store.update_task("a", &serde_json::json!({"assigned_to":"bob"})).await.unwrap();
    assert!(store.list_goals_isolated("alice", &grants, None).await.unwrap().is_empty());
    assert!(store.list_goals_isolated("alice", &["*".into()], None).await.is_err());
}
