//! RM-062 regressions use the owned OAuth fixture, never a live Google account.
use super::{tests::Fixture, flow, pending, scopes};
use crate::google_identity::chat_send::{self, Snapshot, TOOL};
use crate::approval::{ApprovalBroker, ApprovalId, ExecutionBinding, RequestKind, ConversationAudience,
    action_grant::{ActionGrant, verify_and_consume}};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde_json::{json, Value};

async fn consent(f: &Fixture, code: &str) -> Result<Value, String> {
    let result = flow::start_chat_send(&f.home, &f.db, &f.alice.id, "login-a", "http://127.0.0.1:18889", &[], true).await?;
    let url = url::Url::parse(result["authorization_url"].as_str().unwrap()).unwrap();
    let scope = url.query_pairs().find(|(k,_)| k == "scope").unwrap().1.into_owned();
    assert!(scope.contains("chat.messages.create"));
    assert!(!scope.contains("gmail") && !scope.contains("chat.bot"));
    let state = result["state"].as_str().unwrap();
    pending::receive_code(&f.home, state, code)?;
    flow::finish(&f.home, &f.db, &f.alice.id, "login-a", state).await
}
async fn fixture() -> Fixture {
    let f = Fixture::new();
    f.connect().await;
    crate::google_workspace::set_chat_send_enabled(&f.home, true).unwrap();
    consent(&f, "chat-send-alice").await.unwrap();
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id).unwrap();
    duduclaw_core::authority_coordinator::AuthorityCoordinator::new(&f.home).initialize().unwrap();
    f
}
fn who(f: &Fixture) -> RequesterIdentity { RequesterIdentity::DashboardUser { user_id:f.alice.id.clone() } }
async fn register(f: &Fixture, turn: &str) -> (crate::turn_context::TurnGuard, impl Drop) {
    let context = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.alice.id.clone(),
        crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(who(f),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("chat-member-session".into()), async {
                crate::turn_context::register("bot", turn, "webchat:chat-member-session").unwrap()
            }))).await;
    let requester = crate::knowledge::turn_requesters::register("bot", turn, who(f)).unwrap();
    (context, requester)
}
async fn card(f: &Fixture, turn: &str) -> (ApprovalBroker, ApprovalId, ActionGrant) {
    card_with_audience(f, turn, ConversationAudience::Private).await
}
async fn card_with_audience(f: &Fixture, turn: &str, audience: ConversationAudience) -> (ApprovalBroker, ApprovalId, ActionGrant) {
    let snapshot = Snapshot::capture(&f.home, "bot", &who(f)).unwrap();
    let version = serde_json::to_value(&snapshot).unwrap()["version"].as_str().unwrap().to_owned();
    let mut payload = json!({"name":TOOL,"arguments":{"space":"spaces/A","text":"完整的私人文字","thread":"spaces/A/threads/T"},
        "chat_member_confirmation":true,"chat_snapshot":snapshot,"request_id":uuid::Uuid::new_v4().to_string()});
    let grant = ActionGrant::build("bot", TOOL, &payload);
    payload["action_grant"] = grant.to_json();
    let binding = ExecutionBinding { schema_version:1, run_id:turn.into(), run_origin_kind:"ingress".into(),
        actor_principal:"bot".into(), decision_context:crate::turn_context::decision_context_for("bot", turn).unwrap(),
        conversation_audience:audience, task_id:None, task_revision:None, task_snapshot_hash:None,
        payload_hash:crate::approval::payload_hash(&payload), policy_revision:crate::approval::policy_revision(&f.home, "bot").unwrap(),
        cwd:None, environment_hash:version, file_hashes:Default::default(),
        expires_at:(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(), resume_handler:"live_only_v1".into(), resume_version:1 };
    let broker = ApprovalBroker::open(&f.home).unwrap();
    let principal = duduclaw_core::HumanPrincipal::user(&f.alice.id).unwrap().to_string();
    let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal),
        crate::approval::scope_turn("chat-member-session", turn,
            broker.request_bound(RequestKind::Approval, "bot", "以你的 Google 帳號傳送", payload, binding))).await.unwrap();
    (broker, id, grant)
}
fn ctx(f: &Fixture) -> duduclaw_auth::UserContext {
    let mut ctx = duduclaw_auth::UserContext::admin_fallback();
    ctx.user_id = f.alice.id.clone();
    ctx.role = duduclaw_auth::UserRole::Employee;
    ctx
}

