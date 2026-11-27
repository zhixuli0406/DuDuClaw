//! Source-only P7 regression tests; gateway compilation belongs to review.
use super::*;
use crate::approval::chat_admin::{self, ChatChannel, LedgerEntry, ledger::Step, receipts::{self, Receipt}};

const USER: &str = "3f2afc12-4f13-4a28-8b7f-df31eadbb013";

async fn entry_for(broker: &ApprovalBroker, id: &ApprovalId) -> LedgerEntry {
    let rec = broker.get(id).await.unwrap().unwrap();
    let entry = LedgerEntry {
        user_id: USER.parse().unwrap(), channel: ChatChannel::Telegram,
        step: Step::Web, step_required: true, card_id: uuid::Uuid::new_v4().to_string(),
        digest: decision_digest_at(&broker.home_dir().unwrap(), &rec),
    };
    receipts::insert(&broker.store, &Receipt {
        card_id: entry.card_id.clone(), approval_id: id.to_string(), purpose: "rm143".into(),
        user_id: USER.into(), channel: "telegram".into(), account: "bot".into(),
        principal: "12345".into(), dm: "12345".into(), message_id: Some(id.to_string()),
        digest: entry.digest.clone(), tier: "T2".into(),
        valid_until: rec.deadline_rfc3339().unwrap(), status: "live".into(),
        sent_at: Utc::now().to_rfc3339(),
    }).await.unwrap();
    entry
}

