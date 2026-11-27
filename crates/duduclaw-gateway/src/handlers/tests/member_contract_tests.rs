//! RM-213 contract checks use a real Employee, live users.db and an ordinary
//! direct Owner binding. No admin fallback or permission changes on the call path.
use super::*;
use super::super::member_contract::{MEMBER_CONTRACT_METHODS, MemberContractKind};
use super::team_callable_rpc_tests::{self as members, ok_payload};
use std::collections::BTreeSet;

struct Fixture {
    base: members::Fixture,
    member: UserContext,
    session: String,
    task: String,
}

async fn fixture() -> Fixture {
    let base = members::fixture().await;
    let home = base._home.path();
    base.handler.set_cron_store(Arc::new(CronStore::open(home).unwrap())).await;
    let member = base.db.live_user_context(&base.emp1).unwrap().unwrap();
    assert_eq!(member.role, UserRole::Employee);
    assert!(member.has_agent_access("sales", AccessLevel::Owner));

    // A fresh, nonempty cache keeps skills.search entirely offline.
    let mut config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    config.push_str("\n[skill_hubs]\nenabled = [\"anthropic-skills\"]\n[knowledge]\nenabled = true\n");
    std::fs::write(home.join("config.toml"), config).unwrap();
    std::fs::create_dir_all(home.join("skill_hub_cache")).unwrap();
    std::fs::write(home.join("skill_hub_cache/anthropic-skills.json"), json!({
        "updated_at": Utc::now().to_rfc3339(), "source": "fixture",
        "skills": [{"name": "contract-example", "description": "contract fixture"}],
    }).to_string()).unwrap();
    let agent = home.join("agents/sales");
    let config = std::fs::read_to_string(agent.join("agent.toml")).unwrap()
        .replace("[container]", "[container]\nsandbox_enabled = false");
    std::fs::write(agent.join("agent.toml"), config).unwrap();
    std::fs::create_dir_all(agent.join("wiki")).unwrap();
    std::fs::write(agent.join("wiki/contract.md"), "# Contract\n\nFixture page.\n").unwrap();
    base.handler.registry.write().await.scan().await.unwrap();

    let sm = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let session = format!("webchat:webchat:{}:contract", crate::webchat::webchat_owner_tag(&member.user_id));
    {
        let conn = sm.acquire().await;
        conn.execute(
            "INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id) VALUES (?1,'sales',?2,?2,'webchat_user',?3)",
            params![session, Utc::now().to_rfc3339(), member.user_id],
        ).unwrap();
    }
    let registry = Arc::new(RwLock::new(AgentRegistry::new(home.join("agents"))));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    base.handler.set_reply_ctx(Arc::new(crate::channel_reply::ReplyContext::new(
        registry, home.to_path_buf(), sm, Arc::new(RwLock::new(HashMap::new())), tx,
    ))).await;
    let task = ok_payload(&base.handler.handle("tasks.create", json!({
        "assigned_to": "sales", "title": "Contract fixture",
    }), &member).await)["task"]["id"].as_str().unwrap().to_owned();
    Fixture { base, member, session, task }
}

