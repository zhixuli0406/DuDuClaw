//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs` — enqueue cases.

use super::*;

// WP3 (PORTICO): when a kickoff approval clears, the task's declared
// `grant:<tool>` tags are atomically minted as task-scoped grants.
#[tokio::test]
async fn kickoff_approval_mints_declared_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    write_agent_toml(
        dir.path(),
        "alice",
        "[capabilities]\nautonomy_level = \"collaborator\"\n",
    );
    let mut task = goal_task("g1", "alice");
    task.tags = "grant:send_message, other-tag".into();
    store.insert_task(&task).await.unwrap();

    let broker = Arc::new(crate::approval::ApprovalBroker::open(dir.path()).unwrap());
    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf())
        .with_broker(broker.clone());

    let grants = crate::capability_grants::CapabilityGrantStore::open(dir.path()).unwrap();

    // Tick 1: kickoff filed, no grant yet.
    d.tick_once().await.unwrap();
    assert!(!grants.has_active_grant("alice", "send_message").await);
    let approval_id = broker.list_pending(Some("alice")).await.unwrap()[0]
        .id
        .clone();

    // Approve → tick 2 dispatches AND mints the declared grant.
    broker
        .decide(&approval_id, true, "test:alice")
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    assert!(
        grants.has_active_grant("alice", "send_message").await,
        "kickoff approval must mint the declared grant:send_message"
    );
    // A non-grant tag never becomes a grant.
    assert!(!grants.has_active_grant("alice", "other-tag").await);
}

#[tokio::test]
async fn consultant_kickoff_denied_aborts_the_goal() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    write_agent_toml(
        dir.path(),
        "alice",
        "[capabilities]\nautonomy_level = \"consultant\"\n",
    );
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let broker = Arc::new(crate::approval::ApprovalBroker::open(dir.path()).unwrap());
    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf())
        .with_broker(broker.clone());

    d.tick_once().await.unwrap(); // kickoff filed
    let approval_id = broker.list_pending(Some("alice")).await.unwrap()[0]
        .id
        .clone();
    broker
        .decide(&approval_id, false, "test:alice")
        .await
        .unwrap(); // deny (== TTL fail-closed)

    d.tick_once().await.unwrap(); // poll → denied → abort
    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().status,
        "cancelled"
    );
    assert!(
        queue.pending_messages(10).await.unwrap().is_empty(),
        "denied kickoff never dispatches"
    );
}

// ── D4 item 1: dependency DAG gating ────────────────────

/// A goal task with `depends_on` set is frozen until every dependency is
/// `done`, then dispatched. The dependency itself dispatches immediately.
#[tokio::test]
async fn dependent_task_is_frozen_until_dep_done() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    store.insert_task(&goal_task("g1", "alice")).await.unwrap();
    let mut g2 = goal_task("g2", "alice");
    g2.depends_on = r#"["g1"]"#.into();
    store.insert_task(&g2).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();
    // Only g1 dispatched; g2 frozen (dep not done).
    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].payload.contains("task_id=g1"));

    // Mark g1 done → g2 becomes dispatchable next tick.
    store
        .update_task("g1", &serde_json::json!({ "status": "done" }))
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(
        pending.len(),
        2,
        "g2 dispatched once its dependency is done"
    );
    assert!(pending.iter().any(|m| m.payload.contains("task_id=g2")));
}

/// A downstream task whose dependency ends terminally (failed / needs_human /
/// cancelled / missing) inherits the escalation — it is parked `needs_human`
/// rather than frozen forever (never orphaned).
#[tokio::test]
async fn dependency_failure_escalates_downstream() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;

    let mut g1 = goal_task("g1", "alice");
    g1.status = "failed".into();
    store.insert_task(&g1).await.unwrap();
    let mut g2 = goal_task("g2", "alice");
    g2.depends_on = r#"["g1"]"#.into();
    store.insert_task(&g2).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    let got = store.get_task("g2").await.unwrap().unwrap();
    assert_eq!(got.status, "needs_human", "downstream inherits escalation");
    assert!(
        got.judge_feedback
            .as_deref()
            .unwrap_or("")
            .contains("upstream dependency failed")
    );
    // No work message enqueued for the frozen/escalated downstream task.
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
}

