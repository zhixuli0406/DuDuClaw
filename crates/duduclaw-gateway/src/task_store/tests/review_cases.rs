//! Unit tests for [`super`], moved verbatim out of the former `task_store.rs` — review cases.

use super::*;

// ── G1 lease renewal: conservative reclaim ──────────────

#[test]
fn zombie_reclaim_due_semantics() {
    let lease = "2026-07-11T10:05:00Z";
    let anchor = Some("2026-07-11T10:00:00Z"); // window = 5 min
    // Lease still live ⇒ never due.
    assert!(!zombie_reclaim_due(lease, anchor, "2026-07-11T10:04:00Z"));
    // Expired but within the grace window (one further full lease window).
    assert!(!zombie_reclaim_due(lease, anchor, "2026-07-11T10:06:00Z"));
    assert!(!zombie_reclaim_due(lease, anchor, "2026-07-11T10:09:59Z"));
    // Expired + full extra window elapsed with no renewal ⇒ due.
    assert!(zombie_reclaim_due(lease, anchor, "2026-07-11T10:10:00Z"));
    // Legacy row (no anchor): zero grace ⇒ due at plain expiry.
    assert!(zombie_reclaim_due(lease, None, "2026-07-11T10:05:00Z"));
    // Corrupt lease ⇒ due (must not pin a zombie forever).
    assert!(zombie_reclaim_due(
        "garbage",
        anchor,
        "2026-07-11T10:00:00Z"
    ));
    // Corrupt anchor degrades to zero grace, not a panic.
    assert!(zombie_reclaim_due(
        lease,
        Some("garbage"),
        "2026-07-11T10:05:00Z"
    ));
}

#[tokio::test]
async fn renewed_lease_survives_reclaim_and_abandoned_claim_does_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");
    store.insert_task(&pending_task("held")).await.unwrap();
    store.insert_task(&pending_task("abandoned")).await.unwrap();

    // Both claimed at 10:00 with a 5-minute lease.
    for id in ["held", "abandoned"] {
        assert!(
            store
                .atomic_claim(id, "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
                .await
                .unwrap()
                .is_claimed()
        );
    }
    // `held`'s worker heartbeats at 10:04 → lease pushed to 10:09.
    assert!(
        store
            .renew_lease("held", "w", "2026-07-11T10:09:00Z", "2026-07-11T10:04:00Z")
            .await
            .unwrap()
    );

    // At 10:11: `abandoned` expired at 10:05 with a 5-min window ⇒ due at
    // 10:10 ⇒ reclaimed. `held` expires at 10:09, window 5 min ⇒ due only
    // at 10:14 ⇒ untouched.
    let out = store.reclaim_zombies("2026-07-11T10:11:00Z").await.unwrap();
    let ids: Vec<_> = out.iter().map(|o| o.task_id.as_str()).collect();
    assert_eq!(ids, vec!["abandoned"]);
    assert_eq!(
        store.get_task("held").await.unwrap().unwrap().status,
        "in_progress"
    );
    assert_eq!(
        store.get_task("abandoned").await.unwrap().unwrap().status,
        "pending"
    );
}

#[tokio::test]
async fn renew_lease_is_guarded_to_the_holder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open");
    store.insert_task(&pending_task("t")).await.unwrap();
    assert!(
        store
            .atomic_claim("t", "owner", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap()
            .is_claimed()
    );
    // Another agent cannot renew someone else's lease.
    assert!(
        !store
            .renew_lease(
                "t",
                "intruder",
                "2026-07-11T10:30:00Z",
                "2026-07-11T10:01:00Z"
            )
            .await
            .unwrap()
    );
    let t = store.get_task("t").await.unwrap().unwrap();
    assert_eq!(t.lease_expires_at.as_deref(), Some("2026-07-11T10:05:00Z"));
}

