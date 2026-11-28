//! RM-143 P22: exercise real decision entry points with existing in-process seams.
//! No network credentials, gateway process, or live chat applications are used.
use super::*;
use crate::approval::{self, ApprovalId, ApprovalStatus, decision_service::{self, Decider,
    DecisionEnv, DecisionRefusal, tests::Fixture}};
use crate::approval::chat_admin::ledger::Step;
use crate::approval::chat_admin::press::tests::{Fixture as PressFixture, PRIVATE, reply_context};
use crate::approval::chat_admin::deliver::tests::{Recording, deliver_record};
use crate::decision_action::{DecisionAct, DecisionAction, DecisionSource};
use crate::handlers::MethodHandler;
use crate::protocol::WsFrame;
use crate::trigger_route::texts::Lang;
use duduclaw_auth::{UserContext, UserRole};
use serde_json::{Value, json};
use std::sync::Arc;

const DASHBOARD_SESSION_ID: &str = "14300000-0000-4000-8000-000000000022";

const KINDS: &[&str] = &[
    "knowledge_quarantine", "knowledge_add_source", "expert_skills_enable",
    "workflow_activation", "agent_file_change", "org_change", "autopilot_action",
    "cli_egress", "other_outgoing",
];

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("expected successful RPC: {other:?}"),
    }
}

async fn handler(env: &DecisionEnv) -> MethodHandler {
    let handler = MethodHandler::new(env.home.clone()).await;
    handler.set_task_store(Arc::new(crate::task_store::TaskStore::open(&env.home).unwrap())).await;
    handler.set_user_db(env.users.clone().unwrap(),
        Arc::new(duduclaw_auth::JwtConfig::new(b"rm143-p22-test-secret-32-bytes-long"))).await;
    handler
}

async fn dashboard(env: &DecisionEnv, user: &str, id: &ApprovalId, approve: bool) -> WsFrame {
    let rec = env.broker.get(id).await.unwrap().unwrap();
    let ctx = env.users.as_ref().unwrap().live_user_context(user).unwrap().unwrap();
    let h = handler(env).await;
    // Exercise the same live account/epoch check as the dashboard handler.
    let live = crate::review_evidence::audience::fresh_dashboard_context(&env.home, &ctx).unwrap();
    assert!(live.is_admin());
    let frame = crate::google_personal::DASHBOARD_SESSION.scope(
        Some((user.to_owned(), DASHBOARD_SESSION_ID.into())),
        h.handle_approvals_decide(json!({"id":id.as_str(), "approve":approve,
            "expected_digest":approval::decision_digest_at(&env.home, &rec)}), &ctx)).await;
    if rec.action_kind != crate::isolation_background::PERSONAL_DELEGATION_KIND {
        assert!(matches!(&frame, WsFrame::Response { ok:true, .. }),
            "dashboard kind={} approve={approve}: {frame:?}", rec.action_kind);
    }
    frame
}

async fn request(f: &Fixture, kind: &str) -> ApprovalId {
    match kind {
        "knowledge_quarantine" => f.quarantine().await,
        "knowledge_add_source" => f.add_source().await,
        "expert_skills_enable" => {
            let mut skill = crate::expert_skills::tests::skill("skill-target", "p22-skill");
            crate::expert_skills::tests::seed(&f.env.home, "support", &mut skill);
            ApprovalId::from(crate::expert_skills::request_enable(&f.env.home, "support", vec![skill]).await.unwrap())
        }
        "agent_file_change" => f.file_change().await,
        "org_change" => f.org_change(&f.ack).await,
        "workflow_activation" => ApprovalId::from(workflow_card(f).await.acceptance_id),
        _ => f.simple(kind).await,
    }
}

