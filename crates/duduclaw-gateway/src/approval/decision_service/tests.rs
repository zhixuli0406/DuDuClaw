//! Service integration fixtures mirror the existing knowledge/quarantine and
//! ledger tests. These tests are intentionally not run in the P5 sandbox.
use super::*;
use chrono::Utc;
use duduclaw_auth::{UserRole, UserStatus};
use crate::approval::chat_admin::receipts::{self, Receipt};
use crate::knowledge::add_source_approval as sources;

pub(crate) struct Fixture {
    _home: tempfile::TempDir,
    pub(crate) env: DecisionEnv,
    pub(crate) user: String,
    ack: String,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let user = db.create_user("admin@example.com", "Ada", "fixture-password-only", UserRole::Admin).unwrap();
        let ack = db.create_user("ack@example.com", "Owner", "fixture-password-only", UserRole::Admin).unwrap();
        db.bind_channel_identity(&user.id, "telegram", "123456789", true).unwrap();
        // Seed the same durable evidence written by the handshake, without
        // invoking transport/session handlers in a decision-service test.
        rusqlite::Connection::open(home.path().join("users.db")).unwrap().execute(
            "UPDATE channel_identities SET proof_method='chat_handshake', proof_user_id=user_id,
             proof_account='telegram', proof_dm='123456789', proven_at=?1 WHERE user_id=?2",
            rusqlite::params![Utc::now().timestamp(), user.id],
        ).unwrap();
        let env = DecisionEnv { home: home.path().to_path_buf(), registry: None,
            broker: ApprovalBroker::open(home.path()).unwrap(), users: Some(db) };
        let f = Self { _home: home, env, user: user.id, ack: ack.id };
        // Effect/receipt tests explicitly exempt all T2 kinds; default policy
        // is exercised separately with an empty exemption list.
        f.config(true, "telegram", chat_admin::STEP_UP_KIND_KEYS);
        crate::knowledge::test_fixtures::agent(&f.env.home, "support", "[agent]\nname='support'\n");
        f
    }

    pub(crate) fn config(&self, enabled: bool, channel: &str, exempt: &[&str]) {
        // Local discovery treats encrypted credentials as configured. An
        // intentionally undecryptable fixture prevents notification HTTP calls.
        std::fs::write(self.env.home.join("config.toml"), format!(
            "[channels]\ntelegram_bot_token_enc='invalid-encrypted-fixture'\n[approval.chat_admin]\nenabled={enabled}\nchannels=['{channel}']\nstep_up_exempt_kinds={}\nenabled_ack={{by='{}',at='2026-11-27T00:00:00Z'}}\n",
            serde_json::to_string(exempt).unwrap(), self.ack,
        )).unwrap();
    }

    pub(crate) fn db(&self) -> &UserDb { self.env.users.as_deref().unwrap() }

    fn dashboard(&self) -> Decider {
        Decider::Dashboard(self.db().live_user_context(&self.user).unwrap().unwrap())
    }

    pub(crate) async fn simple(&self, kind: &str) -> ApprovalId {
        let payload = if kind == "other_outgoing" { json!({"egress_guard": {}}) } else { json!({}) };
        self.env.broker.request("support", kind, "review", payload, 3600).await.unwrap()
    }

    pub(crate) async fn proof(&self, id: &ApprovalId) -> ChatAdminProof {
        let rec = self.env.broker.get(id).await.unwrap().unwrap();
        let proof = ChatAdminProof { user_id: self.user.clone(), channel: ChatChannel::Telegram,
            account: "telegram".into(), principal: "123456789".into(), dm: "123456789".into(),
            card_id: uuid::Uuid::new_v4().to_string(), step: Step::Tap,
            digest: crate::approval::decision_digest_at(&self.env.home, &rec) };
        receipts::insert(&self.env.broker.store, &Receipt {
            card_id: proof.card_id.clone(), approval_id: id.to_string(), purpose: "rm143".into(),
            user_id: proof.user_id.clone(), channel: "telegram".into(), account: proof.account.clone(),
            principal: proof.principal.clone(), dm: proof.dm.clone(), message_id: Some(uuid::Uuid::new_v4().to_string()),
            digest: proof.digest.clone(), tier: if chat_admin::tier(&rec) == chat_admin::Tier::T1 {"T1"} else {"T2"}.into(),
            valid_until: (Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
            status: "live".into(), sent_at: Utc::now().to_rfc3339(),
        }).await.unwrap();
        proof
    }

    pub(crate) async fn status(&self, id: &ApprovalId) -> ApprovalStatus {
        self.env.broker.get(id).await.unwrap().unwrap().status
    }

    pub(crate) async fn quarantine(&self) -> ApprovalId {
        let db = self.env.home.join("memory.db");
        let engine = duduclaw_memory::SqliteMemoryEngine::new(&db).unwrap();
        let entry = duduclaw_core::types::MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(), agent_id: "support".into(), content: "7 days".into(),
            timestamp: Utc::now(), tags: vec![], embedding: None,
            layer: duduclaw_core::types::MemoryLayer::Semantic, importance: 5.0,
            access_count: 0, last_accessed: None, source_event: "test".into(),
        };
        let meta = duduclaw_memory::TemporalMeta { subject: Some("policy:refund".into()),
            predicate: Some("window".into()), object: Some("7".into()), origin: Some("channel".into()),
            ..Default::default() };
        let held = engine.hold_refused_claim("support", entry, meta, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
        let digest = engine.held_claim_view("support", &held).await.unwrap().unwrap().claim_digest;
        self.env.broker.request("support", crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE, "review",
            json!({"memory_db": db, "agent_id":"support", "quarantined_ids":[held],
                "promote_on_approve":true, "claim_digest":digest}), 3600).await.unwrap()
    }

    async fn add_source(&self) -> ApprovalId {
        use duduclaw_knowledge::acl::{PrincipalKind, Role};
        let (collection, _, _) = crate::knowledge::test_fixtures::collection(&self.env.home, "hr", "internal",
            &[(PrincipalKind::Agent, "support", Role::Viewer)], "既有內容。");
        let p = sources::Proposal { agent_id:"support".into(), collection_id:collection,
            title:None, content:sources::Content::Text("請假規定：特休假依年資計算。".into()),
            proposed_by:"agent:support".into(), via:"mcp", requester_kind:"system".into(),
            requester_channel:None, origin:None };
        let sources::Proposed::Filed { approval_id, .. } = sources::propose(&self.env.home, p).await.unwrap() else { panic!("expected card") };
        ApprovalId::from(approval_id)
    }

    pub(crate) async fn effect(&self, id: &ApprovalId) -> Value {
        let rec = self.env.broker.get(id).await.unwrap().unwrap();
        match rec.action_kind.as_str() {
            sources::ACTION_KIND => {
                let db = duduclaw_knowledge::schema::open_home(&self.env.home).unwrap();
                json!(duduclaw_knowledge::sources::list_sources(&db, rec.payload["collection_id"].as_str().unwrap()).unwrap().len())
            }
            crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE => {
                let engine = duduclaw_memory::SqliteMemoryEngine::new(&self.env.home.join("memory.db")).unwrap();
                json!(engine.get_history("support", "policy:refund", "window").await.unwrap().len())
            }
            // These kinds hand off to existing consumers through the durable
            // status; deciding must not execute the consumer a second time.
            _ => json!(rec.status.as_str()),
        }
    }
}

