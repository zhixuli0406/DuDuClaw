//! Tests for the dashboard approval in front of forget-by-source apply (C-1).

use super::*;
use crate::approval::ApprovalBroker;
use crate::memory_provenance::test_seed;
use duduclaw_memory::{ForgetSelector, PlanOutcome, SourceRef, SqliteMemoryEngine};

const AGENT: &str = "agnes";
const SESSION: &str = "telegram:1";

async fn planned(home: &Path) -> ForgetPlan {
    let engine = SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    test_seed(
        &engine,
        AGENT,
        "likes tea",
        SourceRef::channel_message(SESSION, 3, chrono::Utc::now(), None),
    )
    .await;
    let sel = ForgetSelector {
        session: SESSION.into(),
        messages: vec!["m:3".into()],
        upto_seq: None,
        upto_time: None,
    };
    match engine
        .plan_forget_source(AGENT, &sel, Default::default(), &Default::default())
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => *p,
        other => panic!("{other:?}"),
    }
}

async fn the_request(home: &Path) -> ApprovalRecord {
    let broker = ApprovalBroker::open(home).unwrap();
    let mut all = broker
        .list_by_kind(ACTION_KIND_MEMORY_FORGET_SOURCE)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    all.remove(0)
}

#[tokio::test]
async fn unrequested_pending_approved_and_denied_verdicts() {
    let home = tempfile::tempdir().unwrap();
    let plan = planned(home.path()).await;
    assert_eq!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::Missing
    );

    request_for_plan(
        home.path(),
        &plan,
        "summary",
        serde_json::json!({"memories": 1}),
    )
    .await
    .unwrap();
    let rec = the_request(home.path()).await;
    assert!(matches!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::Pending { .. }
    ));
    // The card carries counts and ids only.
    assert_eq!(rec.payload["plan_hash"], plan.plan_hash);
    assert!(!rec.payload.to_string().contains("likes tea"));

    let broker = ApprovalBroker::open(home.path()).unwrap();
    broker
        .decide(&rec.id, true, "dashboard:admin")
        .await
        .unwrap();
    assert!(matches!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::Approved { .. }
    ));

    // A second plan whose request is denied.
    let home2 = tempfile::tempdir().unwrap();
    let plan2 = planned(home2.path()).await;
    request_for_plan(home2.path(), &plan2, "s", serde_json::json!({}))
        .await
        .unwrap();
    let rec2 = the_request(home2.path()).await;
    ApprovalBroker::open(home2.path())
        .unwrap()
        .decide(&rec2.id, false, "dashboard:admin")
        .await
        .unwrap();
    assert!(matches!(
        verdict_for_plan(home2.path(), &plan2).await.unwrap(),
        GateVerdict::Refused { ref status, .. } if status == "denied"
    ));
}

#[tokio::test]
async fn approval_for_another_hash_does_not_count() {
    let home = tempfile::tempdir().unwrap();
    let mut plan = planned(home.path()).await;
    request_for_plan(home.path(), &plan, "s", serde_json::json!({}))
        .await
        .unwrap();
    let rec = the_request(home.path()).await;
    ApprovalBroker::open(home.path())
        .unwrap()
        .decide(&rec.id, true, "dashboard:admin")
        .await
        .unwrap();
    plan.plan_hash = "0".repeat(64);
    assert!(matches!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::HashMismatch { .. }
    ));
}

#[tokio::test]
async fn an_expired_request_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let plan = planned(home.path()).await;
    request_for_plan(home.path(), &plan, "s", serde_json::json!({}))
        .await
        .unwrap();
    // Age the row past its TTL.
    let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
    conn.execute(
        "UPDATE approvals SET created_at = '2000-01-01T00:00:00+00:00'",
        [],
    )
    .unwrap();
    assert!(matches!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::Refused { ref status, .. } if status == "expired"
    ));
}

#[tokio::test]
async fn a_channel_decision_is_refused_and_the_request_stays_pending() {
    use crate::decision_action::{DecisionAct, DecisionSource};
    let home = tempfile::tempdir().unwrap();
    let plan = planned(home.path()).await;
    request_for_plan(home.path(), &plan, "s", serde_json::json!({}))
        .await
        .unwrap();
    let rec = the_request(home.path()).await;
    let broker = ApprovalBroker::open(home.path()).unwrap();
    broker
        .set_notify_target_for_test(&rec.id, "telegram", "555")
        .await
        .unwrap();
    for data in [
        crate::decision_action::encode(
            DecisionSource::Approval,
            DecisionAct::Approve,
            rec.id.as_str(),
        ),
        format!("duduclaw:approval_ok:{}", rec.id.as_str()),
    ] {
        let out =
            crate::approval_notify::decide_from_channel(home.path(), "telegram", "555", &data)
                .await
                .unwrap();
        assert_eq!(
            out,
            Err(crate::approval_notify::DASHBOARD_ONLY_REFUSAL.to_string())
        );
    }
    assert!(matches!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::Pending { .. }
    ));
}

#[test]
fn the_notice_names_counts_but_no_content() {
    let rec = ApprovalRecord {
        id: crate::approval::ApprovalId::new(),
        agent_id: AGENT.into(),
        action_kind: ACTION_KIND_MEMORY_FORGET_SOURCE.into(),
        summary: "likes tea".into(),
        payload: serde_json::json!({"counts": {"memories": 2, "session_messages": 1}}),
        status: ApprovalStatus::Pending,
        created_at: chrono::Utc::now().to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: 600,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: None,
        simulation: None,
        request_kind: crate::approval::RequestKind::Approval,
        binding: None,
        answer: None,
        invalidated_reason: None,
    };
    let body = crate::approval_notify::dashboard_only_notice_body(&rec, false);
    assert!(
        body.contains("記憶 2 筆") && body.contains("儀表板"),
        "{body}"
    );
    assert!(!body.contains("likes tea"));
}

/// Only an Admin decides a forget request in the dashboard; a Manager's
/// approval leaves it pending.
#[tokio::test]
async fn only_an_admin_decides_in_the_dashboard() {
    use duduclaw_auth::{UserContext, UserRole};
    let home = tempfile::tempdir().unwrap();
    let plan = planned(home.path()).await;
    request_for_plan(home.path(), &plan, "s", serde_json::json!({}))
        .await
        .unwrap();
    let rec = the_request(home.path()).await;
    let handler = crate::handlers::MethodHandler::new(home.path().to_path_buf()).await;
    let ok = |f: &crate::protocol::WsFrame| {
        matches!(f, crate::protocol::WsFrame::Response { ok: true, .. })
    };
    let mut manager = UserContext::admin_fallback();
    manager.role = UserRole::Manager;
    manager.user_id = "manager-1".into();
    let refused = handler
        .handle(
            "approvals.decide",
            serde_json::json!({ "id": rec.id.as_str(), "approve": true }),
            &manager,
        )
        .await;
    assert!(!ok(&refused));
    assert!(matches!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::Pending { .. }
    ));
    let decided = handler
        .handle(
            "approvals.decide",
            serde_json::json!({ "id": rec.id.as_str(), "approve": true }),
            &UserContext::admin_fallback(),
        )
        .await;
    assert!(ok(&decided), "{decided:?}");
    assert!(matches!(
        verdict_for_plan(home.path(), &plan).await.unwrap(),
        GateVerdict::Approved { .. }
    ));
}