// The grant fixture supplies immutable revision material, but deliberately omits
// the dashboard draft. Seed that read-side material before delivering any card;
// draft creation/fixture execution are outside this decision-entry-point test.
async fn workflow_card(f: &Fixture) -> approval::AcceptedRevisionGrant {
    let (_, store, accepted, _) = approval::workflow_grants::tests::fixture_at(&f.env.home, false).await;
    store.initialize_evidence().await.unwrap();
    // Reuse the completed-source-task setup from workflow_review_tests::fx.
    let tasks = crate::task_store::TaskStore::open(&f.env.home).unwrap();
    let mut source = crate::task_store::TaskRow::new(
        "p22-source-task".into(), "Report".into(), String::new(), "normal".into(),
        accepted.spec.actor.clone(), "system".into(),
    );
    source.status = "done".into();
    tasks.insert_task(&source).await.unwrap();
    let mut draft = crate::workflow_drafts::WorkflowDraft {
        schema_version:1, draft_id:"p22-draft".into(), revision:1,
        owner:accepted.spec.actor.clone(), source_task:source.id.clone(),
        skill_id:"p22-skill".into(), source_snapshot_id:"p22-snapshot".into(),
        source_evidence_hash:"p22-evidence".into(),
        definition:accepted.revision.definition.clone(), revision_hash:accepted.spec.revision_hash.clone(),
        source_data:vec![], fixtures:vec![], creator_grant:accepted.spec.creator_grant.clone(),
        effect_templates:accepted.spec.templates.clone(), audience:accepted.spec.audience.clone(),
        budget:accepted.spec.budget.clone(), input_max_age_seconds:accepted.spec.input_max_age_seconds,
        timezone:"Asia/Taipei".into(), routine:None, stop_conditions:vec![],
        created_at:chrono::Utc::now().to_rfc3339(), disabled:true,
        review_status:"draft".into(), draft_hash:String::new(),
    };
    draft.draft_hash = draft.compute_hash();
    store.with_transaction(|tx| {
        tx.execute("INSERT INTO workflow_drafts VALUES(?1,?2,?3,?4,1,?5)",
            rusqlite::params![draft.draft_id, draft.revision, draft.owner, draft.draft_hash,
                serde_json::to_string(&draft).unwrap()]).map_err(|e| e.to_string())?;
        Ok(())
    }).await.unwrap();
    let request = crate::workflow::ActivationRequest {
        activation_id:accepted.activation_id.clone(), draft_id:draft.draft_id,
        draft_hash:draft.draft_hash, revision:accepted.revision.clone(), spec:accepted.spec.clone(),
        fixture_results:vec![], fixture_assertions:vec![], cron:None,
    };
    let payload = crate::workflow::activation::activation_payload(&request);
    let id = ApprovalId::from(accepted.acceptance_id.clone());
    let mut binding = f.env.broker.get(&id).await.unwrap().unwrap().binding.unwrap();
    binding.payload_hash = approval::payload_hash(&payload);
    binding.validate(&payload).unwrap();
    // Complete the seed atomically, before receipts/digests exist. Runtime
    // replace_text intentionally invalidates bound cards and is not a seed API.
    f.env.broker.store.conn.lock().await.execute(
        "UPDATE approvals SET payload=?1,binding_json=?2 WHERE id=?3",
        rusqlite::params![serde_json::to_string(&payload).unwrap(),
            serde_json::to_string(&binding).unwrap(), id.as_str()]).unwrap();
    accepted
}

async fn effect(f: &Fixture, id: &ApprovalId, kind: &str) -> Value {
    match kind {
        "agent_file_change" => f.files_org_effect(false),
        "org_change" => f.files_org_effect(true),
        // Other kinds hand off through durable status to their existing consumers.
        _ => f.effect(id).await,
    }
}

async fn settled_effect(f: &Fixture, id: &ApprovalId, kind: &str) -> Value {
    if kind == "expert_skills_enable" {
        let outcome = crate::expert_skills::apply_skills_decision(&f.env.home, "support").await.unwrap();
        let path = f.env.home.join("agents/skill-target/SKILLS/p22-skill/scripts/run.sh");
        return json!({"outcome":format!("{outcome:?}"), "file":std::fs::read_to_string(path).ok()});
    }
    effect(f, id, kind).await
}

fn methods(f: &Fixture, channel: &str, exempt: &[&str], enabled_methods: &[&str]) {
    f.config(true, channel, exempt);
    let path = f.env.home.join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap()
        .replace("[approval.chat_admin]", "googlechat_project_number='123456789012'\ngooglechat_service_account_json_enc='invalid-encrypted-fixture'\n[approval.chat_admin]");
    std::fs::write(path, format!("{raw}step_up_methods={}\n", serde_json::to_string(enabled_methods).unwrap())).unwrap();
    if channel == "googlechat" {
        f.db().bind_channel_identity(&f.user, "googlechat", "users/123", true).unwrap();
        rusqlite::Connection::open(f.env.home.join("users.db")).unwrap().execute(
            "UPDATE channel_identities SET proof_method='chat_handshake',proof_user_id=user_id,
             proof_account='googlechat',proof_dm='spaces/DM1',proven_at=?1 WHERE user_id=?2 AND channel='googlechat'",
            rusqlite::params![chrono::Utc::now().timestamp(), f.user]).unwrap();
    }
}