#[tokio::test]
async fn chat_and_dashboard_kind_table_has_identical_status_and_effect() {
    for kind in [crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE, sources::ACTION_KIND,
        crate::expert_skills::SKILLS_ACTION_KIND, crate::autopilot_approval::ACTION_KIND,
        crate::approval::cli_egress::KIND, "other_outgoing"] {
        for approve in [true, false] {
            let mut effects = Vec::new();
            for chat in [false, true] {
                let f = Fixture::new();
                let id = match kind {
                    crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE => f.quarantine().await,
                    sources::ACTION_KIND => f.add_source().await,
                    _ => f.simple(kind).await,
                };
                let proof = f.proof(&id).await;
                let expected = Some((id.to_string(), proof.digest.clone()));
                let by = if chat { Decider::ChatAdmin(proof) } else { f.dashboard() };
                let outcome = crate::approval::EXPECTED_DECISION_DIGEST.scope(expected,
                    decide(&f.env, &id, approve, "", &by)).await.unwrap();
                let status = if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied };
                assert_eq!(outcome.status, status, "{kind}");
                let rec = f.env.broker.get(&id).await.unwrap().unwrap();
                assert_eq!(rec.status, status);
                let expected_by = if chat { format!("chat-admin:{}:telegram:tap", f.user) } else { format!("dashboard:{}", f.user) };
                assert_eq!(rec.decided_by.as_deref(), Some(expected_by.as_str()));
                assert_eq!(outcome.decided_by, expected_by);
                assert_eq!(outcome.display_name.as_deref(), Some("Ada"));
                assert_eq!(outcome.channel, chat.then_some(ChatChannel::Telegram));
                let effect = f.effect(&id).await;
                if kind == sources::ACTION_KIND { assert_eq!(effect, json!(if approve { 2 } else { 1 })); }
                if kind == crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE { assert_eq!(effect, json!(if approve { 1 } else { 0 })); }
                effects.push(effect);
                if chat {
                    let db = rusqlite::Connection::open(f.env.home.join("users.db")).unwrap();
                    let count: i64 = db.query_row("SELECT count(*) FROM auth_audit_log WHERE action='approval.chat_admin_decision'", [], |r| r.get(0)).unwrap();
                    assert_eq!(count, 1);
                }
            }
            assert_eq!(effects[0], effects[1], "{kind} approve={approve}");
        }
    }
}

async fn refused_identity(change: impl FnOnce(&Fixture, &mut ChatAdminProof)) {
    let f = Fixture::new();
    let id = f.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    let mut proof = f.proof(&id).await;
    change(&f, &mut proof);
    assert!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await.is_err());
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
}

#[tokio::test]
async fn manager_with_proof_refused() {
    refused_identity(|f, _| { f.db().update_user(&f.user, None, Some(UserRole::Manager), None).unwrap(); }).await;
}

#[tokio::test]
async fn no_identity_system_never_decides() {
    for user in ["system", "", "not-a-uuid", "00000000-0000-0000-0000-000000000001"] {
        refused_identity(|_, proof| proof.user_id = user.into()).await;
    }
}

#[tokio::test]
async fn demoted_admin_old_card_refused() {
    refused_identity(|f, _| { f.db().update_user(&f.user, None, Some(UserRole::Manager), None).unwrap(); }).await;
}

#[tokio::test]
async fn suspended_and_offboarded_admins_refused() {
    for status in [UserStatus::Suspended, UserStatus::Offboarded] {
        refused_identity(|f, _| { f.db().set_user_status(&f.user, status).unwrap(); }).await;
    }
}

#[tokio::test]
async fn unbound_admin_old_card_refused() {
    refused_identity(|f, _| { f.db().unbind_channel_identity("telegram", "123456789").unwrap(); }).await;
}

#[tokio::test]
async fn feature_off_and_channel_disabled_refused() {
    for (enabled, channel) in [(false, "telegram"), (true, "googlechat")] {
        let f = Fixture::new();
        let id = f.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
        let proof = f.proof(&id).await;
        f.config(enabled, channel, &[]);
        assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await, Err(DecisionRefusal::FeatureOff)));
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    }
}

#[tokio::test]
async fn proof_coordinates_must_match_exactly() {
    for coordinate in ["principal", "account", "dm"] {
        refused_identity(|_, proof| match coordinate {
            "principal" => proof.principal.push('0'), "account" => proof.account.push('0'),
            "dm" => proof.dm.push('0'), _ => unreachable!(),
        }).await;
    }
}

#[tokio::test]
async fn removed_configured_account_refused() {
    refused_identity(|f, _| {
        let path = f.env.home.join("config.toml");
        let raw = std::fs::read_to_string(&path).unwrap().replace("telegram_bot_token_enc='invalid-encrypted-fixture'", "telegram_bot_token_enc=''");
        std::fs::write(path, raw).unwrap();
    }).await;
}

