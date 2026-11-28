//! W8: one entry table crossed with every effective configuration state.
//!
//! Background rows exercise the shared runner with the producers' RequestType;
//! goal tick additionally reconciles its actual refusal through a failed queue row.
//! MCP tasks_create/tasks_update
//! live in the CLI crate and retain their own tests in mcp/tasks.rs.
//!
//! Contract exceptions deliberately not disguised as shared-code rows:
//! A2A SendMessage exposes only EMPLOYEE_CONFIGURATION_REFUSED, not the gate's
//! reason code (covered separately below). /takeover is not a ChatCommand and
//! cannot pass through handle_gated_command; exercise its real handler instead.
use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    owned_channel, assert_refused_before_any_side_effect, assert_no_saved_attachments,
};
use crate::goal_create_core::{self, GoalCreateError, GoalCreateRequest};
use crate::task_store::{TaskRow, TaskStore};
use crate::webchat::entry_gate_all_helpers as web;
use duduclaw_auth::{JwtConfig, UserContext, UserDb, UserRole};
use duduclaw_core::types::RuntimeType;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Config { Mismatch, Malformed, UnknownEngine, MissingModel, Healthy }
const CONFIGS: &[Config] = &[
    Config::Mismatch, Config::Malformed, Config::UnknownEngine, Config::MissingModel, Config::Healthy,
];
impl Config {
    fn apply(self, path: &std::path::Path) {
        let original = std::fs::read_to_string(path).unwrap();
        let text = if self == Self::Malformed { "[model".into() } else {
            let mut document: toml::Value = original.parse().unwrap();
            match self {
                Self::Mismatch => document["runtime"]["provider"] = toml::Value::String("grok".into()),
                Self::UnknownEngine => document["runtime"]["provider"] = toml::Value::String("typo".into()),
                Self::MissingModel => {
                    document["runtime"]["provider"] = toml::Value::String("openai_compat".into());
                    document["model"].as_table_mut().unwrap().remove("preferred");
                }
                Self::Healthy => {}
                Self::Malformed => unreachable!(),
            }
            toml::to_string(&document).unwrap()
        };
        std::fs::write(path, text).unwrap();
    }
}

