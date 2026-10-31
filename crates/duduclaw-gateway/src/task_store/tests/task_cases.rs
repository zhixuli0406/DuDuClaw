//! Unit tests for [`super`], moved verbatim out of the former `task_store.rs` — task cases.

use super::*;

// ── W2-7: source_discord_guild_id column round trip ──────

#[tokio::test]
async fn source_discord_guild_id_round_trips_through_insert_and_get() {
    let (store, _dir) = temp_store();
    let mut task = TaskRow::new(
        "t-discord".into(),
        "Goal from Discord".into(),
        String::new(),
        "medium".into(),
        "bot".into(),
        "goal:discord".into(),
    );
    task.source_channel = Some("discord".into());
    task.source_chat_id = Some("chan-1".into());
    task.source_discord_guild_id = Some("guild-1".into());
    store.insert_task(&task).await.expect("insert task");

    let got = store
        .get_task("t-discord")
        .await
        .expect("get task")
        .expect("row exists");
    assert_eq!(got.source_discord_guild_id.as_deref(), Some("guild-1"));

    let listed = store
        .list_tasks(None, None, None)
        .await
        .expect("list tasks");
    let row = listed
        .iter()
        .find(|t| t.id == "t-discord")
        .expect("row in list");
    assert_eq!(row.source_discord_guild_id.as_deref(), Some("guild-1"));
}

#[tokio::test]
async fn source_discord_guild_id_defaults_to_none() {
    let (store, _dir) = temp_store();
    // A non-Discord (or Discord-but-unknown-guild) task never fabricates
    // a value — the column stays NULL / None.
    let task = TaskRow::new(
        "t-telegram".into(),
        "Goal from Telegram".into(),
        String::new(),
        "medium".into(),
        "bot".into(),
        "goal:telegram".into(),
    );
    store.insert_task(&task).await.expect("insert task");
    let got = store
        .get_task("t-telegram")
        .await
        .expect("get task")
        .expect("row exists");
    assert_eq!(got.source_discord_guild_id, None);
}

// ── I-3b: archived/pinned task list operations ───────────

#[tokio::test]
async fn archived_and_pinned_default_to_false_on_insert() {
    let (store, _dir) = temp_store();
    let task = TaskRow::new(
        "t-defaults".into(),
        "Fresh task".into(),
        String::new(),
        "medium".into(),
        "bot".into(),
        "user-1".into(),
    );
    store.insert_task(&task).await.expect("insert task");
    let got = store
        .get_task("t-defaults")
        .await
        .expect("get task")
        .expect("row exists");
    assert!(!got.archived, "archived must default to false");
    assert!(!got.pinned, "pinned must default to false");
}

/// Idempotent migration: opening the same on-disk `tasks.db` a second
/// time (simulating a gateway restart against a pre-existing store)
/// must not error and must leave a pre-existing row's archived/pinned
/// state untouched — same contract as every other `add_dispatch_columns`
/// migration.
#[tokio::test]
async fn archived_pinned_migration_is_idempotent_across_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let store = TaskStore::open(dir.path()).expect("open store");
        let task = TaskRow::new(
            "t-migrate".into(),
            "Pre-migration task".into(),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert task");
    }
    // Reopen — add_dispatch_columns runs again; ALTER TABLE ADD COLUMN
    // must be a no-op the second time, not an error.
    let store = TaskStore::open(dir.path()).expect("reopen store");
    let got = store
        .get_task("t-migrate")
        .await
        .expect("get task")
        .expect("row exists");
    assert!(!got.archived);
    assert!(!got.pinned);
}