async fn evidence(env: &DecisionEnv, id: &ApprovalId, user: &str, channel: &str,
    step: &str, approve: bool, required: bool, chat: bool) {
    let rec = env.broker.get(id).await.unwrap().unwrap();
    let expected = if chat { format!("chat-admin:{user}:{channel}:{step}") }
        else { format!("dashboard:{user}") };
    assert_eq!(rec.status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
    assert_eq!(rec.decided_by.as_deref(), Some(expected.as_str()));
    let conn = env.broker.store.conn.lock().await;
    let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1",
        [id.as_str()], |r| r.get(0)).unwrap();
    assert_eq!(count, i64::from(chat));
    assert_eq!(ledger::accepts(&conn, &rec), chat);
    if chat {
        let saved: (bool, String, String, String) = conn.query_row(
            "SELECT step_required,user_id,channel,step FROM chat_admin_decisions WHERE approval_id=?1",
            [id.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(saved, (required, user.into(), channel.into(), step.into()));
    }
    drop(conn);
    let db = env.users.as_ref().unwrap();
    let activity = db.query_audit_log(Some(user), Some("approval.chat_admin_decision"), 100).unwrap();
    // Each matrix case has an isolated home and exactly one attempted decision.
    assert_eq!(activity.len(), usize::from(chat));
    let audits = std::fs::read_to_string(env.home.join("security_audit.jsonl")).unwrap_or_default();
    let decisions: Vec<Value> = audits.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event_type"] == "approval_chat_admin_decided"
            && row["details"]["approval_id"] == id.as_str()).collect();
    assert_eq!(decisions.len(), usize::from(chat));
    if chat {
        let detail = &decisions[0]["details"];
        assert_eq!(detail["user_id"], user);
        assert_eq!(detail["channel"], channel);
        assert_eq!(detail["step"], step);
        assert_eq!(detail["approve"], approve);
        assert_eq!(detail["step_required"], required);
    }
}

#[tokio::test]
async fn t1_t2_decision_entry_point_matrix() {
    // Web-only and TOTP-only each cover the disabled-method refusals. The
    // exempt profile proves opt-out is per kind, with both methods available.
    for channel in ["telegram", "googlechat"] {
        for (enabled_methods, exempt) in [(&["web"][..], false), (&["dm_totp"][..], false),
            (&["web", "dm_totp"][..], true)] {
            for kind in KINDS {
                for approve in [true, false] {
                    let mut dashboard_effect = None;
                    for entry in ["dashboard", "tap", "web", "totp"] {
                        let f = Fixture::files_org().await;
                        let key = if *kind == "other_outgoing" { "egress_guard" } else { kind };
                        let keys = [key];
                        let selected: &[&str] = if exempt && *kind != "knowledge_quarantine" && *kind != "knowledge_add_source" { &keys } else { &[] };
                        methods(&f, channel, selected, enabled_methods);
                        let id = request(&f, kind).await;
                        let rec = f.env.broker.get(&id).await.unwrap().unwrap();
                        let before = effect(&f, &id, kind).await;
                        let required = approve && !exempt && tier(&rec) == Tier::T2;
                        let allowed = entry == "dashboard" || match entry {
                            "tap" => !required,
                            "web" => enabled_methods.contains(&"web"),
                            "totp" => enabled_methods.contains(&"dm_totp") && channel == "telegram",
                            _ => unreachable!(),
                        };
                        if entry == "dashboard" {
                            payload(dashboard(&f.env, &f.user, &id, approve).await);
                            evidence(&f.env, &id, &f.user, channel, entry, approve, false, false).await;
                            dashboard_effect = Some(settled_effect(&f, &id, kind).await);
                            continue;
                        }
                        let sender = Recording::new(&f.env.broker.store);
                        assert_eq!(deliver_record(&f.env.home, &f.env.broker.store, &rec, &sender).await, 1, "{kind} {channel}");
                        let receipt = receipts::list_live(&f.env.broker.store, &id).await.unwrap().pop().unwrap();
                        drop(sender);
                        let reason = if entry == "tap" {
                            let mut press = PressFixture::from_service(f, id.clone(), receipt).await;
                            press.context.channel = channel.into();
                            press.action.act = if approve { DecisionAct::Approve } else { DecisionAct::Deny };
                            let outcome = press.press().await;
                            assert_eq!(outcome.refused_reason, if allowed { None } else { Some("step_up_required") }, "{kind} {channel} {entry} approve={approve}");
                            if allowed {
                                assert_eq!(outcome.card_text.as_ref(), Some(&outcome.reply));
                                evidence(&press.service.env, &id, &press.service.user, channel, entry, approve, required, true).await;
                                assert_eq!(Some(settled_effect(&press.service, &id, kind).await), dashboard_effect);
                            } else {
                                press.pending().await;
                                assert_eq!(effect(&press.service, &id, kind).await, before);
                            }
                            continue;
                        } else {
                            let proof = decision_service::ChatAdminProof { user_id:f.user.clone(),
                                channel:if channel == "telegram" { ChatChannel::Telegram } else { ChatChannel::Googlechat },
                                account:receipt.account, principal:receipt.principal, dm:receipt.dm,
                                card_id:receipt.card_id, digest:receipt.digest,
                                step:if entry == "web" { Step::Web } else { Step::Totp } };
                            // Authenticated-factor seams; password/TOTP and ticket
                            // verification are exercised separately below/P15/P16.
                            decision_service::decide(&f.env, &id, approve, "P22 matrix", &Decider::ChatAdmin(proof)).await
                        };
                        if allowed {
                            reason.unwrap_or_else(|e| panic!("{kind} {channel} {entry} approve={approve}: {e:?}"));
                            evidence(&f.env, &id, &f.user, channel, entry, approve, required, true).await;
                            assert_eq!(Some(settled_effect(&f, &id, kind).await), dashboard_effect, "{kind} {entry}");
                        } else {
                            assert!(matches!(reason, Err(DecisionRefusal::StepUpRequired)), "{kind} {entry}: {reason:?}");
                            assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
                            assert_eq!(effect(&f, &id, kind).await, before);
                            f.assert_ledger(&id, false).await;
                        }
                    }
                }
            }
        }
    }
}

async fn t3_row(env: &DecisionEnv, user: &str, id: &ApprovalId) {
    let rec = env.broker.get(id).await.unwrap().unwrap();
    assert_eq!(chat_tier(Some(&env.home), &rec), Tier::T3);
    let sender = Recording::new(&env.broker.store);
    assert_eq!(deliver_record(&env.home, &env.broker.store, &rec, &sender).await, 0);
    assert!(sender.sent.lock().unwrap().is_empty());
    assert!(receipts::list_live(&env.broker.store, id).await.unwrap().is_empty());
    let ctx = reply_context(env);
    let context = approval::DecisionContext { channel:"telegram".into(), account_id:"telegram".into(),
        conversation_id:"123456789".into(), principal_id:"123456789".into() };
    let origin = press::PressOrigin { account:"telegram".into(), dm:"123456789".into(),
        private:true, message_id:Some("forged-message".into()) };
    let action = DecisionAction { source:DecisionSource::Approval, act:DecisionAct::Approve, id:id.to_string() };
    assert!(press::handle(&ctx, &context, &action, &rec, &origin, PRIVATE).await.refused_reason.is_some());
    // Even forged internal evidence must not bypass classification.
    let proof = decision_service::ChatAdminProof { user_id:user.into(), channel:ChatChannel::Telegram,
        account:origin.account, principal:context.principal_id, dm:origin.dm,
        card_id:uuid::Uuid::new_v4().to_string(), digest:approval::decision_digest_at(&env.home, &rec), step:Step::Web };
    for approve in [true, false] {
        assert!(matches!(decision_service::decide(env, id, approve, "", &Decider::ChatAdmin(proof.clone())).await,
            Err(DecisionRefusal::NotSupportedInChat)));
    }
    assert_eq!(env.broker.get(id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    let count: i64 = env.broker.store.conn.lock().await.query_row(
        "SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1", [id.as_str()], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn t3_kinds_refused_in_chat() {
    let kinds = std::iter::once(crate::isolation_background::PERSONAL_DELEGATION_KIND)
        .chain(approval::operator_cli_gate::OPERATOR_CLI_KINDS.iter().map(|spec| spec.kind))
        .chain(std::iter::once("unknown_rm143_kind"));
    for kind in kinds {
        let f = Fixture::new();
        let id = f.simple(kind).await;
        t3_row(&f.env, &f.user, &id).await;
        let frame = dashboard(&f.env, &f.user, &id, true).await;
        if kind == crate::isolation_background::PERSONAL_DELEGATION_KIND {
            // This kind retains its dedicated owner-only dashboard RPC.
            match frame {
                WsFrame::Response { ok:false, error:Some(error), .. } =>
                    assert_eq!(error, json!(crate::isolation_background::PERSONAL_DELEGATION_DECIDE_ELSEWHERE)),
                other => panic!("personal delegation must keep its dedicated route: {other:?}"),
            }
        } else {
            payload(frame);
            evidence(&f.env, &id, &f.user, "telegram", "tap", true, false, false).await;
        }
    }
    let mail = approval::mail_requester::tests::Fixture::new();
    let admin = mail.db.create_user("p22-admin@example.test", "Admin", "fixture-password-only", UserRole::Admin).unwrap();
    let (id, _) = mail.request("mail_send").await;
    assert!(approval::mail_requester::eligible(mail.home.path(), &mail.broker.get(&id).await.unwrap().unwrap()));
    let env = DecisionEnv { home:mail.home.path().to_path_buf(), broker:mail.broker.clone(), registry:None,
        users:Some(Arc::new(duduclaw_auth::UserDb::new(&mail.home.path().join("users.db")).unwrap())) };
    t3_row(&env, &admin.id, &id).await;
    payload(dashboard(&env, &admin.id, &id, true).await);
    evidence(&env, &id, &admin.id, "telegram", "tap", true, false, false).await;
}

async fn link_call(handler: &MethodHandler, ctx: &UserContext, method: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), DASHBOARD_SESSION_ID.into())),
        handler.handle_chat_link_rpc(method, params, ctx)).await
}

async fn settings(f: &Fixture, enabled: bool, exempt: &[&str], acknowledge: bool) -> WsFrame {
    let h = handler(&f.env).await;
    let ctx = f.db().live_user_context(&f.user).unwrap().unwrap();
    let view = crate::google_personal::DASHBOARD_SESSION.scope(
        Some((f.user.clone(), DASHBOARD_SESSION_ID.into())),
        h.handle_chat_admin_settings_rpc("approvals.chat_admin.get", json!({}), &ctx)).await;
    let view = payload(view);
    crate::google_personal::DASHBOARD_SESSION.scope(Some((f.user.clone(), DASHBOARD_SESSION_ID.into())),
        h.handle_chat_admin_settings_rpc("approvals.chat_admin.set", json!({
            "expected_hash":view["hash"], "enabled":enabled, "channels":["telegram"],
            "step_up_methods":["web","dm_totp"], "card_valid_hours":168,
            "step_up_exempt_kinds":exempt, "acknowledge_risk":acknowledge,
            "acknowledge_exempt_risk":exempt,
        }), &ctx)).await
}

async fn handshake(f: &Fixture, confirm: bool) -> (String, Value) {
    f.db().unbind_channel_identity("telegram", "123456789").unwrap();
    let h = handler(&f.env).await;
    let ctx = f.db().live_user_context(&f.user).unwrap().unwrap();
    let started = payload(link_call(&h, &ctx, "me.chat_link.start", json!({"channel":"telegram"})).await);
    // Unique principals avoid coupling refusal audit throttles between tests.
    let principal = (u64::from(uuid::Uuid::new_v4().as_fields().0) + 1).to_string();
    assert_eq!(crate::chat_link_intercept::handle(f.db(), "telegram", "telegram", &principal,
        &principal, "Ada", true, started["code"].as_str().unwrap()),
        Some(crate::chat_link_intercept::InterceptReply::Matched));
    assert!(f.db().approval_proofs(&f.user).unwrap().is_empty());
    let status = payload(link_call(&h, &ctx, "me.chat_link.status",
        json!({"handshake_id":started["handshake_id"]})).await);
    assert_eq!(status["masked_target"], duduclaw_auth::otp::mask_target(&principal));
    assert_eq!(status["display_name"], "Ada");
    let method = if confirm { "me.chat_link.confirm" } else { "me.chat_link.reject" };
    payload(link_call(&h, &ctx, method, json!({"handshake_id":started["handshake_id"]})).await);
    if confirm {
        let proof = f.db().approval_proof(&f.user, "telegram").unwrap().unwrap();
        assert_eq!(proof.channel_user_id, principal);
        assert_eq!(proof.proof_dm, principal);
    } else {
        assert!(f.db().approval_proofs(&f.user).unwrap().is_empty());
    }
    (principal, started)
}

async fn linked_card(kind: &str, exempt: bool) -> PressFixture {
    let f = Fixture::files_org().await;
    f.config(false, "telegram", &[]);
    handshake(&f, true).await;
    let kinds = [kind];
    let selected: &[&str] = if exempt { &kinds } else { &[] };
    payload(settings(&f, true, selected, true).await);
    let id = request(&f, kind).await;
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    let receipt = {
        let sender = Recording::new(&f.env.broker.store);
        assert_eq!(deliver_record(&f.env.home, &f.env.broker.store, &rec, &sender).await, 1);
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
        receipts::list_live(&f.env.broker.store, &id).await.unwrap().pop().unwrap()
    };
    assert_eq!(receipt.digest, approval::decision_digest_at(&f.env.home, &rec));
    PressFixture::from_service(f, id, receipt).await
}

async fn assert_dashboard_sync(f: &PressFixture, status: ApprovalStatus) {
    let bus = crate::events_store::EventBusStore::open(&f.service.env.home).unwrap();
    let rows = bus.fetch_since(0, 100).await.unwrap();
    let decided: Vec<Value> = rows.iter().filter(|row| row.event == "approval.decided")
        .map(|row| serde_json::from_str::<Value>(&row.payload).unwrap())
        .filter(|row| row["id"] == f.row.id.as_str()).collect();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0]["status"], status.as_str());
    let h = handler(&f.service.env).await;
    let ctx = f.service.db().live_user_context(&f.service.user).unwrap().unwrap();
    let listed = payload(h.handle_approvals_list(json!({"id":f.row.id.as_str()}), &ctx).await);
    assert_eq!(listed["approvals"].as_array().unwrap().len(), 1);
    assert_eq!(listed["approvals"][0]["status"], status.as_str());
    assert_eq!(listed["approvals"][0]["decided_by_display"], "Ada（Telegram，直接核准）");
    assert!(listed["approvals"][0].get("decided_by").is_none());
    assert!(!listed["approvals"][0]["decided_by_display"].as_str().unwrap().contains(&f.service.user));
}

