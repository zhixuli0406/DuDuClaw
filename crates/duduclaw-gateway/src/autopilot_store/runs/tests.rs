use super::*;
use crate::autopilot_store::{AutopilotDispatchRow, DISPATCH_AWAITING, DISPATCH_REQUESTING};
use crate::autopilot_approval::{RESULT_DENIED, RESULT_EXPIRED};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-27T00:00:00.000Z").unwrap().with_timezone(&Utc)
}

fn run(id: &str, rule_id: &str, count: i64) -> (AutopilotRunRow, Vec<AutopilotRunStepRow>) {
    let row = AutopilotRunRow {
        run_id: id.into(), rule_id: rule_id.into(), rule_name: "Rule".into(), rule_hash: "hash".into(),
        event_name: "task_created".into(), event_ref: "event".into(), dedupe_key: format!("dedupe:{id}"),
        fields_json: "{}".into(), state: RUN_PENDING.into(), step_index: 0, step_count: count,
        next_at: None, lease_owner: None, lease_until: None, owner_department: None, gateway_origin: false,
        outcome: None, detail: None, created_at: run_ts(now()), updated_at: run_ts(now()),
        notice_pending: false, notice_push_state: None, notice_push_attempts: 0, notice_push_next_at: None,
    };
    let steps = (0..count).map(|i| AutopilotRunStepRow {
        run_id: id.into(), step_index: i, step_type: "delegate".into(), state: STEP_PENDING.into(),
        idempotency_key: step_idempotency_key(id, i), dispatch_id: None, queue_message_id: None,
        started_at: None, finished_at: None, detail: None,
    }).collect();
    (row, steps)
}

async fn insert(store: &AutopilotStore, id: &str, count: i64) {
    let (row, steps) = run(id, "rule", count);
    assert_eq!(store.insert_run(&row, &steps, now()).await.unwrap(), RunInsertOutcome::Inserted);
}

fn dispatch(id: &str, run_id: &str, step_index: i64) -> AutopilotDispatchRow {
    AutopilotDispatchRow {
        dispatch_id: id.into(), approval_id: None, rule_id: "rule".into(),
        rule_name: "Rule".into(), rule_hash: "hash".into(), event_name: "task_created".into(),
        event_ref: "event".into(), dedupe_key: format!("dispatch:{id}"),
        frozen_json: "{}".into(), frozen_digest: "digest".into(), state: DISPATCH_REQUESTING.into(),
        outcome: None, detail: None, notice_activity_pending: false, notice_push_state: None,
        notice_push_attempts: 0, notice_push_next_at: None, notice_pushed_at: None,
        created_at: run_ts(now()), updated_at: run_ts(now()),
        run_id: Some(run_id.into()), step_index: Some(step_index),
    }
}

async fn await_approval(store: &AutopilotStore) {
    insert(store, "r", 1).await;
    assert!(store.claim_run("r", "owner", now()).await.unwrap());
    assert!(store.mark_step_running("r", 0, "owner", now()).await.unwrap());
    assert!(store.insert_dispatch(&dispatch("d", "r", 0)).await.unwrap());
    assert!(store.bind_step_dispatch_awaiting("d", "approval", "r", 0, "owner", now(),
        StepApprovalBindMode::Filing).await.unwrap());
    // These rejection tests supply a missing or corrupt replacement ledger.
    store.conn.lock().await.execute("DELETE FROM autopilot_approval_dispatch WHERE dispatch_id='d'", []).unwrap();
}

async fn assert_resume_rejected(store: &AutopilotStore, dispatch_id: &str) {
    let before = store.get_run("r").await.unwrap();
    let before_steps = store.list_run_steps("r").await.unwrap();
    assert!(!store.resume_after_approval("r", 0, dispatch_id, now() + Duration::seconds(1)).await.unwrap());
    assert_eq!(store.get_run("r").await.unwrap(), before);
    assert_eq!(store.list_run_steps("r").await.unwrap(), before_steps);
}

