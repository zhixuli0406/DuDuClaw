use super::*;
use rusqlite::{Connection, params};

#[tokio::test]
async fn authority_revision_migrates_legacy_rows_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let conn = Connection::open(dir.path().join("tasks.db")).unwrap();
        conn.execute_batch("CREATE TABLE tasks (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'todo', priority TEXT NOT NULL DEFAULT 'medium',
                assigned_to TEXT NOT NULL, created_by TEXT NOT NULL DEFAULT 'system',
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, completed_at TEXT,
                blocked_reason TEXT, parent_task_id TEXT, tags TEXT NOT NULL DEFAULT '', message_id TEXT
             );
             INSERT INTO tasks (id,title,assigned_to,created_at,updated_at)
             VALUES ('legacy','Legacy goal','alice','2026-10-04T00:00:00Z','2026-10-04T00:00:00Z');" ,)
            .unwrap();
    }
    let store = TaskStore::open(dir.path()).unwrap();
    let original = store.authority_snapshot("legacy").await.unwrap().unwrap();
    assert_eq!(original.revision, 1);
    store
        .update_task("legacy", &serde_json::json!({"title":"Changed goal"}))
        .await
        .unwrap();
    let edited = store.authority_snapshot("legacy").await.unwrap().unwrap();
    assert_eq!(edited.revision, 2);
    assert_ne!(original.hash, edited.hash);
    drop(store);
    let reopened = TaskStore::open(dir.path()).unwrap();
    assert_eq!(
        edited,
        reopened
            .authority_snapshot("legacy")
            .await
            .unwrap()
            .unwrap()
    );
    assert!(
        reopened
            .authority_snapshot("missing")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn authority_trigger_covers_direct_writers_and_rolls_back_with_the_edit() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("atomic")).await.unwrap();
    let original = store.authority_snapshot("atomic").await.unwrap().unwrap();
    {
        let mut conn = store.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "UPDATE tasks SET title='Changed',description='Changed' WHERE id='atomic'",
            [],
        )
        .unwrap();
        let revision: i64 = tx
            .query_row(
                "SELECT authority_revision FROM tasks WHERE id='atomic'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            revision,
            original.revision + 1,
            "one edit increments once even with several changed fields"
        );
        tx.rollback().unwrap();
    }
    assert_eq!(
        original,
        store.authority_snapshot("atomic").await.unwrap().unwrap()
    );
    let conn = Connection::open(store.db_path.clone()).unwrap();
    conn.execute("UPDATE tasks SET title='Changed' WHERE id='atomic'", [])
        .unwrap();
    conn.execute(
        "UPDATE tasks SET title=?1 WHERE id='atomic'",
        params![pending_task("atomic").title],
    )
    .unwrap();
    let reverted = store.authority_snapshot("atomic").await.unwrap().unwrap();
    assert_eq!(reverted.revision, original.revision + 2);
    assert_ne!(
        original.hash, reverted.hash,
        "reverting text cannot revive an old approval"
    );
}

#[tokio::test]
async fn every_authority_field_invalidates_a_bound_snapshot() {
    let (store, _dir) = temp_store();
    let changes = [
        ("id", "'renamed'"),
        ("title", "'New title'"),
        ("description", "'New instruction'"),
        ("assigned_to", "'bob'"),
        ("created_by", "'operator'"),
        ("created_at", "'2026-10-05T00:00:00Z'"),
        ("parent_task_id", "'parent'"),
        ("goal_id", "'goal'"),
        ("tags", "'grant:Bash'"),
        ("depends_on", "'[\"dependency\"]'"),
        ("max_retries", "9"),
        ("goal_mode", "1"),
        ("acceptance_criteria", "'New criteria'"),
        ("acceptance_criteria_baseline", "'Frozen criteria'"),
        ("deadline_at", "'2099-01-01T00:00:00Z'"),
        ("risk_boundary", "'Read only'"),
        ("plan_pending", "'New plan'"),
        ("team_spec_json", "'{}'"),
        ("kind", "'goal'"),
        ("source_channel", "'telegram'"),
        ("source_chat_id", "'other-chat'"),
        ("source_discord_guild_id", "'other-guild'"),
        ("discovery_spec_json", "'{}'"),
        ("discovery_run_id", "'run'"),
        ("discovery_approval_id", "'approval'"),
        ("archived", "1"),
        ("retry_count", "1"),
        ("revision_round", "1"),
    ];
    for (index, (field, value)) in changes.into_iter().enumerate() {
        let id = format!("field-{index}");
        store.insert_task(&pending_task(&id)).await.unwrap();
        let original = store.authority_snapshot(&id).await.unwrap().unwrap();
        let conn = store.conn.lock().await;
        // Both SQL fragments are test-owned constants, never caller input.
        conn.execute(
            &format!("UPDATE tasks SET {field}={value} WHERE id=?1"),
            params![id],
        )
        .unwrap();
        let lookup = if field == "id" { "renamed" } else { &id };
        let revision: i64 = conn
            .query_row(
                "SELECT authority_revision FROM tasks WHERE id=?1",
                params![lookup],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            revision,
            original.revision + 1,
            "{field} must invalidate a previous approval"
        );
    }
}