#[tokio::test]
async fn all_t2_kinds_require_step_up_by_default_and_exemptions_are_exact() {
    let f = Fixture::new();
    for key in chat_admin::STEP_UP_KIND_KEYS {
        let kind = if *key == "egress_guard" { "other_outgoing" } else { *key };
        let id = f.simple(kind).await;
        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
        let proof = f.proof(&id).await;
        f.config(true, "telegram", &[]);
        assert!(matches!(identity::authorize(&f.env, &rec, true, &proof).await,
            Err(DecisionRefusal::StepUpRequired)), "{key}");
        assert!(!identity::authorize(&f.env, &rec, false, &proof).await.unwrap().step_required);
        f.config(true, "telegram", &[*key]);
        assert!(!identity::authorize(&f.env, &rec, true, &proof).await.unwrap().step_required);
        let other = chat_admin::STEP_UP_KIND_KEYS.iter().copied().find(|other| other != key).unwrap();
        f.config(true, "telegram", &[other]);
        assert!(matches!(identity::authorize(&f.env, &rec, true, &proof).await,
            Err(DecisionRefusal::StepUpRequired)), "{key}");
    }
    f.config(true, "telegram", &[]);
    for kind in [crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE, sources::ACTION_KIND] {
        let id = f.simple(kind).await;
        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
        assert!(!identity::authorize(&f.env, &rec, true, &f.proof(&id).await).await.unwrap().step_required);
    }
}

#[tokio::test]
async fn step_up_kind_with_tap_requires_step_but_denial_does_not() {
    let f = Fixture::new();
    let kind = crate::expert_skills::SKILLS_ACTION_KIND;
    let id = f.simple(kind).await;
    let proof = f.proof(&id).await;
    f.config(true, "telegram", &[]);
    let by = Decider::ChatAdmin(proof);
    assert!(matches!(decide(&f.env, &id, true, "", &by).await, Err(DecisionRefusal::StepUpRequired)));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(decide(&f.env, &id, false, "", &by).await.unwrap().status, ApprovalStatus::Denied);
    let conn = f.env.broker.store.conn.lock().await;
    let required: bool = conn.query_row("SELECT step_required FROM chat_admin_decisions WHERE approval_id=?1", [id.as_str()], |r| r.get(0)).unwrap();
    assert!(!required);
}

#[tokio::test]
async fn t3_and_future_package_kinds_refused_in_chat() {
    for kind in [crate::approval::operator_cli_gate::OPERATOR_CLI_KINDS[0].kind,
        crate::isolation_background::PERSONAL_DELEGATION_KIND, crate::approval::WORKFLOW_ACTIVATION_KIND] {
        let f = Fixture::new();
        let id = f.simple(kind).await;
        let proof = f.proof(&id).await;
        assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await, Err(DecisionRefusal::NotSupportedInChat)), "{kind}");
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    }
}

async fn race(chat_only: bool) {
    let f = Fixture::new();
    let id = f.quarantine().await;
    let by = Decider::ChatAdmin(f.proof(&id).await);
    let other = if chat_only { by.clone() } else { f.dashboard() };
    let (a, b) = tokio::join!(decide(&f.env, &id, true, "", &by), decide(&f.env, &id, true, "", &other));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a.as_ref().err().or(b.as_ref().err()), Some(DecisionRefusal::AlreadyDecided { .. })));
    assert_eq!(f.effect(&id).await, json!(1));
}

#[tokio::test]
async fn double_press_executes_once() { race(true).await; }

#[tokio::test]
async fn dashboard_and_chat_race_one_winner() { race(false).await; }

#[tokio::test]
async fn dashboard_and_chat_race_one_winner_bound() {
    let f = Fixture::new();
    let handler = crate::handlers::MethodHandler::new(f.env.home.clone()).await;
    let ctx = f.db().live_user_context(&f.user).unwrap().unwrap();
    let id = f.bound(json!({"egress_guard":{}}), &f.ack).await;
    let by = Decider::ChatAdmin(f.proof(&id).await);
    let lock = decision_lock(id.as_str());
    let guard = lock.lock().await;
    let dashboard = handler.handle_approvals_decide(json!({"id":id.as_str(), "approve":true}), &ctx);
    tokio::pin!(dashboard);
    // Exercise the real dashboard branch, not Decider::Dashboard (which the
    // service does not handle for bound cards). It must wait on the same lock.
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), dashboard.as_mut()).await.is_err());
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    drop(guard);
    let (dashboard, chat) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(dashboard.as_mut(), decide(&f.env, &id, true, "", &by))
    }).await.expect("bound decisions must finish after releasing the lock");
    let dashboard_ok = match &dashboard {
        crate::protocol::WsFrame::Response { ok, error, .. } => {
            assert_eq!(error.is_some(), !*ok, "{dashboard:?}");
            *ok
        }
        other => panic!("expected dashboard response, got {other:?}"),
    };
    assert_eq!(usize::from(dashboard_ok) + usize::from(chat.is_ok()), 1);
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.status, ApprovalStatus::Approved);
    let expected_by = if dashboard_ok { format!("dashboard:{}", f.user) }
        else { format!("chat-admin:{}:telegram:tap", f.user) };
    assert_eq!(rec.decided_by.as_deref(), Some(expected_by.as_str()));
    assert!(rec.decided_at.is_some());
    // Exactly one ledger row for a chat winner, none for a dashboard winner.
    f.assert_ledger(&id, chat.is_ok()).await;
}