#[tokio::test]
async fn default_off_keeps_dashboard_file_decision() {
    // §7.1: preserve upgrade defaults with no chat_admin subtable at all.
    let f = Fixture::files_org().await;
    std::fs::write(f.env.home.join("config.toml"), "[channels]\ntelegram_bot_token_enc='invalid-encrypted-fixture'\n").unwrap();
    let id = f.file_change().await;
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    assert!(deliver::try_push(&f.env.home, &rec, false).await.is_none());
    let sender = Recording::new(&f.env.broker.store);
    assert_eq!(deliver_record(&f.env.home, &f.env.broker.store, &rec, &sender).await, 0);
    let ctx = reply_context(&f.env);
    let context = approval::DecisionContext { channel:"telegram".into(), account_id:"telegram".into(),
        conversation_id:"123456789".into(), principal_id:"123456789".into() };
    let origin = press::PressOrigin { account:"telegram".into(), dm:"123456789".into(),
        private:true, message_id:Some("invented".into()) };
    let action = DecisionAction { source:DecisionSource::Approval, act:DecisionAct::Approve, id:id.to_string() };
    assert!(press::handle(&ctx, &context, &action, &rec, &origin, PRIVATE).await.refused_reason.is_some());
    payload(dashboard(&f.env, &f.user, &id, true).await);
    assert!(f.files_org_effect(false).as_str().unwrap().contains("先確認問題"));
    evidence(&f.env, &id, &f.user, "telegram", "tap", true, false, false).await;
}