#[tokio::test]
async fn progress_and_normal_claim_do_not_invalidate_authority() {
    let (store, _dir) = temp_store();
    let mut task = pending_task("progress");
    task.assigned_to = "alice".into();
    store.insert_task(&task).await.unwrap();
    let original = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(
        store
            .atomic_claim(
                &task.id,
                "alice",
                "2026-10-04T00:00:00Z",
                "2026-10-04T00:01:00Z"
            )
            .await
            .unwrap(),
        ClaimOutcome::Claimed
    );
    store
        .renew_lease(
            &task.id,
            "alice",
            "2026-10-04T00:02:00Z",
            "2026-10-04T00:00:30Z",
        )
        .await
        .unwrap();
    {
        let conn = store.conn.lock().await;
        conn.execute(
            "UPDATE tasks SET priority='urgent',pinned=1,updated_at='new',agent_seconds=9,
            result_summary='Progress',goal_state_json='{}',criteria_ledger='{}',status='review' WHERE id=?1",
            params![task.id]
        )
        .unwrap();
    }
    let progress = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(progress.revision, original.revision);
    assert_eq!(progress.hash, original.hash);
    assert_eq!(progress.status, "review");
    assert!(progress.eligible);
}

#[tokio::test]
async fn lifecycle_boundaries_invalidate_while_parked_tasks_can_ask_new_questions() {
    let (store, _dir) = temp_store();
    let mut task = pending_task("lifecycle");
    task.assigned_to = "alice".into();
    task.status = "in_progress".into();
    task.claimed_by = Some("alice".into());
    store.insert_task(&task).await.unwrap();
    let original = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    store
        .mark_needs_human(&task.id, "Need an answer")
        .await
        .unwrap();
    let parked = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(parked.revision, original.revision + 1);
    assert!(
        parked.eligible,
        "a fresh question may be bound after entering needs_human"
    );
    store
        .resolve_needs_human(&task.id, "retry", "New instructions")
        .await
        .unwrap();
    let retried = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(retried.revision, parked.revision + 1);
    store.cancel_task(&task.id, "Cancel").await.unwrap();
    let cancelled = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(cancelled.revision, retried.revision + 1);
    assert!(!cancelled.eligible);
}