#[tokio::test]
async fn goal_crud_and_ancestry_is_root_first() {
    let (store, _dir) = temp_store();
    store
        .insert_goal(&goal("init", "Initiative", None))
        .await
        .unwrap();
    store
        .insert_goal(&goal("proj", "Project", Some("init")))
        .await
        .unwrap();
    store
        .insert_goal(&goal("issue", "Issue", Some("proj")))
        .await
        .unwrap();

    let chain = store.goal_ancestry("issue").await.unwrap();
    let titles: Vec<_> = chain.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles, vec!["Initiative", "Project", "Issue"]);

    // Unknown goal ⇒ empty chain, not an error.
    assert!(store.goal_ancestry("nope").await.unwrap().is_empty());

    let active = store.list_goals(Some("active")).await.unwrap();
    assert_eq!(active.len(), 3);
    assert!(store.list_goals(Some("done")).await.unwrap().is_empty());
}

#[tokio::test]
async fn goal_create_rejects_missing_parent_and_update_rejects_cycle() {
    let (store, _dir) = temp_store();
    // Missing parent ⇒ fail-closed.
    assert!(
        store
            .insert_goal(&goal("orphan", "Orphan", Some("ghost")))
            .await
            .is_err()
    );

    store.insert_goal(&goal("a", "A", None)).await.unwrap();
    store.insert_goal(&goal("b", "B", Some("a"))).await.unwrap();
    // Re-parenting a under b closes a 2-cycle ⇒ rejected.
    let err = store
        .update_goal("a", &serde_json::json!({ "parent_goal_id": "b" }))
        .await;
    assert!(err.is_err(), "cycle must be rejected");
    // Self-parent is a trivial cycle.
    assert!(
        store
            .update_goal("a", &serde_json::json!({ "parent_goal_id": "a" }))
            .await
            .is_err()
    );
    // Legit update still works.
    let g = store
        .update_goal("b", &serde_json::json!({ "status": "done" }))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(g.status, "done");
}

#[tokio::test]
async fn task_goal_id_roundtrips() {
    let (store, _dir) = temp_store();
    store.insert_goal(&goal("g", "Goal", None)).await.unwrap();
    let mut t = pending_task("t");
    t.goal_id = Some("g".into());
    store.insert_task(&t).await.unwrap();
    let got = store.get_task("t").await.unwrap().unwrap();
    assert_eq!(got.goal_id.as_deref(), Some("g"));
}

// ── depends_on cycle validation ─────────────────────────

#[test]
fn dependency_cycle_detection() {
    let edges = vec![
        ("a".to_string(), vec!["b".to_string()]),
        ("b".to_string(), vec!["c".to_string()]),
        ("c".to_string(), Vec::new()),
    ];
    // Self-dependency.
    assert!(introduces_dependency_cycle(&edges, "a", &["a".into()]));
    // c → a closes a 3-cycle (a → b → c already exists).
    assert!(introduces_dependency_cycle(&edges, "c", &["a".into()]));
    // Unrelated / forward deps are fine.
    assert!(!introduces_dependency_cycle(&edges, "d", &["a".into()]));
    assert!(!introduces_dependency_cycle(&edges, "a", &["c".into()]));
    assert!(!introduces_dependency_cycle(&edges, "a", &[]));
}

#[tokio::test]
async fn update_task_rejects_dependency_cycle() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("t1")).await.unwrap();
    let mut t2 = pending_task("t2");
    t2.depends_on = r#"["t1"]"#.into();
    store.insert_task(&t2).await.unwrap();

    // t1 depending on t2 would close t1 → t2 → t1.
    let res = store
        .update_task("t1", &serde_json::json!({ "depends_on": "[\"t2\"]" }))
        .await;
    assert!(res.is_err(), "dependency cycle must be rejected");

    // Malformed depends_on is rejected fail-closed, not silently stored.
    assert!(
        store
            .update_task("t1", &serde_json::json!({ "depends_on": "not json" }))
            .await
            .is_err()
    );

    // A legal rewire is accepted.
    let ok = store
        .update_task("t2", &serde_json::json!({ "depends_on": "[]" }))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ok.depends_on, "[]");
}

