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
        handler.set_cron_store(Arc::new(crate::cron_store::CronStore::open(home.path()).unwrap())).await;
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

impl Fixture {
    async fn run_after_reverify(&self, rec: &ApprovalRecord, change: impl std::future::Future<Output = ()>) -> Value {
        let ready = Arc::new(tokio::sync::Notify::new());
        let proceed = Arc::new(tokio::sync::Notify::new());
        let run = AFTER_REVERIFY.scope((ready.clone(), proceed.clone()), self.run(rec));
        let edit = async {
            ready.notified().await;
            change.await;
            proceed.notify_one();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(run, edit)
        }).await.expect("execution must reach and leave the re-verification barrier");
        result.unwrap()
    }

    fn install_skills(&self) {
        let dir = self.home.path().join("agents/worker/SKILLS");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["report", "summary"] {
            std::fs::write(dir.join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: Reports\n---\nSummarize reports.")).unwrap();
        }
    }
}

fn assert_stale_write_stopped(result: &Value, completed: usize, failed: usize) {
    assert_eq!(result["reason"], "stale", "{result}");
    assert_eq!(result["outcome"], if completed == 0 { "voided" } else { "partial" });
    assert_eq!(result["completed"], completed);
    assert_eq!(result["items"][failed]["result"], "failed");
    assert_eq!(result["items"][failed + 1]["result"], "not_executed");
    assert!(result["message"].as_str().unwrap().contains("設定在確認後被其他人改過"));
}

#[tokio::test]
async fn non_target_trigger_taken_before_approval_voids_without_writes() {
    for (name, value) in [("trigger", "@claimed"), ("display_name", "claimed")] {
        let f = Fixture::new().await;
        let before = f.profile("worker");
        let rec = f.card(vec![field("worker", name, json!(value)), schedule("never-run")]).await;
        assert!(rec.payload["target_hashes"]["triggers"].is_string());
        assert!(rec.payload["target_hashes"].get("agent:later").is_none());
        // Deliberately keep the authority epoch unchanged: only the namespace
        // snapshot can detect this non-target edit before approval.
        let mut other: toml::Table = f.profile("later").parse().unwrap();
        other.get_mut("agent").unwrap().as_table_mut().unwrap()
            .insert("trigger".into(), toml::Value::String("@CLAIMED".into()));
        let other = toml::to_string(&other).unwrap();
        std::fs::write(f.home.path().join("agents/later/agent.toml"), &other).unwrap();
        f.decide(&rec, true).await;
        let result = f.run(&rec).await.unwrap();
        assert_eq!(result["outcome"], "voided");
        assert_eq!(result["reason"], "stale");
        assert_eq!(f.profile("worker"), before);
        assert_eq!(f.profile("later"), other);
        assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
        assert!(f.events("agent_settings_changed").is_empty());
    }
}

#[tokio::test]
async fn missing_trigger_namespace_hash_fails_closed() {
    let f = Fixture::new().await;
    let mut prepared = f.prepared(vec![field("worker", "trigger", json!("@claimed"))]).await;
    prepared.target_hashes.remove("triggers");
    let rec = f.card_prepared(prepared).await;
    f.decide(&rec, true).await;
    let before = f.profile("worker");
    assert_eq!(f.run(&rec).await.unwrap()["reason"], "stale");
    assert_eq!(f.profile("worker"), before);
}

