//! F1a: a decision on a workflow step queues the run's resume in the same
//! transaction, one decidable card per step, and the ledger refuses
//! backwards state changes and deletions.
use super::*;

fn outbox_rows(home: &Path) -> Vec<(String, String, String, String)> {
    let conn = rusqlite::Connection::open(home.join("workflow.db")).unwrap();
    let mut q = conn
        .prepare("SELECT outbox_id,kind,entity_id,payload_json FROM workflow_outbox ORDER BY rowid")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A binding backed by an activated run's projection (only the fields the
/// decision transaction reads).
pub(super) fn activated_binding_for(home: &Path, payload: &Value) -> ExecutionBinding {
    activated_binding(home, payload)
}

fn activated_binding(home: &Path, payload: &Value) -> ExecutionBinding {
    let bound = unseeded_binding(home, payload, "workflow_v1");
    drop(crate::workflow::WorkflowStore::open(home).unwrap());
    rusqlite::Connection::open(home.join("workflow.db"))
        .unwrap()
        .execute(
            "INSERT INTO workflow_runs VALUES(?1,?1,'wf',1,'h',?2,'waiting_approval',?3)",
            rusqlite::params![
                bound.run_id,
                json!({"run_id": bound.run_id, "activation_id": "act-1", "actor": "alice"})
                    .to_string(),
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
    bound
}

#[tokio::test]
async fn decision_on_activated_run_writes_one_resume_row_in_the_decision() {
    for (approve, kind) in [
        (true, RequestKind::Approval),
        (false, RequestKind::Approval),
    ] {
        let home = fixture();
        let payload = json!({"step":"confirm"});
        let bound = activated_binding(home.path(), &payload);
        let b = ApprovalBroker::open(home.path()).unwrap();
        let id = b
            .request_bound(kind, "alice", "confirm", payload.clone(), bound.clone())
            .await
            .unwrap();
        assert!(outbox_rows(home.path()).is_empty());
        b.decide_bound(&id, &context(), approve).await.unwrap();
        let (outbox_id, payload) =
            crate::workflow::resume_outbox(&bound.run_id, "alice", id.as_str());
        assert_eq!(
            outbox_rows(home.path()),
            vec![(outbox_id, "resume".into(), bound.run_id.clone(), payload)],
            "approve={approve}"
        );
        // A second decision is refused and adds nothing.
        assert!(b.decide_bound(&id, &context(), approve).await.is_err());
        assert_eq!(outbox_rows(home.path()).len(), 1);
    }
}

#[tokio::test]
async fn answered_question_resumes_and_fixture_runs_do_not() {
    let home = fixture();
    let payload = json!({"options":["A","B"]});
    let bound = activated_binding(home.path(), &payload);
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(
            RequestKind::Question,
            "alice",
            "pick",
            payload.clone(),
            bound.clone(),
        )
        .await
        .unwrap();
    b.answer_question(&id, &context(), json!("B"))
        .await
        .unwrap();
    assert_eq!(outbox_rows(home.path()).len(), 1);
    // Fixture runs (no activation) are driven by their own caller.
    let fixture_bound = binding(home.path(), &payload, "workflow_v1");
    let id = b
        .request_bound(
            RequestKind::Approval,
            "alice",
            "x",
            payload.clone(),
            fixture_bound,
        )
        .await
        .unwrap();
    b.decide_bound(&id, &context(), true).await.unwrap();
    assert_eq!(outbox_rows(home.path()).len(), 1);
}

#[tokio::test]
async fn keyed_request_returns_the_same_card_and_refuses_a_changed_contract() {
    let home = fixture();
    let payload = json!({"step":"confirm"});
    let bound = binding(home.path(), &payload, "workflow_v1");
    let b = ApprovalBroker::open(home.path()).unwrap();
    let first = b
        .request_bound_keyed(
            "run:step",
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            bound.clone(),
        )
        .await
        .unwrap();
    let again = b
        .request_bound_keyed(
            "run:step",
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            bound.clone(),
        )
        .await
        .unwrap();
    assert_eq!(first, again);
    assert!(uuid::Uuid::parse_str(first.as_str()).is_ok());
    let count: i64 = rusqlite::Connection::open(home.path().join("approvals.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM approvals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "one decidable card per step");
    let other = json!({"step":"changed"});
    let mut changed = bound.clone();
    changed.payload_hash = payload_hash(&other);
    assert!(
        b.request_bound_keyed(
            "run:step",
            RequestKind::Approval,
            "alice",
            "s",
            other,
            changed
        )
        .await
        .is_err()
    );
    assert_ne!(
        b.request_bound_keyed(
            "run:other",
            RequestKind::Approval,
            "alice",
            "s",
            payload,
            bound
        )
        .await
        .unwrap(),
        first
    );
}

#[tokio::test]
async fn decided_card_and_operation_state_cannot_move_backwards_or_vanish() {
    let home = fixture();
    let payload = json!({"step":"confirm"});
    let b = ApprovalBroker::open(home.path()).unwrap();
    let denied = b
        .request_bound(
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            binding(home.path(), &payload, "workflow_v1"),
        )
        .await
        .unwrap();
    b.decide_bound(&denied, &context(), false).await.unwrap();
    let approved = b
        .request_bound(
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            binding(home.path(), &payload, "workflow_v1"),
        )
        .await
        .unwrap();
    b.decide_bound(&approved, &context(), true).await.unwrap();
    let op = b.prepare_operation(&approved, "step", None).await.unwrap();
    let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
    for status in ["pending", "approved"] {
        assert!(
            conn.execute(
                "UPDATE approvals SET status=?1 WHERE id=?2",
                [status, denied.as_str()]
            )
            .is_err(),
            "denied -> {status}"
        );
    }
    assert!(
        conn.execute(
            "UPDATE approvals SET status='pending' WHERE id=?1",
            [approved.as_str()]
        )
        .is_err()
    );
    assert!(
        conn.execute("DELETE FROM approvals WHERE id=?1", [denied.as_str()])
            .is_err()
    );
    // Invalidating a decided card stays possible (cancellation, scrub), and
    // is final.
    b.invalidate_request(&approved, "test").await.unwrap();
    assert!(
        conn.execute(
            "UPDATE approvals SET status='approved' WHERE id=?1",
            [approved.as_str()]
        )
        .is_err()
    );
    for state in ["succeeded", "failed", "uncertain"] {
        assert!(
            conn.execute(
                "UPDATE approval_operations SET state=?1 WHERE operation_id=?2",
                [state, op.as_str()]
            )
            .is_err(),
            "prepared -> {state} skips execution"
        );
    }
    assert!(
        conn.execute(
            "UPDATE approval_operations SET payload_json='{}' WHERE operation_id=?1",
            [op.as_str()]
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "DELETE FROM approval_operations WHERE operation_id=?1",
            [op.as_str()]
        )
        .is_err()
    );
}

/// F1b item 9: a prepared row is only a reservation. One made while the card
/// was pending (the computer-use pattern) cannot be claimed after a denial,
/// so a refused card never stands behind an execution.
#[tokio::test]
async fn prepared_operation_on_a_denied_card_is_never_claimable() {
    let home = fixture();
    let payload = json!({"send": "x"});
    let bound = binding(home.path(), &payload, "workflow_v1");
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "s", payload, bound.clone())
        .await
        .unwrap();
    let op = b.prepare_operation(&id, "step", None).await.unwrap();
    assert!(b.claim_operation(&op, &bound, "worker", 30).await.is_err());
    b.decide_bound(&id, &context(), false).await.unwrap();
    assert!(b.claim_operation(&op, &bound, "worker", 30).await.is_err());
    assert_eq!(
        b.inspect_operation(&op).await.unwrap().unwrap().state,
        OperationState::Prepared
    );
}
