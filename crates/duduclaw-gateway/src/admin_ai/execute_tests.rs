//! Execution-layer regressions: real broker, isolated files, real handlers.
use super::*;
use crate::approval::{ConversationAudience, DecisionContext};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_core::admin_ai_settings::Proposal;

struct Fixture {
    home: tempfile::TempDir,
    handler: MethodHandler,
    broker: ApprovalBroker,
    users: UserDb,
    turn: AdminTurn,
}
impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        std::fs::write(home.path().join("config.toml"), "[admin_ai]\nenabled=true\n").unwrap();
        duduclaw_core::authority_coordinator::AuthorityCoordinator::new(home.path()).initialize().unwrap();
        let users = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = users.create_user("executor@example.test", "管理員", "fixture-password", UserRole::Admin).unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        for name in ["avatar", "worker", "later"] {
            let frame = handler.handle_agents_create_as(json!({"name":name,
                "model_preferred":"claude-sonnet-4-6", "runtime":{"provider":"claude"}}),
                Some(&UserContext::admin_fallback())).await;
            assert!(matches!(frame, WsFrame::Response {ok:true,..}), "{frame:?}");
        }
        users.bind_agent(&user.id, "avatar", AccessLevel::Operator).unwrap();
        users.set_avatar_as(&user.id, &user.id, Some("avatar")).unwrap();
        let turn = AdminTurn { user_id:user.id.clone(), agent:"avatar".into(),
            session_id:format!("webchat:{}", uuid::Uuid::new_v4()), turn_id:uuid::Uuid::new_v4().to_string(),
            channel:"webchat".into(), audience:ConversationAudience::Private,
            decision_context:Some(DecisionContext { channel:"webchat".into(), account_id:"webchat".into(),
                conversation_id:"execution-fixture".into(), principal_id:user.id }) };
        let broker = ApprovalBroker::open(home.path()).unwrap();
        Self { home, handler, broker, users, turn }
    }
    async fn prepared(&self, items: Vec<Value>) -> super::super::proposal::Prepared {
        let proposal: Proposal = serde_json::from_value(json!({"items":items,"reason":"測試設定變更"})).unwrap();
        super::super::proposal::prepare(self.home.path(), &self.turn, proposal).await.unwrap()
    }
    async fn card(&self, items: Vec<Value>) -> ApprovalRecord {
        self.card_prepared(self.prepared(items).await).await
    }
    async fn card_prepared(&self, prepared: super::super::proposal::Prepared) -> ApprovalRecord {
        super::super::card::create(self.home.path(), &self.turn, prepared, None).await.unwrap();
        self.broker.list_by_kind(ACTION_KIND).await.unwrap().into_iter()
            .find(|r| r.status == ApprovalStatus::Pending).unwrap()
    }
    async fn decide(&self, rec: &ApprovalRecord, approve: bool) {
        let ctx = self.users.live_user_context(&self.turn.user_id).unwrap().unwrap();
        crate::approval::EXPECTED_DECISION_DIGEST.scope(Some((rec.id.to_string(),
            crate::approval::decision_digest_at(self.home.path(), rec))),
            self.broker.decide_bound_dashboard(&rec.id, &ctx, approve)).await.unwrap();
    }
    async fn run(&self, rec: &ApprovalRecord) -> Option<Value> {
        execute_once(self.home.path(), &self.handler, &self.broker, rec).await
    }
    fn profile(&self, id: &str) -> String {
        std::fs::read_to_string(self.home.path().join("agents").join(id).join("agent.toml")).unwrap()
    }
    fn events(&self, kind: &str) -> Vec<Value> {
        std::fs::read_to_string(self.home.path().join("security_audit.jsonl")).unwrap_or_default().lines()
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .filter(|v| v["event_type"] == kind).collect()
    }
}
fn field(agent: &str, name: &str, value: Value) -> Value {
    json!({"kind":"agent_field","agent_id":agent,"field":name,"value":value})
}
fn schedule(name: &str) -> Value {
    json!({"kind":"schedule","op":"create","agent_id":"worker","name":name,
        "cron":"0 9 * * *","timezone":"Asia/Taipei","task":"Summarize the daily report"})
}

#[tokio::test]
async fn stale_target_voids_whole_batch() {
    let f = Fixture::new().await;
    let rec = f.card(vec![field("worker", "icon", json!("🦊")), field("later", "icon", json!("🐈"))]).await;
    f.decide(&rec, true).await;
    let path = f.home.path().join("agents/worker/agent.toml");
    let changed = format!("{}\n# concurrent edit\n", f.profile("worker"));
    std::fs::write(&path, &changed).unwrap();
    let later = f.profile("later");
    let result = f.run(&rec).await.unwrap();
    assert_eq!(result["outcome"], "voided");
    assert_eq!(result["reason"], "stale");
    assert_eq!(f.profile("worker"), changed);
    assert_eq!(f.profile("later"), later);
    assert_eq!(f.events("admin_ai_settings_voided").len(), 1);
    assert!(f.events("admin_ai_settings_changed").is_empty());
}