async fn assert_pending(broker: &ApprovalBroker, id: &ApprovalId, entry: &LedgerEntry) {
    let rec = broker.get(id).await.unwrap().unwrap();
    assert_eq!(rec.status, ApprovalStatus::Pending);
    assert_eq!(rec.decided_at, None);
    assert_eq!(rec.decided_by, None);
    let conn = broker.store.conn.lock().await;
    let status: String = conn.query_row("SELECT status FROM chat_admin_cards WHERE card_id=?1",
        [&entry.card_id], |r| r.get(0)).unwrap();
    assert_eq!(status, "live");
    let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1",
        [id.as_str()], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

fn resume_count(home: &Path) -> i64 {
    rusqlite::Connection::open(home.join("workflow.db")).unwrap()
        .query_row("SELECT count(*) FROM workflow_outbox WHERE kind='resume'", [], |r| r.get(0)).unwrap()
}

// The same deserializable activation projection as live_event_cases. This
// exercises the actual dashboard facts/digest (including live pricing).
fn activation_payload() -> Value {
    let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let creator = json!({"actor":"alice", "allowed_tools":[], "policy_revision":"p"});
    let request = json!({
        "activation_id":"activation", "draft_id":"draft", "draft_hash":"hash",
        "revision": {
            "definition": {"schema_version":1, "workflow_id":"wf", "revision":1,
                "skill_revision_hash":"skill", "input_schema":{"type":"null"},
                "output_schema":{"type":"null"}, "required_capabilities":[], "steps":[]},
            "revision_hash":"revision", "owner":"alice", "creator_grant":creator,
            "audience":[], "fixtures_digest":"fixtures", "acceptance_id":"",
            "accepted_at":"", "expires_at":expires
        },
        "spec": {
            "schema_version":1, "workflow_id":"wf", "workflow_revision":1,
            "revision_hash":"revision", "skill_hash":"skill", "fixtures_digest":"fixtures",
            "activation_id":"activation", "actor":"alice",
            "operator_context":{"channel":"dashboard", "account_id":"system",
                "conversation_id":"approval", "principal_id":USER},
            "creator_grant":creator, "audience":[], "templates":{},
            "input_max_age_seconds":60, "fixtures_expires_at":expires,
            "budget":{"per_run_micros":100, "monthly_micros":1000, "max_consecutive_failures":3},
            "expires_at":expires, "policy_revision":"p"
        },
        "fixture_results":[], "fixture_assertions":[], "cron":null
    });
    let _: crate::workflow::ActivationRequest = serde_json::from_value(request.clone()).unwrap();
    json!({"kind":WORKFLOW_ACTIVATION_KIND, "request":request})
}

#[tokio::test]
async fn channel_press_still_refused_for_activation() {
    let home = fixture();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let payload = activation_payload();
    let bound = binding(home.path(), &payload, "live_only_v1");
    let id = broker.request_bound(RequestKind::Approval, "alice", "activate", payload, bound).await.unwrap();
    for approve in [true, false] {
        let error = broker.decide_bound(&id, &context(), approve).await.unwrap_err();
        assert!(error.contains("workflow activation"), "{error}");
    }
    assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
}

#[tokio::test]
async fn chat_admin_activation_writes_identity_ledger_and_consumes_receipt() {
    for approve in [true, false] {
        let home = fixture();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let payload = activation_payload();
        let bound = binding(home.path(), &payload, "live_only_v1");
        let id = broker.request_bound(RequestKind::Approval, "alice", "activate", payload, bound).await.unwrap();
        let entry = entry_for(&broker, &id).await;
        let rec = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(entry.digest, decision_digest_with_activation(&rec, &activation_facts(home.path(), &rec)));
        assert_ne!(entry.digest, decision_digest(&rec));
        broker.decide_bound_chat_admin(&id, &entry, approve).await.unwrap();
        let saved = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(saved.status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
        assert_eq!(saved.decided_by, Some(format!("chat-admin:{USER}:telegram:web")));
        let conn = broker.store.conn.lock().await;
        assert!(chat_admin::ledger::accepts(&conn, &saved));
        let evidence: (String, String, String, bool, bool) = conn.query_row(
            "SELECT c.status,d.digest,d.decided_at,d.step_required,d.approve
             FROM chat_admin_cards c JOIN chat_admin_decisions d ON d.card_id=c.card_id
             WHERE d.approval_id=?1", [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!(evidence, ("consumed".into(), entry.digest.clone(), saved.decided_at.unwrap(), true, approve));
        drop(conn);
        assert!(broker.decide_bound_chat_admin(&id, &entry, approve).await.is_err());
    }
}

#[tokio::test]
async fn chat_admin_bound_egress_decision_resumes_workflow_atomically() {
    for approve in [true, false] {
        let home = fixture();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let payload = json!({"name":"outgoing", "egress_guard":{}});
        let bound = super::resume_cases::activated_binding_for(home.path(), &payload);
        let id = broker.request_bound(RequestKind::Approval, "alice", "send", payload, bound).await.unwrap();
        let entry = entry_for(&broker, &id).await;
        assert!(broker.decide_bound(&id, &context(), approve).await.is_err());
        assert_eq!(resume_count(home.path()), 0);
        broker.decide_bound_chat_admin(&id, &entry, approve).await.unwrap();
        assert_eq!(resume_count(home.path()), 1);
        let rec = broker.get(&id).await.unwrap().unwrap();
        let conn = broker.store.conn.lock().await;
        assert!(chat_admin::ledger::accepts(&conn, &rec));
    }
}

#[tokio::test]
async fn chat_admin_member_confirmation_card_refused() {
    let home = fixture();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"name":"google_chat_send", "chat_member_confirmation":true, "egress_guard":{}});
    let bound = binding(home.path(), &payload, "live_only_v1");
    let id = broker.request_bound(RequestKind::Approval, "alice", "confirm", payload, bound).await.unwrap();
    let entry = entry_for(&broker, &id).await;
    let error = broker.decide_bound_chat_admin(&id, &entry, true).await.unwrap_err();
    assert_eq!(error, "Only the requesting member may confirm");
    assert_pending(&broker, &id, &entry).await;
}

#[tokio::test]
async fn chat_admin_stale_activation_digest_keeps_pending() {
    let home = fixture();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let payload = activation_payload();
    let bound = binding(home.path(), &payload, "live_only_v1");
    let id = broker.request_bound(RequestKind::Approval, "alice", "activate", payload, bound).await.unwrap();
    let entry = entry_for(&broker, &id).await;
    std::fs::write(home.path().join("config.toml"), "[workflow.unit_cost_micros]\napproval=17\n").unwrap();
    // No task-local expected digest here: the broker independently validates
    // the receipt against the current activation facts.
    for approve in [true, false] {
        let error = broker.decide_bound_chat_admin(&id, &entry, approve).await.unwrap_err();
        assert!(error.contains("digest mismatch"), "{error}");
    }
    assert_pending(&broker, &id, &entry).await;
}

#[tokio::test]
async fn chat_admin_bound_required_step_cannot_be_tap() {
    let home = fixture();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let payload = activation_payload();
    let bound = binding(home.path(), &payload, "live_only_v1");
    let id = broker.request_bound(RequestKind::Approval, "alice", "activate", payload, bound).await.unwrap();
    let mut entry = entry_for(&broker, &id).await;
    entry.step = Step::Tap;
    let error = broker.decide_bound_chat_admin(&id, &entry, true).await.unwrap_err();
    assert!(error.contains("requires step-up"), "{error}");
    assert_pending(&broker, &id, &entry).await;
}

#[tokio::test]
async fn dashboard_bound_decision_preserves_identity_and_does_not_consume_chat_receipt() {
    let home = fixture();
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let user = users.create_user("admin@example.com", "Admin", "fixture-password", duduclaw_auth::UserRole::Admin).unwrap();
    let ctx = users.live_user_context(&user.id).unwrap().unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    for payload in [activation_payload(), json!({"egress_guard":{}})] {
        let bound = binding(home.path(), &payload, "live_only_v1");
        let id = broker.request_bound(RequestKind::Approval, "alice", "review", payload, bound).await.unwrap();
        let entry = entry_for(&broker, &id).await;
        broker.decide_bound_dashboard(&id, &ctx, true).await.unwrap();
        let rec = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.status, ApprovalStatus::Approved);
        assert_eq!(rec.decided_by, Some(format!("dashboard:{}", user.id)));
        let conn = broker.store.conn.lock().await;
        let status: String = conn.query_row("SELECT status FROM chat_admin_cards WHERE card_id=?1",
            [&entry.card_id], |r| r.get(0)).unwrap();
        assert_eq!(status, "live");
        assert!(!chat_admin::ledger::accepts(&conn, &rec));
    }
}