#[tokio::test]
async fn bound_dashboard_rereads_record_after_waiting_for_decision_lock() {
    let f = Fixture::new();
    let handler = crate::handlers::MethodHandler::new(f.env.home.clone()).await;
    let ctx = f.db().live_user_context(&f.user).unwrap().unwrap();
    let id = f.bound(json!({"egress_guard":{}}), &f.ack).await;
    let proof = f.proof(&id).await;
    let lock = decision_lock(id.as_str());
    let guard = lock.lock().await;
    let dashboard = handler.handle_approvals_decide(json!({"id":id.as_str(), "approve":true}), &ctx);
    tokio::pin!(dashboard);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), dashboard.as_mut()).await.is_err());
    // Simulate the service's winning transaction while owning its lock.
    let entry = chat_admin::LedgerEntry {
        user_id: f.user.parse().unwrap(), channel: proof.channel, step: proof.step,
        step_required: false, card_id: proof.card_id, digest: proof.digest,
    };
    f.env.broker.decide_bound_chat_admin(&id, &entry, false).await.unwrap();
    drop(guard);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), dashboard.as_mut())
        .await.expect("dashboard must finish after releasing the lock");
    let expected = crate::protocol::WsFrame::error_response("", &format!("approval {id} already denied"));
    assert_eq!(serde_json::to_value(response).unwrap(), serde_json::to_value(expected).unwrap());
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.decided_by, Some(format!("chat-admin:{}:telegram:tap", f.user)));
    assert_eq!(rec.status, ApprovalStatus::Denied);
    f.assert_ledger(&id, true).await;
}

#[tokio::test]
async fn side_effect_failure_leaves_pending() {
    for chat in [false, true] {
        let f = Fixture::new();
        let id = f.env.broker.request("support", crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE, "review",
            json!({"memory_db": f.env.home.join("missing").join("memory.db"),
                "agent_id":"support", "quarantined_ids":["x"], "promote_on_approve":true, "claim_digest":"d"}), 3600).await.unwrap();
        let by = if chat { Decider::ChatAdmin(f.proof(&id).await) } else { f.dashboard() };
        assert!(decide(&f.env, &id, true, "", &by).await.is_err());
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    }
}

#[tokio::test]
async fn digest_and_missing_receipt_refuse_before_side_effect() {
    for missing in [false, true] {
        let f = Fixture::new();
        let id = f.quarantine().await;
        let mut proof = f.proof(&id).await;
        if missing { proof.card_id = uuid::Uuid::new_v4().to_string(); }
        else { proof.digest = "0".repeat(64); }
        let result = decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await;
        if missing { assert!(matches!(result, Err(DecisionRefusal::Forbidden(_)))); }
        else { assert!(matches!(result, Err(DecisionRefusal::DigestMismatch))); }
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
        assert_eq!(f.effect(&id).await, json!(0));
    }
}

#[test]
fn refusal_texts_do_not_leak_internal_details() {
    let id = ApprovalId::new();
    let already = DecisionRefusal::AlreadyDecided { id: id.clone(), by: Some("dashboard:system".into()), status: ApprovalStatus::Approved };
    assert_eq!(already.dashboard_text(), format!("approval {id} already approved"));
    for lang in [Lang::ZhTw, Lang::En, Lang::Ja] {
        let refusal = DecisionRefusal::Internal("secret/path/users.db".into());
        assert!(!refusal.user_text(lang).contains("users.db"));
    }
}

#[tokio::test]
async fn personal_employee_card_refused_even_for_its_admin_owner() {
    let f = Fixture::new();
    let id = f.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    let proof = f.proof(&id).await;
    rusqlite::Connection::open(f.env.home.join("users.db")).unwrap().execute(
        "INSERT INTO isolation_personal_agents(agent_name,owner_user_id,created_at) VALUES('support',?1,?2)",
        rusqlite::params![f.user, Utc::now().to_rfc3339()],
    ).unwrap();
    assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await, Err(DecisionRefusal::NotSupportedInChat)));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
}

impl Fixture {
    async fn bound(&self, payload: Value, submitter: &str) -> ApprovalId {
        let binding = crate::approval::ExecutionBinding {
            schema_version:1, run_id:uuid::Uuid::new_v4().to_string(), run_origin_kind:"ingress".into(),
            actor_principal:"support".into(), conversation_audience:Default::default(),
            decision_context:crate::approval::DecisionContext { channel:"dashboard".into(),
                account_id:"dashboard".into(), conversation_id:"approval".into(), principal_id:submitter.into() },
            task_id:None, task_revision:None, task_snapshot_hash:None,
            payload_hash:crate::approval::payload_hash(&payload),
            policy_revision:crate::approval::policy_revision(&self.env.home, "support").unwrap(), cwd:None,
            environment_hash:"fixture".into(), file_hashes:Default::default(),
            expires_at:(Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
            resume_handler:"live_only_v1".into(), resume_version:1,
        };
        self.env.broker.request_bound(crate::approval::RequestKind::Approval, "support", "review",
            payload, binding).await.unwrap()
    }
}

#[tokio::test]
async fn bound_egress_uses_verified_admin_and_receipt() {
    let f = Fixture::new();
    let id = f.bound(json!({"egress_guard":{}}), "other-submitter").await;
    let proof = f.proof(&id).await;
    let outcome = decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await.unwrap();
    assert_eq!(outcome.value, json!({"id":id.as_str(), "decided":"approved",
        "execution_status":"awaiting_execution", "submitter_is_decider":false}));
    assert_eq!(outcome.decided_by, format!("chat-admin:{}:telegram:tap", f.user));
    assert_eq!(f.status(&id).await, ApprovalStatus::Approved);
    f.assert_ledger(&id, true).await;
    assert!(receipts::list_live(&f.env.broker.store, &id).await.unwrap().is_empty());
}

#[tokio::test]
async fn bound_activation_service_reports_submitter_and_web_step() {
    let f = Fixture::new();
    f.config(true, "telegram", &[]);
    for submitter in [&f.user, &f.ack] {
        let id = f.bound(json!({"kind":crate::approval::WORKFLOW_ACTIVATION_KIND}), submitter).await;
        let mut proof = f.proof(&id).await;
        assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof.clone())).await,
            Err(DecisionRefusal::StepUpRequired)));
        proof.step = Step::Web;
        let outcome = decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await.unwrap();
        assert_eq!(outcome.value, json!({"id":id.as_str(), "decided":"approved",
            "execution_status":"awaiting_execution", "submitter_is_decider":submitter == &f.user}));
        assert_eq!(outcome.decided_by, format!("chat-admin:{}:telegram:web", f.user));
        f.assert_ledger(&id, true).await;
        let audits: Vec<Value> = std::fs::read_to_string(f.env.home.join("security_audit.jsonl")).unwrap()
            .lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|event| event["event_type"] == "workflow_activation_decided"
                && event["details"]["approval_id"] == id.as_str()).collect();
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0]["details"]["decided_by"], json!(f.user));
        assert_eq!(audits[0]["details"]["submitter_is_decider"], json!(submitter == &f.user));
    }
}

