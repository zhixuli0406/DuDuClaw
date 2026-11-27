//! RM-194 decision hooks; intentionally source-only in the delegated package.
use super::*;
use crate::approval::{action_grant::{self, ActionGrant}, ConversationAudience, DecisionContext, ExecutionBinding};

impl Fixture {
    async fn admin_ai(&self) -> ApprovalId {
        self.db().bind_agent(&self.user, "support", duduclaw_auth::AccessLevel::Operator).unwrap();
        rusqlite::Connection::open(self.env.home.join("users.db")).unwrap().execute(
            "UPDATE users SET avatar_agent_id='support', must_change_password=0 WHERE id=?1", [&self.user]).unwrap();
        let id = ApprovalId::new();
        let arguments = json!({"admin_user_id": self.user, "items":[], "reason":"test",
            "check_code_hash":crate::admin_ai::card::check_code_hash(id.as_str(), "A9b2")});
        let mut payload = arguments.clone();
        payload["arguments"] = arguments;
        payload["name"] = json!("admin_settings_propose");
        payload["kind"] = json!(crate::admin_ai::card::ACTION_KIND);
        payload["action_grant"] = ActionGrant::build("support", "admin_settings_propose", &payload).to_json();
        let binding = ExecutionBinding {
            schema_version:1, run_id:uuid::Uuid::new_v4().to_string(), run_origin_kind:"ingress".into(),
            actor_principal:"support".into(), conversation_audience:ConversationAudience::Private,
            decision_context:DecisionContext { channel:"telegram".into(), account_id:"telegram".into(),
                conversation_id:"123456789".into(), principal_id:"123456789".into() },
            task_id:None, task_revision:None, task_snapshot_hash:None,
            payload_hash:crate::approval::payload_hash(&payload),
            policy_revision:crate::approval::policy_revision(&self.env.home, "support").unwrap(),
            cwd:None, environment_hash:"fixture".into(), file_hashes:Default::default(),
            expires_at:(Utc::now() + chrono::Duration::seconds(600)).to_rfc3339(),
            resume_handler:"live_only_v1".into(), resume_version:1,
        };
        let turn = binding.run_id.clone();
        let principal = duduclaw_core::HumanPrincipal::user(&self.user).unwrap().to_string();
        crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal), crate::approval::scope_turn(
            "admin-ai-session", &turn,
            self.env.broker.request_admin_ai(id, "support", payload, binding))).await.unwrap()
    }

    async fn decide_admin_ai(&self, id: &ApprovalId, approve: bool, by: &Decider) -> Result<DecisionOutcome, DecisionRefusal> {
        let rec = self.env.broker.get(id).await.unwrap().unwrap();
        crate::approval::EXPECTED_DECISION_DIGEST.scope(
            Some((id.to_string(), crate::approval::decision_digest_at(&self.env.home, &rec))),
            decide(&self.env, id, approve, "", by)).await
    }
}