#[tokio::test]
async fn chat_admin_bound_ledger_insert_failure_rolls_back_status_and_receipt() {
    let home = fixture();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"egress_guard":{}});
    let bound = super::resume_cases::activated_binding_for(home.path(), &payload);
    let id = broker.request_bound(RequestKind::Approval, "alice", "send", payload, bound).await.unwrap();
    let entry = entry_for(&broker, &id).await;
    broker.store.conn.lock().await.execute_batch(
        "CREATE TRIGGER fail_chat_ledger BEFORE INSERT ON chat_admin_decisions
         BEGIN SELECT RAISE(ABORT, 'fixture ledger failure'); END;"
    ).unwrap();
    let error = broker.decide_bound_chat_admin(&id, &entry, true).await.unwrap_err();
    assert!(error.contains("insert chat-admin decision"), "{error}");
    assert_pending(&broker, &id, &entry).await;
    assert_eq!(resume_count(home.path()), 0);
}

#[tokio::test]
async fn chat_admin_bound_resume_failure_rolls_back_status_ledger_and_receipt() {
    let home = fixture();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let payload = json!({"egress_guard":{}});
    let bound = super::resume_cases::activated_binding_for(home.path(), &payload);
    let id = broker.request_bound(RequestKind::Approval, "alice", "send", payload, bound).await.unwrap();
    let entry = entry_for(&broker, &id).await;
    rusqlite::Connection::open(home.path().join("workflow.db")).unwrap().execute_batch(
        "CREATE TRIGGER fail_chat_resume BEFORE INSERT ON workflow_outbox
         BEGIN SELECT RAISE(ABORT, 'fixture resume failure'); END;"
    ).unwrap();
    let error = broker.decide_bound_chat_admin(&id, &entry, true).await.unwrap_err();
    assert!(error.contains("fixture resume failure"), "{error}");
    assert_pending(&broker, &id, &entry).await;
    assert_eq!(resume_count(home.path()), 0);
}