#[tokio::test]
async fn handshake_settings_delivery_press_rewrite_dashboard_sync() {
    // §7.2,3,5,6: the owner opt-out replaces the obsolete default-no-step-up scenario.
    let f = Fixture::files_org().await;
    f.config(false, "telegram", &[]);
    let original = std::fs::read_to_string(f.env.home.join("config.toml")).unwrap();
    assert!(matches!(settings(&f, true, &[], false).await, WsFrame::Response { ok:false, .. }));
    assert_eq!(std::fs::read_to_string(f.env.home.join("config.toml")).unwrap(), original);
    let (_, started) = handshake(&f, true).await;
    assert_eq!(crate::chat_link_intercept::route(started["code"].as_str().unwrap(), true),
        crate::chat_link_intercept::CodeRoute::Match);
    assert!(!crate::chat_link_intercept::context_text(started["code"].as_str().unwrap()).contains(started["code"].as_str().unwrap()));
    payload(settings(&f, true, &["expert_skills_enable"], true).await);
    let before_raw_edit = std::fs::read_to_string(f.env.home.join("config.toml")).unwrap();
    let ctx = f.db().live_user_context(&f.user).unwrap().unwrap();
    assert!(matches!(handler(&f.env).await.handle_system_update_config(
        json!({"approval":{"chat_admin":{"enabled":false}}}), &ctx).await, WsFrame::Response { ok:false, .. }));
    assert_eq!(std::fs::read_to_string(f.env.home.join("config.toml")).unwrap(), before_raw_edit);
    let cfg = load(&f.env.home);
    assert_eq!(cfg.enabled_ack.unwrap().by.to_string(), f.user);
    let audit = std::fs::read_to_string(f.env.home.join("security_audit.jsonl")).unwrap();
    let changes: Vec<Value> = audit.lines().map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r["event_type"] == "approval_chat_admin_settings_changed" && r["details"]["phase"] == "committed").collect();
    assert_eq!(changes.len(), 1);
    assert_ne!(changes[0]["details"]["before"], changes[0]["details"]["after"]);
    assert!(!audit.contains(started["code"].as_str().unwrap()));
    let id = request(&f, "expert_skills_enable").await;
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    let receipt = {
        let sender = Recording::new(&f.env.broker.store);
        assert_eq!(deliver_record(&f.env.home, &f.env.broker.store, &rec, &sender).await, 1);
        let live = receipts::list_live(&f.env.broker.store, &id).await.unwrap();
        assert_eq!(live.len(), 1);
        assert!(sender.sent.lock().unwrap().iter().all(|(_, channel, dm)| channel == "telegram" && !dm.starts_with('-')));
        live.into_iter().find(|r| r.user_id == f.user).unwrap()
    };
    let f = PressFixture::from_service(f, id, receipt).await;
    let outcome = f.press().await;
    assert_eq!(outcome.refused_reason, None);
    assert_eq!(outcome.card_text.as_ref(), Some(&outcome.reply));
    assert_eq!(f.saved_receipt().await.status, "consumed");
    let rec = f.service.env.broker.get(&f.row.id).await.unwrap().unwrap();
    let sender = Recording::new(&f.service.env.broker.store);
    sync::collapse_with_sender(&f.service.env.broker.store, &rec, true, Some("Ada".into()), "Telegram", Lang::ZhTw, &sender).await;
    // The adapter rewrites the pressed card from card_text; sync skips that
    // consumed receipt. Multi-Admin rewrites use the existing P17 tests.
    assert!(sender.edits.lock().unwrap().is_empty());
    assert_eq!(settled_effect(&f.service, &f.row.id, "expert_skills_enable").await["file"], "echo hi");
    assert!(receipts::list_live(&f.service.env.broker.store, &f.row.id).await.unwrap().is_empty());
    assert_eq!(f.press().await.refused_reason, Some("already_decided"));
    evidence(&f.service.env, &f.row.id, &f.service.user, "telegram", "tap", true, false, true).await;
    assert_dashboard_sync(&f, ApprovalStatus::Approved).await;
}