#[tokio::test]
async fn approval_resume_rejects_wrong_dispatch_id() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    await_approval(&store).await;
    assert!(store.insert_dispatch(&dispatch("other", "r", 0)).await.unwrap());
    {
        let conn = store.conn.lock().await;
        conn.execute("UPDATE autopilot_approval_dispatch SET state=?1 WHERE dispatch_id='other'",
            params![DISPATCH_EXECUTING]).unwrap();
    }
    // Even an eligible ledger row must match the step's dispatch id.
    assert_resume_rejected(&store, "other").await;
    assert_resume_rejected(&store, "missing").await;
    assert_resume_rejected(&store, "d").await;
}

#[tokio::test]
async fn approval_resume_rejects_mismatched_dispatch_run_or_step() {
    for (run_id, step_index) in [("other", 0), ("r", 1)] {
        let home = tempfile::tempdir().unwrap();
        let store = AutopilotStore::open(home.path()).unwrap();
        await_approval(&store).await;
        assert!(store.insert_dispatch(&dispatch("d", run_id, step_index)).await.unwrap());
        {
            let conn = store.conn.lock().await;
            conn.execute("UPDATE autopilot_approval_dispatch SET state=?1 WHERE dispatch_id='d'",
                params![DISPATCH_EXECUTING]).unwrap();
        }
        assert_resume_rejected(&store, "d").await;
    }
}

#[tokio::test]
async fn approval_resume_rejects_unapproved_denied_or_expired_dispatch() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    await_approval(&store).await;
    assert!(store.insert_dispatch(&dispatch("d", "r", 0)).await.unwrap());
    for (state, outcome) in [
        (DISPATCH_REQUESTING, None), (DISPATCH_AWAITING, None),
        (DISPATCH_SETTLED, Some(RESULT_DENIED)), (DISPATCH_SETTLED, Some(RESULT_EXPIRED)),
        (DISPATCH_SETTLED, None), (DISPATCH_SETTLED, Some("failure")),
    ] {
        {
            let conn = store.conn.lock().await;
            conn.execute("UPDATE autopilot_approval_dispatch SET state=?1, outcome=?2 WHERE dispatch_id='d'",
                params![state, outcome]).unwrap();
        }
        assert_resume_rejected(&store, "d").await;
    }
}

#[tokio::test]
async fn approval_resume_requires_running_step() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    await_approval(&store).await;
    assert!(store.insert_dispatch(&dispatch("d", "r", 0)).await.unwrap());
    {
        let conn = store.conn.lock().await;
        conn.execute("UPDATE autopilot_approval_dispatch SET state=?1 WHERE dispatch_id='d'",
            params![DISPATCH_EXECUTING]).unwrap();
    }
    for state in [STEP_PENDING, STEP_DONE, STEP_FAILED, STEP_SKIPPED] {
        {
            let conn = store.conn.lock().await;
            conn.execute("UPDATE autopilot_run_steps SET state=?1 WHERE run_id='r' AND step_index=0",
                params![state]).unwrap();
        }
        assert_resume_rejected(&store, "d").await;
    }
}

#[tokio::test]
async fn insert_dispatch_refuses_second_dispatch_for_run_step() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let original = dispatch("first", "r", 0);
    assert!(store.insert_dispatch(&original).await.unwrap());
    let error = store.insert_dispatch(&dispatch("second", "r", 0)).await.unwrap_err();
    assert!(error.contains("UNIQUE constraint failed"), "{error}");
    assert_eq!(store.get_dispatch("first").await.unwrap(), Some(original));
    assert!(store.get_dispatch("second").await.unwrap().is_none());
    // The failed insert rolled back and other steps remain insertable.
    assert!(store.insert_dispatch(&dispatch("next", "r", 1)).await.unwrap());
    for id in ["legacy-a", "legacy-b"] {
        let mut row = dispatch(id, "r", 0);
        row.run_id = None;
        row.step_index = None;
        assert!(store.insert_dispatch(&row).await.unwrap());
    }
}