#[tokio::test]
async fn update_task_sets_archived_and_pinned_booleans() {
    let (store, _dir) = temp_store();
    let task = TaskRow::new(
        "t-toggle".into(),
        "Toggle me".into(),
        String::new(),
        "medium".into(),
        "bot".into(),
        "user-1".into(),
    );
    store.insert_task(&task).await.expect("insert task");

    store
        .update_task("t-toggle", &serde_json::json!({ "archived": true }))
        .await
        .expect("archive");
    let got = store.get_task("t-toggle").await.unwrap().unwrap();
    assert!(got.archived);
    assert!(
        !got.pinned,
        "pinning must be untouched by an archive-only update"
    );

    store
        .update_task("t-toggle", &serde_json::json!({ "pinned": true }))
        .await
        .expect("pin");
    let got = store.get_task("t-toggle").await.unwrap().unwrap();
    assert!(
        got.archived,
        "archiving must be untouched by a pin-only update"
    );
    assert!(got.pinned);

    store
        .update_task(
            "t-toggle",
            &serde_json::json!({ "archived": false, "pinned": false }),
        )
        .await
        .expect("unarchive+unpin");
    let got = store.get_task("t-toggle").await.unwrap().unwrap();
    assert!(!got.archived);
    assert!(!got.pinned);
}

#[tokio::test]
async fn list_tasks_filtered_excludes_archived_by_default() {
    let (store, _dir) = temp_store();
    for id in ["visible-1", "visible-2", "archived-1"] {
        let task = TaskRow::new(
            id.into(),
            format!("Task {id}"),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert");
    }
    store
        .update_task("archived-1", &serde_json::json!({ "archived": true }))
        .await
        .expect("archive");

    let listed = store.list_tasks(None, None, None).await.expect("list");
    let ids: Vec<&str> = listed.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&"visible-1"));
    assert!(ids.contains(&"visible-2"));
    assert!(
        !ids.contains(&"archived-1"),
        "archived task must be hidden from the default list: {ids:?}"
    );

    // list_tasks_filtered shares the same default.
    let filtered = store
        .list_tasks_filtered(None, None, None, None)
        .await
        .expect("list filtered");
    assert!(!filtered.iter().any(|t| t.id == "archived-1"));
}

#[tokio::test]
async fn list_tasks_paginated_can_browse_the_archive_explicitly() {
    let (store, _dir) = temp_store();
    for id in ["p-1", "p-2"] {
        let task = TaskRow::new(
            id.into(),
            format!("Task {id}"),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert");
    }
    store
        .update_task("p-2", &serde_json::json!({ "archived": true }))
        .await
        .expect("archive p-2");

    // Default (archived=None) excludes the archived row.
    let (rows, total) = store
        .list_tasks_paginated(None, None, None, None, None, 50, 0)
        .await
        .expect("paginated default");
    assert_eq!(total, 1, "only p-1 is non-archived");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "p-1");

    // Explicit archived=true surfaces only the archived row.
    let (rows, total) = store
        .list_tasks_paginated(None, None, None, None, Some(true), 50, 0)
        .await
        .expect("paginated archived-only");
    assert_eq!(total, 1);
    assert_eq!(rows[0].id, "p-2");
}