#[tokio::test]
async fn chat_send_incremental_consent_refuses_changed_sub_missing_scope_and_switch() {
    let f = Fixture::new(); f.connect().await;
    assert!(consent(&f, "chat-send-alice").await.is_err());
    crate::google_workspace::set_chat_send_enabled(&f.home, true).unwrap();
    for code in ["chat-send-bob", "alice"] {
        assert!(consent(&f, code).await.is_err());
        let record = crate::person_credentials::get(&f.home, &f.alice.id).unwrap().unwrap();
        assert_eq!(record.metadata.google_sub, "sub-alice");
        assert!(!scopes::permitted_for(TOOL, &record.metadata));
    }
    consent(&f, "chat-send-alice").await.unwrap();
    let mut metadata = crate::person_credentials::get(&f.home, &f.alice.id).unwrap().unwrap().metadata;
    assert!(scopes::permitted_for(TOOL, &metadata));
    metadata.local_purposes.retain(|p| p != "chat_send");
    assert!(!scopes::permitted_for(TOOL, &metadata));
    metadata.local_purposes.push("chat_send".into()); metadata.granted_scopes.clear();
    assert!(!scopes::permitted_for(TOOL, &metadata));
}

#[tokio::test]
async fn chat_send_refuses_other_member_system_shared_and_bot_fallback() {
    let f = fixture().await;
    let other = f.db.create_user("other@c.test", "Other", "fixture-password", duduclaw_auth::UserRole::Admin).unwrap();
    for who in [RequesterIdentity::DashboardUser { user_id:other.id }, RequesterIdentity::Unknown,
        RequesterIdentity::System { kind:"cron".into(), on_behalf_of:Some(f.alice.id.clone()) }] {
        assert!(Snapshot::capture(&f.home, "bot", &who).is_err());
    }
    assert!(Snapshot::capture(&f.home, "bot", &who(&f)).is_ok());
    std::fs::write(f.home.join("agents/bot/agent.toml"), "[google]\nidentity='home'\n").unwrap();
    assert!(Snapshot::capture(&f.home, "bot", &who(&f)).is_err());
}

#[tokio::test]
async fn chat_send_member_confirmation_rejects_admin_allow_and_replay() {
    let f = fixture().await;
    let employee = f.home.join("agents/bot/agent.toml");
    let mut config = std::fs::read_to_string(&employee).unwrap();
    config.push_str("\n[capabilities]\naction_rules=[{effect='send',verdict='allow'}]\n");
    std::fs::write(&employee, config).unwrap();
    assert_eq!(duduclaw_core::agent_toml::load_action_rules(&f.home.join("agents/bot"))
        .resolve(TOOL, duduclaw_core::tool_effect::ToolEffect::Send), Some(duduclaw_core::ActionVerdict::Allow));
    let _turn = register(&f, "chat-confirm").await;
    let (broker, id, grant) = card(&f, "chat-confirm").await;
    assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    let admin = f.db.create_user("admin@c.test", "Admin", "fixture-password", duduclaw_auth::UserRole::Admin).unwrap();
    let mut admin_ctx = duduclaw_auth::UserContext::admin_fallback(); admin_ctx.user_id = admin.id;
    // Creating another account may advance authority; take a new snapshot.
    let (broker, id, grant) = card(&f, "chat-confirm").await;
    assert!(broker.decide_bound_dashboard(&id, &admin_ctx, true).await.is_err());
    assert!(broker.decide(&id, true, "admin").await.is_err());
    broker.decide_bound_dashboard(&id, &ctx(&f), true).await.unwrap();
    let rec = broker.get(&id).await.unwrap().unwrap();
    for (field, value) in [("text", "edited"), ("space", "spaces/B"), ("thread", "spaces/A/threads/Other")] {
        let mut payload = rec.payload.clone(); payload["arguments"][field] = json!(value);
        assert!(verify_and_consume(&broker, &id, &ActionGrant::build("bot", TOOL, &payload)).await.is_err());
    }
    verify_and_consume(&broker, &id, &grant).await.unwrap();
    assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    let (_, next, next_grant) = card(&f, "chat-confirm").await;
    assert_ne!(next, id);
    assert!(verify_and_consume(&broker, &next, &next_grant).await.is_err());
}