#[tokio::test]
async fn settle_claimed_run_fences_expired_and_reclaimed_owners() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "r", 1).await;
    assert!(!store.settle_claimed_run("r", "a", "completed", None, now()).await.unwrap());
    assert!(store.claim_run("r", "a", now()).await.unwrap());
    let later = now() + Duration::seconds(RUN_LEASE_SECS) + Duration::milliseconds(1);
    let expired = store.get_run("r").await.unwrap();
    assert!(!store.settle_claimed_run("r", "a", "completed", None, later).await.unwrap());
    assert_eq!(store.get_run("r").await.unwrap(), expired);
    assert!(store.reclaim_expired_run("r", "b", later).await.unwrap());
    let reclaimed = store.get_run("r").await.unwrap();
    assert!(!store.settle_claimed_run("r", "a", "completed", None, later).await.unwrap());
    assert_eq!(store.get_run("r").await.unwrap(), reclaimed);
    assert!(store.settle_unclaimed_run("r", &[RUN_RUNNING], "completed", None, later).await.is_err());
    assert!(!store.settle_unclaimed_run("r", &[RUN_PENDING], "completed", None, later).await.unwrap());
    assert_eq!(store.get_run("r").await.unwrap(), reclaimed);
    assert!(store.settle_claimed_run("r", "b", "completed", Some("done"), later).await.unwrap());
    let settled = store.get_run("r").await.unwrap().unwrap();
    assert_eq!(settled.state, RUN_SETTLED);
    assert_eq!(settled.outcome.as_deref(), Some("completed"));
    assert_eq!(settled.detail.as_deref(), Some("done"));
    assert!(settled.next_at.is_none() && settled.lease_owner.is_none() && settled.lease_until.is_none());
    assert!(!store.settle_claimed_run("r", "b", "cancelled", None, later).await.unwrap());
    assert!(store.settle_unclaimed_run("r", &[RUN_SETTLED], "cancelled", None, later).await.is_err());
    assert_eq!(store.get_run("r").await.unwrap(), Some(settled));
}

#[tokio::test]
async fn insert_run_dedupes_limits_and_excludes_settled_runs() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "first", 2).await;
    let (mut duplicate, steps) = run("duplicate", "rule", 2);
    duplicate.dedupe_key = "dedupe:first".into();
    assert_eq!(store.insert_run(&duplicate, &steps, now()).await.unwrap(), RunInsertOutcome::Duplicate);
    assert!(store.get_run("duplicate").await.unwrap().is_none());
    assert!(store.list_run_steps("duplicate").await.unwrap().is_empty());
    assert_eq!(store.list_run_steps("first").await.unwrap().len(), 2);
    for i in 1..MAX_UNSETTLED_RUNS_PER_RULE {
        insert(&store, &format!("run-{i}"), 1).await;
    }
    let (row, steps) = run("overflow", "rule", 1);
    assert_eq!(store.insert_run(&row, &steps, now()).await.unwrap(), RunInsertOutcome::RuleLimit);
    assert!(store.settle_unclaimed_run("first", &[RUN_PENDING], "completed", None, now()).await.unwrap());
    assert_eq!(store.count_unsettled_runs(Some("rule")).await.unwrap(), 19);
    assert_eq!(store.insert_run(&row, &steps, now()).await.unwrap(), RunInsertOutcome::Inserted);
    assert_eq!(store.count_unsettled_runs(None).await.unwrap(), 20);
    let (mut large, steps) = run("large", "other", 1);
    large.fields_json = "x".repeat(crate::autopilot_approval::MAX_FROZEN_PAYLOAD_BYTES + 1);
    assert_eq!(store.insert_run(&large, &steps, now()).await.unwrap(), RunInsertOutcome::FieldsTooLarge);
    assert!(store.get_run("large").await.unwrap().is_none());
    assert!(store.list_run_steps("large").await.unwrap().is_empty());
    large.fields_json.pop();
    assert_eq!(store.insert_run(&large, &steps, now()).await.unwrap(), RunInsertOutcome::Inserted);
}