#[tokio::test]
async fn list_tasks_paginated_reports_total_across_pages() {
    let (store, _dir) = temp_store();
    for i in 0..5 {
        let task = TaskRow::new(
            format!("page-{i}"),
            format!("Task {i}"),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert");
    }
    let (first_page, total) = store
        .list_tasks_paginated(None, None, None, None, None, 2, 0)
        .await
        .expect("page 1");
    assert_eq!(
        total, 5,
        "total reflects the whole filtered set, not just this page"
    );
    assert_eq!(first_page.len(), 2);

    let (second_page, total2) = store
        .list_tasks_paginated(None, None, None, None, None, 2, 2)
        .await
        .expect("page 2");
    assert_eq!(total2, 5);
    assert_eq!(second_page.len(), 2);

    // Pages don't overlap.
    let first_ids: HashSet<&str> = first_page.iter().map(|t| t.id.as_str()).collect();
    let second_ids: HashSet<&str> = second_page.iter().map(|t| t.id.as_str()).collect();
    assert!(first_ids.is_disjoint(&second_ids));
}

#[tokio::test]
async fn pinned_tasks_sort_first() {
    let (store, _dir) = temp_store();
    for id in ["older", "newer"] {
        let task = TaskRow::new(
            id.into(),
            format!("Task {id}"),
            String::new(),
            "medium".into(),
            "bot".into(),
            "user-1".into(),
        );
        store.insert_task(&task).await.expect("insert");
    }
    // "newer" would naturally sort first (updated_at DESC on insert
    // order in a fresh store with monotonic timestamps isn't
    // guaranteed within the same tick, so pin "older" explicitly to
    // prove the ORDER BY clause, not insertion order, decides this).
    store
        .update_task("older", &serde_json::json!({ "pinned": true }))
        .await
        .expect("pin older");

    let listed = store.list_tasks(None, None, None).await.expect("list");
    assert_eq!(
        listed[0].id, "older",
        "pinned task must sort first regardless of recency"
    );
}

#[tokio::test]
async fn rename_via_update_task_title_field() {
    let (store, _dir) = temp_store();
    let task = TaskRow::new(
        "t-rename".into(),
        "Original title".into(),
        String::new(),
        "medium".into(),
        "bot".into(),
        "user-1".into(),
    );
    store.insert_task(&task).await.expect("insert");

    let updated = store
        .update_task("t-rename", &serde_json::json!({ "title": "Renamed title" }))
        .await
        .expect("rename")
        .expect("row exists");
    assert_eq!(updated.title, "Renamed title");

    let got = store.get_task("t-rename").await.unwrap().unwrap();
    assert_eq!(got.title, "Renamed title");
}

#[tokio::test]
async fn comment_insert_and_list_roundtrip_is_chronological() {
    let (store, _dir) = temp_store();
    // Seed a task so the comment references a real row.
    let task = TaskRow::new(
        "t1".into(),
        "Task One".into(),
        String::new(),
        "medium".into(),
        "bot".into(),
        "user-1".into(),
    );
    store.insert_task(&task).await.expect("insert task");

    // Insert out of chronological order; list must return oldest-first.
    store
        .insert_comment(&comment("c2", "t1", "2026-07-10T10:05:00Z", "second"))
        .await
        .expect("insert c2");
    store
        .insert_comment(&comment("c1", "t1", "2026-07-10T10:00:00Z", "first"))
        .await
        .expect("insert c1");

    let rows = store.list_comments("t1").await.expect("list");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].body, "first", "oldest comment leads");
    assert_eq!(rows[1].body, "second");
    assert_eq!(rows[0].author_user, "user-1");
}

#[tokio::test]
async fn comment_list_unknown_task_is_empty() {
    let (store, _dir) = temp_store();
    let rows = store.list_comments("does-not-exist").await.expect("list");
    assert!(rows.is_empty(), "no comments for an unknown task");
}

#[tokio::test]
async fn reassign_open_tasks_moves_only_unfinished_work() {
    let (store, _dir) = temp_store();
    // Two open tasks + one done task, all owned by alice.
    for id in ["open1", "open2", "done1"] {
        let t = TaskRow::new(
            id.into(),
            format!("Task {id}"),
            String::new(),
            "medium".into(),
            "alice".into(),
            "user-1".into(),
        );
        store.insert_task(&t).await.expect("insert");
    }
    store
        .update_task("done1", &serde_json::json!({ "status": "done" }))
        .await
        .expect("mark done");

    let moved = store
        .reassign_open_tasks("alice", "bob", "2026-07-12T00:00:00Z")
        .await
        .expect("reassign");
    assert_eq!(moved, 2, "only the two open tasks move");

    // Bob now owns the open tasks; alice keeps the completed one.
    let bob = store.list_tasks(None, Some("bob"), None).await.unwrap();
    assert_eq!(bob.len(), 2);
    let alice = store.list_tasks(None, Some("alice"), None).await.unwrap();
    assert_eq!(alice.len(), 1, "done task stays with the original owner");
    assert_eq!(alice[0].id, "done1");

    // Idempotent: a re-run finds nothing left open for alice.
    let again = store
        .reassign_open_tasks("alice", "bob", "2026-07-12T00:01:00Z")
        .await
        .expect("reassign again");
    assert_eq!(again, 0);
}

