//! Unit tests for [`super`], moved verbatim out of the former `task_store.rs` — claim cases.

use super::*;

fn discovery_fixture(id: &str) -> TaskRow {
    let mut value = serde_json::to_value(pending_task(id)).unwrap();
    value["kind"] = serde_json::json!("discovery");
    value["discovery_run_id"] = serde_json::json!(format!("discovery-run-{id}"));
    value["discovery_spec_json"] = serde_json::json!("{\"approved_root_id\":\"root-fixture\",\"evaluator\":\"score-fixture\"}");
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn discovery_task_kind_and_frozen_spec_survive_real_store_roundtrip() {
    let (store, _directory) = temp_store();
    store.insert_task(&discovery_fixture("discovery-roundtrip")).await.unwrap();
    let row=store.get_task("discovery-roundtrip").await.unwrap().unwrap();
    assert_eq!(row.discovery_spec_json.as_deref(),Some("{\"approved_root_id\":\"root-fixture\",\"evaluator\":\"score-fixture\"}"));
    let value = serde_json::to_value(row).unwrap();
    assert_eq!(value["kind"], "discovery");
    assert_eq!(value["discovery_run_id"], "discovery-run-discovery-roundtrip");
    assert!(value.get("discovery_spec_json").is_none());
}

#[tokio::test]
async fn discovery_task_cannot_enter_any_normal_worker_claim_or_zombie_path() {
    let (store, _directory) = temp_store();
    store.insert_task(&discovery_fixture("discovery-pending")).await.unwrap();
    assert!(store.claimable_tasks().await.unwrap().iter().all(|row| row.id != "discovery-pending"));
    assert_eq!(store.atomic_claim("discovery-pending", "ordinary-worker",
        "2026-09-30T10:00:00Z", "2026-09-30T10:05:00Z").await.unwrap(), ClaimOutcome::NotClaimable);
    let mut running = discovery_fixture("discovery-running");
    running.status = "in_progress".into();
    running.claimed_by = Some("discovery-runner".into());
    running.claimed_at = Some("2026-09-30T09:00:00Z".into());
    running.lease_renewed_at = running.claimed_at.clone();
    running.lease_expires_at = Some("2026-09-30T09:05:00Z".into());
    store.insert_task(&running).await.unwrap();
    assert!(store.reclaim_zombies("2026-09-30T10:00:00Z").await.unwrap().iter()
        .all(|outcome| outcome.task_id != "discovery-running"));
    assert_eq!(store.get_task("discovery-running").await.unwrap().unwrap().status, "in_progress");
}

#[tokio::test]
async fn legacy_goal_rows_receive_goal_kind_without_changing_their_worker_contract() {
    let (store, _directory) = temp_store();
    let mut goal = pending_task("legacy-kind-goal");
    goal.goal_mode = true;
    store.insert_task(&goal).await.unwrap();
    assert_eq!(serde_json::to_value(store.get_task("legacy-kind-goal").await.unwrap().unwrap()).unwrap()["kind"], "goal");
    assert_eq!(store.atomic_claim("legacy-kind-goal", "ordinary-worker",
        "2026-09-30T10:00:00Z", "2026-09-30T10:05:00Z").await.unwrap(), ClaimOutcome::Claimed);
}

#[test]
fn deep_back_edge_is_cycle() {
    // a -> b -> c. Setting c's parent = a closes a 3-cycle.
    let e = edges(&[("a", Some("b")), ("b", Some("c")), ("c", None)]);
    assert!(introduces_parent_cycle(&e, "c", "a"));
}

#[test]
fn unrelated_parent_is_safe() {
    let e = edges(&[("a", None), ("b", None), ("c", None)]);
    assert!(!introduces_parent_cycle(&e, "a", "b"));
}

// ── G1 dispatch: pure helpers ───────────────────────────

#[test]
fn parse_depends_on_handles_valid_and_malformed() {
    assert_eq!(parse_depends_on("[]"), Vec::<String>::new());
    assert_eq!(parse_depends_on(r#"["a","b"]"#), vec!["a", "b"]);
    // Malformed / non-array ⇒ empty (no deps), never a panic.
    assert!(parse_depends_on("not json").is_empty());
    assert!(parse_depends_on("{}").is_empty());
}

#[test]
fn deps_satisfied_semantics() {
    let done: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
    assert!(deps_satisfied(&[], &done), "no deps ⇒ satisfied");
    assert!(deps_satisfied(&["a".into(), "b".into()], &done));
    assert!(
        !deps_satisfied(&["a".into(), "c".into()], &done),
        "c not done"
    );
}

#[test]
fn lease_expiry_compares_timestamps() {
    assert!(lease_is_expired(
        "2026-07-11T10:00:00Z",
        "2026-07-11T10:00:01Z"
    ));
    assert!(!lease_is_expired(
        "2026-07-11T10:00:05Z",
        "2026-07-11T10:00:01Z"
    ));
    // Corrupt lease ⇒ treated as expired (fail-safe toward reclaim).
    assert!(lease_is_expired("garbage", "2026-07-11T10:00:01Z"));
}

#[test]
fn zombie_action_respects_retry_budget() {
    assert_eq!(zombie_action(0, 3), ZombieAction::Requeue);
    assert_eq!(zombie_action(2, 3), ZombieAction::Requeue);
    assert_eq!(zombie_action(3, 3), ZombieAction::Fail);
    assert_eq!(zombie_action(5, 3), ZombieAction::Fail);
    assert_eq!(zombie_action(0, 0), ZombieAction::Fail, "zero budget");
}

#[tokio::test]
async fn atomic_claim_is_exclusive_under_concurrency() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(TaskStore::open(dir.path()).expect("open"));
    store
        .insert_task(&pending_task("t1"))
        .await
        .expect("insert");

    // Two workers race for the same task; exactly one may win.
    let s1 = store.clone();
    let s2 = store.clone();
    let now = "2026-07-11T10:00:00Z";
    let lease = "2026-07-11T10:05:00Z";
    let (r1, r2) = tokio::join!(
        async move {
            s1.atomic_claim("t1", "worker-a", now, lease)
                .await
                .unwrap()
                .is_claimed()
        },
        async move {
            s2.atomic_claim("t1", "worker-b", now, lease)
                .await
                .unwrap()
                .is_claimed()
        },
    );
    assert_ne!(r1, r2, "exactly one claimer wins");
    assert!(r1 ^ r2, "one true, one false");

    let t = store.get_task("t1").await.unwrap().unwrap();
    assert_eq!(t.status, "in_progress");
    assert!(matches!(
        t.claimed_by.as_deref(),
        Some("worker-a") | Some("worker-b")
    ));

    // A third claim on an already-claimed task fails.
    assert!(
        !store
            .atomic_claim("t1", "worker-c", now, lease)
            .await
            .unwrap()
            .is_claimed()
    );
}

// ── M7: merge_goal_state_json read-merge-write ──────────

#[tokio::test]
async fn merge_goal_state_json_concurrent_merges_do_not_lose_either_write() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(TaskStore::open(dir.path()).expect("open"));
    store
        .insert_task(&pending_task("g1"))
        .await
        .expect("insert");

    // Two concurrent merges, each touching a DIFFERENT key of the same
    // JSON blob — before M7 a naive read-then-`set_goal_state_json`
    // pattern would let whichever write lands second clobber the other's
    // key with a stale read.
    let s1 = store.clone();
    let s2 = store.clone();
    let (r1, r2) = tokio::join!(
        async move {
            s1.merge_goal_state_json("g1", |v| {
                v["confirmed_facts"] = serde_json::json!(["fact one"]);
            })
            .await
        },
        async move {
            s2.merge_goal_state_json("g1", |v| {
                v["pending_hypotheses"] = serde_json::json!(["hypothesis one"]);
            })
            .await
        },
    );
    r1.unwrap();
    r2.unwrap();

    let t = store.get_task("g1").await.unwrap().unwrap();
    let value: serde_json::Value =
        serde_json::from_str(t.goal_state_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        value["confirmed_facts"],
        serde_json::json!(["fact one"]),
        "first writer's key must survive"
    );
    assert_eq!(
        value["pending_hypotheses"],
        serde_json::json!(["hypothesis one"]),
        "second writer's key must survive too — neither merge may clobber the other"
    );
}

#[tokio::test]
async fn merge_goal_state_json_degrades_malformed_existing_json_to_empty_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");
    store
        .insert_task(&pending_task("g2"))
        .await
        .expect("insert");
    store
        .set_goal_state_json("g2", Some("not json at all"))
        .await
        .unwrap();

    store
        .merge_goal_state_json("g2", |v| {
            v["confirmed_facts"] = serde_json::json!(["a"]);
        })
        .await
        .unwrap();

    let t = store.get_task("g2").await.unwrap().unwrap();
    let value: serde_json::Value =
        serde_json::from_str(t.goal_state_json.as_deref().unwrap()).unwrap();
    assert_eq!(value["confirmed_facts"], serde_json::json!(["a"]));
}