#[tokio::test]
async fn bound_autopilot_uses_receipt_digest_even_without_outer_scope() {
    let f = Fixture::new();
    for drift in [false, true] {
        let id = f.bound(json!({"frozen_action":"fixture"}), &f.ack).await;
        // Production currently files unbound autopilot cards. A future bound
        // record must still receive the exact receipt scope from decide().
        f.env.broker.store.conn.lock().await.execute(
            "UPDATE approvals SET action_kind=?1 WHERE id=?2",
            rusqlite::params![crate::autopilot_approval::ACTION_KIND, id.as_str()],
        ).unwrap();
        let proof = f.proof(&id).await;
        if drift {
            f.env.broker.store.conn.lock().await.execute(
                "UPDATE approvals SET summary='changed' WHERE id=?1", [id.as_str()],
            ).unwrap();
        }
        let result = decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await;
        if drift {
            assert!(matches!(result, Err(DecisionRefusal::DigestMismatch)));
            assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
            f.assert_ledger(&id, false).await;
        } else {
            assert_eq!(result.unwrap().status, ApprovalStatus::Approved);
            f.assert_ledger(&id, true).await;
        }
    }
}

#[tokio::test]
async fn bound_chat_still_requires_live_identity_and_receipt() {
    let f = Fixture::new();
    let id = f.bound(json!({"egress_guard":{}}), &f.ack).await;
    let mut proof = f.proof(&id).await;
    proof.card_id = uuid::Uuid::new_v4().to_string();
    assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await,
        Err(DecisionRefusal::Forbidden(_))));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    // Revocation must be checked even though the bound context is a different
    // identity and the caller presents an otherwise valid receipt.
    let proof = f.proof(&id).await;
    f.db().update_user(&f.user, None, Some(UserRole::Manager), None).unwrap();
    assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await,
        Err(DecisionRefusal::Forbidden(_))));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
}

#[tokio::test]
async fn bound_chat_self_approval_refused_but_denial_allowed() {
    let f = Fixture::new();
    let path = f.env.home.join("config.toml");
    let mut config = std::fs::read_to_string(&path).unwrap();
    config.push_str("\n[approval]\nforbid_self_approval=true\n");
    std::fs::write(path, config).unwrap();
    let principal = duduclaw_core::HumanPrincipal::user(&f.user).unwrap().to_string();
    let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal),
        f.bound(json!({"egress_guard":{}}), &f.user)).await;
    let by = Decider::ChatAdmin(f.proof(&id).await);
    assert!(matches!(decide(&f.env, &id, true, "", &by).await, Err(DecisionRefusal::Forbidden(_))));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(decide(&f.env, &id, false, "", &by).await.unwrap().status, ApprovalStatus::Denied);
    f.assert_ledger(&id, true).await;
}

#[tokio::test]
async fn bound_chat_t3_and_questions_stay_unsupported() {
    let f = Fixture::new();
    for question in [false, true] {
        let payload = if question { json!({"egress_guard":{}}) } else { json!({}) };
        let id = f.bound(payload, &f.ack).await;
        if question {
            f.env.broker.store.conn.lock().await.execute(
                "UPDATE approvals SET request_kind='question' WHERE id=?1", [id.as_str()],
            ).unwrap();
        }
        let proof = f.proof(&id).await;
        assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await,
            Err(DecisionRefusal::NotSupportedInChat)));
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
        f.assert_ledger(&id, false).await;
    }
}

#[tokio::test]
async fn chat_self_approval_forbidden_but_denial_allowed() {
    let f = Fixture::new();
    let path = f.env.home.join("config.toml");
    let mut config = std::fs::read_to_string(&path).unwrap();
    config.push_str("\n[approval]\nforbid_self_approval=true\n");
    std::fs::write(path, config).unwrap();
    let principal = duduclaw_core::HumanPrincipal::user(&f.user).unwrap().to_string();
    let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal), f.simple(crate::expert_skills::SKILLS_ACTION_KIND)).await;
    let by = Decider::ChatAdmin(f.proof(&id).await);
    assert!(matches!(decide(&f.env, &id, true, "", &by).await, Err(DecisionRefusal::Forbidden(_))));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(decide(&f.env, &id, false, "", &by).await.unwrap().status, ApprovalStatus::Denied);
}

#[tokio::test]
async fn add_source_hash_mismatch_still_denies_and_audits_actual_decision() {
    let f = Fixture::new();
    let id = f.add_source().await;
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    let by = Decider::ChatAdmin(f.proof(&id).await);
    let staged = rec.payload["staged"].as_str().unwrap();
    std::fs::write(sources::sidecar_dir(&f.env.home).join(format!("{staged}.bin")), "changed content").unwrap();
    let result = decide(&f.env, &id, true, "", &by).await;
    assert_eq!(result.unwrap_err().dashboard_text(), sources::HASH_MISMATCH);
    assert_eq!(f.status(&id).await, ApprovalStatus::Denied);
    assert_eq!(f.effect(&id).await, json!(1));
    let db = rusqlite::Connection::open(f.env.home.join("users.db")).unwrap();
    let detail: String = db.query_row("SELECT detail FROM auth_audit_log WHERE action='approval.chat_admin_decision'", [], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&detail).unwrap()["approve"], false);
}

