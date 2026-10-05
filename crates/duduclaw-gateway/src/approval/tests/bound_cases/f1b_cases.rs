//! F1b: scripted fixture decisions never reach a formal run, and a
//! workflow activation is decided by a current Admin in the dashboard only.
use super::*;
use crate::approval::{FIXTURE_DECISION_KIND, WORKFLOW_ACTIVATION_KIND};
use crate::workflow_drafts::FixtureDecision;

#[tokio::test]
async fn fixture_decisions_are_recorded_decided_and_never_pushed() {
    let home = fixture();
    let b = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"step": "confirm"});
    let bound = binding(home.path(), &payload, "workflow_v1");
    let id = b
        .record_fixture_decision(
            "k-approve",
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            bound.clone(),
            &FixtureDecision::Approve,
        )
        .await
        .unwrap();
    let rec = b.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.status, ApprovalStatus::Approved);
    assert_eq!(rec.action_kind, FIXTURE_DECISION_KIND);
    assert!(rec.decided_by.as_deref().unwrap().starts_with("fixture:"));
    assert!(rec.notify_channel.is_none() && rec.notify_chat_id.is_none());
    // Idempotent per key.
    let again = b
        .record_fixture_decision(
            "k-approve",
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            bound.clone(),
            &FixtureDecision::Approve,
        )
        .await
        .unwrap();
    assert_eq!(again, id);
    // A scripted approval stands behind an operation of the fixture run.
    let op = b.prepare_operation(&id, "confirm", None).await.unwrap();
    b.claim_operation(&op, &bound, "worker", 30).await.unwrap();
    // "A person refused" is expressible.
    let denied = b
        .record_fixture_decision(
            "k-deny",
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            binding(home.path(), &payload, "workflow_v1"),
            &FixtureDecision::Deny,
        )
        .await
        .unwrap();
    assert_eq!(b.poll(&denied).await.unwrap(), ApprovalStatus::Denied);
    let q = json!({"q": "which?"});
    let answered = b
        .record_fixture_decision(
            "k-answer",
            RequestKind::Question,
            "alice",
            "s",
            q.clone(),
            binding(home.path(), &q, "workflow_v1"),
            &FixtureDecision::Answer { text: "B".into() },
        )
        .await
        .unwrap();
    let rec = b.get(&answered).await.unwrap().unwrap();
    assert_eq!(rec.status, ApprovalStatus::Answered);
    assert_eq!(rec.answer, Some(json!("B")));
    // An answer cannot approve an action.
    assert!(
        b.record_fixture_decision(
            "k-mismatch",
            RequestKind::Approval,
            "alice",
            "s",
            payload.clone(),
            binding(home.path(), &payload, "workflow_v1"),
            &FixtureDecision::Answer { text: "yes".into() },
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn fixture_decisions_are_refused_for_formal_runs() {
    let home = fixture();
    let b = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"step": "confirm"});
    // A run that is not a fixture-trigger run (an activated projection).
    let formal = super::resume_cases::activated_binding_for(home.path(), &payload);
    assert!(
        b.record_fixture_decision(
            "k-formal",
            RequestKind::Approval,
            "alice",
            "s",
            payload,
            formal,
            &FixtureDecision::Approve,
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn activation_cards_are_admin_and_dashboard_only() {
    let home = fixture();
    let b = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"kind": "workflow_activation", "activation_id": "a1"});
    let id = b
        .request_bound(
            RequestKind::Approval,
            "alice",
            "接受固定工作流版本與排程範圍",
            payload.clone(),
            unseeded_binding(home.path(), &payload, "workflow_v1"),
        )
        .await
        .unwrap();
    let rec = b.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.action_kind, WORKFLOW_ACTIVATION_KIND);
    assert!(crate::approval_notify::is_dashboard_only_kind(
        &rec.action_kind
    ));
    // A channel press never decides it, whatever its context.
    assert!(b.decide_bound(&id, &context(), true).await.is_err());
    // A manager (no users.db: the local system principal) is not enough.
    let mut manager = duduclaw_auth::UserContext::admin_fallback();
    manager.role = duduclaw_auth::UserRole::Manager;
    assert!(b.decide_bound_dashboard(&id, &manager, true).await.is_err());
    assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Pending);
    b.decide_bound_dashboard(&id, &duduclaw_auth::UserContext::admin_fallback(), true)
        .await
        .unwrap();
    assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Approved);
}

/// F5-A R-M2: a claim refused because another executor's claim is still
/// live says so, so the runner retries instead of failing the step.
#[tokio::test]
async fn a_live_claim_by_another_executor_is_reported_as_such() {
    let home = fixture();
    let payload = json!({"send": "lease"});
    let bound = binding(home.path(), &payload, "workflow_v1");
    let b = ApprovalBroker::open(home.path()).unwrap();
    let id = b
        .request_bound(RequestKind::Approval, "alice", "s", payload, bound.clone())
        .await
        .unwrap();
    b.decide_bound(&id, &context(), true).await.unwrap();
    let op = b.prepare_operation(&id, "step", None).await.unwrap();
    b.claim_operation(&op, &bound, "first", 300).await.unwrap();
    let err = b.claim_operation(&op, &bound, "second", 300).await.unwrap_err();
    assert_eq!(err, crate::approval::OPERATION_LEASE_HELD);
}