#[tokio::test]
async fn merge_goal_state_json_starts_from_empty_object_when_column_is_null() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");
    store
        .insert_task(&pending_task("g3"))
        .await
        .expect("insert");
    // goal_state_json starts NULL for a freshly inserted task.
    assert_eq!(
        store.get_task("g3").await.unwrap().unwrap().goal_state_json,
        None
    );

    store
        .merge_goal_state_json("g3", |v| {
            v["pending_hypotheses"] = serde_json::json!(["h"]);
        })
        .await
        .unwrap();

    let t = store.get_task("g3").await.unwrap().unwrap();
    let value: serde_json::Value =
        serde_json::from_str(t.goal_state_json.as_deref().unwrap()).unwrap();
    assert_eq!(value["pending_hypotheses"], serde_json::json!(["h"]));
}

#[tokio::test]
async fn zombie_reclaim_requeues_then_fails_at_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");
    let mut t = pending_task("z1");
    t.max_retries = 1; // one requeue, then fail
    store.insert_task(&t).await.expect("insert");

    // Claim with a lease in the past so it's immediately a zombie.
    let past_lease = "2026-07-11T09:00:00Z";
    assert!(
        store
            .atomic_claim("z1", "w", "2026-07-11T08:55:00Z", past_lease)
            .await
            .unwrap()
            .is_claimed()
    );

    // First reclaim: retry_count 0 < 1 ⇒ requeue.
    let out = store.reclaim_zombies("2026-07-11T10:00:00Z").await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].action, ZombieAction::Requeue);
    let t1 = store.get_task("z1").await.unwrap().unwrap();
    assert_eq!(t1.status, "pending");
    assert_eq!(t1.retry_count, 1);
    assert!(t1.claimed_by.is_none() && t1.lease_expires_at.is_none());

    // Re-claim, expire again: retry_count 1 == max 1 ⇒ fail.
    assert!(
        store
            .atomic_claim("z1", "w", "2026-07-11T10:00:00Z", "2026-07-11T10:01:00Z")
            .await
            .unwrap()
            .is_claimed()
    );
    let out2 = store.reclaim_zombies("2026-07-11T11:00:00Z").await.unwrap();
    assert_eq!(out2.len(), 1);
    assert_eq!(out2[0].action, ZombieAction::Fail);
    assert_eq!(
        store.get_task("z1").await.unwrap().unwrap().status,
        "failed"
    );
}