#[tokio::test]
async fn demoted_admin_voids() {
    let f = Fixture::new().await;
    let rec = f.card(vec![field("worker", "icon", json!("🦊"))]).await;
    f.decide(&rec, true).await;
    let before = f.profile("worker");
    rusqlite::Connection::open(f.home.path().join("users.db")).unwrap().execute(
        "UPDATE users SET role='employee' WHERE id=?1", [&f.turn.user_id]).unwrap();
    assert_eq!(f.run(&rec).await.unwrap()["reason"], "admin_changed");
    assert_eq!(f.profile("worker"), before);
    assert!(f.run(&rec).await.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn execution_refuses_extra_changed_paths() {
    let f = Fixture::new().await;
    let mut prepared = f.prepared(vec![field("worker", "icon", json!("🦊")),
        field("later", "icon", json!("🐈"))]).await;
    // Deliberately bypass the proposal's model check to exercise the final
    // writer diff gate through a correctly bound and consumed execution grant.
    prepared.items[0]["item"] = field("worker", "model", json!("gpt-5.4"));
    prepared.items[0]["before"] = json!("claude-sonnet-4-6");
    prepared.items[0]["after"] = json!("gpt-5.4");
    let rec = f.card_prepared(prepared).await;
    f.decide(&rec, true).await;
    let before = f.profile("worker");
    let later = f.profile("later");
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let result = f.run(&rec).await.unwrap();
    assert_eq!(result["outcome"], "partial");
    assert_eq!(result["items"][0]["result"], "failed");
    assert_eq!(result["items"][1]["result"], "not_executed");
    assert!(result["items"][0]["error"].as_str().unwrap().contains("未核准"));
    assert_eq!(f.profile("worker"), before);
    assert_eq!(f.profile("later"), later);
}

#[tokio::test]
async fn grant_consumed_once() {
    let f = Fixture::new().await;
    let rec = f.card(vec![schedule("once")]).await;
    f.decide(&rec, true).await;
    let (first, second) = tokio::join!(f.run(&rec), f.run(&rec));
    assert_eq!(usize::from(first.is_some()) + usize::from(second.is_some()), 1);
    assert_eq!(first.or(second).unwrap()["outcome"], "complete");
    assert!(f.run(&rec).await.is_none());
    assert_eq!(f.events("admin_ai_settings_changed").len(), 1);
    assert_eq!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().len(), 1);
}

#[tokio::test]
async fn partial_failure_stops_later_items_and_reports_counts() {
    let f = Fixture::new().await;
    // Both creates are valid proposals; the second collides at execution.
    let rec = f.card(vec![schedule("duplicate"), schedule("duplicate"), schedule("never-run")]).await;
    f.decide(&rec, true).await;
    let result = f.run(&rec).await.unwrap();
    assert_eq!(result["outcome"], "partial");
    assert_eq!(result["completed"], 1);
    assert_eq!(result["not_executed"], 1);
    assert_eq!(result["items"][0]["result"], "complete");
    assert_eq!(result["items"][1]["result"], "failed");
    assert_eq!(result["items"][2]["result"], "not_executed");
    assert!(result["message"].as_str().unwrap().contains("已完成 1 項；第 2 項失敗"));
    let store = crate::cron_store::CronStore::open(f.home.path()).unwrap();
    assert!(store.get_by_name("duplicate").await.unwrap().is_some());
    assert!(store.get_by_name("never-run").await.unwrap().is_none());
}

#[tokio::test]
async fn schedule_creation_records_admin_as_creator() {
    let f = Fixture::new().await;
    let rec = f.card(vec![schedule("owned")]).await;
    f.decide(&rec, true).await;
    assert_eq!(f.run(&rec).await.unwrap()["outcome"], "complete");
    let row = crate::cron_store::CronStore::open(f.home.path()).unwrap().get_by_name("owned").await.unwrap().unwrap();
    assert_eq!(row.created_by_user_id.as_deref(), Some(f.turn.user_id.as_str()));
    assert_eq!(row.on_behalf_of, Some(format!("user:{}", f.turn.user_id)));
    assert_eq!(row.cron_timezone.as_deref(), Some("Asia/Taipei"));
}

#[tokio::test]
async fn audit_uses_gateway_turn() {
    // A child process supplies hostile environment values without mutating
    // the shared environment of concurrently running Rust tests.
    const CHILD: &str = "DUDUCLAW_ADMIN_AI_AUDIT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let test = format!("{}::audit_uses_gateway_turn", module_path!().split_once("::").unwrap().1);
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env(CHILD, "1")
            .env(duduclaw_core::ENV_TRUST_SESSION_ID, "untrusted-env-session")
            .env(duduclaw_core::ENV_TRUST_TURN_ID, "00000000-0000-4000-8000-000000000001")
            .output().await.unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let f = Fixture::new().await;
    let rec = f.card(vec![schedule("audited")]).await;
    f.decide(&rec, true).await;
    // Ambient runtime correlation is deliberately different from the saved
    // proposal. The executor also has no environment-variable fallback.
    let result = duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("untrusted-session".into()),
        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("untrusted-turn".into()), f.run(&rec))).await.unwrap();
    assert_eq!(result["session_id"], f.turn.session_id);
    assert_eq!(result["turn_id"], f.turn.turn_id);
    let rows = f.events("admin_ai_settings_changed");
    let details = &rows[0]["details"];
    assert_eq!(details["session_id"], rec.payload["session_id"]);
    assert_eq!(details["turn_id"], rec.binding.as_ref().unwrap().run_id);
    assert_eq!(details["decided_via"], "dashboard");
    assert_ne!(details["session_id"], std::env::var(duduclaw_core::ENV_TRUST_SESSION_ID).unwrap());
    assert_ne!(details["turn_id"], std::env::var(duduclaw_core::ENV_TRUST_TURN_ID).unwrap());
    assert_eq!(details["items"][0]["after"]["task_sha256"], hash_text("Summarize the daily report"));
    assert!(!details.to_string().contains("Summarize the daily report"));
}