impl Fixture {
    /// Minimal target-bearing calls. External connection flows deliberately
    /// stop at validation without contacting Google or starting an AI runner.
    async fn params(&self, method: &str) -> Value {
        match method {
            "agents.update" => json!({"agent_id": "sales", "display_name": "Sales"}),
            "users.set_primary_agent" => json!({"agent_id": "sales"}),
            "onboarding.update" => json!({"dismissed": true}),
            "chat.sessions.history" | "chat.sessions.restore" | "knowledge.citations.for_session" =>
                json!({"session_id": self.session}),
            "tasks.create" => json!({"assigned_to": "sales", "title": "Contract task"}),
            "tasks.update" => json!({"task_id": self.task, "title": "Updated fixture"}),
            "tasks.assign" => json!({"task_id": self.task, "assigned_to": "sales"}),
            "tasks.comment" => json!({"task_id": self.task, "body": "Fixture comment"}),
            "tasks.comments" => json!({"task_id": self.task}),
            "tasks.goal_decide" => json!({"task_id": self.task, "action": "done"}),
            // Invalid duration is a validation error, after naming a valid runner.
            "tasks.goal_create" => json!({"agent_id": "sales", "description": "Fixture", "duration_hours": -1}),
            "tasks.skill_assign" => json!({"skill_name": "contract-missing", "agent_id": "sales"}),
            "cron.add" => json!({"agent_id": "sales", "name": "Contract", "cron": "0 9 * * *", "task": "Prepare report"}),
            "cron.update" | "cron.pause" | "cron.resume" | "cron.remove" | "cron.run_now" => {
                let id = uuid::Uuid::new_v4().to_string();
                let mut row = CronTaskRow::new(id.clone(), "Contract".into(), "sales".into(),
                    "0 9 * * *".into(), "Prepare report".into());
                row.created_by_user_id = Some(self.member.user_id.clone());
                row.on_behalf_of = Some(format!("user:{}", self.member.user_id));
                row.scoped_by_user_id = Some(self.member.user_id.clone());
                self.base.handler.cron_store().await.unwrap().insert_scoped(&row).await.unwrap();
                json!({"id": id})
            }
            "reminders.cancel" => {
                let home = self.base._home.path();
                let row = json!({"id": "contract-reminder", "agent_id": "sales",
                    "trigger_at": (Utc::now() + ChronoDuration::days(1)).to_rfc3339(),
                    "channel": "webchat", "chat_id": self.session, "message": "Fixture",
                    "on_behalf_of": format!("user:{}", self.member.user_id)});
                // Isolated fixture file, never appended to or shared with another process.
                std::fs::write(home.join("reminders.jsonl"), format!("{row}\n")).unwrap();
                json!({"id": "contract-reminder"})
            }
            "assign_schedules.pause" | "assign_schedules.resume" | "assign_schedules.remove" => {
                let created = ok_payload(&self.base.handler.handle("tasks.create", json!({
                    "assigned_to": "sales", "title": "Scheduled fixture",
                    "schedule": {"kind": "daily", "time": "09:00"}, "schedule_timezone": "Asia/Taipei",
                }), &self.member).await);
                json!({"id": created["schedule"]["id"]})
            }
            "skills.search" => json!({"query": "contract"}),
            "skills.install_request" => json!({"scope": "sales", "content": "---\nname: contract-example\ndescription: Fixture\n---\nPrepare a report."}),
            "skills.share" => json!({"agent_id": "sales", "skill_name": "contract-missing"}),
            "skills.adopt" => json!({"target_agent_id": "sales", "skill_name": "contract-missing"}),
            "memory.search" | "wiki.search" => json!({"agent_id": "sales", "query": "contract"}),
            "memory.history" => json!({"agent_id": "sales", "subject": "contract", "predicate": "fixture"}),
            "wiki.read" => json!({"agent_id": "sales", "page_path": "contract.md"}),
            "me.memory.forget" => json!({"id": uuid::Uuid::new_v4().to_string()}),
            "dashboard.layout.set" => json!({"widgets": []}),
            "users.change_password" => json!({"current_password": "incorrect-fixture-password", "new_password": "new-fixture-password-123"}),
            "users.change_email" => json!({"email": "changed@test.invalid", "current_password": "incorrect-fixture-password"}),
            "me.status_tokens.create" => json!({"name": "Contract fixture"}),
            "me.status_tokens.revoke" => json!({"id": uuid::Uuid::new_v4().to_string()}),
            "me.google.connect_start" => json!({"redirect_origin": "http://localhost:18889", "acknowledge_google_data": false}),
            "me.google.write_connect_start" => json!({"redirect_origin": "http://localhost:18889", "acknowledge_google_data": false, "acknowledge_write": false}),
            "me.google.connect_finish" => json!({"state": "missing-fixture-state"}),
            "me.google.connect_paste" => json!({"url": "invalid-fixture-callback"}),
            "me.google.disconnect" | "me.google.write_disconnect" => json!({"confirm": false}),
            "me.google.chat_confirm" | "me.google.write_confirm" => json!({"id":"missing", "approve":false}),
            "me.google.consents.set" => json!({"agent_id": "sales", "allowed": false}),
            "agents.list" | "chat.sessions.list" | "tasks.list" | "activity.list"
            | "decisions.list" | "skills.list" | "skills.recent" | "memory.browse"
            | "memory.key_facts" | "wiki.pages" => json!({"agent_id": "sales"}),
            "users.me" | "about.get" | "onboarding.get" | "agents.callable" | "agents.org_chart"
            | "cron.list" | "cron.templates" | "reminders.list" | "assign_schedules.list"
            | "skills.shared" | "skills.leaderboard" | "shared_wiki.pages" | "me.memory.list"
            | "dashboard.layout.get" | "me.status_tokens.list" | "me.google.status" | "me.google.chat_confirmations" | "me.google.write_confirmations" => json!({}),
            other => panic!("missing member contract fixture: {other}"),
        }
    }
}