#[tokio::test]
async fn insert_run_enforces_global_limit() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    // Seed capacity directly to keep this boundary test inexpensive.
    {
        let conn = store.conn.lock().await;
        conn.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1000)
            INSERT INTO autopilot_runs
            (run_id,rule_id,rule_name,rule_hash,event_name,event_ref,dedupe_key,fields_json,state,step_count,created_at,updated_at)
            SELECT 'run-'||i,'rule-'||i,'Rule','hash','event','ref','key-'||i,'{}','pending',1,
                '2026-11-27T00:00:00.000Z','2026-11-27T00:00:00.000Z' FROM n;").unwrap();
    }
    assert_eq!(store.count_unsettled_runs(None).await.unwrap(), MAX_UNSETTLED_RUNS_TOTAL);
    let (row, steps) = run("overflow", "unused-rule", 1);
    assert_eq!(store.insert_run(&row, &steps, now()).await.unwrap(), RunInsertOutcome::GlobalLimit);
    assert!(store.list_run_steps("overflow").await.unwrap().is_empty());
    assert!(store.settle_unclaimed_run("run-1", &[RUN_PENDING], "completed", None, now()).await.unwrap());
    assert_eq!(store.insert_run(&row, &steps, now()).await.unwrap(), RunInsertOutcome::Inserted);
}

#[tokio::test]
async fn insert_run_rejects_malformed_steps_and_rolls_back_partial_inserts() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let (row, mut steps) = run("bad", "rule", 2);
    assert!(store.insert_run(&row, &steps[..1], now()).await.is_err());
    steps[1].step_index = 3;
    assert!(store.insert_run(&row, &steps, now()).await.is_err());
    steps[1].step_index = 1;
    steps[1].run_id = "other".into();
    assert!(store.insert_run(&row, &steps, now()).await.is_err());
    steps[1].run_id = "bad".into();
    steps[1].idempotency_key = steps[0].idempotency_key.clone();
    assert!(store.insert_run(&row, &steps, now()).await.is_err());
    assert!(store.get_run("bad").await.unwrap().is_none());
    assert!(store.list_run_steps("bad").await.unwrap().is_empty());
    steps[1].idempotency_key = step_idempotency_key("bad", 1);
    assert_eq!(store.insert_run(&row, &steps, now()).await.unwrap(), RunInsertOutcome::Inserted);
}

#[tokio::test]
async fn claim_run_is_exclusive_and_waiting_runs_obey_next_at() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let second = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "pending", 1).await;
    assert!(store.claim_run("pending", "owner-a", now()).await.unwrap());
    assert!(!second.claim_run("pending", "owner-b", now()).await.unwrap());
    let (mut row, steps) = run("future", "rule", 1);
    row.state = RUN_WAITING.into();
    // Input offsets and precision are normalized before TEXT comparisons.
    row.next_at = Some("2026-11-27T08:01:00+08:00".into());
    assert_eq!(store.insert_run(&row, &steps, now()).await.unwrap(), RunInsertOutcome::Inserted);
    assert_eq!(store.get_run("future").await.unwrap().unwrap().next_at.as_deref(), Some("2026-11-27T00:01:00.000Z"));
    assert!(!store.claim_run("future", "owner-a", now()).await.unwrap());
    assert!(store.list_due_runs(now(), 10).await.unwrap().is_empty());
    let due = now() + Duration::seconds(60);
    assert_eq!(store.list_due_runs(due, 10).await.unwrap(), vec!["future"]);
    assert!(store.claim_run("future", "owner-a", due).await.unwrap());
}