#[tokio::test]
async fn only_proposer_may_decide() {
    // Both approval and cancellation use the same gate on both transports.
    for chat in [false, true] {
        for approve in [false, true] {
            let f = Fixture::new();
            let id = f.admin_ai().await;
            let proof = f.proof(&id).await;
            let manager = f.db().create_user("manager@test.invalid", "Manager", "fixture-password", UserRole::Manager).unwrap();
            for other in [&f.ack, &manager.id] {
                let by = if chat {
                    let mut other_proof = proof.clone();
                    other_proof.user_id = other.clone();
                    Decider::ChatAdmin(other_proof)
                } else { Decider::Dashboard(f.db().live_user_context(other).unwrap().unwrap()) };
                assert!(matches!(f.decide_admin_ai(&id, approve, &by).await, Err(DecisionRefusal::Forbidden(_))));
                assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
            }
            let by = if chat { Decider::ChatAdmin(proof) } else { f.dashboard() };
            f.decide_admin_ai(&id, approve, &by).await.unwrap();
            assert_eq!(f.status(&id).await, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
        }
    }
}

#[tokio::test]
async fn demoted_proposer_cannot_decide() {
    for change in ["role='manager'", "status='suspended'", "must_change_password=1", "avatar_agent_id='other'"] {
        let f = Fixture::new();
        let id = f.admin_ai().await;
        let dashboard = f.dashboard();
        let proof = f.proof(&id).await;
        rusqlite::Connection::open(f.env.home.join("users.db")).unwrap().execute(
            &format!("UPDATE users SET {change} WHERE id=?1"), [&f.user]).unwrap();
        for by in [dashboard, Decider::ChatAdmin(proof)] {
            assert!(matches!(f.decide_admin_ai(&id, true, &by).await, Err(DecisionRefusal::Forbidden(_))));
        }
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    }
}

#[tokio::test]
async fn avatar_must_be_unique_and_operator_bound() {
    let f = Fixture::new();
    let id = f.admin_ai().await;
    let dashboard = f.dashboard();
    let db = rusqlite::Connection::open(f.env.home.join("users.db")).unwrap();
    db.execute("DROP INDEX idx_users_avatar", []).unwrap();
    db.execute("UPDATE users SET avatar_agent_id='support' WHERE id=?1", [&f.ack]).unwrap();
    assert!(f.decide_admin_ai(&id, true, &dashboard).await.is_err());
    db.execute("UPDATE users SET avatar_agent_id=NULL WHERE id=?1", [&f.ack]).unwrap();
    f.db().unbind_agent(&f.user, "support").unwrap();
    assert!(f.decide_admin_ai(&id, true, &dashboard).await.is_err());
}

#[tokio::test]
async fn receipt_required_bare_id_and_group_refused() {
    let f = Fixture::new();
    let id = f.admin_ai().await;
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    let context = &rec.binding.as_ref().unwrap().decision_context;
    // The old bound route has no receipt proof; even an exact context and
    // full id cannot decide, in either direction. Codes cannot grant access.
    for approve in [true, false] {
        assert!(f.env.broker.decide_bound(&id, context, approve).await.is_err());
    }
    for code in ["", " wrong", " A9b2"] {
        assert!(crate::approval_notify::apply_decision(&f.env.home, "telegram", "123456789",
            &format!("{id}{code}"), true).await.is_err());
    }
    let proof = f.proof(&id).await;
    for field in ["card_id", "account", "principal", "dm", "digest"] {
        let mut wrong = proof.clone();
        match field {
            "card_id" => wrong.card_id = "missing-receipt".into(),
            "account" => wrong.account = "other-bot".into(),
            "principal" => wrong.principal = "other-person".into(),
            "dm" => wrong.dm = "other-conversation".into(),
            _ => wrong.digest = "wrong-digest".into(),
        }
        assert!(f.decide_admin_ai(&id, true, &Decider::ChatAdmin(wrong)).await.is_err(), "{field}");
    }
    let mut binding = rec.binding.unwrap();
    binding.conversation_audience = ConversationAudience::MultiParty;
    {
        let conn = f.env.broker.store.conn.lock().await;
        conn.execute("UPDATE approvals SET binding_json=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&binding).unwrap(), id.as_str()]).unwrap();
    }
    assert!(f.decide_admin_ai(&id, true, &Decider::ChatAdmin(proof)).await.is_err());
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
}

#[tokio::test]
async fn broker_rechecks_receipt_destination() {
    let f = Fixture::new();
    let id = f.admin_ai().await;
    let proof = f.proof(&id).await;
    let entry = chat_admin::LedgerEntry { user_id:proof.user_id.parse().unwrap(), channel:proof.channel,
        step:proof.step, step_required:false, card_id:proof.card_id.clone(), digest:proof.digest };
    {
        let conn = f.env.broker.store.conn.lock().await;
        conn.execute("UPDATE chat_admin_cards SET dm='other-conversation' WHERE card_id=?1", [&proof.card_id]).unwrap();
    }
    assert!(f.env.broker.decide_bound_chat_admin(&id, &entry, true).await.is_err());
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
}

#[tokio::test]
async fn grant_consumed_once() {
    for chat in [false, true] {
        let f = Fixture::new();
        let id = f.admin_ai().await;
        let by = if chat { Decider::ChatAdmin(f.proof(&id).await) } else { f.dashboard() };
        f.decide_admin_ai(&id, true, &by).await.unwrap();
        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
        let grant = ActionGrant::build("support", "admin_settings_propose", &rec.payload);
        action_grant::verify_and_consume(&f.env.broker, &id, &grant).await.unwrap();
        assert!(action_grant::verify_and_consume(&f.env.broker, &id, &grant).await.is_err());
    }
}

#[tokio::test]
async fn grant_refuses_other_decider_and_missing_ledger() {
    let f = Fixture::new();
    let id = f.admin_ai().await;
    let proof = f.proof(&id).await;
    f.decide_admin_ai(&id, true, &Decider::ChatAdmin(proof)).await.unwrap();
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    let grant = ActionGrant::build("support", "admin_settings_propose", &rec.payload);
    for by in [format!("dashboard:{}", f.ack), format!("chat-admin:{}:telegram:tap", f.ack), "telegram:123456789".into()] {
        {
            let conn = f.env.broker.store.conn.lock().await;
            conn.execute("UPDATE approvals SET decided_by=?1 WHERE id=?2", rusqlite::params![by, id.as_str()]).unwrap();
        }
        assert!(action_grant::verify_and_consume(&f.env.broker, &id, &grant).await.is_err());
    }
    {
        let conn = f.env.broker.store.conn.lock().await;
        conn.execute("UPDATE approvals SET decided_by=?1 WHERE id=?2", rusqlite::params![rec.decided_by, id.as_str()]).unwrap();
        conn.execute("DELETE FROM chat_admin_decisions WHERE approval_id=?1", [id.as_str()]).unwrap();
    }
    assert!(action_grant::verify_and_consume(&f.env.broker, &id, &grant).await.is_err());
}

#[tokio::test]
async fn restart_invalidates_pending() {
    for approved in [false, true] {
        let f = Fixture::new();
        let id = f.admin_ai().await;
        if approved { f.decide_admin_ai(&id, true, &f.dashboard()).await.unwrap(); }
        f.env.broker.invalidate_live_on_restart().await.unwrap();
        assert_eq!(f.status(&id).await, ApprovalStatus::Invalidated);
        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.invalidated_reason.as_deref(), Some("restart_requires_reobserve"));
        assert!(f.decide_admin_ai(&id, true, &f.dashboard()).await.is_err());
        let grant = ActionGrant::build("support", "admin_settings_propose", &rec.payload);
        assert!(action_grant::verify_and_consume(&f.env.broker, &id, &grant).await.is_err());
    }
}

#[tokio::test]
async fn expiry_600_seconds_never_grants() {
    let f = Fixture::new();
    let id = f.admin_ai().await;
    assert_eq!(f.env.broker.get(&id).await.unwrap().unwrap().ttl_seconds, 600);
    {
        let conn = f.env.broker.store.conn.lock().await;
        conn.execute("UPDATE approvals SET created_at=?1 WHERE id=?2", rusqlite::params![
            (Utc::now() - chrono::Duration::seconds(601)).to_rfc3339(), id.as_str()]).unwrap();
    }
    assert!(matches!(f.decide_admin_ai(&id, true, &f.dashboard()).await, Err(DecisionRefusal::Expired(_))));
    assert_eq!(f.status(&id).await, ApprovalStatus::Expired);
}