#[tokio::test]
async fn zombie_reclaim_ignores_unexpired_and_unleased() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");

    // Fresh lease far in the future — not a zombie.
    store.insert_task(&pending_task("live")).await.unwrap();
    assert!(
        store
            .atomic_claim("live", "w", "2026-07-11T10:00:00Z", "2026-07-11T23:00:00Z")
            .await
            .unwrap()
            .is_claimed()
    );

    // Manual board task: in_progress but NULL lease — must be left alone.
    let mut manual = pending_task("manual");
    manual.status = "in_progress".into();
    store.insert_task(&manual).await.unwrap();

    let out = store.reclaim_zombies("2026-07-11T10:05:00Z").await.unwrap();
    assert!(out.is_empty(), "nothing reclaimed");
    assert_eq!(
        store.get_task("live").await.unwrap().unwrap().status,
        "in_progress"
    );
    assert_eq!(
        store.get_task("manual").await.unwrap().unwrap().status,
        "in_progress"
    );
}

#[tokio::test]
async fn dependency_gating_blocks_until_deps_done() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");

    store.insert_task(&pending_task("dep")).await.unwrap();
    let mut child = pending_task("child");
    child.depends_on = r#"["dep"]"#.into();
    store.insert_task(&child).await.unwrap();

    // While `dep` is pending, only `dep` is claimable.
    let claimable = store.claimable_tasks().await.unwrap();
    let ids: HashSet<_> = claimable.iter().map(|t| t.id.clone()).collect();
    assert!(ids.contains("dep"));
    assert!(!ids.contains("child"), "child gated by unmet dep");

    // Complete `dep` → child unlocks.
    store.complete_task("dep", "done", "system").await.unwrap();
    let claimable2 = store.claimable_tasks().await.unwrap();
    let ids2: HashSet<_> = claimable2.iter().map(|t| t.id.clone()).collect();
    assert!(ids2.contains("child"), "child claimable once dep done");
}