#[tokio::test]
async fn complete_step_checks_owner_waits_and_settles_terminally() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "r", 2).await;
    assert!(store.claim_run("r", "owner", now()).await.unwrap());
    assert!(!store.mark_step_running("r", 0, "other", now()).await.unwrap());
    assert!(!store.mark_step_running("r", 1, "owner", now()).await.unwrap());
    assert!(store.mark_step_running("r", 0, "owner", now()).await.unwrap());
    assert!(!store.mark_step_running("r", 0, "owner", now()).await.unwrap());
    let before = store.get_run("r").await.unwrap();
    let before_steps = store.list_run_steps("r").await.unwrap();
    let next_at = now() + Duration::minutes(5);
    assert!(!store.complete_step("r", "other", 0, STEP_DONE, Some("no"), Some("no"),
        RunNext::Wait { step_index: 1, next_at }, now()).await.unwrap());
    assert_eq!(store.get_run("r").await.unwrap(), before);
    assert_eq!(store.list_run_steps("r").await.unwrap(), before_steps);
    assert!(store.complete_step("r", "owner", 0, STEP_DONE, Some("done"), Some("q"),
        RunNext::Wait { step_index: 1, next_at }, now()).await.unwrap());
    let waiting = store.get_run("r").await.unwrap().unwrap();
    assert_eq!(waiting.state, RUN_WAITING);
    assert_eq!(waiting.step_index, 1);
    assert_eq!(waiting.next_at, Some(run_ts(next_at)));
    assert!(waiting.lease_owner.is_none() && waiting.lease_until.is_none());
    let steps = store.list_run_steps("r").await.unwrap();
    assert_eq!(steps[0].state, STEP_DONE);
    assert_eq!(steps[0].queue_message_id.as_deref(), Some("q"));
    assert_eq!(steps[0].finished_at.as_deref(), Some(run_ts(now()).as_str()));
    assert!(!store.claim_run("r", "owner", now()).await.unwrap());
    assert!(store.claim_run("r", "owner", next_at).await.unwrap());
    assert!(store.mark_step_running("r", 1, "owner", next_at).await.unwrap());
    assert!(store.complete_step("r", "owner", 1, STEP_DONE, None, None,
        RunNext::Settle { outcome: "completed".into(), detail: Some("all done".into()) }, next_at).await.unwrap());
    let settled = store.get_run("r").await.unwrap().unwrap();
    assert_eq!(settled.state, RUN_SETTLED);
    assert_eq!(settled.outcome.as_deref(), Some("completed"));
    assert_eq!(settled.detail.as_deref(), Some("all done"));
    assert!(settled.lease_owner.is_none() && settled.lease_until.is_none());
    assert!(store.settle_unclaimed_run("r", &[RUN_SETTLED, RUN_RUNNING], "cancelled", None, next_at).await.is_err());
    assert!(!store.settle_unclaimed_run("r", &[RUN_PENDING], "cancelled", None, next_at).await.unwrap());
    assert!(!store.claim_run("r", "owner", next_at).await.unwrap());
}

#[tokio::test]
async fn reclaim_expired_run_requires_strict_expiry_and_fences_old_owner() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "r", 1).await;
    assert!(store.claim_run("r", "old", now()).await.unwrap());
    assert!(store.mark_step_running("r", 0, "old", now()).await.unwrap());
    let expiry = now() + Duration::seconds(RUN_LEASE_SECS);
    assert!(!store.reclaim_expired_run("r", "new", now()).await.unwrap());
    assert!(!store.reclaim_expired_run("r", "new", expiry).await.unwrap());
    assert!(store.list_expired_running_runs(expiry, 10).await.unwrap().is_empty());
    let later = expiry + Duration::milliseconds(1);
    assert_eq!(store.list_expired_running_runs(later, 10).await.unwrap(), vec!["r"]);
    assert!(!store.complete_step("r", "old", 0, STEP_DONE, None, None,
        RunNext::Settle { outcome: "completed".into(), detail: None }, later).await.unwrap());
    assert!(store.reclaim_expired_run("r", "new", later).await.unwrap());
    assert!(!store.complete_step("r", "old", 0, STEP_DONE, None, None,
        RunNext::Settle { outcome: "completed".into(), detail: None }, later).await.unwrap());
    assert!(store.complete_step("r", "new", 0, STEP_DONE, None, None,
        RunNext::Settle { outcome: "completed".into(), detail: None }, later).await.unwrap());
}