// ── HIGH-1: dependency gating at the claim boundary ──────

#[tokio::test]
async fn atomic_claim_is_gated_by_unfinished_dependencies() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("dep")).await.unwrap();
    let mut child = pending_task("child");
    child.depends_on = r#"["dep","ghost"]"#.into();
    store.insert_task(&child).await.unwrap();

    // Unmet deps (including a dep referencing a MISSING task — fail-closed)
    // block the claim and are named in the outcome.
    let out = store
        .atomic_claim("child", "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
        .await
        .unwrap();
    match out {
        super::ClaimOutcome::BlockedByDeps(unmet) => {
            assert_eq!(unmet, vec!["dep".to_string(), "ghost".to_string()]);
        }
        other => panic!("expected BlockedByDeps, got {other:?}"),
    }
    let t = store.get_task("child").await.unwrap().unwrap();
    assert_eq!(
        t.status, "pending",
        "blocked claim must not mutate the task"
    );
    assert!(t.claimed_by.is_none());

    // Finish `dep`; `ghost` still missing ⇒ still blocked (fail-closed).
    store.complete_task("dep", "done", "system").await.unwrap();
    assert!(matches!(
        store
            .atomic_claim("child", "w", "2026-07-11T10:06:00Z", "2026-07-11T10:11:00Z")
            .await
            .unwrap(),
        super::ClaimOutcome::BlockedByDeps(ref unmet) if unmet == &vec!["ghost".to_string()]
    ));

    // Drop the ghost dep → claimable.
    store
        .update_task("child", &serde_json::json!({ "depends_on": "[\"dep\"]" }))
        .await
        .unwrap();
    assert!(
        store
            .atomic_claim("child", "w", "2026-07-11T10:07:00Z", "2026-07-11T10:12:00Z")
            .await
            .unwrap()
            .is_claimed()
    );
}

// ── HIGH-2: holder-guarded completion ────────────────────

#[tokio::test]
async fn complete_task_is_guarded_to_the_claim_holder() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("t")).await.unwrap();
    assert!(
        store
            .atomic_claim("t", "owner", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap()
            .is_claimed()
    );

    // A zombie worker (reclaimed elsewhere, stale identity) cannot clobber
    // the holder's in_progress task.
    let err = store.complete_task("t", "stale result", "zombie").await;
    assert!(err.is_err(), "non-holder completion must error");
    let t = store.get_task("t").await.unwrap().unwrap();
    assert_eq!(t.status, "in_progress", "task untouched by the intruder");
    assert!(t.result_summary.is_none());

    // The holder completes normally.
    let done = store
        .complete_task("t", "real result", "owner")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "done");
    assert_eq!(done.result_summary.as_deref(), Some("real result"));
}

#[tokio::test]
async fn complete_task_unclaimed_keeps_legacy_any_caller_behavior() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("legacy")).await.unwrap();
    // Unclaimed (claimed_by IS NULL) → any caller may complete (legacy
    // board-task behavior preserved).
    let done = store
        .complete_task("legacy", "ok", "anyone")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "done");
}

// ── MED: zombie reclaim lease CAS ────────────────────────