/// WP-10B: archiving a pending/unclaimed task (the `/goals` board
/// "take out of active consideration" action) must remove it from the
/// dispatch engine's pickup queue — mirrors the existing
/// `list_tasks_filtered_excludes_archived_by_default` guarantee.
#[tokio::test]
async fn claimable_tasks_excludes_archived() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");

    store.insert_task(&pending_task("visible")).await.unwrap();
    store.insert_task(&pending_task("archived")).await.unwrap();
    store
        .update_task("archived", &serde_json::json!({ "archived": true }))
        .await
        .unwrap();

    let claimable = store.claimable_tasks().await.unwrap();
    let ids: HashSet<_> = claimable.iter().map(|t| t.id.clone()).collect();
    assert!(ids.contains("visible"), "non-archived task stays claimable");
    assert!(
        !ids.contains("archived"),
        "archived task must not be claimable by the dispatch engine: {ids:?}"
    );
}

#[tokio::test]
async fn goal_mode_completion_routes_to_review_then_accept_reject() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");

    let mut g = pending_task("goal");
    g.goal_mode = true;
    g.max_retries = 1;
    g.acceptance_criteria = Some("must compile".into());
    store.insert_task(&g).await.unwrap();

    // Completion of a goal-mode task parks in `review`, not `done`.
    let updated = store
        .complete_task("goal", "did the thing", "w")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, "review");
    assert_eq!(updated.result_summary.as_deref(), Some("did the thing"));

    // Reject → routes to `revising` (Iterative Kanban, retry 0 < 1) with
    // feedback and the round counter bumped.
    let status = store
        .reject_review("goal", "criteria not met", 3)
        .await
        .unwrap();
    assert_eq!(status, "revising");
    let t = store.get_task("goal").await.unwrap().unwrap();
    assert_eq!(t.retry_count, 1);
    assert_eq!(t.revision_round, 1);
    assert_eq!(t.judge_feedback.as_deref(), Some("criteria not met"));

    // Complete again → review → reject at cap ⇒ needs_human (fail-safe).
    store.complete_task("goal", "attempt 2", "w").await.unwrap();
    let status2 = store
        .reject_review("goal", "still failing", 3)
        .await
        .unwrap();
    assert_eq!(status2, "needs_human");
    assert_eq!(
        store.get_task("goal").await.unwrap().unwrap().status,
        "needs_human"
    );
}

#[tokio::test]
async fn complete_task_does_not_overwrite_terminal_state() {
    // A stale worker completing an already-`done` task must not clobber the
    // authoritative result (the `status NOT IN ('done','cancelled')` guard).
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");
    store.insert_task(&pending_task("t")).await.unwrap();

    let first = store
        .complete_task("t", "authoritative result", "w")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.status, "done");
    assert_eq!(
        first.result_summary.as_deref(),
        Some("authoritative result")
    );

    // Second (stale) completion is a no-op on the terminal row.
    let second = store
        .complete_task("t", "stale overwrite", "w")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.status, "done", "still done");
    assert_eq!(
        second.result_summary.as_deref(),
        Some("authoritative result"),
        "stale complete must not overwrite the first result"
    );
}

#[tokio::test]
async fn goal_mode_accept_promotes_to_done() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");
    let mut g = pending_task("goal2");
    g.goal_mode = true;
    store.insert_task(&g).await.unwrap();
    store.complete_task("goal2", "result", "w").await.unwrap();
    assert!(store.accept_review("goal2", "criteria met").await.unwrap());
    let t = store.get_task("goal2").await.unwrap().unwrap();
    assert_eq!(t.status, "done");
    assert!(t.completed_at.is_some());
}