#[tokio::test]
async fn complete_step_continues_and_approval_resumes_once() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "r", 3).await;
    for i in 0..3 {
        assert!(store.claim_run("r", "owner", now()).await.unwrap());
        assert!(store.mark_step_running("r", i, "owner", now()).await.unwrap());
        if i == 0 {
            assert!(store.complete_step("r", "owner", i, STEP_DONE, None, None,
                RunNext::Continue { step_index: 1 }, now()).await.unwrap());
        } else {
            let dispatch_id = format!("d-{i}");
            assert!(store.insert_dispatch(&dispatch(&dispatch_id, "r", i)).await.unwrap());
            assert!(store.bind_step_dispatch_awaiting(&dispatch_id, &format!("approval-{i}"),
                "r", i, "owner", now(), StepApprovalBindMode::Filing).await.unwrap());
            {
                let conn = store.conn.lock().await;
                conn.execute("UPDATE autopilot_approval_dispatch SET state=?1, outcome=?2 WHERE dispatch_id=?3",
                    params![if i == 1 { DISPATCH_EXECUTING } else { DISPATCH_SETTLED },
                        if i == 1 { None } else { Some(RESULT_SUCCESS) }, dispatch_id]).unwrap();
            }
            let awaiting = store.get_run("r").await.unwrap().unwrap();
            assert_eq!(awaiting.state, RUN_AWAITING_APPROVAL);
            assert!(awaiting.lease_owner.is_none() && awaiting.lease_until.is_none());
            assert_eq!(store.list_run_steps("r").await.unwrap()[i as usize].dispatch_id, Some(format!("d-{i}")));
            assert!(!store.resume_after_approval("r", i - 1, &dispatch_id, now()).await.unwrap());
            assert!(store.resume_after_approval("r", i, &dispatch_id, now()).await.unwrap());
            assert!(!store.resume_after_approval("r", i, &dispatch_id, now()).await.unwrap());
        }
    }
    let completed = store.get_run("r").await.unwrap().unwrap();
    assert_eq!(completed.state, RUN_SETTLED);
    assert_eq!(completed.outcome.as_deref(), Some("completed"));
    assert!(completed.notice_pending);
    assert!(completed.notice_push_state.is_none());
    assert!(store.list_run_steps("r").await.unwrap().iter().all(|s| s.state == STEP_DONE));
}

#[tokio::test]
async fn run_queries_order_and_settlement_handles_unknown_states() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "first", 1).await;
    let (row, steps) = run("second", "rule", 1);
    store.insert_run(&row, &steps, now() + Duration::seconds(1)).await.unwrap();
    assert_eq!(store.list_runs_for_rule("rule", 1).await.unwrap()[0].run_id, "second");
    assert_eq!(store.list_due_runs(now(), 1).await.unwrap(), vec!["first"]);
    assert!(store.settle_unclaimed_run("first", &[RUN_RUNNING], "cancelled", None, now()).await.is_err());
    assert!(!store.settle_unclaimed_run("first", &[RUN_WAITING], "cancelled", None, now()).await.unwrap());
    {
        let conn = store.conn.lock().await;
        conn.execute("UPDATE autopilot_runs SET state='unknown' WHERE run_id='first'", []).unwrap();
    }
    assert_eq!(store.count_unsettled_runs(Some("rule")).await.unwrap(), 2);
    assert!(!store.claim_run("first", "owner", now()).await.unwrap());
    let before = store.get_run("first").await.unwrap();
    assert!(store.settle_unclaimed_run("first", &["unknown"], "completed", None, now()).await.is_err());
    assert_eq!(store.get_run("first").await.unwrap(), before);
    assert!(store.settle_unclaimed_run("first", &["unknown"], "unsupported_state", None, now()).await.unwrap());
    assert_eq!(store.get_run("first").await.unwrap().unwrap().outcome.as_deref(), Some("unsupported_state"));
}

#[test]
fn step_idempotency_key_is_stable_and_separates_steps() {
    let key = step_idempotency_key("執行🦀", 0);
    assert_eq!(key, step_idempotency_key("執行🦀", 0));
    assert_ne!(key, step_idempotency_key("執行🦀", 1));
    assert_eq!(key, hex::encode(Sha256::digest("autopilot-step:v1:執行🦀:0".as_bytes())));
    assert_eq!(key.len(), 64);
}

#[test]
fn run_states_are_closed_and_timestamps_have_fixed_precision() {
    for state in [RUN_PENDING, RUN_RUNNING, RUN_WAITING, RUN_AWAITING_APPROVAL, RUN_DEFERRED, RUN_SETTLED] {
        assert!(is_known_run_state(state));
    }
    for state in ["", "Pending", "pending-other", "unknown"] { assert!(!is_known_run_state(state)); }
    assert_eq!(run_ts(now()), "2026-11-27T00:00:00.000Z");
    assert!(run_ts(now()) < run_ts(now() + Duration::milliseconds(1)));
}