fn error_text(frame: &WsFrame) -> Option<String> {
    match frame {
        WsFrame::Response { ok: true, payload: Some(_), .. } => None,
        WsFrame::Response { ok: false, error: Some(error), .. } => Some(error.to_string().to_lowercase()),
        other => panic!("invalid response: {other:?}"),
    }
}

#[tokio::test]
async fn member_contract_methods_are_routed() {
    let f = fixture().await;
    for method in MEMBER_CONTRACT_METHODS {
        let frame = f.base.handler.handle(method.name, f.params(method.name).await, &f.member).await;
        if let Some(error) = error_text(&frame) {
            for unknown in ["unknown method", "unknown_method", "method not found"] {
                assert!(!error.contains(unknown), "{} is not routed: {frame:?}", method.name);
            }
        }
    }
}

#[tokio::test]
async fn member_contract_accepts_an_ordinary_employee_with_normal_bindings() {
    let f = fixture().await;
    for method in MEMBER_CONTRACT_METHODS {
        let frame = f.base.handler.handle(method.name, f.params(method.name).await, &f.member).await;
        if let Some(error) = error_text(&frame) {
            for refusal in ["permission denied", "permission_denied", "forbidden", "unauthorized",
                "access denied", "not allowed", "admin_only", "manager_only", "login_required",
                "不允許", "權限不足", "無權", "僅限管理員", "需要管理員"] {
                assert!(!error.contains(refusal), "{} refused a member: {frame:?}", method.name);
            }
        }
    }
}

#[tokio::test]
async fn member_contract_read_payloads_keep_their_key_fields() {
    let f = fixture().await;
    for method in MEMBER_CONTRACT_METHODS.iter().filter(|m| m.kind == MemberContractKind::Read) {
        assert!(!method.key_fields.is_empty(), "{} needs a snapshot", method.name);
        let frame = f.base.handler.handle(method.name, f.params(method.name).await, &f.member).await;
        let payload = ok_payload(&frame);
        let fields = payload.as_object().unwrap_or_else(|| panic!("{}: {frame:?}", method.name));
        for key in method.key_fields {
            assert!(fields.contains_key(*key), "{} lost {key}: {frame:?}", method.name);
        }
        // RM-212's avatar lives inside user, rather than at the payload root.
        if method.name == "users.me" {
            assert!(payload["user"].as_object().unwrap().contains_key("avatar_agent_id"));
        }
    }
}

#[test]
fn member_contract_markdown_lists_exactly_the_contract_methods() {
    let doc = include_str!("../../../../../docs/api/member-contract.md");
    let table = doc.split("<!-- member-contract-methods:start -->").nth(1).unwrap()
        .split("<!-- member-contract-methods:end -->").next().unwrap();
    let names: Vec<&str> = table.lines().filter_map(|line| {
        let cell = line.strip_prefix('|')?.split('|').next()?.trim();
        cell.strip_prefix('`')?.strip_suffix('`')
    }).collect();
    let actual: BTreeSet<&str> = names.iter().copied().collect();
    let expected: BTreeSet<&str> = MEMBER_CONTRACT_METHODS.iter().map(|m| m.name).collect();
    assert_eq!(names.len(), actual.len(), "duplicate method in Markdown");
    assert_eq!(MEMBER_CONTRACT_METHODS.len(), expected.len(), "duplicate contract method");
    assert_eq!(actual, expected);
    assert!(MEMBER_CONTRACT_METHODS.iter().all(|m| matches!(m.since, "1.85.0" | "1.86.0") && !m.summary_en.is_empty()));
}

#[test]
fn drive_confirmation_contract_is_browser_only_since_1_86() {
    for method in ["me.google.write_confirmations", "me.google.write_confirm"] {
        let entry = MEMBER_CONTRACT_METHODS.iter().find(|entry| entry.name == method).unwrap();
        assert_eq!(entry.since, "1.86.0");
        assert!(super::super::member_contract::member_key_method(method).is_none());
    }
}