#[tokio::test]
async fn rejected_handshake_never_grants_card_authority() {
    // §7.4: matching is insufficient; rejection permanently invalidates the code.
    let f = Fixture::new();
    f.config(false, "telegram", &[]);
    let (principal, started) = handshake(&f, false).await;
    assert_eq!(crate::chat_link_intercept::handle(f.db(), "telegram", "telegram", &principal,
        &principal, "Stranger", true, started["code"].as_str().unwrap()),
        Some(crate::chat_link_intercept::InterceptReply::InvalidOrExpired));
    assert_eq!(f.db().query_audit_log(Some(&f.user), Some("channel_link_rejected"), 10).unwrap().len(), 1);
    payload(settings(&f, true, &[], true).await);
    let id = f.simple("expert_skills_enable").await;
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    let sender = Recording::new(&f.env.broker.store);
    assert_eq!(deliver_record(&f.env.home, &f.env.broker.store, &rec, &sender).await, 0);
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
}

#[tokio::test]
async fn prefilled_manager_group_and_revoked_proof_flows_refused() {
    // §7.7,12 plus the owner's proof-only revocation rule.
    for case in ["prefilled", "manager", "group", "demoted", "unbound", "proof_revoked"] {
        let mut f = linked_card("expert_skills_enable", true).await;
        match case {
            "prefilled" | "proof_revoked" => {
                assert!(f.service.db().clear_approval_proof(&f.service.user, "telegram", &f.context.principal_id).unwrap());
                assert_eq!(f.service.db().find_verified_user_id_by_channel("telegram", &f.context.principal_id).unwrap(), Some(f.service.user.clone()));
            }
            "manager" | "demoted" => f.service.db().update_user(&f.service.user, None, Some(UserRole::Manager), None).unwrap(),
            "group" => f.origin.private = false,
            "unbound" => { f.service.db().unbind_channel_identity("telegram", &f.context.principal_id).unwrap(); }
            _ => unreachable!(),
        }
        assert!(f.press().await.refused_reason.is_some(), "{case}");
        f.pending().await;
        let audit = std::fs::read_to_string(f.service.env.home.join("security_audit.jsonl")).unwrap();
        assert_eq!(audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|r| r["event_type"] == "approval_chat_admin_refused" && r["details"]["approval_id"] == f.row.id.as_str()).count(), 1, "{case}");
    }
}