#[tokio::test]
async fn expired_kind_texts_preserved() {
    for kind in [crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE, sources::ACTION_KIND,
        crate::expert_skills::SKILLS_ACTION_KIND, crate::autopilot_approval::ACTION_KIND, crate::approval::cli_egress::KIND] {
        let f = Fixture::new();
        let id = f.simple(kind).await;
        {
            let conn = f.env.broker.store.conn.lock().await;
            conn.execute("UPDATE approvals SET created_at='2020-01-01T00:00:00Z' WHERE id=?1", [id.as_str()]).unwrap();
        }
        let expected = if crate::approval_notify::is_dashboard_only_kind(kind) {
            crate::approval_notify::dashboard_only_expired_text(kind)
        } else { "approval expired" };
        // Denial does not require an autopilot digest and must still expire.
        let error = decide(&f.env, &id, false, "", &f.dashboard()).await.unwrap_err();
        assert!(matches!(error, DecisionRefusal::Expired(_)), "{kind}: {error:?}");
        assert_eq!(error.dashboard_text(), expected);
        assert_eq!(f.status(&id).await, ApprovalStatus::Expired);
    }
}

#[tokio::test]
async fn password_change_required_refused() {
    refused_identity(|f, _| {
        rusqlite::Connection::open(f.env.home.join("users.db")).unwrap().execute(
            "UPDATE users SET must_change_password=1 WHERE id=?1", [&f.user],
        ).unwrap();
    }).await;
}

// P6 fixtures use the same profile writer/sidecar and org Proposal/journal
// shapes as handlers/tests/{agent_files_rpc,org_change_rpc}_tests.rs, with
// UUID Admins and the verified transport evidence from the P5 fixture.
const FILE_BEFORE: &str = "我是客服，協助回答產品問題。\n";
const FILE_AFTER: &str = "我是客服，先確認問題，再說明處理方式。\n";

impl Fixture {
    async fn files_org() -> Self {
        let mut f = Self::new();
        // Registry scans require all non-default AgentConfig sections.
        crate::knowledge::test_fixtures::agent(&f.env.home, "support", r#"
[agent]
name = "support"
display_name = "Support"
role = "specialist"
status = "active"
trigger = "@support"
reports_to = ""
icon = "x"
department = "原部門"
[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]
[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true
[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""
[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false
[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []
[evolution]
skill_auto_activate = false
skill_security_scan = true
"#);
        std::fs::write(f.env.home.join("agents/support/SOUL.md"), FILE_BEFORE).unwrap();
        {
            let guard = duduclaw_core::org_store::lock_org_mutations(&f.env.home).unwrap();
            guard.upsert("support", duduclaw_core::org_store::OrgEntry::new("", "原部門")).unwrap();
        }
        let mut registry = AgentRegistry::new(f.env.home.join("agents"));
        registry.scan().await.unwrap();
        assert!(registry.get("support").is_some(), "fixture employee must load");
        f.env.registry = Some(Arc::new(RwLock::new(registry)));
        f
    }

    async fn file_change(&self) -> ApprovalId {
        use crate::agent_files::{FileKey, approval, verdict::{Analysis, Verdict}};
        let analysis = Analysis {
            user_id: self.ack.clone(), agent_id: "support".into(), file: FileKey::Soul,
            base_hash: crate::agent_files::hash_of(FILE_BEFORE),
            content_sha256: crate::agent_files::hash_of(FILE_AFTER),
            verdict: Verdict::Warn, basis: "review", summary: String::new(),
            reasons: vec![], source: "dashboard",
        };
        ApprovalId::from(approval::file_request(&self.env.home, &analysis, FILE_AFTER,
            "proposer@example.test", "調整說明方式").await.unwrap())
    }

    async fn org_change(&self, proposer: &str) -> ApprovalId {
        use crate::org_change_approval::{self as oc, Proposal};
        let before = duduclaw_core::org_store::load_for_write(&self.env.home).unwrap()
            .get("support").cloned().unwrap();
        let proposal = Proposal {
            agent_id: "support".into(), before,
            after: duduclaw_core::org_store::OrgEntry::new("", "新部門"),
            base: duduclaw_core::org_change::entry_hash(&self.env.home, "support"),
            proposer: proposer.into(), proposer_email: "proposer@example.test".into(),
            reason: "調整負責部門".into(),
        };
        self.env.broker.request("support", oc::ACTION_KIND, "review", proposal.to_payload(), oc::TTL_SECS).await.unwrap()
    }

    async fn files_org_card(&self, org: bool) -> ApprovalId {
        if org { self.org_change(&self.ack).await } else { self.file_change().await }
    }

    fn files_org_effect(&self, org: bool) -> Value {
        if org {
            let authority = duduclaw_core::org_store::load_for_write(&self.env.home).unwrap();
            let mirror = duduclaw_core::org_store::read_mirror(&self.env.home.join("agents/support/agent.toml")).unwrap();
            json!({"authority":crate::org_change_approval::entry_json(authority.get("support").unwrap()),
                "mirror":crate::org_change_approval::entry_json(&mirror)})
        } else {
            json!(std::fs::read_to_string(self.env.home.join("agents/support/SOUL.md")).unwrap())
        }
    }

    async fn assert_ledger(&self, id: &ApprovalId, chat: bool) {
        let rec = self.env.broker.get(id).await.unwrap().unwrap();
        let conn = self.env.broker.store.conn.lock().await;
        let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1",
            [id.as_str()], |r| r.get(0)).unwrap();
        assert_eq!(count, i64::from(chat));
        assert_eq!(chat_admin::ledger::accepts(&conn, &rec), chat);
    }

    fn org_journal_path(&self, id: &ApprovalId) -> std::path::PathBuf {
        self.env.home.join("org_change_journal").join(format!("{id}.json"))
    }

    fn decision_failure_audits(&self) -> usize {
        std::fs::read_to_string(self.env.home.join("security_audit.jsonl")).unwrap()
            .lines().filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["event_type"] == "org_change_decision_unrecorded").count()
    }
}

#[tokio::test]
async fn files_org_chat_and_dashboard_have_identical_effects_and_actor_evidence() {
    for org in [false, true] {
        for approve in [false, true] {
            let mut effects = Vec::new();
            for chat in [false, true] {
                let f = Fixture::files_org().await;
                let id = f.files_org_card(org).await;
                let by = if chat { Decider::ChatAdmin(f.proof(&id).await) } else { f.dashboard() };
                let outcome = Box::pin(decide(&f.env, &id, approve, "fixture decision", &by)).await.unwrap();
                let expected_by = if chat { format!("chat-admin:{}:telegram:tap", f.user) }
                    else { format!("dashboard:{}", f.user) };
                let rec = f.env.broker.get(&id).await.unwrap().unwrap();
                assert_eq!(rec.status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
                assert_eq!(outcome.status, rec.status);
                assert_eq!(rec.decided_by.as_deref(), Some(expected_by.as_str()));
                assert_eq!(outcome.decided_by, expected_by);
                assert_eq!(outcome.channel, chat.then_some(ChatChannel::Telegram));
                f.assert_ledger(&id, chat).await;
                let effect = f.files_org_effect(org);
                if org {
                    let department = if approve { "新部門" } else { "原部門" };
                    assert_eq!(effect["authority"]["department"], department);
                    assert_eq!(effect["mirror"], effect["authority"]);
                } else {
                    assert_eq!(effect, json!(if approve { FILE_AFTER } else { FILE_BEFORE }));
                }
                effects.push(effect);
            }
            assert_eq!(effects[0], effects[1], "org={org} approve={approve}");
        }
    }
}

#[tokio::test]
async fn files_org_side_effect_failures_leave_both_entry_points_pending() {
    for org in [false, true] {
        for chat in [false, true] {
            let f = Fixture::files_org().await;
            let id = f.files_org_card(org).await;
            let by = if chat { Decider::ChatAdmin(f.proof(&id).await) } else { f.dashboard() };
            if org {
                // A corrupt authority is an IO/config failure, not a stale
                // proposal: do not turn it into a terminal denial.
                std::fs::write(f.env.home.join("org.toml"), "[broken").unwrap();
            } else {
                crate::agent_files::approval::remove_sidecar(&f.env.home, id.as_str());
            }
            assert!(Box::pin(decide(&f.env, &id, true, "", &by)).await.is_err());
            assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
            f.assert_ledger(&id, false).await;
            if org {
                assert_eq!(duduclaw_core::org_store::read_mirror(&f.env.home.join("agents/support/agent.toml")).unwrap().department, "原部門");
            } else {
                assert_eq!(f.files_org_effect(false), json!(FILE_BEFORE));
            }
        }
    }
}

#[tokio::test]
async fn file_change_missing_registry_fails_closed_for_chat_and_dashboard() {
    for chat in [false, true] {
        let mut f = Fixture::files_org().await;
        let id = f.file_change().await;
        let by = if chat { Decider::ChatAdmin(f.proof(&id).await) } else { f.dashboard() };
        f.env.registry = None;
        assert!(matches!(Box::pin(decide(&f.env, &id, true, "", &by)).await, Err(DecisionRefusal::Internal(_))));
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
        assert_eq!(f.files_org_effect(false), json!(FILE_BEFORE));
        f.assert_ledger(&id, false).await;
    }
}

#[tokio::test]
async fn org_change_chat_proposer_cannot_self_approve_but_can_deny() {
    let f = Fixture::files_org().await;
    let path = f.env.home.join("config.toml");
    let config = std::fs::read_to_string(&path).unwrap() + "\n[approval]\nforbid_self_approval=true\n";
    std::fs::write(path, config).unwrap();
    // No on_behalf_of: this specifically exercises the payload proposer gate.
    let id = f.org_change(&f.user).await;
    let by = Decider::ChatAdmin(f.proof(&id).await);
    let result = Box::pin(decide(&f.env, &id, true, "", &by)).await;
    assert!(matches!(result, Err(DecisionRefusal::Forbidden(_))));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(f.files_org_effect(true)["authority"]["department"], "原部門");
    Box::pin(decide(&f.env, &id, false, "", &by)).await.unwrap();
    assert_eq!(f.status(&id).await, ApprovalStatus::Denied);
    f.assert_ledger(&id, true).await;
}

#[tokio::test]
async fn file_change_step_up_tap_refused_and_web_succeeds() {
    let f = Fixture::files_org().await;
    f.config(true, "telegram", &[]);
    let id = f.file_change().await;
    let mut proof = f.proof(&id).await;
    assert!(matches!(Box::pin(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof.clone()))).await,
        Err(DecisionRefusal::StepUpRequired)));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(f.files_org_effect(false), json!(FILE_BEFORE));
    f.assert_ledger(&id, false).await;
    proof.step = Step::Web;
    let outcome = Box::pin(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof))).await.unwrap();
    assert_eq!(outcome.decided_by, format!("chat-admin:{}:telegram:web", f.user));
    assert_eq!(f.files_org_effect(false), json!(FILE_AFTER));
    f.assert_ledger(&id, true).await;
    let conn = f.env.broker.store.conn.lock().await;
    let required: bool = conn.query_row("SELECT step_required FROM chat_admin_decisions WHERE approval_id=?1",
        [id.as_str()], |row| row.get(0)).unwrap();
    assert!(required);
}