#[tokio::test]
async fn reset_running_step_requires_current_live_lease() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "r", 2).await;
    assert!(store.claim_run("r", "old", now()).await.unwrap());
    assert!(!store.reset_running_step("r", 0, "old", now()).await.unwrap());
    assert!(store.mark_step_running("r", 0, "old", now()).await.unwrap());
    assert!(!store.reset_running_step("r", 0, "other", now()).await.unwrap());
    assert!(!store.reset_running_step("r", 1, "old", now()).await.unwrap());
    let later = now() + Duration::seconds(RUN_LEASE_SECS + 1);
    assert!(!store.reset_running_step("r", 0, "old", later).await.unwrap());
    assert!(store.reclaim_expired_run("r", "new", later).await.unwrap());
    assert!(!store.reset_running_step("r", 0, "old", later).await.unwrap());
    assert!(store.reset_running_step("r", 0, "new", later).await.unwrap());
    let steps = store.list_run_steps("r").await.unwrap();
    assert_eq!(steps[0].state, STEP_PENDING);
    assert!(steps[0].started_at.is_none());
    assert!(store.mark_step_running("r", 0, "new", later).await.unwrap());
    assert!(store.complete_step("r", "new", 0, STEP_DONE, None, None,
        RunNext::Continue { step_index: 1 }, later).await.unwrap());
    assert!(!store.reset_running_step("r", 0, "new", later).await.unwrap());
}

#[tokio::test]
async fn unsupported_run_scan_excludes_known_reserved_states() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "bad", 1).await;
    insert(&store, "reserved", 1).await;
    insert(&store, "pending", 1).await;
    {
        let conn = store.conn.lock().await;
        conn.execute("UPDATE autopilot_runs SET state='future' WHERE run_id='bad'", []).unwrap();
        conn.execute("UPDATE autopilot_runs SET state='deferred' WHERE run_id='reserved'", []).unwrap();
    }
    let rows = store.list_unsupported_runs(50).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_id, "bad");
    assert!(store.list_unsupported_runs(0).await.unwrap().is_empty());
}

#[tokio::test]
async fn duplicate_run_at_capacity_is_not_a_limit_rejection() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    for i in 0..MAX_UNSETTLED_RUNS_PER_RULE {
        insert(&store, &format!("r{i}"), 1).await;
    }
    let (mut replay, steps) = run("replay", "rule", 1);
    replay.dedupe_key = "dedupe:r0".into();
    assert_eq!(store.insert_run(&replay, &steps, now()).await.unwrap(), RunInsertOutcome::Duplicate);
    assert!(store.get_run("replay").await.unwrap().is_none());
}

#[tokio::test]
async fn fenced_step_start_requires_lease_and_commit() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "r", 1).await;
    assert!(store.claim_run("r", "owner", now()).await.unwrap());
    {
        let (fence, _) = store.fence_rule("rule").await.unwrap();
        assert!(!fence.mark_step_running("r", 0, "other", now()).unwrap());
        assert!(!fence.mark_step_running("r", 0, "owner", now() + Duration::seconds(61)).unwrap());
        assert!(fence.mark_step_running("r", 0, "owner", now()).unwrap());
        // Dropping an uncommitted point of no return rolls it back.
    }
    assert_eq!(store.list_run_steps("r").await.unwrap()[0].state, STEP_PENDING);
    let (fence, _) = store.fence_rule("rule").await.unwrap();
    assert!(fence.mark_step_running("r", 0, "owner", now()).unwrap());
    fence.release().unwrap();
    assert_eq!(store.list_run_steps("r").await.unwrap()[0].state, STEP_RUNNING);
}