#[tokio::test]
async fn zombie_reclaim_cas_misses_when_renewal_landed_after_scan() {
    let (store, _dir) = temp_store();
    store.insert_task(&pending_task("t")).await.unwrap();
    let scanned_lease = "2026-07-11T10:05:00Z";
    assert!(
        store
            .atomic_claim("t", "w", "2026-07-11T10:00:00Z", scanned_lease)
            .await
            .unwrap()
            .is_claimed()
    );

    // A renewal lands between the zombie scan and the requeue write —
    // the CAS on the scanned lease value must miss and leave the claim.
    assert!(
        store
            .renew_lease("t", "w", "2026-07-11T10:20:00Z", "2026-07-11T10:04:00Z")
            .await
            .unwrap()
    );
    let requeued = store
        .requeue_zombie_cas("t", "w", scanned_lease, 1, "2026-07-11T10:16:00Z")
        .await
        .unwrap();
    assert!(
        !requeued,
        "stale scanned lease must not requeue a renewed claim"
    );
    let t = store.get_task("t").await.unwrap().unwrap();
    assert_eq!(t.status, "in_progress");
    assert_eq!(t.claimed_by.as_deref(), Some("w"));
    assert_eq!(t.retry_count, 0);

    // Same race on the fail path.
    let failed = store
        .fail_zombie_cas("t", "w", scanned_lease, "2026-07-11T10:16:00Z")
        .await
        .unwrap();
    assert!(!failed, "stale scanned lease must not fail a renewed claim");
    assert_eq!(
        store.get_task("t").await.unwrap().unwrap().status,
        "in_progress"
    );

    // With the CURRENT lease value the CAS applies (the genuine zombie path).
    let requeued2 = store
        .requeue_zombie_cas("t", "w", "2026-07-11T10:20:00Z", 1, "2026-07-11T10:30:00Z")
        .await
        .unwrap();
    assert!(requeued2);
    assert_eq!(
        store.get_task("t").await.unwrap().unwrap().status,
        "pending"
    );
}

#[test]
fn plan_order_for_insert_semantics() {
    // Empty plan: first step lands at one gap.
    assert_eq!(plan_order_for_insert(&[], 0), Some(PLAN_STEP_ORDER_GAP));
    // Append always succeeds at last + GAP.
    assert_eq!(
        plan_order_for_insert(&[1024, 2048], 2),
        Some(2048 + PLAN_STEP_ORDER_GAP)
    );
    // Between two neighbours ⇒ midpoint.
    assert_eq!(plan_order_for_insert(&[1024, 2048], 1), Some(1536));
    // Front ⇒ midpoint of (0, first).
    assert_eq!(plan_order_for_insert(&[1024, 2048], 0), Some(512));
    // Exhausted gap (adjacent keys) ⇒ None — caller renormalizes.
    assert_eq!(plan_order_for_insert(&[5, 6], 1), None);
    assert_eq!(plan_order_for_insert(&[1], 0), None);
}

#[tokio::test]
async fn plan_steps_append_insert_and_move_keep_order() {
    let (store, _dir) = temp_store();
    store.insert_plan(&plan("p1", "bot")).await.unwrap();

    // Append three steps.
    for (id, text) in [("s1", "first"), ("s2", "second"), ("s3", "third")] {
        store
            .add_plan_step("p1", id, text, "agent", "bot", None)
            .await
            .unwrap();
    }
    let texts = |steps: &[super::PlanStepRow]| -> Vec<String> {
        steps.iter().map(|s| s.text.clone()).collect()
    };
    let steps = store.list_plan_steps("p1").await.unwrap();
    assert_eq!(texts(&steps), vec!["first", "second", "third"]);

    // Insert at index 1 (between first and second).
    store
        .add_plan_step("p1", "s4", "one-point-five", "user", "louis", Some(1))
        .await
        .unwrap();
    let steps = store.list_plan_steps("p1").await.unwrap();
    assert_eq!(
        texts(&steps),
        vec!["first", "one-point-five", "second", "third"]
    );

    // Move "third" to the front.
    assert!(store.move_plan_step("p1", "s3", 0).await.unwrap());
    let steps = store.list_plan_steps("p1").await.unwrap();
    assert_eq!(
        texts(&steps),
        vec!["third", "first", "one-point-five", "second"]
    );

    // Move front step to the end (index clamps to len-1).
    assert!(store.move_plan_step("p1", "s3", 99).await.unwrap());
    let steps = store.list_plan_steps("p1").await.unwrap();
    assert_eq!(
        texts(&steps),
        vec!["first", "one-point-five", "second", "third"]
    );

    // Moving an unknown step is a no-op `false`, not an error.
    assert!(!store.move_plan_step("p1", "ghost", 0).await.unwrap());
}