async fn applied_chat_journal(f: &Fixture) -> (ApprovalId, ChatAdminProof) {
    let id = f.org_change(&f.ack).await;
    let proof = f.proof(&id).await;
    files_org::decide_failpoint::arm(id.as_str());
    let result = Box::pin(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof.clone()))).await;
    files_org::decide_failpoint::disarm(id.as_str());
    assert!(matches!(result, Err(DecisionRefusal::Kind(ref error)) if error["code"] == "decision_unrecorded"));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(f.files_org_effect(true)["authority"]["department"], "新部門");
    f.assert_ledger(&id, false).await;
    (id, proof)
}

#[tokio::test]
async fn org_change_recovery_preserves_chat_actor_and_atomically_replays_ledger() {
    for chat_retry in [false, true] {
        let mut f = Fixture::files_org().await;
        let (id, proof) = applied_chat_journal(&f).await;
        let journal: Value = serde_json::from_str(&std::fs::read_to_string(f.org_journal_path(&id)).unwrap()).unwrap();
        let expected_by = format!("chat-admin:{}:telegram:tap", f.user);
        assert_eq!(journal["decided_by"], expected_by);
        assert_eq!(journal["chat_admin"]["card_id"], proof.card_id);
        assert_eq!(journal["chat_admin"]["digest"], proof.digest);
        assert_eq!(journal["chat_admin"]["channel"], "telegram");
        assert_eq!(journal["chat_admin"]["step"], "tap");
        assert_eq!(journal["chat_admin"]["step_required"], false);
        let effect = f.files_org_effect(true);
        // Drop/reopen the broker to ensure replay reads durable state.
        f.env.broker = ApprovalBroker::open(&f.env.home).unwrap();
        let by = if chat_retry { Decider::ChatAdmin(proof) } else {
            Decider::Dashboard(f.db().live_user_context(&f.ack).unwrap().unwrap())
        };
        let outcome = Box::pin(decide(&f.env, &id, true, "retry", &by)).await.unwrap();
        assert_eq!(outcome.decided_by, expected_by);
        assert_eq!(outcome.channel, Some(ChatChannel::Telegram));
        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.decided_by.as_deref(), Some(expected_by.as_str()));
        assert_eq!(rec.status, ApprovalStatus::Approved);
        assert_eq!(f.files_org_effect(true), effect);
        f.assert_ledger(&id, true).await;
        assert!(!f.org_journal_path(&id).exists());
        let conn = f.env.broker.store.conn.lock().await;
        let status: String = conn.query_row("SELECT status FROM chat_admin_cards WHERE approval_id=?1",
            [id.as_str()], |row| row.get(0)).unwrap();
        assert_eq!(status, "consumed");
    }
}