#[tokio::test]
async fn fenced_wait_completion_is_atomic_and_lease_guarded() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let (row, mut steps) = run("r", "rule", 2);
    steps[0].step_type = "wait".into();
    store.insert_run(&row, &steps, now()).await.unwrap();
    assert!(store.claim_run("r", "owner", now()).await.unwrap());
    let next_at = now() + Duration::minutes(60);
    {
        let (fence, _) = store.fence_rule("rule").await.unwrap();
        assert!(!fence.complete_wait_step("r", 0, "other", next_at, now()).unwrap());
        assert!(!fence.complete_wait_step("r", 0, "owner", next_at, now() + Duration::seconds(61)).unwrap());
        assert!(fence.complete_wait_step("r", 0, "owner", next_at, now()).unwrap());
    }
    assert_eq!(store.get_run("r").await.unwrap().unwrap().state, RUN_RUNNING);
    assert_eq!(store.list_run_steps("r").await.unwrap()[0].state, STEP_PENDING);
    let (fence, _) = store.fence_rule("rule").await.unwrap();
    assert!(fence.complete_wait_step("r", 0, "owner", next_at, now()).unwrap());
    fence.release().unwrap();
    let waiting = store.get_run("r").await.unwrap().unwrap();
    assert_eq!(waiting.state, RUN_WAITING);
    assert_eq!(waiting.step_index, 1);
    assert_eq!(waiting.next_at, Some(run_ts(next_at)));
    assert!(waiting.lease_owner.is_none() && waiting.lease_until.is_none());
    let steps = store.list_run_steps("r").await.unwrap();
    assert_eq!(steps[0].state, STEP_DONE);
    assert_eq!(steps[0].started_at, Some(run_ts(now())));
    assert_eq!(steps[0].finished_at, Some(run_ts(now())));
}

#[tokio::test]
async fn settlement_transitions_persist_notice_outbox() {
    for outcome in ["completed", "missed_late", "execution_uncertain"] {
        for path in ["unclaimed", "claimed", "step"] {
            let home = tempfile::tempdir().unwrap();
            let store = AutopilotStore::open(home.path()).unwrap();
            insert(&store, "r", 1).await;
            if path == "unclaimed" {
                assert!(store.settle_unclaimed_run("r", &[RUN_PENDING], outcome, None, now()).await.unwrap());
            } else {
                assert!(store.claim_run("r", "owner", now()).await.unwrap());
                if path == "claimed" {
                    assert!(store.settle_claimed_run("r", "owner", outcome, None, now()).await.unwrap());
                } else {
                    assert!(store.mark_step_running("r", 0, "owner", now()).await.unwrap());
                    assert!(store.complete_step("r", "owner", 0, STEP_DONE, None, None,
                        RunNext::Settle { outcome: outcome.into(), detail: None }, now()).await.unwrap());
                }
            }
            let row = store.get_run("r").await.unwrap().unwrap();
            assert!(row.notice_pending);
            assert_eq!(row.notice_push_state.as_deref(), if outcome == "completed" { None } else { Some("pending") });
            assert_eq!(row.notice_push_attempts, 0);
            assert!(row.notice_push_next_at.is_none());
            assert_eq!(store.list_run_notices_due(now(), 10).await.unwrap(), vec![row]);
        }
    }
}

#[tokio::test]
async fn run_notice_push_lease_fences_results_and_bounds_crash_retries() {
    use crate::autopilot_approval::{NoticePush, MAX_NOTICE_PUSH_ATTEMPTS};
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    insert(&store, "r", 1).await;
    assert!(store.settle_unclaimed_run("r", &[RUN_PENDING], "missed_late", None, now()).await.unwrap());
    store.clear_run_notice_pending("r").await.unwrap();
    let mut due = now();
    for attempt in 1..=MAX_NOTICE_PUSH_ATTEMPTS {
        assert!(store.lease_run_notice_push("r", due).await.unwrap());
        assert!(!store.lease_run_notice_push("r", due).await.unwrap());
        let row = store.get_run("r").await.unwrap().unwrap();
        assert_eq!(row.notice_push_attempts, attempt);
        assert_eq!(row.notice_push_next_at, Some(run_ts(due + Duration::seconds(60))));
        assert!(store.list_run_notices_due(due, 10).await.unwrap().is_empty());
        // A result from the prior lease cannot overwrite the current attempt.
        assert!(!store.record_run_notice_push("r", attempt - 1, NoticePush::Sent, due).await.unwrap());
        due += Duration::seconds(60);
    }
    assert!(!store.lease_run_notice_push("r", due).await.unwrap());
    let row = store.get_run("r").await.unwrap().unwrap();
    assert_eq!(row.notice_push_attempts, MAX_NOTICE_PUSH_ATTEMPTS);
    assert_eq!(row.notice_push_state.as_deref(), Some("gave_up"));
    assert!(row.notice_push_next_at.is_none());
    assert!(store.list_run_notices_due(due, 10).await.unwrap().is_empty());
}
