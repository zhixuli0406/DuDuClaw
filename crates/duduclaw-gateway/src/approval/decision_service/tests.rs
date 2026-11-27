//! Service integration fixtures mirror the existing knowledge/quarantine and
//! ledger tests. These tests are intentionally not run in the P5 sandbox.
use super::*;
use chrono::Utc;
use duduclaw_auth::{UserRole, UserStatus};
use crate::approval::chat_admin::receipts::{self, Receipt};
use crate::knowledge::add_source_approval as sources;

struct Fixture {
    _home: tempfile::TempDir,
    env: DecisionEnv,
    user: String,
    ack: String,
}

impl Fixture {
    fn new() -> Self {
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
        f.config(true, "telegram", &[]);
        crate::knowledge::test_fixtures::agent(&f.env.home, "support", "[agent]\nname='support'\n");
        f
    }

    fn config(&self, enabled: bool, channel: &str, step_up: &[&str]) {
        // Local discovery treats encrypted credentials as configured. An
        // intentionally undecryptable fixture prevents notification HTTP calls.
        std::fs::write(self.env.home.join("config.toml"), format!(
            "[channels]\ntelegram_bot_token_enc='invalid-encrypted-fixture'\n[approval.chat_admin]\nenabled={enabled}\nchannels=['{channel}']\nstep_up_kinds={}\nenabled_ack={{by='{}',at='2026-11-27T00:00:00Z'}}\n",
            serde_json::to_string(step_up).unwrap(), self.ack,
        )).unwrap();
    }

    fn db(&self) -> &UserDb { self.env.users.as_deref().unwrap() }

    fn dashboard(&self) -> Decider {
        Decider::Dashboard(self.db().live_user_context(&self.user).unwrap().unwrap())
    }

    async fn simple(&self, kind: &str) -> ApprovalId {
        let payload = if kind == "other_outgoing" { json!({"egress_guard": {}}) } else { json!({}) };
        self.env.broker.request("support", kind, "review", payload, 3600).await.unwrap()
    }

    async fn proof(&self, id: &ApprovalId) -> ChatAdminProof {
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

    async fn status(&self, id: &ApprovalId) -> ApprovalStatus {
        self.env.broker.get(id).await.unwrap().unwrap().status
    }

    async fn quarantine(&self) -> ApprovalId {
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

    async fn effect(&self, id: &ApprovalId) -> Value {
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
async fn step_up_kind_with_tap_requires_step_but_denial_does_not() {
    let f = Fixture::new();
    let kind = crate::expert_skills::SKILLS_ACTION_KIND;
    let id = f.simple(kind).await;
    let proof = f.proof(&id).await;
    f.config(true, "telegram", &[kind]);
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
        crate::isolation_background::PERSONAL_DELEGATION_KIND, crate::agent_files::approval::ACTION_KIND,
        crate::org_change_approval::ACTION_KIND, crate::approval::WORKFLOW_ACTIVATION_KIND] {
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

#[tokio::test]
async fn bound_egress_temporarily_unsupported() {
    let f = Fixture::new();
    let id = f.simple("other_outgoing").await;
    // Seed a fully shaped binding as in approval/tests/bound_cases. No bound
    // resolver should be reached in this package.
    let binding = crate::approval::ExecutionBinding {
        schema_version:1, run_id:uuid::Uuid::new_v4().to_string(), run_origin_kind:"workflow".into(),
        actor_principal:"support".into(), conversation_audience:Default::default(),
        decision_context:crate::approval::DecisionContext { channel:"telegram".into(),
            account_id:"telegram".into(), conversation_id:"123456789".into(), principal_id:"123456789".into() },
        task_id:None, task_revision:None, task_snapshot_hash:None,
        payload_hash:"fixture".into(), policy_revision:"fixture".into(), cwd:None,
        environment_hash:"fixture".into(), file_hashes:Default::default(),
        expires_at:(Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
        resume_handler:"fixture".into(), resume_version:1,
    };
    {
        let conn = f.env.broker.store.conn.lock().await;
        conn.execute("UPDATE approvals SET binding_json=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&binding).unwrap(), id.as_str()]).unwrap();
    }
    let proof = f.proof(&id).await;
    assert!(matches!(decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await, Err(DecisionRefusal::NotSupportedInChat)));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
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