#[tokio::test]
async fn org_change_recovery_expired_chat_receipt_never_falls_back_to_dashboard() {
    for chat_retry in [false, true] {
        let f = Fixture::files_org().await;
        let (id, proof) = applied_chat_journal(&f).await;
        {
            let conn = f.env.broker.store.conn.lock().await;
            conn.execute("UPDATE chat_admin_cards SET valid_until='2000-01-01T00:00:00Z' WHERE card_id=?1",
                [&proof.card_id]).unwrap();
        }
        let audits_before = f.decision_failure_audits();
        let by = if chat_retry { Decider::ChatAdmin(proof) } else { f.dashboard() };
        assert!(Box::pin(decide(&f.env, &id, true, "retry", &by)).await.is_err());
        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.status, ApprovalStatus::Pending);
        assert_eq!(rec.decided_by, None);
        f.assert_ledger(&id, false).await;
        assert!(f.org_journal_path(&id).exists());
        assert_eq!(f.decision_failure_audits(), audits_before + 1);
        assert_eq!(f.files_org_effect(true)["authority"]["department"], "新部門");
    }
}

#[tokio::test]
async fn org_change_old_format_journal_replays_original_dashboard_user() {
    let mut f = Fixture::files_org().await;
    let id = f.org_change(&f.ack).await;
    files_org::decide_failpoint::arm(id.as_str());
    let result = Box::pin(decide(&f.env, &id, true, "", &f.dashboard())).await;
    files_org::decide_failpoint::disarm(id.as_str());
    assert!(result.is_err());
    let path = f.org_journal_path(&id);
    let mut journal: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    journal.as_object_mut().unwrap().remove("decided_by");
    journal.as_object_mut().unwrap().remove("chat_admin");
    std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    f.env.broker = ApprovalBroker::open(&f.env.home).unwrap();
    let by = Decider::Dashboard(f.db().live_user_context(&f.ack).unwrap().unwrap());
    let outcome = Box::pin(decide(&f.env, &id, true, "retry", &by)).await.unwrap();
    let expected_by = format!("dashboard:{}", f.user);
    assert_eq!(outcome.decided_by, expected_by);
    assert_eq!(outcome.channel, None);
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.decided_by.as_deref(), Some(expected_by.as_str()));
    assert_eq!(rec.status, ApprovalStatus::Approved);
    f.assert_ledger(&id, false).await;
    assert!(!f.org_journal_path(&id).exists());
}

#[tokio::test]
async fn files_org_chat_receipt_and_digest_checked_before_any_effect() {
    for org in [false, true] {
        for missing in [false, true] {
            let f = Fixture::files_org().await;
            let id = f.files_org_card(org).await;
            let before = f.files_org_effect(org);
            let mut proof = f.proof(&id).await;
            if missing { proof.card_id = uuid::Uuid::new_v4().to_string(); }
            else { proof.digest = "0".repeat(64); }
            assert!(Box::pin(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof))).await.is_err());
            assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
            assert_eq!(f.files_org_effect(org), before);
            f.assert_ledger(&id, false).await;
        }
    }
}

#[tokio::test]
async fn org_change_malformed_new_journal_identity_never_uses_legacy_fallback() {
    for field in ["decided_by", "chat_admin"] {
        let f = Fixture::files_org().await;
        let (id, _) = applied_chat_journal(&f).await;
        let path = f.org_journal_path(&id);
        let mut journal: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Removing only one new field must not turn a chat journal into an
        // old-format dashboard journal. Only absence of both is legacy.
        journal.as_object_mut().unwrap().remove(field);
        std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(Box::pin(decide(&f.env, &id, true, "retry", &f.dashboard())).await.is_err());
        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.status, ApprovalStatus::Pending);
        assert_eq!(rec.decided_by, None);
        f.assert_ledger(&id, false).await;
        assert!(f.org_journal_path(&id).exists());
    }
}

#[path = "admin_ai_tests.rs"]
mod admin_ai_tests;

#[tokio::test]
async fn db_source_grant_only_admin_dashboard_can_decide() {
    let f = Fixture::new();
    let id = f.simple("db_source_grant").await;
    let manager = f.db().create_user("db-manager@test.invalid", "Manager", "fixture-password", UserRole::Manager).unwrap();
    let manager = Decider::Dashboard(f.db().live_user_context(&manager.id).unwrap().unwrap());
    assert!(matches!(decide(&f.env, &id, true, "", &manager).await, Err(DecisionRefusal::Forbidden(_))));
    let proof = f.proof(&id).await;
    assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await, Err(DecisionRefusal::NotSupportedInChat)));
    assert!(f.env.broker.decide(&id, true, "dashboard:forged").await.is_err());
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    decide(&f.env, &id, true, "", &f.dashboard()).await.unwrap();
    assert_eq!(f.status(&id).await, ApprovalStatus::Approved);
}