/// A missing dependency id (never resolvable) also escalates downstream —
/// fail-closed, does not wait forever.
#[tokio::test]
async fn missing_dependency_escalates_downstream() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    let mut g2 = goal_task("g2", "alice");
    g2.depends_on = r#"["ghost"]"#.into();
    store.insert_task(&g2).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();
    assert_eq!(
        store.get_task("g2").await.unwrap().unwrap().status,
        "needs_human"
    );
}

// ── D4 item 2: dispatch policy integration ──────────────

/// With a RoundRobin policy wired, a task assigned to a non-roster agent is
/// re-routed to a roster member and the reassignment is persisted.
#[tokio::test]
async fn round_robin_policy_reassigns_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    // Roster = {alice, bob} (approver so no kickoff gate).
    write_agent_toml(
        dir.path(),
        "alice",
        "[capabilities]\nautonomy_level = \"approver\"\n",
    );
    write_agent_toml(
        dir.path(),
        "bob",
        "[capabilities]\nautonomy_level = \"approver\"\n",
    );

    // Task assigned to someone NOT in the roster ⇒ policy must re-route.
    store.insert_task(&goal_task("g1", "zzz")).await.unwrap();

    let policy: Arc<dyn DispatchPolicy> = Arc::new(crate::dispatch_policy::RoundRobin::new());
    let d = GoalLoopDriver::new(store.clone(), queue.clone(), small_cfg())
        .with_home_dir(dir.path().to_path_buf())
        .with_policy(policy);
    d.tick_once().await.unwrap();

    // RoundRobin picks the first roster member (sorted): "alice".
    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(
        got.assigned_to, "alice",
        "reassignment persisted to the roster member"
    );
    let pending = queue.pending_messages(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].target, "alice",
        "work dispatched to the re-routed agent"
    );
}

/// The default (no policy) path is unchanged: dispatch to the stored
/// `assigned_to`, no reassignment.
#[tokio::test]
async fn default_policy_keeps_assigned_to() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    assert_eq!(
        store.get_task("g1").await.unwrap().unwrap().assigned_to,
        "alice"
    );
    assert_eq!(queue.pending_messages(10).await.unwrap()[0].target, "alice");
}

// ── L4: derive_goal_kind / count_distinct_hits ──────────

#[test]
fn count_distinct_hits_dedupes_overlapping_cjk_keywords() {
    // "程式", "程式碼", "寫程式" are three separate entries of
    // GOAL_KIND_CODING_KEYWORDS, but all three match inside "寫程式碼"
    // and mutually overlap the same characters — must count as ONE
    // cluster, not three.
    let lower = "幫我寫程式碼".to_lowercase();
    assert_eq!(count_distinct_hits(&lower, &GOAL_KIND_CODING_KEYWORDS), 1);
}

#[test]
fn count_distinct_hits_counts_non_overlapping_separately() {
    let lower = "寫程式碼順便修一個 bug".to_lowercase();
    // "寫程式碼" cluster (1) + separate "bug" (1) = 2, not 4 raw keyword
    // matches (程式/程式碼/寫程式/bug).
    assert_eq!(count_distinct_hits(&lower, &GOAL_KIND_CODING_KEYWORDS), 2);
}

#[test]
fn count_distinct_hits_is_anchored_not_substring() {
    // Old `contains`-based counting matched "send" inside "sender" and
    // "email" inside "emailed" — both false positives for an OPS signal.
    let lower = "check the sender field, already emailed them".to_lowercase();
    assert_eq!(
        count_distinct_hits(&lower, &GOAL_KIND_OPS_KEYWORDS),
        0,
        "unanchored substrings inside sender/emailed must not count as OPS hits"
    );
    // A real word-boundary hit still counts.
    let real_hit = "please send it now".to_lowercase();
    assert_eq!(count_distinct_hits(&real_hit, &GOAL_KIND_OPS_KEYWORDS), 1);
}

#[test]
fn count_distinct_hits_empty_on_no_match() {
    let lower = "just chatting, nothing special".to_lowercase();
    assert_eq!(count_distinct_hits(&lower, &GOAL_KIND_CODING_KEYWORDS), 0);
}

#[test]
fn derive_goal_kind_classifies_coding_despite_overlapping_keywords() {
    // Before L4 this text scored coding=3 (dominating trivially); after
    // the dedup fix it scores coding=1 — still correctly the dominant
    // (only) topical signal, still classified as a Coding variant.
    let kind = derive_goal_kind("請幫我寫程式碼");
    assert!(
        matches!(kind, GoalKind::CodingSimple | GoalKind::CodingComplex),
        "unexpected kind: {kind:?}"
    );
}

