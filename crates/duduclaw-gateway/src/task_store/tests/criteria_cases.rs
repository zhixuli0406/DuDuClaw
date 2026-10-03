//! WP-G2: the `criteria_ledger` task column and the per-round
//! `task_iterations.criteria_ledger_json` snapshot.

use super::*;

#[tokio::test]
async fn criteria_ledger_migrates_an_old_database_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    // A tasks.db from before every additive column (the original v1 shape).
    {
        let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (
                 id TEXT PRIMARY KEY, title TEXT NOT NULL,
                 description TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'todo',
                 priority TEXT NOT NULL DEFAULT 'medium', assigned_to TEXT NOT NULL,
                 created_by TEXT NOT NULL DEFAULT 'system', created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL, completed_at TEXT, blocked_reason TEXT,
                 parent_task_id TEXT, tags TEXT NOT NULL DEFAULT '', message_id TEXT
             );
             INSERT INTO tasks (id, title, assigned_to, created_at, updated_at)
             VALUES ('old', 'legacy', 'a', '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z');
             CREATE TABLE task_iterations (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL,
                 round INTEGER NOT NULL, dispatched_at TEXT NOT NULL, submitted_at TEXT,
                 judged_at TEXT, verdict TEXT, judge_feedback TEXT, feedback_class TEXT
             );
             INSERT INTO task_iterations (task_id, round, dispatched_at)
             VALUES ('old', 1, '2026-08-01T00:00:00Z');",
        )
        .unwrap();
    }
    let store = TaskStore::open(dir.path()).expect("old db must open");
    let _again = TaskStore::open(dir.path()).expect("migration is idempotent");
    let old = store.get_task("old").await.unwrap().unwrap();
    assert!(
        old.criteria_ledger.is_none(),
        "old rows read back as no ledger"
    );
    assert_eq!(
        store.iteration_criteria_snapshots("old").await.unwrap(),
        vec![(1, None)]
    );

    // The open round gets the snapshot; the task column gets the latest.
    store.set_criteria_ledger("old", "{\"v\":1}").await.unwrap();
    assert_eq!(
        store
            .get_task("old")
            .await
            .unwrap()
            .unwrap()
            .criteria_ledger
            .as_deref(),
        Some("{\"v\":1}")
    );
    assert_eq!(
        store.iteration_criteria_snapshots("old").await.unwrap(),
        vec![(1, Some("{\"v\":1}".to_string()))]
    );
}

#[tokio::test]
async fn criteria_ledger_insert_round_trip_and_sealed_rounds_are_not_rewritten() {
    let (store, _dir) = temp_store();
    let mut t = pending_task("g1");
    t.criteria_ledger = Some("{\"seed\":true}".into());
    store.insert_task(&t).await.unwrap();
    assert_eq!(
        store
            .get_task("g1")
            .await
            .unwrap()
            .unwrap()
            .criteria_ledger
            .as_deref(),
        Some("{\"seed\":true}")
    );
    // No open round ⇒ only the task column moves.
    store.set_criteria_ledger("g1", "{\"v\":2}").await.unwrap();
    assert!(
        store
            .iteration_criteria_snapshots("g1")
            .await
            .unwrap()
            .is_empty()
    );
    // `TaskRow` serialization never carries the raw column.
    let json = serde_json::to_value(store.get_task("g1").await.unwrap().unwrap()).unwrap();
    assert!(json.get("criteria_ledger").is_none());
}

#[tokio::test]
async fn rewrite_result_summary_is_compare_and_set() {
    let (store, _dir) = temp_store();
    let mut t = pending_task("g1");
    t.result_summary = Some("raw <criteria_status>[]</criteria_status>".into());
    store.insert_task(&t).await.unwrap();
    assert!(!store.rewrite_result_summary("g1", "something else", "x").await.unwrap());
    assert!(store
        .rewrite_result_summary("g1", "raw <criteria_status>[]</criteria_status>", "raw")
        .await
        .unwrap());
    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().result_summary.as_deref(),
        Some("raw")
    );
}
