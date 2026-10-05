//! P2-A C1 (D10): `needs_human` writes never reopen a terminal task.
//!
//! Before the guard, a tick or settle holding an older snapshot could flip a
//! `cancelled` task back to `needs_human`; a later human "retry" then
//! revived it. Both writers now carry the same terminal-state guard.

use super::*;
use crate::pause_reason::PauseReason;

async fn store_with_goal(status: &str) -> (TaskStore, tempfile::TempDir) {
    let (store, dir) = temp_store();
    let mut t = goal_review_task("g1");
    t.status = status.into();
    store.insert_task(&t).await.expect("insert");
    (store, dir)
}

#[tokio::test]
async fn mark_needs_human_with_pause_leaves_terminal_tasks_alone() {
    for status in ["cancelled", "done", "failed"] {
        let (store, _dir) = store_with_goal(status).await;
        let changed = store
            .mark_needs_human_with_pause("g1", "stale escalation", PauseReason::BudgetExhausted)
            .await
            .expect("write");
        assert!(!changed, "{status}: guard must refuse");
        let row = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(row.status, status);
        assert_eq!(row.pause_reason, None, "{status}: pause class untouched");
    }
}

#[tokio::test]
async fn mark_needs_human_sealing_round_leaves_terminal_tasks_alone() {
    for status in ["cancelled", "done", "failed"] {
        let (store, _dir) = store_with_goal(status).await;
        let changed = store
            .mark_needs_human_sealing_round("g1", "late settle", PauseReason::Infra, None)
            .await
            .expect("write");
        assert!(!changed, "{status}: guard must refuse");
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, status);
    }
}

#[tokio::test]
async fn cancelled_task_cannot_be_revived_through_needs_human_retry() {
    let (store, _dir) = store_with_goal("cancelled").await;
    store.mark_needs_human("g1", "stale").await.expect("write");
    // The retry button only acts on needs_human; with the guard the task
    // never got there, so retry is a no-op and the task stays cancelled.
    assert!(!store.resolve_needs_human("g1", "retry", "").await.unwrap());
    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "cancelled"
    );
}

#[tokio::test]
async fn non_terminal_tasks_still_escalate() {
    for status in [
        "todo",
        "pending",
        "revising",
        "in_progress",
        "review",
        "needs_human",
    ] {
        let (store, _dir) = store_with_goal(status).await;
        assert!(
            store
                .mark_needs_human_with_pause("g1", "stuck", PauseReason::NoProgress)
                .await
                .unwrap(),
            "{status}"
        );
        assert_eq!(
            store.get_task("g1").await.unwrap().unwrap().status,
            "needs_human"
        );
    }
}
