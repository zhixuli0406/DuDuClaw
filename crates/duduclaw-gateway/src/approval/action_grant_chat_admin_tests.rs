use super::*;
use crate::approval::{ConversationAudience, DecisionContext, ExecutionBinding, RequestKind,
    chat_admin::ledger::consumer_test_support::{self, Evidence}, payload_hash, policy_revision};
use serde_json::json;

async fn fixture() -> (tempfile::TempDir, ApprovalBroker, ApprovalId, ActionGrant) {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
    std::fs::write(home.path().join("agents/alice/agent.toml"), "[capabilities]\n").unwrap();
    // Ensure a failing ledger cannot fall through to requester authority.
    std::fs::write(home.path().join("config.toml"), "[security]\nmail_requester_approval=false\n").unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let mut payload = json!({"name":"mail_send", "arguments":{
        "to":"recipient@example.com", "subject":"Test", "body":"Mail body"},
        "egress_guard":{"sources":["memory_read"]}});
    let grant = ActionGrant::build("alice", "mail_send", &payload);
    payload[ACTION_GRANT_KEY] = grant.to_json();
    let binding = ExecutionBinding {
        conversation_audience: ConversationAudience::Private,
        schema_version: 1, run_id: "test-turn".into(), run_origin_kind: "ingress".into(),
        actor_principal: "alice".into(), decision_context: DecisionContext {
            channel: "telegram".into(), account_id: "bot".into(),
            conversation_id: "human".into(), principal_id: "human".into(),
        },
        task_id: None, task_revision: None, task_snapshot_hash: None,
        payload_hash: payload_hash(&payload), policy_revision: policy_revision(home.path(), "alice").unwrap(),
        cwd: None, environment_hash: crate::approval::mail_requester::environment_hash(home.path(), "alice", "test-turn").unwrap(),
        file_hashes: Default::default(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
        resume_handler: "live_only_v1".into(), resume_version: 1,
    };
    let id = broker.request_bound(RequestKind::Approval, "alice", "mail", payload, binding).await.unwrap();
    (home, broker, id, grant)
}

async fn consume(evidence: Evidence) -> Result<(), GrantRefusal> {
    let (home, broker, id, grant) = fixture().await;
    {
        let conn = broker.store.conn.lock().await;
        consumer_test_support::install(&conn, id.as_str(), evidence);
    }
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert!(!crate::approval::mail_requester::eligible(home.path(), &rec));
    let result = verify_and_consume(&broker, &id, &grant).await;
    if result.is_ok() {
        assert_eq!(verify_and_consume(&broker, &id, &grant).await, Err(GrantRefusal::NotApproved));
    } else {
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Approved);
    }
    result
}

#[tokio::test]
async fn decided_by_without_ledger_refused() {
    assert_eq!(consume(Evidence::Missing).await,
        Err(GrantRefusal::Store("mail requester authority changed".into())));
}

#[tokio::test]
async fn chat_admin_grant_requires_matching_ledger_and_step() {
    for &(evidence, accepted) in consumer_test_support::CASES {
        let result = consume(evidence).await;
        if accepted {
            assert_eq!(result, Ok(()), "{evidence:?}");
        } else {
            assert_eq!(result, Err(GrantRefusal::Store("mail requester authority changed".into())), "{evidence:?}");
        }
    }
}