#[tokio::test]
async fn trigger_taken_after_reverify_is_refused_inside_agent_writer() {
    let f = Fixture::new().await;
    let rec = f.card(vec![field("worker", "trigger", json!("@claimed")), schedule("never-run")]).await;
    f.decide(&rec, true).await;
    let before = f.profile("worker");
    let result = f.run_after_reverify(&rec, async {
        let ctx = f.users.live_user_context(&f.turn.user_id).unwrap().unwrap();
        let frame = f.handler.handle_agents_update_as(
            json!({"agent_id":"later", "trigger":"@CLAIMED"}), Some(&ctx)).await;
        assert!(matches!(frame, WsFrame::Response {ok:true,..}), "{frame:?}");
    }).await;
    assert_stale_write_stopped(&result, 0, 0);
    assert_eq!(f.profile("worker"), before);
    assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn agent_text_cas_rejects_post_reverify_edit_and_stops_later_items() {
    for partial in [false, true] {
        let f = Fixture::new().await;
        let mut items = vec![];
        if partial { items.push(field("worker", "icon", json!("🦊"))); }
        items.push(field("later", "icon", json!("🐈")));
        items.push(schedule("never-run"));
        let rec = f.card(items).await;
        f.decide(&rec, true).await;
        let worker = f.profile("worker");
        // Even a comment-only edit must invalidate the approved raw text.
        let changed = format!("{}\n# concurrent edit after verification\n", f.profile("later"));
        let result = f.run_after_reverify(&rec, async {
            std::fs::write(f.home.path().join("agents/later/agent.toml"), &changed).unwrap();
        }).await;
        assert_stale_write_stopped(&result, usize::from(partial), usize::from(partial));
        assert_eq!(f.profile("later"), changed);
        if partial {
            let table: toml::Value = f.profile("worker").parse().unwrap();
            assert_eq!(table["agent"]["icon"].as_str(), Some("🦊"));
        } else { assert_eq!(f.profile("worker"), worker); }
        assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn schedule_cas_rejects_post_reverify_edit_for_every_existing_row_operation() {
    for op in ["update", "pause", "resume", "delete"] {
        let f = Fixture::new().await;
        let store = crate::cron_store::CronStore::open(f.home.path()).unwrap();
        let mut row = crate::cron_store::CronTaskRow::new("existing".into(), "Existing".into(),
            "worker".into(), "0 9 * * *".into(), "Summarize the report".into());
        row.enabled = op != "resume";
        store.insert(&row).await.unwrap();
        let mut edit = json!({"kind":"schedule", "op":op, "schedule_id":"existing"});
        if op == "update" {
            edit["task"] = json!("Summarize the weekly report");
            edit["timezone"] = json!("Asia/Taipei");
        }
        let rec = f.card(vec![edit, schedule("never-run")]).await;
        f.decide(&rec, true).await;
        let result = f.run_after_reverify(&rec, async {
            store.update_fields(&row.id, "Concurrent name", &row.agent_id, &row.cron, &row.task, row.enabled)
                .await.unwrap();
        }).await;
        assert_stale_write_stopped(&result, 0, 0);
        let current = store.get(&row.id).await.unwrap().unwrap();
        assert_eq!(current.name, "Concurrent name");
        assert_eq!(current.task, row.task);
        assert_eq!(current.enabled, row.enabled);
        assert_eq!(current.cron_timezone, row.cron_timezone);
        assert!(store.get_by_name("never-run").await.unwrap().is_none());
    }
}

#[tokio::test]
async fn schedule_cas_keeps_snapshot_check_through_transaction_and_commits_timezone_together() {
    let f = Fixture::new().await;
    let store = crate::cron_store::CronStore::open(f.home.path()).unwrap();
    let row = crate::cron_store::CronTaskRow::new("existing".into(), "Existing".into(),
        "worker".into(), "0 9 * * *".into(), "Summarize the report".into());
    store.insert(&row).await.unwrap();
    let row = store.get(&row.id).await.unwrap().unwrap();
    let expected = hash_schedule(&row).unwrap();
    let mut changed = row.clone();
    changed.task = "Summarize the weekly report".into();
    changed.cron_timezone = Some("Asia/Taipei".into());
    // This edit lands after the handler read its expected row.
    store.set_enabled(&row.id, false).await.unwrap();
    assert!(!store.edit_admin_ai(&row, Some(&changed), &expected).await.unwrap());
    let current = store.get(&row.id).await.unwrap().unwrap();
    assert!(!current.enabled);
    assert_eq!(current.task, row.task);
    assert_eq!(current.cron_timezone, row.cron_timezone);

    let rec = f.card(vec![json!({"kind":"schedule", "op":"update", "schedule_id":"existing",
        "task":"Summarize the weekly report", "timezone":"Asia/Taipei"})]).await;
    f.decide(&rec, true).await;
    assert_eq!(f.run(&rec).await.unwrap()["outcome"], "complete");
    let current = store.get(&row.id).await.unwrap().unwrap();
    assert_eq!(current.task, changed.task);
    assert_eq!(current.cron_timezone, changed.cron_timezone);
    assert!(!current.enabled);
}

#[tokio::test]
async fn skill_cas_rejects_post_reverify_edit_without_overwriting_switches() {
    let f = Fixture::new().await;
    f.install_skills();
    let rec = f.card(vec![json!({"kind":"skill", "agent_id":"worker", "skill":"report", "enabled":false}),
        schedule("never-run")]).await;
    f.decide(&rec, true).await;
    let result = f.run_after_reverify(&rec, async {
        duduclaw_agent::skill_state::set_disabled(f.home.path(), "worker", &["summary".into()], true).unwrap();
    }).await;
    assert_stale_write_stopped(&result, 0, 0);
    let state = duduclaw_agent::skill_state::read_disabled(f.home.path(), "worker");
    assert_eq!(state.names(), vec!["summary"]);
    assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn consecutive_skill_edits_use_only_their_own_committed_hash() {
    let f = Fixture::new().await;
    f.install_skills();
    duduclaw_agent::skill_state::set_disabled(f.home.path(), "worker", &["summary".into()], true).unwrap();
    let rec = f.card(vec![json!({"kind":"skill", "agent_id":"worker", "skill":"report", "enabled":false}),
        json!({"kind":"skill", "agent_id":"worker", "skill":"summary", "enabled":true})]).await;
    f.decide(&rec, true).await;
    let result = f.run(&rec).await.unwrap();
    assert_eq!(result["outcome"], "complete", "{result}");
    assert_eq!(result["completed"], 2);
    assert_eq!(duduclaw_agent::skill_state::read_disabled(f.home.path(), "worker").names(), vec!["report"]);
}

#[tokio::test]
async fn effective_resolved_text_cas_refuses_post_reverify_drift() {
    let f = Fixture::new().await;
    f.bind_identity_test_preset("worker").await;
    let before = f.profile("worker");
    let effective = route::effective_text(&f.home.path().join("agents/worker")).unwrap();
    let resolved = duduclaw_core::preset::agent_resolved_path(f.home.path(), "worker");
    let rec = f.card(vec![field("worker", "icon", json!("🦊")), schedule("never-run")]).await;
    f.decide(&rec, true).await;
    let changed = format!("{effective}\n# concurrent resolved artifact edit\n");
    let result = f.run_after_reverify(&rec, async {
        std::fs::write(&resolved, &changed).unwrap();
    }).await;
    assert_stale_write_stopped(&result, 0, 0);
    assert_eq!(f.profile("worker"), before);
    assert_eq!(std::fs::read_to_string(resolved).unwrap(), changed);
    assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
}

impl Fixture {
    async fn bind_identity_test_preset(&self, id: &str) {
        let dir = self.home.path().join("agents").join(id);
        // The raw and effective text hashes must be distinct even when their
        // parsed identity is the same; materialization drops this comment.
        std::fs::write(dir.join("agent.toml"), format!("{}\n# raw-only comment\n", self.profile(id))).unwrap();
        let preset_dir = self.home.path().join("presets/identity-test");
        std::fs::create_dir_all(&preset_dir).unwrap();
        std::fs::write(preset_dir.join("preset.toml"),
            "[preset]\nversion='1.0.0'\n[agent]\ndisplay_name='Ignored'\ntrigger='@ignored'\nstatus='terminated'\n").unwrap();
        duduclaw_core::preset::bind(self.home.path(), id, &dir, "identity-test", "tester", "regression").unwrap();
        let mut registry = duduclaw_agent::registry::AgentRegistry::new(self.home.path().join("agents"));
        registry.scan().await.unwrap();
        assert!(registry.get(id).is_some());
        let effective = route::effective_text(&dir).unwrap();
        let table: toml::Value = effective.parse().unwrap();
        let raw: toml::Value = self.profile(id).parse().unwrap();
        assert_eq!(table["agent"], raw["agent"], "presets cannot override identity");
        assert_ne!(hash_text(&effective), hash_text(&self.profile(id)));
    }
}

#[tokio::test]
async fn preset_non_target_raw_trigger_changes_namespace_and_refuses_writes() {
    for after_reverify in [false, true] {
        for (name, value) in [("trigger", "@claimed"), ("display_name", "claimed")] {
            let f = Fixture::new().await;
            f.bind_identity_test_preset("later").await;
            let before = f.profile("worker");
            let rec = f.card(vec![field("worker", name, json!(value)), schedule("never-run")]).await;
            assert!(rec.payload["target_hashes"].get("agent:later").is_none());
            let other_dir = f.home.path().join("agents/later");
            let effective = route::effective_text(&other_dir).unwrap();
            let mut raw: toml::Table = f.profile("later").parse().unwrap();
            raw.get_mut("agent").unwrap().as_table_mut().unwrap()
                .insert("trigger".into(), toml::Value::String("@CLAIMED".into()));
            let raw = toml::to_string(&raw).unwrap();
            let change = async {
                std::fs::write(other_dir.join("agent.toml"), &raw).unwrap();
                assert_eq!(route::effective_text(&other_dir).unwrap(), effective);
                assert_ne!(rec.payload["target_hashes"]["triggers"],
                    json!(hash_triggers(&trigger_namespace(f.home.path()).unwrap())));
            };
            let result = if after_reverify {
                f.decide(&rec, true).await;
                let result = f.run_after_reverify(&rec, change).await;
                assert_eq!(result["completed"], 0);
                let items = result["items"].as_array().unwrap();
                let failed = items.iter().find(|item| item["target"] == "worker"
                    && item["field"] == name).unwrap();
                assert_eq!(failed["result"], "failed");
                for item in items {
                    // Derived trigger fields fail with the same atomic write.
                    assert_eq!(item["result"], if item["target"] == "worker" && item.get("field").is_some() {
                        "failed"
                    } else { "not_executed" });
                }
                let schedule = items.iter().find(|item| item["op"] == "create").unwrap();
                assert_eq!(schedule["result"], "not_executed");
                assert!(result["message"].as_str().unwrap().contains("設定在確認後被其他人改過"));
                result
            } else {
                change.await;
                f.decide(&rec, true).await;
                f.run(&rec).await.unwrap()
            };
            assert_eq!(result["outcome"], "voided");
            assert_eq!(result["reason"], "stale");
            assert_eq!(f.profile("worker"), before);
            assert_eq!(f.profile("later"), raw);
            assert_eq!(route::effective_text(&other_dir).unwrap(), effective);
            assert!(f.events("agent_settings_changed").is_empty());
            assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
        }
    }
}

#[test]
fn raw_replace_child() {
    let Some(source) = std::env::var_os("DUDUCLAW_ADMIN_AI_RAW_REPLACE_SOURCE") else { return; };
    let target = std::env::var_os("DUDUCLAW_ADMIN_AI_RAW_REPLACE_TARGET").unwrap();
    std::fs::rename(source, target).unwrap();
}

#[tokio::test]
async fn preset_raw_cas_refuses_cross_process_replace_with_stale_resolved_text() {
    for after_reverify in [false, true] {
        let f = Fixture::new().await;
        f.bind_identity_test_preset("worker").await;
        let rec = f.card(vec![field("worker", "icon", json!("🦊")), schedule("never-run")]).await;
        let dir = f.home.path().join("agents/worker");
        let effective = route::effective_text(&dir).unwrap();
        let before = f.profile("worker");
        assert_eq!(rec.payload["target_hashes"]["agent:worker"], hash_text(&effective));
        assert_eq!(rec.payload["target_hashes"]["agent_raw:worker"], hash_text(&before));
        let changed = format!("{before}\n# cross-process raw replacement\n");
        let change = async {
            // A separate process atomically replaces the raw commit base,
            // without a registry rescan or an authority epoch change.
            let source = dir.join("replacement.toml");
            std::fs::write(&source, &changed).unwrap();
            let test = format!("{}::raw_replace_child", module_path!().split_once("::").unwrap().1);
            let output = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test, "--nocapture"])
                .env("DUDUCLAW_ADMIN_AI_RAW_REPLACE_SOURCE", &source)
                .env("DUDUCLAW_ADMIN_AI_RAW_REPLACE_TARGET", dir.join("agent.toml"))
                .output().await.unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            assert_eq!(f.profile("worker"), changed);
            assert_eq!(route::effective_text(&dir).unwrap(), effective);
        };
        let result = if after_reverify {
            f.decide(&rec, true).await;
            let result = f.run_after_reverify(&rec, change).await;
            assert_stale_write_stopped(&result, 0, 0);
            result
        } else {
            change.await;
            f.decide(&rec, true).await;
            f.run(&rec).await.unwrap()
        };
        assert_eq!(result["outcome"], "voided");
        assert_eq!(result["reason"], "stale");
        assert_eq!(f.profile("worker"), changed);
        assert_eq!(route::effective_text(&dir).unwrap(), effective);
        assert!(f.events("agent_settings_changed").is_empty());
        assert!(crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn missing_raw_agent_hash_fails_closed() {
    let f = Fixture::new().await;
    let mut prepared = f.prepared(vec![field("worker", "icon", json!("🦊"))]).await;
    prepared.target_hashes.remove("agent_raw:worker");
    let rec = f.card_prepared(prepared).await;
    f.decide(&rec, true).await;
    let before = f.profile("worker");
    assert_eq!(f.run(&rec).await.unwrap()["reason"], "stale");
    assert_eq!(f.profile("worker"), before);
}

#[tokio::test]
async fn preset_agent_write_accepts_distinct_matching_raw_and_effective_hashes() {
    let f = Fixture::new().await;
    f.bind_identity_test_preset("worker").await;
    let rec = f.card(vec![field("worker", "icon", json!("🦊"))]).await;
    assert_ne!(rec.payload["target_hashes"]["agent:worker"], rec.payload["target_hashes"]["agent_raw:worker"]);
    f.decide(&rec, true).await;
    assert_eq!(f.run(&rec).await.unwrap()["outcome"], "complete");
    let table: toml::Value = f.profile("worker").parse().unwrap();
    assert_eq!(table["agent"]["icon"].as_str(), Some("🦊"));
}

#[tokio::test]
async fn execution_results_persist_once_with_sanitized_projection() {
    for outcome in ["complete", "partial", "voided"] {
        let f = Fixture::new().await;
        let items = if outcome == "partial" {
            vec![schedule("duplicate"), schedule("duplicate"), schedule("never-run")]
        } else { vec![field("worker", "icon", json!("🦊"))] };
        let rec = f.card(items).await;
        f.decide(&rec, true).await;
        if outcome == "voided" {
            std::fs::write(f.home.path().join("agents/worker/agent.toml"),
                format!("{}\n# changed\n", f.profile("worker"))).unwrap();
        }
        let result = f.run(&rec).await.unwrap();
        assert_eq!(result["outcome"], outcome);
        let stored = f.broker.get(&rec.id).await.unwrap().unwrap().execution_result.unwrap();
        assert_eq!(stored["outcome"], outcome);
        assert_eq!(stored["completed"], result["completed"]);
        assert_eq!(stored["not_executed"], result["not_executed"]);
        let mut keys: Vec<_> = stored.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["completed", "items", "message", "not_executed", "outcome", "reason"]);
        for item in stored["items"].as_array().unwrap() {
            let mut keys: Vec<_> = item.as_object().unwrap().keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["error", "result", "seq"]);
        }
        assert!(!stored.to_string().contains(&f.turn.user_id));
        assert!(!stored.to_string().contains("Summarize the daily report"));
        let ctx = f.users.live_user_context(&f.turn.user_id).unwrap().unwrap();
        let frame = f.handler.handle_approvals_get(json!({"id":rec.id.as_str()}), &ctx).await;
        let WsFrame::Response { ok:true, payload:Some(view), .. } = frame else { panic!("{frame:?}"); };
        assert_eq!(view["count"], 1);
        assert_eq!(view["approvals"][0]["execution_result"], stored);
        assert_eq!(view["approvals"][0]["decided_at"],
            json!(f.broker.get(&rec.id).await.unwrap().unwrap().decided_at));
        assert!(!view["approvals"][0]["decided_at"].is_null());

        assert!(f.run(&rec).await.is_none());
        assert!(!f.broker.set_execution_result_once(&rec.id, &json!({"outcome":"voided"})).await.unwrap());
        assert_eq!(f.broker.get(&rec.id).await.unwrap().unwrap().execution_result, Some(stored));
    }
}

#[tokio::test]
async fn result_store_failure_does_not_change_outcome_or_reexecute() {
    let f = Fixture::new().await;
    let rec = f.card(vec![schedule("once-only")]).await;
    f.decide(&rec, true).await;
    rusqlite::Connection::open(f.home.path().join("approvals.db")).unwrap().execute_batch(
        "CREATE TRIGGER refuse_execution_result BEFORE UPDATE OF execution_result ON approvals
         BEGIN SELECT RAISE(FAIL, 'test result store unavailable'); END;").unwrap();
    let result = f.run(&rec).await.unwrap();
    assert_eq!(result["outcome"], "complete");
    assert_eq!(result["completed"], 1);
    assert!(f.broker.get(&rec.id).await.unwrap().unwrap().execution_result.is_none());
    assert!(f.run(&rec).await.is_none());
    let schedules = crate::cron_store::CronStore::open(f.home.path()).unwrap().list_all().await.unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(f.events("admin_ai_settings_changed").len(), 1);
}