#[tokio::test]
async fn reassignment_and_owner_release_invalidate_previous_bindings() {
    let (store, _dir) = temp_store();
    let mut task = pending_task("owner");
    task.status = "in_progress".into();
    task.assigned_to = "alice".into();
    task.claimed_by = Some("alice".into());
    store.insert_task(&task).await.unwrap();
    assert_eq!(
        store
            .reassign_open_tasks("alice", "bob", "now")
            .await
            .unwrap(),
        1
    );
    let reassigned = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(
        reassigned.revision, 2,
        "owner and assignee changed in one statement increment once"
    );
    let conn = store.conn.lock().await;
    conn.execute(
        "UPDATE tasks SET claimed_by=NULL WHERE id=?1",
        params![task.id],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        store
            .authority_snapshot(&task.id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        3
    );
}

#[tokio::test]
async fn requeue_and_continue_start_a_new_authority_epoch() {
    let (store, _dir) = temp_store();
    let mut task = pending_task("new-attempt");
    task.status = "in_progress".into();
    task.goal_mode = true;
    store.insert_task(&task).await.unwrap();
    store
        .update_task(&task.id, &serde_json::json!({"status":"pending"}))
        .await
        .unwrap();
    assert_eq!(
        store
            .authority_snapshot(&task.id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    store
        .update_task(&task.id, &serde_json::json!({"status":"done"}))
        .await
        .unwrap();
    assert!(
        store
            .continue_from_terminal(&task.id, "Continue with the next part")
            .await
            .unwrap()
    );
    let continued = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(continued.revision, 4);
    assert!(continued.eligible);
    store
        .update_task(&task.id, &serde_json::json!({"status":"unknown"}))
        .await
        .unwrap();
    store
        .update_task(&task.id, &serde_json::json!({"status":"pending"}))
        .await
        .unwrap();
    assert_eq!(
        store
            .authority_snapshot(&task.id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        6,
        "an unknown-state round trip cannot revive the previous epoch"
    );
}

#[test]
fn authority_hash_is_stable_and_invalid_task_states_fail_closed() {
    let mut task = pending_task("unicode");
    task.title = "寄送確認 🐾".into();
    assert_eq!(task.authority_revision, 1);
    assert_eq!(
        task.authority_snapshot_hash(),
        super::super::task_snapshot_hash(&task)
    );
    let expected = task.authority_snapshot_hash();
    task.description = "Send after approval".into();
    assert_ne!(expected, task.authority_snapshot_hash());
    for status in [
        "todo",
        "pending",
        "in_progress",
        "review",
        "revising",
        "needs_human",
        "blocked",
        "queued",
        "pending_approval",
    ] {
        task.status = status.into();
        assert!(task.approval_eligible(), "{status}");
    }
    for status in ["done", "failed", "cancelled", "completed", "unknown", ""] {
        task.status = status.into();
        assert!(!task.approval_eligible(), "{status}");
    }
    task.status = "pending".into();
    task.archived = true;
    assert!(!task.approval_eligible());
    task.archived = false;
    task.authority_revision = 0;
    assert!(!task.approval_eligible());
    task.authority_revision = 1;
    task.deadline_at = Some("invalid".into());
    assert!(!task.approval_eligible());
    task.deadline_at = Some("2000-01-01T00:00:00Z".into());
    assert!(!task.approval_eligible());
}

#[test]
fn legacy_task_json_defaults_to_the_first_authority_revision() {
    let mut json = serde_json::to_value(pending_task("legacy-json")).unwrap();
    json.as_object_mut().unwrap().remove("authority_revision");
    let task: TaskRow = serde_json::from_value(json).unwrap();
    assert_eq!(task.authority_revision, 1);
}

#[tokio::test]
async fn deleted_task_epoch_survives_identical_recreation_purge_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::open(dir.path()).unwrap();
    let task = pending_task("same-id");
    store.insert_task(&task).await.unwrap();
    let first = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert!(store.remove_task(&task.id).await.unwrap());
    drop(store);
    let store = TaskStore::open(dir.path()).unwrap();
    store.insert_task(&task).await.unwrap();
    let second = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(second.revision, first.revision + 1);
    assert_ne!(second.hash, first.hash);
    {
        let conn = store.conn.lock().await;
        assert!(
            conn.execute("DELETE FROM task_authority_epochs", [])
                .is_err()
        );
        assert!(
            conn.execute(
                "UPDATE task_authority_epochs SET revision_floor=1 WHERE task_id=?1",
                params![task.id]
            )
            .is_err()
        );
        assert!(
            conn.execute(
                "UPDATE tasks SET authority_revision=1 WHERE id=?1",
                params![task.id]
            )
            .is_err()
        );
        // Bulk task cleanup does not erase authority tombstones.
        conn.execute("DELETE FROM tasks", []).unwrap();
    }
    drop(store);
    let store = TaskStore::open(dir.path()).unwrap();
    store.insert_task(&task).await.unwrap();
    let third = store.authority_snapshot(&task.id).await.unwrap().unwrap();
    assert_eq!(third.revision, second.revision + 1);
    assert_ne!(third.hash, second.hash);
    assert_eq!(
        store.authority_snapshot(&task.id).await.unwrap().unwrap(),
        third
    );
}

#[tokio::test]
async fn rename_preserves_both_identity_tombstones_and_transaction_rollback() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("old-id")).await.unwrap();
    let old = store.authority_snapshot("old-id").await.unwrap().unwrap();
    {
        let mut conn = store.conn.lock().await;
        let tx = conn.transaction().unwrap();
        tx.execute("DELETE FROM tasks WHERE id='old-id'", [])
            .unwrap();
        tx.rollback().unwrap();
    }
    assert_eq!(
        store.authority_snapshot("old-id").await.unwrap().unwrap(),
        old
    );
    store
        .conn
        .lock()
        .await
        .execute("UPDATE tasks SET id='new-id' WHERE id='old-id'", [])
        .unwrap();
    store.insert_task(&pending_task("old-id")).await.unwrap();
    assert_eq!(
        store
            .authority_snapshot("old-id")
            .await
            .unwrap()
            .unwrap()
            .revision,
        old.revision + 1
    );
    assert!(store.remove_task("new-id").await.unwrap());
    store
        .conn
        .lock()
        .await
        .execute("UPDATE tasks SET id='new-id' WHERE id='old-id'", [])
        .unwrap();
    assert!(
        store
            .authority_snapshot("new-id")
            .await
            .unwrap()
            .unwrap()
            .revision
            > old.revision + 1
    );
}