#[tokio::test]
async fn chat_send_revocation_and_policy_changes_invalidate_approved_card() {
    let f = fixture().await;
    let _turn = register(&f, "chat-revoke").await;
    let (broker, id, grant) = card(&f, "chat-revoke").await;
    broker.decide_bound_dashboard(&id, &ctx(&f), true).await.unwrap();
    f.db.set_google_consent(&f.alice.id, "bot", false, &f.alice.id).unwrap();
    assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id).unwrap();
    let (broker, id, grant) = card(&f, "chat-revoke").await;
    broker.decide_bound_dashboard(&id, &ctx(&f), true).await.unwrap();
    crate::google_workspace::set_chat_send_enabled(&f.home, false).unwrap();
    assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
}

#[tokio::test]
async fn chat_send_full_text_is_bound_and_foreign_account_edits_invalidate() {
    let f = fixture().await;
    let _turn = register(&f, "chat-card").await;
    let (broker, id, _) = card(&f, "chat-card").await;
    let mut rec = broker.get(&id).await.unwrap().unwrap();
    assert!(chat_send::card_body(&rec).contains("完整的私人文字"));
    assert!(chat_send::eligible(&f.home, &rec));
    rec.payload["chat_snapshot"]["account"] = json!("other@c.test");
    assert!(!chat_send::eligible(&f.home, &rec));
}

#[tokio::test]
async fn chat_send_group_requires_member_page_and_expired_card_refuses() {
    let f = fixture().await;
    let _turn = register(&f, "chat-group").await;
    let (broker, id, grant) = card_with_audience(&f, "chat-group", ConversationAudience::MultiParty).await;
    let mut rec = broker.get(&id).await.unwrap().unwrap();
    let context = rec.binding.as_ref().unwrap().decision_context.clone();
    assert!(broker.decide_bound(&id, &context, true).await.is_err());
    assert!(chat_send::may_decide(&f.home, &rec, &f.alice.id));
    rec.binding.as_mut().unwrap().expires_at = (chrono::Utc::now()-chrono::Duration::seconds(1)).to_rfc3339();
    assert!(!chat_send::may_decide(&f.home, &rec, &f.alice.id));
    broker.decide_bound_dashboard(&id, &ctx(&f), true).await.unwrap();
    verify_and_consume(&broker, &id, &grant).await.unwrap();
}

#[tokio::test]
async fn chat_send_revocation_at_transport_boundary_and_unknown_outcome_never_retry() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use crate::google_workspace::native_call::testing;
    for revoke in [true, false] {
        let f = fixture().await;
        let _turn = register(&f, if revoke { "chat-race" } else { "chat-unknown" }).await;
        let turn = if revoke { "chat-race" } else { "chat-unknown" };
        let hits = Arc::new(AtomicUsize::new(0));
        let observed = hits.clone();
        let router = axum::Router::new().route("/v1/spaces/A/messages", axum::routing::post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!({"provider_reply":"missing message resource"})) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let _transport = testing::install(&f.home, &base);
        if revoke {
            let db = f.db.clone(); let uid = f.alice.id.clone();
            // Before the transport takes its shared lease, race with revocation.
            testing::hook(&f.home, "before_builder", Box::new(move || {
                db.set_google_consent(&uid, "bot", false, &uid).unwrap();
            }));
        }
        let snapshot = Snapshot::capture(&f.home, "bot", &who(&f)).unwrap();
        let args = json!({"space":"spaces/A","text":"single operation"});
        let execution = chat_send::execute(&f.home, "bot", &args, who(&f), turn, snapshot, uuid::Uuid::new_v4().to_string());
        let confirmation = async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let broker = ApprovalBroker::open(&f.home).unwrap();
                loop {
                    if let Some(rec) = broker.list_by_kind("bound_action").await.unwrap().into_iter().find(|r| chat_send::is_member_card(r)) {
                        broker.decide_bound_dashboard(&rec.id, &ctx(&f), true).await.unwrap();
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.expect("member confirmation created");
        };
        let (result, ()) = tokio::join!(execution, confirmation);
        let error = result.unwrap_err();
        assert_eq!(hits.load(Ordering::SeqCst), if revoke { 0 } else { 1 });
        if !revoke { assert_eq!(error.code, "result_unconfirmed"); }
        server.abort();
    }
}