#[test]
fn derive_goal_kind_ops_dominates_over_research() {
    let kind = derive_goal_kind("請部署新版本並通知團隊");
    assert_eq!(kind, GoalKind::OpsOrExternal);
}

#[test]
fn derive_goal_kind_no_signal_falls_back_on_difficulty() {
    let kind = derive_goal_kind("哈囉");
    assert_eq!(
        kind,
        GoalKind::Unknown,
        "short, no-keyword text ⇒ Simple ⇒ Unknown"
    );
}

// ── L3: state_capture_seen must not leak across terminal states ──

#[tokio::test]
async fn state_capture_seen_is_cleared_when_task_reaches_done() {
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap(); // dispatch iter 1

    // Agent moves it to review — the next tick's reconcile loop should
    // record a first capture.
    store
        .update_task("g1", &serde_json::json!({ "status": "review" }))
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    assert!(
        d.state_capture_seen.lock().await.contains("g1"),
        "review sitting must be recorded as captured"
    );

    // Task reaches a terminal state (done) without ever re-entering the
    // candidate set (todo/pending/revising) — the top-of-tick prune
    // alone can never clear it; the `done` branch's explicit removal
    // must.
    store
        .update_task("g1", &serde_json::json!({ "status": "done" }))
        .await
        .unwrap();
    d.tick_once().await.unwrap();

    assert!(
        !d.state_capture_seen.lock().await.contains("g1"),
        "state_capture_seen must not leak once the task is terminal"
    );
}

#[tokio::test]
async fn state_capture_seen_is_cleared_when_task_reaches_needs_human_directly() {
    // Simulates DispatchEngine's own judge-retry-budget path setting
    // needs_human directly (not via this driver's `escalate()`) — the
    // reconcile loop's `_` catch-all branch must clear the flag too.
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap();

    store
        .update_task("g1", &serde_json::json!({ "status": "review" }))
        .await
        .unwrap();
    d.tick_once().await.unwrap();
    assert!(d.state_capture_seen.lock().await.contains("g1"));

    store
        .update_task("g1", &serde_json::json!({ "status": "needs_human" }))
        .await
        .unwrap();
    d.tick_once().await.unwrap();

    assert!(
        !d.state_capture_seen.lock().await.contains("g1"),
        "state_capture_seen must be cleared on a directly-set needs_human too"
    );
}

#[tokio::test]
async fn state_capture_seen_is_cleared_on_driver_escalate() {
    // Exercises this driver's OWN `escalate()` path — here via the A2
    // no-progress guard, the cheapest trigger to set up without
    // fighting `update_task`'s field whitelist (which doesn't allow
    // rewriting `created_at` post-insert for a deadline trigger). Note:
    // under the current `tick_once` control flow the top-of-tick prune
    // already clears a candidate task's entry before `escalate()` runs
    // in the same tick (a task must be a candidate to reach
    // `escalate()` at all, and the prune runs first) — so
    // `escalate()`'s own removal is defense-in-depth for future call
    // sites/orderings rather than the only thing keeping this specific
    // scenario clean today. The end-to-end invariant asserted below (no
    // leak once terminal) must hold either way.
    let dir = tempfile::tempdir().unwrap();
    let (store, queue) = open_stores(dir.path()).await;
    store.insert_task(&goal_task("g1", "alice")).await.unwrap();

    let d = driver(store.clone(), queue.clone(), small_cfg());
    d.tick_once().await.unwrap(); // dispatch iter 1 (commits state_hash A)

    store
        .update_task("g1", &serde_json::json!({ "status": "review" }))
        .await
        .unwrap();
    d.tick_once().await.unwrap(); // captures review state
    assert!(d.state_capture_seen.lock().await.contains("g1"));

    // Return it to `pending` WITHOUT going through a real judge
    // rejection (no new `judge_feedback`) — the recomputed `<state>`
    // hash is therefore byte-identical to state_hash A, so the A2
    // no-progress guard's `would_be_streak` reaches 2 on this very next
    // tick and `escalate()` fires.
    store
        .update_task("g1", &serde_json::json!({ "status": "pending" }))
        .await
        .unwrap();
    d.tick_once().await.unwrap();

    let got = store.get_task("g1").await.unwrap().unwrap();
    assert_eq!(got.status, "needs_human", "A2 guard must have escalated");
    assert!(
        !d.state_capture_seen.lock().await.contains("g1"),
        "escalate() must clear state_capture_seen too"
    );
}