#[tokio::test]
async fn strict_text_fake_card_flow_never_decides() {
    // §7.8 / §4 fake AI text card: a real UUID is not decision authority.
    let mut f = linked_card("expert_skills_enable", true).await;
    for verb in ["確認", "取消", "approve", "deny"] {
        let result = crate::decision_notify::route_trusted_decision_fastlane_with_scope(
            &f.ctx, &f.context, &format!("{verb} {}", f.row.id), PRIVATE).await;
        assert_eq!(result, Some(Err("這類審批請按卡片上的按鈕".into())));
    }
    f.origin.message_id = Some("ai-forged-card".into());
    assert_eq!(f.press().await.refused_reason, Some("receipt_missing"));
    assert!(crate::decision_text::route_verified_text_reply(&f.ctx, &f.context, &f.origin, PRIVATE, "核准").await.is_none());
    f.pending().await;
}

#[tokio::test]
async fn digest_drift_flow_delivers_new_receipt_before_retry() {
    // §7.9: use the recording delivery seam, not an unconditional resend success.
    let mut f = linked_card("expert_skills_enable", true).await;
    f.service.env.broker.replace_text(&f.row.id, "changed after delivery", &f.row.payload).await.unwrap();
    let receipt = {
        let mut sender = Recording::new(&f.service.env.broker.store);
        sender.message_offset = 100;
        let outcome = press::tests::resend_with_recording(&f, &sender).await;
        assert_eq!(outcome.refused_reason, Some("digest_mismatch"));
        assert!(outcome.reply.contains("已送出新卡"));
        f.pending().await;
        assert_eq!(f.saved_receipt().await.status, "retired");
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
        let live = receipts::list_live(&f.service.env.broker.store, &f.row.id).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_ne!(live[0].digest, f.receipt.digest);
        live[0].clone()
    };
    f.origin.message_id = receipt.message_id.clone();
    f.receipt = receipt;
    assert_eq!(f.press().await.refused_reason, None);
    evidence(&f.service.env, &f.row.id, &f.service.user, "telegram", "tap", true, false, true).await;
    assert_dashboard_sync(&f, ApprovalStatus::Approved).await;
}

#[tokio::test]
async fn feature_off_sweep_flow_retires_and_refuses_old_card() {
    // §7.14: inspect actual rewritten text and durable receipt retirement.
    let f = linked_card("expert_skills_enable", true).await;
    f.service.config(false, "telegram", &[]);
    let sender = Recording::new(&f.service.env.broker.store);
    sync::sweep_feature_off_with_sender(&f.service.env.home, &f.service.env.broker.store, &sender).await;
    assert_eq!(sender.edits.lock().unwrap().len(), 1);
    assert_eq!(sender.edits.lock().unwrap()[0].2, card::feature_off_text(Lang::ZhTw));
    assert_eq!(f.saved_receipt().await.status, "retired");
    assert!(f.press().await.refused_reason.is_some());
    f.pending().await;
    f.service.config(true, "telegram", &["expert_skills_enable"]);
    assert_eq!(f.press().await.refused_reason, Some("receipt_not_live"));
}