#[tokio::test]
async fn expired_card_voids_without_writing() {
    let f = Fixture::new().await;
    let rec = f.card(vec![field("worker", "icon", json!("🦊"))]).await;
    let before = f.profile("worker");
    rusqlite::Connection::open(f.home.path().join("approvals.db")).unwrap().execute(
        "UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1", [rec.id.as_str()]).unwrap();
    let result = wait(f.home.path(), &f.handler, &rec).await.unwrap();
    assert_eq!(result["reason"], "expired");
    assert_eq!(f.profile("worker"), before);
    assert!(f.events("admin_ai_settings_changed").is_empty());
}

#[tokio::test]
async fn restart_invalidates_pending() {
    for approved in [false, true] {
        let f = Fixture::new().await;
        let rec = f.card(vec![field("worker", "icon", json!("🦊"))]).await;
        if approved { f.decide(&rec, true).await; }
        let before = f.profile("worker");
        f.broker.invalidate_live_on_restart().await.unwrap();
        assert!(f.run(&rec).await.is_none());
        assert_eq!(wait(f.home.path(), &f.handler, &rec).await.unwrap()["reason"], "restart");
        assert_eq!(f.profile("worker"), before);
    }
}

#[tokio::test]
async fn denied_and_cancelled_cards_do_not_execute() {
    for cancelled in [false, true] {
        let f = Fixture::new().await;
        let rec = f.card(vec![field("worker", "icon", json!("🦊"))]).await;
        if cancelled { f.broker.invalidate_request(&rec.id, "turn_cancelled").await.unwrap(); }
        else { f.decide(&rec, false).await; }
        let before = f.profile("worker");
        assert_eq!(wait(f.home.path(), &f.handler, &rec).await.unwrap()["reason"], "denied");
        assert_eq!(f.profile("worker"), before);
    }
}

#[test]
fn card_order_matches_grouped_execution_and_sanitizes_audit() {
    let mut items = vec![json!({"target":"worker","item":field("worker","icon",json!("x"))}),
        json!({"target":"worker","item":schedule("daily")}),
        json!({"target":"later","item":field("later","icon",json!("y"))}),
        json!({"target":"worker","item":field("worker","trigger",json!("@worker"))})];
    order_items(&mut items);
    assert_eq!(items.iter().map(|v| v["target"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["worker", "worker", "later", "worker"]);
    assert_eq!(items[3]["seq"], 4);
    let value = audit_value(&json!({"text":format!("{}\u{202e}", "中".repeat(250)), "task":"secret prompt"}));
    assert_eq!(value["text"].as_str().unwrap().chars().count(), 200);
    assert!(value.get("task").is_none());
    assert_eq!(value["task_sha256"], hash_text("secret prompt"));
}

#[tokio::test]
async fn disabled_feature_voids_after_approval() {
    let f = Fixture::new().await;
    let rec = f.card(vec![schedule("disabled")]).await;
    f.decide(&rec, true).await;
    std::fs::write(f.home.path().join("config.toml"), "[admin_ai]\nenabled=false\n").unwrap();
    assert_eq!(f.run(&rec).await.unwrap()["reason"], "disabled");
    assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn grouped_rename_keeps_explicit_trigger_override() {
    let f = Fixture::new().await;
    let table: toml::Value = f.profile("worker").parse().unwrap();
    let trigger = table["agent"]["trigger"].as_str().unwrap();
    let rec = f.card(vec![field("worker", "display_name", json!("新名字")),
        field("worker", "trigger", json!(trigger)), field("worker", "icon", json!("🦊"))]).await;
    f.decide(&rec, true).await;
    let result = f.run(&rec).await.unwrap();
    assert_eq!(result["outcome"], "complete");
    assert_eq!(result["completed"], 2); // The explicit unchanged trigger is an override, not another item.
    let table: toml::Value = f.profile("worker").parse().unwrap();
    assert_eq!(table["agent"]["trigger"].as_str(), Some(trigger));
    assert_eq!(table["agent"]["display_name"].as_str(), Some("新名字"));
    let events = f.events("agent_settings_changed");
    assert_eq!(events.iter().filter(|v| v["details"]["approval_id"] == rec.id.as_str()).count(), 1);
}
