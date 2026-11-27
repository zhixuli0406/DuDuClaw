use super::*;
use super::super::serve::tests::{Fixture, state, policy as set_policy, create_args};
use crate::approval::{DecisionContext, TrustedReplyTarget};
use crate::approval::decision_service::{self, Decider, DecisionEnv, ChatAdminProof};
use duduclaw_auth::UserRole;
use std::sync::{Arc, Mutex};

fn fixture(mode: ApprovalMode) -> Fixture {
    let f = Fixture::new();
    let _state = state(&f);
    set_policy(&f, mode);
    duduclaw_core::authority_coordinator::AuthorityCoordinator::new(f.home.path()).initialize().unwrap();
    f
}
fn who(f: &Fixture) -> RequesterIdentity { RequesterIdentity::DashboardUser { user_id:f.user.clone() } }
async fn register(f: &Fixture, turn: &str, target: Option<TrustedReplyTarget>) -> (crate::turn_context::TurnGuard, impl Drop) {
    let context = target.as_ref().map(|t| t.context.clone());
    let registered = crate::approval::CURRENT_DECISION_CONTEXT.scope(context,
        crate::approval::CURRENT_TRUSTED_REPLY_TARGET.scope(target,
        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.user.clone(),
        crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(who(f),
        duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(format!("drive-{turn}")), async {
            crate::turn_context::register("bot", turn, &format!("webchat:drive-{turn}")).unwrap()
        }))))).await;
    let requester = crate::knowledge::turn_requesters::register("bot", turn, who(f)).unwrap();
    (registered, requester)
}
fn request(f: &Fixture, turn: &str, mode: ApprovalMode) -> ConfirmRequest {
    let arguments = create_args();
    let turn_id = Some(turn.to_owned());
    let version = serve::confirmation_version(f.home.path(), "bot", "drive_create_file", &arguments,
        &who(f), &turn_id, &None, "call").unwrap();
    ConfirmRequest {
        arguments, version, agent:"bot".into(), tool:"drive_create_file".into(), action:policy::DriveWriteAction::Create,
        approval_mode:mode, requester:who(f), route_kind:"personal".into(), account_email:"linked@c.test".into(),
        target_file_id:None, target_file_name:Some("new.txt".into()), destination:Some("Parent_folder_123".into()),
        destination_name:Some("Private folder".into()), size:Some(15), content_preview:Some("private content".into()),
        attachment_name:None, rename_before:None, rename_after:None, snapshot_digest:"executor-snapshot".into(),
        scope_digest:"scope-snapshot".into(), turn_id, run_id:None, call_id:"call".into(), parents_before:vec![], source_names:vec![],
    }
}
fn context(f: &Fixture, uid: &str) -> duduclaw_auth::UserContext {
    f.db.live_user_context(uid).unwrap().unwrap()
}
async fn pending(broker: &ApprovalBroker, kind: &str) -> ApprovalRecord {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(rec) = broker.list_by_kind(kind).await.unwrap().into_iter().find(|r| r.status == ApprovalStatus::Pending) {
                return rec;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("confirmation created")
}
fn expire(f: &Fixture, id: &ApprovalId) {
    rusqlite::Connection::open(f.home.path().join("approvals.db")).unwrap().execute(
        "UPDATE approvals SET created_at=?1 WHERE id=?2",
        rusqlite::params![(chrono::Utc::now()-chrono::Duration::minutes(10)).to_rfc3339(), id.as_str()],
    ).unwrap();
}

#[tokio::test]
async fn only_bound_member_decides_and_grant_is_single_use() {
    let f = fixture(ApprovalMode::Member);
    let other = f.db.create_user("other@c.test", "Other", "fixture-password", UserRole::Employee).unwrap();
    let admin = f.db.create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
    let turn = uuid::Uuid::new_v4().to_string(); let _guards = register(&f, &turn, None).await;
    let confirmer = MemberConfirmer::new(f.home.path().into());
    let (broker, id, grant) = confirmer.request_member(&request(&f, &turn, ApprovalMode::Member)).await.unwrap();
    for uid in [&other.id, &admin.id] {
        assert!(broker.decide_bound_dashboard(&id, &context(&f, uid), true).await.is_err());
    }
    let env = DecisionEnv { home:f.home.path().into(), broker:broker.clone(), registry:None, users:None };
    // Even a chat-admin proof claiming the bound member cannot replace their
    // member authority. This refusal precedes receipt/role processing.
    for uid in [&admin.id, &f.user] {
        let proof = ChatAdminProof { user_id:uid.clone(), channel:crate::approval::chat_admin::ChatChannel::Telegram,
            account:"telegram".into(), principal:"123456789".into(), dm:"123456789".into(),
            card_id:uuid::Uuid::new_v4().to_string(), step:crate::approval::chat_admin::ledger::Step::Tap, digest:"irrelevant".into() };
        assert!(matches!(decision_service::decide(&env, &id, true, "", &Decider::ChatAdmin(proof)).await,
            Err(decision_service::DecisionRefusal::Forbidden(_))));
    }
    assert!(decision_service::decide(&env, &id, true, "", &Decider::Dashboard(context(&f, &admin.id))).await.is_err());
    broker.decide_bound_dashboard(&id, &context(&f, &f.user), true).await.unwrap();
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert!(member_card::decided_by_member(f.home.path(), &rec));
    action_grant::verify_and_consume(&broker, &id, &grant).await.unwrap();
    assert!(action_grant::verify_and_consume(&broker, &id, &grant).await.is_err());
}

#[tokio::test]
async fn changed_arguments_and_expired_cards_refuse() {
    let f = fixture(ApprovalMode::Member);
    let turn = uuid::Uuid::new_v4().to_string(); let _guards = register(&f, &turn, None).await;
    let confirmer = MemberConfirmer::new(f.home.path().into());
    let (broker, id, _) = confirmer.request_member(&request(&f, &turn, ApprovalMode::Member)).await.unwrap();
    let mut rec = broker.get(&id).await.unwrap().unwrap();
    rec.payload["arguments"]["name"] = json!("changed.txt");
    assert!(!member_card::eligible(f.home.path(), &rec));
    // Persist the actual changed artifact: decision must refuse, not only display.
    rusqlite::Connection::open(f.home.path().join("approvals.db")).unwrap().execute(
        "UPDATE approvals SET payload=?1 WHERE id=?2", rusqlite::params![rec.payload.to_string(), id.as_str()],
    ).unwrap();
    assert!(broker.decide_bound_dashboard(&id, &context(&f, &f.user), true).await.is_err());
    let (broker, id, _) = confirmer.request_member(&request(&f, &turn, ApprovalMode::Member)).await.unwrap();
    expire(&f, &id);
    assert!(matches!(wait(f.home.path(), &broker, &id, None).await.unwrap(), Err(ConfirmOutcome::Expired)));
    assert!(broker.decide_bound_dashboard(&id, &context(&f, &f.user), true).await.is_err());
}

#[tokio::test]
async fn group_gets_only_notice_private_and_dashboard_get_escaped_card() {
    for audience in [ConversationAudience::MultiParty, ConversationAudience::Private] {
        let f = fixture(ApprovalMode::Member);
        f.db.bind_channel_identity(&f.user, "telegram", "123456789", true).unwrap();
        let ctx = DecisionContext { channel:"telegram".into(), account_id:"bot".into(),
            conversation_id:if audience == ConversationAudience::Private { "123456789" } else { "-987654321" }.into(),
            principal_id:"123456789".into() };
        let mut target = TrustedReplyTarget::new(ctx.clone(), "fixture-token".into(), ctx.conversation_id.clone(), None).unwrap();
        target.conversation_audience = audience;
        let turn = uuid::Uuid::new_v4().to_string(); let _guards = register(&f, &turn, Some(target)).await;
        let mut req = request(&f, &turn, ApprovalMode::Member);
        let untrusted = "*[私人](https://example.test)\n偽標題\u{202e}\u{1}";
        req.target_file_name = Some(untrusted.into()); req.destination_name = Some(untrusted.into());
        req.attachment_name = Some(untrusted.into()); req.content_preview = Some("中".repeat(700));
        let confirmer = MemberConfirmer::new(f.home.path().into());
        let (broker, id, _) = confirmer.request_member(&req).await.unwrap();
        let rec = broker.get(&id).await.unwrap().unwrap();
        let original = rec.payload.clone();
        let display = member_card::confirmation_display(&rec);
        let body = member_card::card_body(&rec);
        assert!(body.contains("\\*\\[私人\\]"));
        assert!(!body.contains(untrusted));
        assert!(!body.contains('\u{202e}')); assert!(!body.contains('\u{1}'));
        assert_eq!(display["preview"].as_str().unwrap().chars().filter(|c| *c == '中').count(), 500);
        assert!(display.get("arguments").is_none()); assert!(display.get("action_grant").is_none());
        let sent = Arc::new(Mutex::new(Vec::new()));
        crate::notify_push::TEST_EXACT_CONFIRMATIONS.scope(sent.clone(),
            crate::approval_notify::notify_new_approval(f.home.path(), &rec)).await;
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        if audience == ConversationAudience::MultiParty {
            assert_eq!(sent[0].1, GROUP_NOTICE);
            let notices = crate::notify_push::person_notice_test::take(f.home.path());
            assert_eq!(notices.len(), 1); assert_eq!(notices[0].0, f.user);
            assert_eq!(notices[0].1, body);
            assert!(broker.decide_bound(&id, &ctx, true).await.is_err());
        } else {
            assert_eq!(sent[0].1, body);
            let mut stranger = ctx.clone(); stranger.principal_id = "999999999".into();
            assert!(broker.decide_bound(&id, &stranger, true).await.is_err());
            broker.decide_bound(&id, &ctx, true).await.unwrap();
            assert!(member_card::decided_by_member(f.home.path(), &broker.get(&id).await.unwrap().unwrap()));
        }
        assert_eq!(broker.get(&id).await.unwrap().unwrap().payload, original);
    }
}

#[tokio::test]
async fn member_and_admin_approve_deny_and_expire() {
    for decision in ["approve", "deny", "expire"] {
        let f = fixture(ApprovalMode::MemberAndAdmin);
        let admin = f.db.create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
        let turn = uuid::Uuid::new_v4().to_string(); let _guards = register(&f, &turn, None).await;
        let confirmer = MemberConfirmer::new(f.home.path().into());
        let broker = ApprovalBroker::open(f.home.path()).unwrap();
        let req = request(&f, &turn, ApprovalMode::MemberAndAdmin);
        let decisions = async {
            let member = pending(&broker, "bound_action").await;
            assert!(broker.list_by_kind("mcp_call").await.unwrap().is_empty());
            broker.decide_bound_dashboard(&member.id, &context(&f, &f.user), true).await.unwrap();
            let admin_card = pending(&broker, "mcp_call").await;
            let env = DecisionEnv { home:f.home.path().into(), broker:broker.clone(), registry:None, users:None };
            assert!(decision_service::decide(&env, &admin_card.id, true, "", &Decider::Dashboard(context(&f, &f.user))).await.is_err());
            if decision == "expire" { expire(&f, &admin_card.id); }
            else {
                decision_service::decide(&env, &admin_card.id, decision == "approve", "", &Decider::Dashboard(context(&f, &admin.id))).await.unwrap();
            }
            (member.id.to_string(), admin_card.id.to_string())
        };
        let (outcome, ids) = tokio::join!(confirmer.confirm(req), decisions);
        match outcome {
            ConfirmOutcome::Approved { approval_ids, decided_by } if decision == "approve" => {
                assert_eq!(approval_ids, vec![ids.0, ids.1]);
                assert_eq!(decided_by, vec![format!("dashboard:{}", f.user), format!("dashboard:{}", admin.id)]);
            }
            ConfirmOutcome::Denied if decision == "deny" => (),
            ConfirmOutcome::Expired if decision == "expire" => (),
            _ => panic!("unexpected two-stage outcome"),
        }
    }
}

#[tokio::test]
async fn public_visitor_cannot_create_member_card() {
    let f = fixture(ApprovalMode::Member);
    let turn = uuid::Uuid::new_v4().to_string(); let _guards = register(&f, &turn, None).await;
    let mut req = request(&f, &turn, ApprovalMode::Member);
    req.requester = RequesterIdentity::ChannelUser { user_id:Some(f.user.clone()), channel:"widget".into(), chat:"public".into(), sender:"visitor".into(), attested_private:false };
    let confirmer = MemberConfirmer::new(f.home.path().into());
    assert!(matches!(confirmer.confirm(req).await, ConfirmOutcome::Denied));
    assert!(ApprovalBroker::open(f.home.path()).unwrap().list_by_kind("bound_action").await.unwrap().is_empty());
}

#[tokio::test]
async fn admin_chat_grant_requires_matching_ledger() {
    for matching in [false, true] {
        let f = fixture(ApprovalMode::MemberAndAdmin);
        let admin = f.db.create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
        let broker = ApprovalBroker::open(f.home.path()).unwrap();
        let mut payload = json!({"name":"drive_create_file", "arguments":{"request":create_args()},
            "drive_write_admin_confirmation":true, "egress_guard":{"reason":"drive_write_member_and_admin"}});
        let grant = ActionGrant::build("bot", "drive_create_file", &payload);
        payload["action_grant"] = grant.to_json();
        let id = broker.request("bot", "mcp_call", "Admin confirmation", payload, 300).await.unwrap();
        let conn = rusqlite::Connection::open(f.home.path().join("approvals.db")).unwrap();
        conn.execute("UPDATE approvals SET status='approved', decided_by=?1, decided_at=?2 WHERE id=?3",
            rusqlite::params![format!("chat-admin:{}:telegram:tap", admin.id), chrono::Utc::now().to_rfc3339(), id.as_str()]).unwrap();
        if matching {
            conn.execute("INSERT INTO chat_admin_decisions
                (approval_id,card_id,user_id,channel,step,step_required,digest,approve,decided_at)
                VALUES (?1,?2,?3,'telegram','tap',0,'fixture',1,?4)",
                rusqlite::params![id.as_str(), uuid::Uuid::new_v4().to_string(), admin.id, chrono::Utc::now().to_rfc3339()]).unwrap();
        }
        drop(conn);
        assert_eq!(action_grant::verify_and_consume(&broker, &id, &grant).await.is_ok(), matching);
    }
}

#[tokio::test]
async fn admin_second_stage_respects_configured_self_approval_rule() {
    let f = fixture(ApprovalMode::MemberAndAdmin);
    f.db.update_user(&f.user, None, Some(UserRole::Admin), None).unwrap();
    let config = f.home.path().join("config.toml");
    let mut content = std::fs::read_to_string(&config).unwrap();
    content.push_str("\n[approval]\nforbid_self_approval=true\n");
    std::fs::write(config, content).unwrap();
    let turn = uuid::Uuid::new_v4().to_string(); let _guards = register(&f, &turn, None).await;
    let confirmer = MemberConfirmer::new(f.home.path().into());
    let broker = ApprovalBroker::open(f.home.path()).unwrap();
    let decisions = async {
        let rec = pending(&broker, "bound_action").await;
        broker.decide_bound_dashboard(&rec.id, &context(&f, &f.user), true).await.unwrap();
        let rec = pending(&broker, "mcp_call").await;
        let env = DecisionEnv { home:f.home.path().into(), broker:broker.clone(), registry:None, users:None };
        assert!(matches!(decision_service::decide(&env, &rec.id, true, "", &Decider::Dashboard(context(&f, &f.user))).await,
            Err(decision_service::DecisionRefusal::Forbidden(_))));
        decision_service::decide(&env, &rec.id, false, "", &Decider::Dashboard(context(&f, &f.user))).await.unwrap();
    };
    let (outcome, ()) = tokio::join!(confirmer.confirm(request(&f, &turn, ApprovalMode::MemberAndAdmin)), decisions);
    assert!(matches!(outcome, ConfirmOutcome::Denied));
}