#[tokio::test]
async fn workflow_web_password_totp_flow_grants_revision() {
    // §7.10: reuse the real fixed-revision fixture, leaving its approval pending.
    let f = Fixture::new();
    f.config(false, "telegram", &[]);
    handshake(&f, true).await;
    payload(settings(&f, true, &[], true).await);
    let accepted = workflow_card(&f).await;
    let id = ApprovalId::from(accepted.acceptance_id.clone());
    let rec = f.env.broker.get(&id).await.unwrap().unwrap();
    let receipt = {
        let sender = Recording::new(&f.env.broker.store);
        assert_eq!(deliver_record(&f.env.home, &f.env.broker.store, &rec, &sender).await, 1);
        receipts::list_live(&f.env.broker.store, &id).await.unwrap().pop().unwrap()
    };
    assert_eq!(receipt.digest, approval::decision_digest_at(&f.env.home, &rec));
    let f = PressFixture::from_service(f, id.clone(), receipt.clone()).await;
    assert_eq!(f.press().await.refused_reason, Some("step_up_required"));
    f.pending().await;
    // Issue the same ticket that an HTTPS card link locates. The externally
    // reachable link itself requires live deployment and is not simulated.
    let web = step_up::tests::WebFixture::from_service(f.service, id.clone(), receipt);
    assert!(web.post(web.input(true, None)).await.is_err());
    let csrf = web.step_up(); // Real password + TOTP verifier, same dashboard session.
    assert_eq!(web.get().await.unwrap()["has_totp"], true);
    let result = web.post(web.input(true, Some(csrf))).await.unwrap();
    assert_eq!(result["decided_by"], format!("chat-admin:{}:telegram:web", web.service.user));
    evidence(&web.service.env, &id, &web.service.user, "telegram", "web", true, true, true).await;
    let prepared = web.service.env.broker.prepare_revision_grant(&accepted).await.unwrap();
    let active = web.service.env.broker.activate_revision_grant(&prepared).await.unwrap();
    assert_eq!(active.grant_id, prepared.grant_id);
    assert!(active.epoch > prepared.epoch);
    assert!(web.post(web.input(true, Some(csrf))).await.is_err());
}

#[tokio::test]
async fn confirm_page_reject_flow_needs_no_step_up() {
    // Owner correction: rejecting at the confirm page needs no factors.
    let f = linked_card("workflow_activation", false).await;
    let id = f.row.id.clone();
    let web = step_up::tests::WebFixture::from_service(f.service, id.clone(), f.receipt);
    assert_eq!(web.get().await.unwrap()["has_totp"], false);
    assert_eq!(web.post(web.input(false, None)).await.unwrap()["status"], "done");
    evidence(&web.service.env, &id, &web.service.user, "telegram", "web", false, false, true).await;
}

#[tokio::test]
async fn dm_totp_flow_rejects_recovery_decides_once_and_deletes_codes() {
    // §7.11: real enrollment, recovery-code rejection, one valid code, and
    // replay against another delivered card. Recording captures delete calls.
    use step_up::dm_totp::tests::{DmFixture, Recording as DmRecording, code};
    use std::sync::atomic::Ordering;
    let f = linked_card("expert_skills_enable", false).await;
    let mut dm = DmFixture::from_service(f.service, f.row.id, f.receipt).await;
    let sink = DmRecording::default();
    assert!(dm.receive(dm.event(&dm.recovery), &sink).await);
    assert_eq!(dm.state(), ("live".into(), 1));
    assert_eq!(dm.service.status(&dm.id).await, ApprovalStatus::Pending);
    let valid = code(&dm.secret, chrono::Utc::now().timestamp()/30);
    assert!(dm.receive(dm.event(&valid), &sink).await);
    evidence(&dm.service.env, &dm.id, &dm.service.user, "telegram", "totp", true, true, true).await;
    assert_eq!(settled_effect(&dm.service, &dm.id, "expert_skills_enable").await["file"], "echo hi");
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 2);
    let id = dm.service.simple("expert_skills_enable").await;
    let rec = dm.service.env.broker.get(&id).await.unwrap().unwrap();
    let receipt = {
        let mut sender = Recording::new(&dm.service.env.broker.store);
        sender.message_offset = 100;
        assert_eq!(deliver_record(&dm.service.env.home, &dm.service.env.broker.store, &rec, &sender).await, 1);
        receipts::list_live(&dm.service.env.broker.store, &id).await.unwrap().pop().unwrap()
    };
    dm.next_prompt(id.clone(), receipt, "51");
    let mut event = dm.event(&valid);
    event.reply_to = Some("51".into());
    assert!(dm.receive(event, &sink).await);
    assert_eq!(dm.state(), ("live".into(), 1));
    assert_eq!(dm.service.status(&id).await, ApprovalStatus::Pending);
    dm.service.assert_ledger(&id, false).await;
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 3);
    assert!(sink.replies.lock().unwrap().iter().all(|text| !text.contains(&valid) && !text.contains(&dm.recovery)));
}