#[derive(Clone, Copy, Debug)]
struct Case { config: Config, session: &'static str }
impl Case {
    fn healthy(self) -> bool { self.config == Config::Healthy }
    fn public(self) -> bool { self.session != "telegram:42" }
}

struct Fixture {
    _home: tempfile::TempDir,
    ctx: Arc<ReplyContext>,
    _runtime: crate::runtime_dispatch::testing::RegistryGuard,
    calls: Arc<AtomicUsize>,
    store: Arc<TaskStore>,
    handler: crate::handlers::MethodHandler,
    chat: Arc<crate::webchat::WebChatState>,
    admin: UserContext,
    token: String,
    expected_code: Option<&'static str>,
}
impl Fixture {
    async fn new(config: Config) -> Self {
        let (home, ctx, runtime, calls) = owned_channel(RuntimeType::Codex).await;
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
        let skills = home.path().join("agents/worker/SKILLS");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("zz-entry-report.md"),
            "---\nname: zz-entry-report\ndescription: Prepare a report\n---\nPrepare the requested report.\n").unwrap();
        // All registries are populated while healthy; disk changes below must
        // take effect without a rescan rewriting the effective artifact.
        ctx.registry.write().await.scan().await.unwrap();
        let users = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let user = users.create_user("all-entry@test.invalid", "Admin", "Entry-Fixture-Password-12345",
            UserRole::Admin).unwrap();
        for channel in ["telegram", "line", "slack"] {
            users.bind_channel_identity(&user.id, channel, "42", true).unwrap();
        }
        ctx.channel_settings.set("telegram", "global", "admin_users", r#"["42"]"#).await.unwrap();
        let admin = users.live_user_context(&user.id).unwrap().unwrap();
        let jwt = Arc::new(JwtConfig::new(b"all-entry-fixture-signing-secret"));
        let token = jwt.issue_access_token(&user, &[]).unwrap();
        let handler = crate::handlers::MethodHandler::new(home.path().to_path_buf()).await;
        let store = Arc::new(TaskStore::open(home.path()).unwrap());
        handler.set_task_store(store.clone()).await;
        handler.set_user_db(users.clone(), jwt.clone()).await;
        let ctx = Arc::new(ctx);
        let chat = Arc::new(crate::webchat::WebChatState::new(ctx.clone(), jwt, users));
        let path = {
            let registry = ctx.registry.read().await;
            crate::entry_gate::resolve_and_check(home.path(), &*registry, "worker").unwrap().config_path
        };
        config.apply(&path);
        let result = crate::entry_gate::check_task_assignee(home.path(), "worker");
        let expected_code = match (config, result) {
            (Config::Healthy, Ok(())) => None,
            (Config::Healthy, Err(error)) => panic!("healthy fixture refused: {error:?}"),
            (_, Err(error)) => Some(error.code()),
            (_, Ok(())) => panic!("broken fixture passed: {config:?}"),
        };
        Self { _home: home, ctx, _runtime: runtime, calls, store, handler, chat, admin, token, expected_code }
    }
    fn home(&self) -> &std::path::Path { &self.ctx.home_dir }
    async fn counts(&self) -> (usize, i64) {
        let tasks = self.store.list_tasks(None, None, None).await.unwrap().len();
        let db = rusqlite::Connection::open(self.home().join("tasks.db")).unwrap();
        let schedules = db.query_row("SELECT COUNT(*) FROM assign_schedules", [], |r| r.get(0)).unwrap();
        (tasks, schedules)
    }
    fn no_models(&self, before: usize) {
        assert_eq!(self.calls.load(Ordering::SeqCst), 0, "no runtime calls");
        assert_eq!(crate::model_call_probe::calls(), before, "no model entry points");
    }
}

struct EntryResult {
    refusal: Option<String>,
    proceeded: bool,
    seed_tasks: usize,
}
impl EntryResult {
    fn result(result: Result<(), String>) -> Self {
        match result {
            Ok(()) => Self { refusal: None, proceeded: true, seed_tasks: 0 },
            Err(text) => Self { refusal: Some(text), proceeded: false, seed_tasks: 0 },
        }
    }
    fn frame(frame: crate::protocol::WsFrame) -> Self {
        match frame {
            crate::protocol::WsFrame::Response { ok: true, .. } => Self::result(Ok(())),
            crate::protocol::WsFrame::Response { ok: false, error: Some(error), .. } =>
                Self::result(Err(error.as_str().or_else(|| error["message"].as_str()).expect("error text").into())),
            other => panic!("unexpected response: {other:?}"),
        }
    }
    fn chat(frame: Value) -> Self {
        if frame["type"] == "error" {
            Self::result(Err(frame["message"].as_str().unwrap().into()))
        } else {
            assert_eq!(frame["type"], "assistant_done", "{frame}");
            Self::result(Ok(()))
        }
    }
}

struct Probes {
    model_calls: usize,
    runtime_calls: usize,
    tasks: usize,
    schedules: i64,
}
struct Outcome { entry: EntryResult, probes: Probes }
async fn observe(f: &Fixture, case: Case, entry: Entry) -> Outcome {
    let entry = drive(f, case, entry).await;
    let (tasks, schedules) = f.counts().await;
    Outcome { entry, probes: Probes { model_calls: crate::model_call_probe::calls(),
        runtime_calls: f.calls.load(Ordering::SeqCst), tasks, schedules } }
}

type Driver = for<'a> fn(&'a Fixture, Case) -> Pin<Box<dyn Future<Output = Outcome> + 'a>>;
struct Row { name: &'static str, sessions: bool, driver: Driver }
macro_rules! row {
    ($name:literal, $sessions:expr, $kind:expr) => {
        Row { name: $name, sessions: $sessions, driver: |f, c| Box::pin(observe(f, c, $kind)) }
    };
}
#[derive(Clone, Copy)]
enum Entry {
    Channel, Arrival, Turn(bool), Pending, AgUi, Command(&'static str), Attachment,
    Goal, Admission, ChatGoal, Schedule, Rpc(&'static str), Retry, Kickoff, Bound,
    TriggerPending, TriggerMention, TriggerReply, AssignArm(bool),
    Background(crate::cost_telemetry::RequestType), GoalTick,
}
const ROWS: &[Row] = &[
    row!("dispatch", false, Entry::Background(crate::cost_telemetry::RequestType::Dispatch)),
    row!("cron", false, Entry::Background(crate::cost_telemetry::RequestType::Cron)),
    row!("heartbeat (Cron request)", false, Entry::Background(crate::cost_telemetry::RequestType::Cron)),
    row!("reminder (Cron request)", false, Entry::Background(crate::cost_telemetry::RequestType::Cron)),
    row!("goal tick refused run", false, Entry::GoalTick),
    row!("channel pipeline", true, Entry::Channel),
    row!("channel trigger pending pipeline", false, Entry::TriggerPending),
    row!("channel trigger bare mention pipeline", false, Entry::TriggerMention),
    // Public conversations bypass messaging-app trigger routing. Exercise the
    // exact pending-execution seam with their original session separately.
    row!("trigger assignment reply session", true, Entry::TriggerReply),
    row!("trigger assignment arm", true, Entry::AssignArm(false)),
    row!("trigger assignment admission arm", true, Entry::AssignArm(true)),
    row!("WebChat arrival", true, Entry::Arrival),
    row!("WebChat turn", true, Entry::Turn(false)),
    row!("WebChat turn with attachment", true, Entry::Turn(true)),
    row!("WebChat pending assignment", true, Entry::Pending),
    row!("AG-UI start", false, Entry::AgUi),
    row!("command /goal", true, Entry::Command("/goal 整理報表")),
    row!("command /交辦", true, Entry::Command("/交辦 @worker 整理報表")),
    row!("channel attachment preflight", true, Entry::Attachment),
    row!("goal core", false, Entry::Goal),
    row!("goal core with admission", false, Entry::Admission),
    row!("goal core chat", true, Entry::ChatGoal),
    row!("schedule creation", false, Entry::Schedule),
    row!("dashboard tasks.goal_create", false, Entry::Rpc("tasks.goal_create")),
    row!("dashboard tasks.skill_assign", false, Entry::Rpc("tasks.skill_assign")),
    row!("dashboard tasks.create", false, Entry::Rpc("tasks.create")),
    row!("dashboard tasks.update reassignment", false, Entry::Rpc("tasks.update")),
    row!("needs-human retry", false, Entry::Retry),
    row!("kickoff approve", false, Entry::Kickoff),
    row!("bound approve", true, Entry::Bound),
];

fn request() -> GoalCreateRequest {
    GoalCreateRequest { agent_id: "worker".into(), created_by: "goal:dashboard".into(),
        description: "整理報表".into(), ..Default::default() }
}
async fn command(f: &Fixture, case: Case, text: &str) -> String {
    command_in_session(f, case.session, text).await
}
async fn command_in_session(f: &Fixture, session: &str, text: &str) -> String {
    let cmd = crate::chat_commands::parse_command(text, None).expect("ChatCommand");
    crate::chat_commands::handle_gated_command(&cmd, &f.ctx,
        &crate::chat_commands::CommandOrigin { channel: "telegram", gate_session: session,
            command_session: session, sender: "42", text, shared: false },
        if matches!(cmd, crate::chat_commands::ChatCommand::Assign(_)) { "main" } else { "worker" },
    ).await
}
async fn drive(f: &Fixture, case: Case, entry: Entry) -> EntryResult {
    match entry {
        Entry::Background(_) | Entry::GoalTick => {
            let request_type = match entry {
                Entry::Background(kind) => kind,
                _ => crate::cost_telemetry::RequestType::Dispatch,
            };
            let result = crate::claude_runner::call_claude_for_agent_with_type(
                f.home(), &f.ctx.registry, "worker", "work", request_type).await;
            if case.healthy() {
                // The fixture runtime intentionally returns an auth failure.
                assert!(f.calls.load(Ordering::SeqCst) > 0, "{result:?}");
                return EntryResult::result(Ok(()));
            }
            let error = result.unwrap_err();
            assert_eq!(crate::entry_gate::background_refusal_code(&error), f.expected_code);
            let mut outcome = EntryResult::result(Err(error.clone()));
            if matches!(entry, Entry::GoalTick) {
                crate::goal_loop::assert_entry_refusal_tick(f.home(), f.store.clone(), &error).await;
                outcome.seed_tasks = 1;
            }
            outcome
        }
        Entry::Channel => {
            let spy = crate::channel_reply::reply_origin::testing::NoImplicitCloud::at(f.home());
            let text = build_reply_for_agent("整理報表", &f.ctx, "worker", case.session, "42", None).await;
            assert_eq!(spy.counts(), [0, 0]);
            if case.healthy() {
                assert_eq!(f.calls.load(Ordering::SeqCst), 1);
                if case.public() {
                    assert!(text.starts_with(crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE), "{text}");
                    assert!(!text.contains("原因代碼"), "{text}");
                } else {
                    assert!(text.contains("Actual Worker"), "{text}");
                }
                EntryResult::result(Ok(()))
            } else { EntryResult::result(Err(text)) }
        }
        Entry::TriggerPending | Entry::TriggerMention | Entry::TriggerReply | Entry::AssignArm(_) => {
            use crate::trigger_route::{assign, pending};
            let session = format!("{}:trigger:{}", case.session, uuid::Uuid::new_v4());
            let key = pending::PendingKey::new("telegram", &session, "42");
            let caller = assign::AssignCaller {
                home: f.home(), channel: "telegram", conversation: &session, sender: "42",
                event: Some("entry-trigger"), seq: pending::next_seq(), fallback: Some("main"),
                access: crate::trigger_route::access::for_channel_sender(f.home(), "telegram", "42"),
            };
            let spy = crate::channel_reply::reply_origin::testing::NoImplicitCloud::at(f.home());
            let reply = match entry {
                Entry::TriggerPending => {
                    // Represents an assignment armed while the target was healthy.
                    pending::arm_kind(&key, "worker", pending::PendingKind::Assign, "prior", caller.seq);
                    crate::channel_reply::inner::build_reply_with_session_inner(
                        "整理報表", &f.ctx, None, &session, "42", None,
                    ).await
                }
                Entry::TriggerMention => crate::channel_reply::inner::build_reply_with_session_inner(
                    "@worker", &f.ctx, None, &session, "42", None,
                ).await,
                Entry::TriggerReply => crate::channel_reply::inner::run_trigger_assignment(
                    &f.ctx, "worker", "整理報表", &caller,
                ).await,
                Entry::AssignArm(admission) => {
                    if admission {
                        let registry = f.ctx.registry.read().await;
                        assign::arm_on_admission(&registry, "/交辦 @worker", &caller);
                        assert_eq!(pending::peek_kind(&key, pending::next_seq()).is_some(), case.healthy());
                    }
                    command_in_session(f, &session, "/交辦 @worker").await
                }
                _ => unreachable!(),
            };
            assert_eq!(spy.counts(), [0, 0]);
            assert_eq!(f.calls.load(Ordering::SeqCst), 0);
            let arms = matches!(entry, Entry::TriggerMention | Entry::AssignArm(_));
            let waiting = pending::peek_kind(&key, pending::next_seq());
            assert_eq!(waiting.is_some(), case.healthy() && arms, "{reply}");
            if let Some((id, kind)) = waiting {
                assert_eq!(id, "worker");
                assert_eq!(kind, if matches!(entry, Entry::TriggerMention) {
                    pending::PendingKind::Chat
                } else { pending::PendingKind::Assign });
                pending::cancel(&key, "test-cleanup", pending::next_seq());
            }
            if case.healthy() {
                assert!(!reply.is_empty());
                assert_eq!(f.counts().await.0, usize::from(!arms), "{reply}");
                EntryResult::result(Ok(()))
            } else { EntryResult::result(Err(reply)) }
        }
        Entry::Arrival => EntryResult::result(web::arrival(&f.chat, &f.admin.user_id, case.session, "整理報表", true).await),
        Entry::Turn(attach) => {
            // A healthy assignment has a deterministic normal terminal frame,
            // without relying on the fixture runtime's intentional auth error.
            let frame = web::turn(&f.chat, &f.admin.user_id, case.session,
                "/交辦 @worker 整理報表", attach).await;
            if case.healthy() {
                assert_eq!(frame["task"]["agent"], "worker", "{frame}");
                assert_eq!(f.counts().await.0, 1);
                if attach {
                    assert!(std::fs::read_dir(f.home().join("agents/worker/attachments")).unwrap().next().is_some());
                }
            }
            EntryResult::chat(frame)
        }
        Entry::Pending => {
            let frame = web::pending(&f.chat, &f.admin.user_id, case.session).await;
            if case.healthy() { assert_eq!(frame["task"]["agent"], "worker", "{frame}"); }
            EntryResult::chat(frame)
        }
        Entry::AgUi => ag_ui(f, case).await,
        Entry::Command(text) => {
            let reply = command(f, case, text).await;
            if case.healthy() {
                assert!(!reply.is_empty());
                assert_eq!(f.counts().await.0, 1, "normal command creates work: {reply}");
                EntryResult::result(Ok(()))
            } else { EntryResult::result(Err(reply)) }
        }
        Entry::Attachment => {
            use crate::channel_reply::attachment_gate::{self, AttachmentEntry};
            match attachment_gate::check(&f.ctx, case.session, case.session, "42", "附件", Some("worker"), None).await {
                AttachmentEntry::Proceed => EntryResult::result(Ok(())),
                AttachmentEntry::Refuse(text) => EntryResult::result(Err(text)),
                other => panic!("unexpected attachment outcome: {other:?}"),
            }
        }
        Entry::Goal | Entry::Admission | Entry::ChatGoal => {
            let polled = Arc::new(AtomicBool::new(false));
            let result = match entry {
                Entry::Admission => {
                    let observed = polled.clone();
                    goal_create_core::create_goal_task_with_admission(f.home(), &f.store, request(),
                        Box::pin(async move { observed.store(true, Ordering::SeqCst); Ok(None) })).await
                }
                Entry::ChatGoal => {
                    let result = goal_create_core::create_goal_task_for_chat(f.home(), &f.store, request()).await;
                    match result {
                        Ok(created) => Ok(created),
                        Err(GoalCreateError::Entry(refusal)) => Err(refusal.user_text(case.session, None, "all-entry")),
                        Err(error) => panic!("expected typed entry refusal: {error:?}"),
                    }
                }
                _ => goal_create_core::create_goal_task(f.home(), &f.store, request()).await,
            };
            if matches!(entry, Entry::Admission) { assert_eq!(polled.load(Ordering::SeqCst), case.healthy()); }
            EntryResult::result(result.map(|created| {
                assert_eq!(created.task.assigned_to, "worker");
                assert_eq!(created.task.status, "todo");
                assert_eq!(created.task.acceptance_criteria_baseline.as_deref(), Some("整理報表"));
            }))
        }
        Entry::Schedule => {
            let creator = crate::assign_schedule::Creator::new(&f.admin.user_id, true).unwrap();
            let req = crate::assign_schedule::NewSchedule {
                agent_id: "worker".into(), kind: crate::assign_schedule::AssignKind::Goal,
                payload: crate::assign_schedule::AssignPayload { description: "整理報表".into(), ..Default::default() },
                spec: crate::assign_schedule::ScheduleSpec::Weekly { days: vec![1], hour: 9, minute: 0 },
                timezone: Some("Asia/Taipei".into()), created_via: "dashboard".into(), source: None,
            };
            let result = crate::assign_schedule::manage::create(f.home(), &f.store, &creator, req, chrono::Utc::now()).await;
            match result {
                Ok(_) => { assert_eq!(f.counts().await, (0, 1)); EntryResult::result(Ok(())) }
                Err(crate::assign_schedule::CreateError::Entry(refusal)) =>
                    EntryResult::result(Err(refusal.user_text(case.session, None, "all-entry"))),
                Err(error) => panic!("unexpected schedule error: {error:?}"),
            }
        }
        Entry::Rpc(method) => rpc(f, case, method).await,
        Entry::Retry | Entry::Kickoff | Entry::Bound => decision(f, case, entry, true).await,
    }
}

async fn entry_contract_for(config: Config) {
    for row in ROWS {
        let audiences: &[&str] = if row.sessions {
            &["telegram:42", "widget-visitor:all-entry", "odoolivechat:all-entry:1"]
        } else { &["telegram:42"] };
        for &session in audiences {
            let f = Fixture::new(config).await;
            let case = Case { config, session };
            let before = crate::model_call_probe::calls();
            let Outcome { entry: outcome, probes } = (row.driver)(&f, case).await;
            if case.healthy() {
                assert!(outcome.proceeded && outcome.refusal.is_none(), "{} {case:?}", row.name);
            } else {
                assert!(!outcome.proceeded, "{} {case:?}", row.name);
                let refusal = outcome.refusal.expect(row.name);
                if case.public() {
                    assert!(refusal.starts_with(crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE), "{} {case:?}: {refusal}", row.name);
                    assert!(!refusal.contains(f.expected_code.unwrap()), "{refusal}");
                    assert!(!refusal.contains("原因代碼"), "{refusal}");
                } else {
                    assert!(refusal.contains(f.expected_code.unwrap()), "{} {case:?}: {refusal}", row.name);
                }
                assert_eq!(probes.runtime_calls, 0, "{} {case:?}: no runtime calls", row.name);
                assert_eq!(probes.model_calls, before, "{} {case:?}: no model entry points", row.name);
                // Decisions and reassignment operate on a pre-existing task:
                // zero NEW rows, and their drivers assert the old row unchanged.
                assert_eq!((probes.tasks, probes.schedules), (outcome.seed_tasks, 0), "{} {case:?}", row.name);
                assert_no_saved_attachments(f.home());
                if outcome.seed_tasks == 0 {
                    assert_refused_before_any_side_effect(&f.ctx, session, &f.calls, before).await;
                }
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn all_entries_share_contract_mismatch() { entry_contract_for(Config::Mismatch).await; }

#[tokio::test(flavor = "current_thread")]
async fn all_entries_share_contract_malformed() { entry_contract_for(Config::Malformed).await; }

#[tokio::test(flavor = "current_thread")]
async fn all_entries_share_contract_unknown_engine() { entry_contract_for(Config::UnknownEngine).await; }

#[tokio::test(flavor = "current_thread")]
async fn all_entries_share_contract_missing_model() { entry_contract_for(Config::MissingModel).await; }

#[tokio::test(flavor = "current_thread")]
async fn all_entries_share_contract_healthy() { entry_contract_for(Config::Healthy).await; }

#[test]
fn entry_contract_tests_cover_every_config() {
    assert_eq!(CONFIGS, &[
        Config::Mismatch, Config::Malformed, Config::UnknownEngine,
        Config::MissingModel, Config::Healthy,
    ]);
}

async fn rpc(f: &Fixture, case: Case, method: &str) -> EntryResult {
    let params = match method {
        "tasks.goal_create" => json!({"agent_id":"worker", "description":"整理報表", "plan_first":false}),
        "tasks.skill_assign" => json!({"skill_name":"zz-entry-report"}),
        "tasks.create" => json!({"title":"整理報表", "assigned_to":"worker"}),
        "tasks.update" => {
            f.store.insert_task(&TaskRow::new("existing".into(), "work".into(), "original".into(),
                "medium".into(), "main".into(), "system".into())).await.unwrap();
            json!({"task_id":"existing", "assigned_to":"worker", "description":"updated"})
        }
        _ => unreachable!(),
    };
    let row_before = if method == "tasks.update" {
        Some(serde_json::to_value(f.store.get_task("existing").await.unwrap().unwrap()).unwrap())
    } else { None };
    let mut outcome = EntryResult::frame(f.handler.handle(method, params, &f.admin).await);
    if method == "tasks.update" {
        outcome.seed_tasks = 1;
        let saved = f.store.get_task("existing").await.unwrap().unwrap();
        assert_eq!(saved.assigned_to, if case.healthy() { "worker" } else { "main" });
        assert_eq!(saved.description, if case.healthy() { "updated" } else { "original" });
        if !case.healthy() { assert_eq!(serde_json::to_value(saved).unwrap(), row_before.unwrap()); }
    } else if case.healthy() {
        let tasks = f.store.list_tasks(None, None, None).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].assigned_to, "worker");
    }
    outcome
}

async fn decision(f: &Fixture, case: Case, entry: Entry, approve: bool) -> EntryResult {
    use crate::approval::{ApprovalBroker, ApprovalStatus, DecisionContext, ExecutionBinding, RequestKind};
    use crate::decision_action::DecisionAct;
    let original_status = match entry { Entry::Retry => "needs_human", Entry::Bound => "in_progress", _ => "todo" };
    let mut task = TaskRow::new("existing".into(), "報表".into(), "整理報表".into(),
        "medium".into(), "worker".into(), "system".into());
    task.goal_mode = true;
    task.status = original_status.into();
    f.store.insert_task(&task).await.unwrap();
    let broker = ApprovalBroker::open(f.home()).unwrap();
    let task_before = serde_json::to_value(f.store.get_task("existing").await.unwrap().unwrap()).unwrap();
    let mut approval_id = None;
    let result = match entry {
        Entry::Retry => crate::goal_notify::apply_needs_human(f.home(), "telegram", "42", "existing",
            if approve { DecisionAct::Retry } else { DecisionAct::Abort }).await,
        Entry::Kickoff => {
            let id = broker.request("main", "goal_kickoff", "Start", json!({"task_id":"existing"}), 600).await.unwrap();
            let result = crate::goal_notify::apply_kickoff(f.home(), "telegram", "42", id.as_str(), approve).await;
            approval_id = Some(id);
            result
        }
        Entry::Bound => {
            let snapshot = f.store.authority_snapshot("existing").await.unwrap().unwrap();
            let context = DecisionContext { channel: "slack".into(), account_id: "bot".into(),
                principal_id: "42".into(), conversation_id: case.session.into() };
            let payload = json!({"action":"continue"});
            let binding = ExecutionBinding {
                schema_version: 1, run_id: uuid::Uuid::new_v4().to_string(), run_origin_kind: "ingress".into(),
                actor_principal: "main".into(), conversation_audience: Default::default(),
                decision_context: context.clone(), task_id: Some("existing".into()),
                task_revision: Some(snapshot.revision), task_snapshot_hash: Some(snapshot.hash),
                payload_hash: crate::approval::payload_hash(&payload),
                policy_revision: crate::approval::policy_revision(f.home(), "main").unwrap(),
                cwd: None, environment_hash: "fixture".into(), file_hashes: Default::default(),
                expires_at: (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
                resume_handler: "live_only_v1".into(), resume_version: 1,
            };
            let id = broker.request_bound(RequestKind::Approval, "main", "Continue", payload, binding).await.unwrap();
            let data = format!("duduclaw:decide:apv:{}:{}", if approve { "ok" } else { "no" }, id.as_str());
            let result = crate::decision_notify::route_bound_press(f.home(), &context, &data).await
                .expect("bound approval route claimed");
            approval_id = Some(id);
            result
        }
        _ => unreachable!(),
    };
    let applied = case.healthy() || !approve;
    if let Some(id) = approval_id {
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, if applied {
            if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied }
        } else { ApprovalStatus::Pending });
    }
    let saved = f.store.get_task("existing").await.unwrap().unwrap();
    let expected_status = if matches!(entry, Entry::Retry) && applied {
        if approve { "pending" } else { "cancelled" }
    } else { original_status };
    assert_eq!(saved.status, expected_status);
    assert_eq!(saved.assigned_to, "worker");
    assert_eq!(saved.description, "整理報表");
    if !applied {
        assert_eq!(serde_json::to_value(&saved).unwrap(), task_before, "refusal leaves the complete row unchanged");
    }
    let mut outcome = EntryResult::result(result.map(|text| assert!(!text.is_empty())));
    outcome.seed_tasks = 1;
    outcome
}

async fn ag_ui(f: &Fixture, case: Case) -> EntryResult {
    use axum::{body::{Body, to_bytes}, http::{Request, StatusCode}};
    use tower::ServiceExt;
    let request = Request::builder().method("POST").uri("/api/ag-ui/agents/worker/runs")
        .header("content-type", "application/json").header("authorization", format!("Bearer {}", f.token))
        .body(Body::from(json!({"threadId":"entry-thread", "runId":"entry-run",
            "messages":[{"id":"u", "role":"user", "content":"整理報表"}]}).to_string())).unwrap();
    let spy = crate::channel_reply::reply_origin::testing::NoImplicitCloud::at(f.home());
    let response = crate::webchat::ag_ui::router(f.chat.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let body = tokio::time::timeout(std::time::Duration::from_secs(30),
        to_bytes(response.into_body(), 1024 * 1024)).await.expect("AG-UI stream terminates").unwrap();
    assert_eq!(spy.counts(), [0, 0]);
    if case.healthy() {
        assert_eq!(status, StatusCode::OK);
        assert_eq!(f.calls.load(Ordering::SeqCst), 1, "healthy AG-UI reaches its configured runtime");
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("RUN_STARTED"), "{text}");
        for gate_error in ["employee_entry_refused", "agent_config_unreadable", "model_provider_mismatch", "runtime_unavailable"] {
            assert!(!text.contains(gate_error), "unexpected entry-gate refusal {gate_error}: {text}");
        }
        // The fixture runtime counts the call and then always fails.
        assert!(
            text.contains("RUN_FINISHED")
                || (text.contains("RUN_ERROR") && text.contains("\"code\":\"runtime_failed\"")),
            "{text}"
        );
        EntryResult::result(Ok(()))
    } else {
        assert_eq!(status, StatusCode::CONFLICT);
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"], "employee_entry_refused");
        EntryResult::result(Err(value["message"].as_str().unwrap().into()))
    }
}

// Exempt commands have their own table because success, rather than refusal,
// is the invariant even when the effective artifact is malformed.
const EXEMPT: &[&str] = &["!STOP", "!RESUME", "/status", "/help", "/new", "/takeover"];
#[tokio::test(flavor = "current_thread")]
async fn broken_employees_keep_exempt_commands_available() {
    for &config in CONFIGS.iter().filter(|&&c| c != Config::Healthy) {
        for &text in EXEMPT {
            for webchat in [false, true] {
                // /takeover is not a ChatCommand. Like v1.85, run falls
                // through to terminal delivery via subscriptions, which this
                // direct-run helper does not install (even when healthy).
                // Keep its real channel handler covered below; this WebChat
                // matrix covers commands that answer directly on the sink.
                if webchat && text == "/takeover" {
                    assert!(crate::chat_commands::parse_command(text, None).is_none());
                    continue;
                }
                let f = Fixture::new(config).await;
                let case = Case { config, session: "telegram:42" };
                let before = crate::model_call_probe::calls();
                if webchat {
                    web::arrival(&f.chat, &f.admin.user_id, case.session, text, false).await.unwrap();
                    let frame = web::turn(&f.chat, &f.admin.user_id, case.session, text, false).await;
                    assert!(!frame.to_string().contains(f.expected_code.unwrap()), "{text}: {frame}");
                    let outcome = EntryResult::chat(frame);
                    assert!(outcome.proceeded && outcome.refusal.is_none(), "{config:?} {text}");
                } else {
                    let reply = if text == "/takeover" {
                        // This separate parser never creates a ChatCommand.
                        let cmd = crate::chat_commands::parse_takeover(text).unwrap();
                        assert!(crate::entry_gate::takeover_is_exempt(&cmd));
                        crate::chat_commands::handle_takeover(&f.ctx, case.session, "42", &cmd).await
                    } else { command(&f, case, text).await };
                    assert!(!reply.is_empty(), "{text}");
                    assert!(!reply.contains(f.expected_code.unwrap()), "{text}: {reply}");
                }
                f.no_models(before);
                assert_eq!(f.counts().await, (0, 0));
                assert_no_saved_attachments(f.home());
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn broken_assignees_still_allow_decline_and_cancel() {
    for &config in CONFIGS.iter().filter(|&&c| c != Config::Healthy) {
        for entry in [Entry::Retry, Entry::Kickoff, Entry::Bound] {
            let f = Fixture::new(config).await;
            let before = crate::model_call_probe::calls();
            let outcome = decision(&f, Case { config, session: "telegram:42" }, entry, false).await;
            assert!(outcome.proceeded && outcome.refusal.is_none());
            f.no_models(before);
            assert_eq!(f.counts().await, (1, 0));
            assert_no_saved_attachments(f.home());
        }
    }
}

// A2A's wire contract intentionally masks the specific EntryRefusal code.
// Keep its real transport result separate from the shared text-code matrix.
#[tokio::test(flavor = "current_thread")]
async fn a2a_send_preserves_its_protocol_refusal_and_creates_no_work() {
    for &config in CONFIGS {
        let f = Fixture::new(config).await;
        let path = f.home().join("config.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{original}\n[a2a]\nexposed_agents=['worker']\npublic_url='https://gateway.example/a2a'\n")).unwrap();
        let before = crate::model_call_probe::calls();
        let frame = f.handler.handle("a2a.SendMessage", json!({
            "message":{"messageId":"all-entry", "role":"ROLE_USER", "parts":[{"text":"Prepare a report"}]},
            "configuration":{"returnImmediately":true}, "metadata":{"agentId":"worker"}
        }), &f.admin).await;
        if config == Config::Healthy {
            assert!(EntryResult::frame(frame).proceeded);
            assert_eq!(f.counts().await, (1, 0));
        } else {
            let crate::protocol::WsFrame::Response { ok: false, error: Some(error), .. } = frame
                else { panic!("A2A must refuse") };
            assert_eq!(error["data"][0]["reason"], "EMPLOYEE_CONFIGURATION_REFUSED");
            assert_eq!(error["code"], -32000);
            assert_eq!(f.counts().await, (0, 0));
            assert_refused_before_any_side_effect(&f.ctx, "telegram:42", &f.calls, before).await;
        }
        f.no_models(before);
        assert_no_saved_attachments(f.home());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inline_safety_commands_refuse_non_admins_and_public_visitors_without_mutation() {
    use crate::channel_reply::entry_gate_test_helpers::misaligned_worker;
    for session in ["telegram:42:7", "telegram-evil:42", "widget-visitor:test", "odoolivechat:test"] {
        let (_home, ctx, _runtime, calls) = misaligned_worker("").await;
        let channel = session.split(':').next().unwrap();
        // Exact identities and channel names are required. Public visitors
        // remain unprivileged even if their channel setting lists their id.
        let admins = if session == "telegram:42:7" { r#"["142","telegram:42"]"# } else { r#"["42"]"# };
        ctx.channel_settings.set("telegram", "global", crate::channel_settings::keys::ADMIN_USERS, admins).await.unwrap();
        if crate::channel_reply::assembly::public_session(session) {
            ctx.channel_settings.set(channel, "global", crate::channel_settings::keys::ADMIN_USERS, r#"["42"]"#).await.unwrap();
        }
        let failsafe = ctx.failsafe.as_ref().unwrap();
        let before = crate::model_call_probe::calls();
        for text in ["!STOP", "!STOP ALL", "!RESUME"] {
            if text == "!RESUME" {
                failsafe.force_halt(session, "existing halt").await;
            }
            let state_before = failsafe.get_state(session).await
                .map(|s| (s.level, s.since, s.reason, s.auto_recover_at));
            let reply = crate::channel_reply::inner::build_reply_with_session_inner(text, &ctx, Some("worker"), session, "42", None).await;
            assert_eq!(reply, "只有管理員可以使用這個指令。", "{session}: {text}");
            let state_after = failsafe.get_state(session).await
                .map(|s| (s.level, s.since, s.reason, s.auto_recover_at));
            assert_eq!(state_after, state_before, "{session}: {text}");
            assert!(failsafe.get_state("__global__").await.is_none());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(crate::model_call_probe::calls(), before);
            assert!(ctx.session_manager.session_agent(session).await.unwrap().is_none());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inline_safety_admins_can_control_broken_employees_but_stop_all_stays_scoped() {
    use crate::channel_reply::entry_gate_test_helpers::misaligned_worker;
    use duduclaw_security::failsafe::FailsafeLevel;
    for malformed in [false, true] {
        for admin in ["42", "telegram:42:7"] {
            let (home, ctx, _runtime, calls) = misaligned_worker("").await;
            if malformed {
                std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap();
            }
            let session = "telegram:42:7";
            ctx.channel_settings.set("telegram", "global", crate::channel_settings::keys::ADMIN_USERS,
                &serde_json::to_string(&[admin]).unwrap()).await.unwrap();
            let failsafe = ctx.failsafe.as_ref().unwrap();
            let before = crate::model_call_probe::calls();
            for stop in ["!STOP", "!STOP ALL"] {
                let reply = crate::channel_reply::inner::build_reply_with_session_inner(stop, &ctx, Some("worker"), session, "42", None).await;
                assert!(reply.contains("stopped"), "{reply}");
                assert_eq!(failsafe.get_level(session).await, FailsafeLevel::L4Halted);
                let states = failsafe.active_states().await;
                assert_eq!(states.len(), 1);
                assert_eq!(states[0].0, session, "global halt must not be set");
                let reply = crate::channel_reply::inner::build_reply_with_session_inner("!RESUME", &ctx, Some("worker"), session, "42", None).await;
                assert!(reply.contains("resumed"), "{reply}");
                assert!(failsafe.active_states().await.is_empty());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(crate::model_call_probe::calls(), before);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inline_safety_status_remains_available_without_admin_on_a_broken_employee() {
    let (_home, ctx, _runtime, calls) = crate::channel_reply::entry_gate_test_helpers::misaligned_worker("").await;
    let session = "telegram:42:7";
    let before = crate::model_call_probe::calls();
    let reply = crate::channel_reply::inner::build_reply_with_session_inner("!STATUS", &ctx, Some("worker"), session, "42", None).await;
    assert_eq!(reply, duduclaw_security::failsafe::format_status(session, None));
    assert_refused_before_any_side_effect(&ctx, session, &calls, before).await;
}