#[test]
fn self_parent_is_cycle() {
    assert!(introduces_parent_cycle(&[], "a", "a"));
}

#[test]
fn simple_acyclic_is_safe() {
    // a -> b -> c (root). Adding d's parent = a is safe.
    let e = edges(&[("a", Some("b")), ("b", Some("c")), ("c", None)]);
    assert!(!introduces_parent_cycle(&e, "d", "a"));
}

#[test]
fn direct_back_edge_is_cycle() {
    // b's parent is a. Setting a's parent = b closes a 2-cycle.
    let e = edges(&[("b", Some("a")), ("a", None)]);
    assert!(introduces_parent_cycle(&e, "a", "b"));
}

// Written for F1; not run (the review task prohibits builds/tests).
#[tokio::test]
async fn derived_task_sinks_mask_text_and_preserve_json() {
    let (store, _dir) = temp_store();
    let mut task = pending_task("credential-task");
    task.title = "password: fixture-title".into();
    task.description = "password: fixture-description".into();
    task.acceptance_criteria = Some(r#"[{"text":"password: fixture-criteria"}]"#.into());
    store.insert_task(&task).await.unwrap();
    let row = store.get_task(&task.id).await.unwrap().unwrap();
    assert!(!serde_json::to_string(&row).unwrap().contains("fixture-"));
    assert!(serde_json::from_str::<serde_json::Value>(row.acceptance_criteria.as_deref().unwrap()).is_ok());
    let changed = store.update_task(&task.id, &serde_json::json!({
        "title":"password: fixture-updated", "description":"password: fixture-body",
        "blocked_reason":"password: fixture-blocked", "tags":"password: fixture-tag",
    })).await.unwrap().unwrap();
    assert!(!serde_json::to_string(&changed).unwrap().contains("fixture-"));
    store.set_goal_state_json(&task.id, Some(r#"{"password":"fixture-state"}"#)).await.unwrap();
    store.merge_goal_state_json(&task.id, |v| v["note"] = serde_json::json!("password: fixture-merged")).await.unwrap();
    let row = store.get_task(&task.id).await.unwrap().unwrap();
    let state = row.goal_state_json.unwrap();
    assert!(!state.contains("fixture-"));
    assert!(serde_json::from_str::<serde_json::Value>(&state).is_ok());
    store.complete_task(&task.id, "password: fixture-result", "worker").await.unwrap();
    let row = store.get_task(&task.id).await.unwrap().unwrap();
    assert!(!row.result_summary.unwrap().contains("fixture-result"));
}

// Written for F1; not run (the review task prohibits builds/tests).
#[tokio::test]
async fn task_activity_comments_plans_and_goals_mask_at_write_boundary() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("credential-task")).await.unwrap();
    store.append_activity(&ActivityRow {
        id: "activity".into(), event_type: "test".into(), agent_id: "worker".into(),
        task_id: Some("credential-task".into()), summary: "password: fixture-summary".into(),
        timestamp: "2026-10-30T00:00:00Z".into(),
        metadata: Some(r#"{"password":"fixture-metadata"}"#.into()),
    }).await.unwrap();
    let activity = store.list_activity_for_task("credential-task", 10).await.unwrap();
    assert!(!activity[0].summary.contains("fixture-summary"));
    assert!(!activity[0].metadata.as_deref().unwrap().contains("fixture-metadata"));
    assert!(serde_json::from_str::<serde_json::Value>(activity[0].metadata.as_deref().unwrap()).is_ok());
    store.insert_comment(&comment("comment", "credential-task", "2026-10-30T00:00:00Z", "password: fixture-comment")).await.unwrap();
    assert!(!store.list_comments("credential-task").await.unwrap()[0].body.contains("fixture-comment"));
    let mut p = plan("credential-plan", "worker");
    p.title = "password: fixture-plan".into();
    p.description = "password: fixture-planbody".into();
    store.insert_plan(&p).await.unwrap();
    assert!(!store.get_plan(&p.id).await.unwrap().unwrap().title.contains("fixture-plan"));
    let step = store.add_plan_step(&p.id, "step", "password: fixture-step", "agent", "worker", None).await.unwrap();
    assert!(!step.text.contains("fixture-step"));
    let step = store.update_plan_step("step", &serde_json::json!({"text":"password: fixture-next"})).await.unwrap().unwrap();
    assert!(!step.text.contains("fixture-next"));
    let g = goal("credential-goal", "password: fixture-goal", None);
    store.insert_goal(&g).await.unwrap();
    assert!(!store.get_goal(&g.id).await.unwrap().unwrap().title.contains("fixture-goal"));
    let g = store.update_goal(&g.id, &serde_json::json!({"description":"password: fixture-goalbody"})).await.unwrap().unwrap();
    assert!(!g.description.contains("fixture-goalbody"));
}

// Written for F1; not run (the review task prohibits builds/tests).
#[tokio::test]
async fn frozen_task_contract_with_credentials_is_refused_without_rewriting_authority() {
    let (store, _dir) = temp_store();
    let mut task = pending_task("credential-frozen");
    task.acceptance_criteria_baseline = Some("password: fixture-baseline".into());
    assert!(store.insert_task(&task).await.is_err());
    assert!(store.get_task(&task.id).await.unwrap().is_none());
    task.acceptance_criteria_baseline = Some("Safe baseline".into());
    store.insert_task(&task).await.unwrap();
    assert!(store.freeze_team_spec(&task.id, r#"{"prompt":"password: fixture-team"}"#).await.is_err());
    assert!(store.set_criteria_ledger(&task.id, r#"{"password":"fixture-ledger"}"#).await.is_err());
    assert_eq!(store.get_task(&task.id).await.unwrap().unwrap().acceptance_criteria_baseline, task.acceptance_criteria_baseline);
}

// Written for F1; not run (the review task prohibits builds/tests).
#[tokio::test]
async fn task_review_sinks_mask_feedback_and_structured_verdicts() {
    let (store, _dir) = temp_store();
    let task = goal_review_task("credential-review");
    store.insert_task(&task).await.unwrap();
    store.record_iteration_dispatch(&task.id, 1, "2026-10-30T00:00:00Z").await.unwrap();
    store.reject_review_with_verdict(&task.id, "password: fixture-feedback", 3,
        Some(r#"[{"name":"safety","pass":false,"reason":"password: fixture-verdict"}]"#)).await.unwrap();
    let row = store.get_task(&task.id).await.unwrap().unwrap();
    assert!(!row.judge_feedback.as_deref().unwrap().contains("fixture-feedback"));
    let iterations = store.list_iterations(&task.id).await.unwrap();
    assert!(!iterations[0].judge_feedback.as_deref().unwrap().contains("fixture-feedback"));
    let verdict = iterations[0].verdict_json.as_deref().unwrap();
    assert!(!verdict.contains("fixture-verdict"));
    assert!(serde_json::from_str::<serde_json::Value>(verdict).is_ok());
}

#[tokio::test]
async fn numeric_uuid_criteria_ledger_is_preserved_without_weakening_secret_refusal() {
    let (store, dir) = temp_store();
    let mut task = pending_task("12345678-abcd-4abc-abcd-123456789abc");
    task.acceptance_criteria_baseline = Some("Produce a summary".into());
    task.criteria_ledger = crate::goal_loop::criteria_ledger::ledger_for_new_goal(
        dir.path(), &task.id, task.acceptance_criteria_baseline.as_deref());
    assert!(task.criteria_ledger.is_some());
    store.insert_task(&task).await.unwrap();
    let stored = store.get_task(&task.id).await.unwrap().unwrap();
    assert_eq!(stored.criteria_ledger, task.criteria_ledger);
    assert!(store.set_criteria_ledger(&task.id, r#"{"text":"password: fixture-secret"}"#).await.is_err());
    assert_eq!(store.get_task(&task.id).await.unwrap().unwrap().criteria_ledger, task.criteria_ledger);
}
