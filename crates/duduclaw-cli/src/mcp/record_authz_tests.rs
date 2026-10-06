//! Caller ↔ record relationship checks (`record_authz.rs`) wired into
//! `tasks_update` / `tasks_claim` / `tasks_complete` / `tasks_block`, the cron
//! management tools, `create_reminder`, `activity_post`, `tasks_create`
//! and `agent_update`.
//!
//! Org used throughout (no `config.toml` ⇒ the default `department` policy):
//!
//! ```text
//! ceo ─ sales-lead (業務) ─ sales-rep (業務), sales-rep2 (業務)
//!     └ mkt-lead   (行銷) ─ mkt-rep   (行銷)
//! ```

use super::*;
use duduclaw_gateway::task_store::{TaskRow, TaskStore};
use std::fs;

struct Home(std::path::PathBuf);
impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("duduclaw-authz-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("agents")).unwrap();
        for (name, parent, dept) in [
            ("ceo", "", ""),
            ("sales-lead", "ceo", "業務"),
            ("sales-rep", "sales-lead", "業務"),
            ("sales-rep2", "sales-lead", "業務"),
            ("mkt-lead", "ceo", "行銷"),
            ("mkt-rep", "mkt-lead", "行銷"),
        ] {
            write_agent(&path, name, parent, dept);
        }
        Self(path)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_agent(home: &std::path::Path, name: &str, reports_to: &str, department: &str) {
    let dir = home.join("agents").join(name);
    fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = "@{name}"
reports_to = "{reports_to}"
icon = "🤖"
department = "{department}"

[model]
preferred = "claude-sonnet-4-6"
fallback = ""
api_mode = "cli"
account_pool = []

[budget]
monthly_limit_cents = 1000
warn_threshold_percent = 80
hard_stop = false

[container]
sandbox_enabled = false
network_access = false
timeout_ms = 60000
max_concurrent = 2
readonly_project = false
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 300
max_concurrent_runs = 1
cron = ""

[permissions]
can_create_agents = true
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = true
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = false

[capabilities]
computer_use = false
browser_via_bash = false
allowed_tools = []
denied_tools = []

[proactive]
enabled = false

[cultural_context]
locale = "zh-TW"
high_context = true
"#
    );
    fs::write(dir.join("agent.toml"), toml).unwrap();
}

fn is_error(v: &Value) -> bool {
    v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false)
}

fn text(v: &Value) -> String {
    v["content"][0]["text"].as_str().unwrap_or("").to_string()
}

/// Audit rows of `tool_calls.jsonl` whose `tool_name` is `tool`.
fn audit_rows(home: &std::path::Path, tool: &str) -> Vec<Value> {
    fs::read_to_string(home.join("tool_calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|row| row.get("tool_name").and_then(|t| t.as_str()) == Some(tool))
        .collect()
}

async fn seed_task(
    home: &std::path::Path,
    assigned_to: &str,
    created_by: &str,
    status: &str,
    goal_mode: bool,
    tags: &str,
) -> String {
    let store = TaskStore::open(home).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut row = TaskRow::new(
        id.clone(),
        "original title".into(),
        "original description".into(),
        "medium".into(),
        assigned_to.into(),
        created_by.into(),
    );
    row.status = status.into();
    row.goal_mode = goal_mode;
    row.tags = tags.into();
    store.insert_task(&row).await.unwrap();
    id
}

async fn load_task(home: &std::path::Path, id: &str) -> TaskRow {
    TaskStore::open(home).unwrap().get_task(id).await.unwrap().unwrap()
}

#[allow(non_snake_case)]
fn A(id: &str) -> RecordActor<'_> {
    RecordActor::Agent(id)
}

const OP: RecordActor<'static> = RecordActor::Operator("dudu");

/// Make `name` the main agent (`[agent] role = "main"`), which the cron
/// `"default"` alias resolves to.
fn make_main(home: &std::path::Path, name: &str) {
    let path = home.join("agents").join(name).join("agent.toml");
    let text = fs::read_to_string(&path).unwrap().replace("role = \"specialist\"", "role = \"main\"");
    fs::write(path, text).unwrap();
}

// ── RecordActor classification ──────────────────────────────────────────────

#[test]
fn actor_classification_follows_the_key_and_the_process_identity() {
    let internal = duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
    // Internal key with a process identity → the resolved process identity.
    assert_eq!(RecordActor::resolve(internal, "sales-rep", Some("sales-rep"), false), A("sales-rep"));
    // ...even when the raw env text was refused by `get_default_agent`.
    assert_eq!(
        RecordActor::resolve(internal, duduclaw_core::UNTRUSTED_AGENT_ID, Some("cron"), false),
        A(duduclaw_core::UNTRUSTED_AGENT_ID)
    );
    // Internal key without one → the internal client id, which owns nothing.
    assert_eq!(RecordActor::resolve(internal, "dudu", None, false), A(internal));
    assert_eq!(RecordActor::resolve("", "dudu", Some("  "), false), A(internal));
    // A per-agent key → that agent, whatever the process says.
    assert_eq!(RecordActor::resolve("mkt-rep", "dudu", None, true), A("mkt-rep"));
    // A non-agent key in a process started for an employee → the employee.
    assert_eq!(
        RecordActor::resolve("claude-desktop", "sales-rep", Some("sales-rep"), false),
        A("sales-rep")
    );
    // A non-agent key with no employee identity → operator, stamping the
    // process default agent as before.
    let op = RecordActor::resolve("admin-cli", "dudu", None, false);
    assert_eq!(op, RecordActor::Operator("dudu"));
    assert_eq!(op.id(), "dudu");
    assert_eq!(op.agent(), None);
}

/// The record checks and the memory tools must never disagree about who is
/// an AI employee: wherever `ai_employee_caller` names one, `resolve` names
/// the same one, and it only adds employees (a process started for one).
#[test]
fn actor_classification_agrees_with_the_memory_tools() {
    let internal = duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
    let clients = ["", internal, "mkt-rep", "eph-agnes-r1-planner-abc", "admin-cli", "claude-desktop"];
    let envs: [Option<&str>; 4] = [None, Some(""), Some("  "), Some("sales-rep")];
    for client in clients {
        for env in envs {
            for client_is_agent in [false, true] {
                let memory = crate::mcp_memory_handlers::ai_employee_caller(client, env, client_is_agent);
                // `default_agent` equals the env claim when it is set, the
                // config fallback otherwise — as `get_default_agent` resolves it.
                let default_agent = env.map(str::trim).filter(|e| !e.is_empty()).unwrap_or("dudu");
                let actor = RecordActor::resolve(client, default_agent, env, client_is_agent);
                let ctx = format!("client={client:?} env={env:?} is_agent={client_is_agent}");
                match memory {
                    Some(id) => assert_eq!(actor.agent(), Some(id.as_str()), "{ctx}"),
                    None => {
                        let env_set = env.is_some_and(|e| !e.trim().is_empty());
                        assert_eq!(actor.agent().is_some(), env_set, "{ctx}");
                    }
                }
            }
        }
    }
}

#[test]
fn reserved_tags_match_whole_tags_by_prefix_only_and_keep_order() {
    assert!(is_reserved_task_tag("grant:send_email"));
    assert!(is_reserved_task_tag("  outcome:eyJ0In0  "));
    assert!(is_reserved_task_tag("auto-research"));
    assert!(!is_reserved_task_tag("my-grant:x"));
    assert!(!is_reserved_task_tag("auto-research-2"));
    assert!(!is_reserved_task_tag("billing"));
    assert_eq!(
        reserved_task_tags("billing, grant:a ,outcome:b,,auto-research,x-grant:c,grant:a"),
        vec!["grant:a", "outcome:b", "auto-research", "grant:a"]
    );
    // Order and duplicates are part of the comparison.
    assert_ne!(reserved_task_tags("outcome:a,outcome:b"), reserved_task_tags("outcome:b,outcome:a"));
    assert_ne!(reserved_task_tags("grant:a"), reserved_task_tags("grant:a,grant:a"));
}

// ── The shared check ────────────────────────────────────────────────────────

#[tokio::test]
async fn shared_check_allows_and_refuses_by_relationship() {
    let home = Home::new();
    let h = home.path();
    let task = RecordKind::Task;
    // Operator and self.
    assert!(check_record_change_allowed(h, OP, "mkt-rep", "t", task).await.is_ok());
    assert!(check_record_change_allowed(h, A("mkt-rep"), "mkt-rep", "t", task).await.is_ok());
    // Same department, manager → report, report → manager.
    let r = check_record_change_allowed(h, A("sales-rep2"), "sales-rep", "t", task).await;
    assert!(r.is_ok(), "{r:?}");
    assert!(check_record_change_allowed(h, A("sales-lead"), "sales-rep", "t", task).await.is_ok());
    assert!(check_record_change_allowed(h, A("sales-rep"), "sales-lead", "t", task).await.is_ok());
    // Unrelated departments: the reply names the owner and does not hand the
    // employee org-editing advice it cannot follow.
    let err = check_record_change_allowed(h, A("mkt-rep"), "sales-rep", "t", task)
        .await
        .unwrap_err();
    assert!(err.contains("屬於「sales-rep」"), "{err}");
    assert!(!err.contains("reports_to"), "{err}");
    assert!(!audit_rows(h, "delegation_denied").is_empty(), "the predicate audits its refusal");
    // No owner: the hint depends on the record kind.
    let err = check_record_change_allowed(h, A("sales-rep"), "  ", "tasks_x", task).await.unwrap_err();
    assert!(err.contains("tasks_claim"), "{err}");
    let err = check_record_change_allowed(h, A("sales-rep"), "", "cron_x", RecordKind::Cron)
        .await
        .unwrap_err();
    assert!(err.contains("例行工作") && !err.contains("tasks_claim"), "{err}");
    let rows = audit_rows(h, "tasks_x");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["reason"], "owner_unknown");
    // An unproven internal caller owns nothing.
    let internal = duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
    assert!(check_record_change_allowed(h, A(internal), "sales-rep", "t", task).await.is_err());
}

/// A system-sender name presented as an MCP identity is self-asserted (no
/// gateway spawn path stamps one), so it is refused rather than given the
/// system senders' unconditional reach.
#[tokio::test]
async fn system_sender_identity_is_refused_and_audited() {
    let home = Home::new();
    let h = home.path();
    for sender in duduclaw_core::SYSTEM_SENDERS {
        let r = check_record_change_allowed(h, A(sender), "mkt-rep", "sys_x", RecordKind::Task).await;
        assert!(r.is_err(), "{sender}");
    }
    let rows = audit_rows(h, "sys_x");
    assert_eq!(rows.len(), duduclaw_core::SYSTEM_SENDERS.len());
    assert!(rows.iter().all(|r| r["reason"] == "system_sender_identity"), "{rows:?}");

    // ...including where the caller would otherwise count as a party.
    let id = seed_task(h, "cron", "cron", "todo", false, "").await;
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "title": "x" }), h, A("cron")).await;
    assert!(is_error(&out), "{out}");
    let out = handle_agent_update(&serde_json::json!({ "agent_id": "mkt-rep", "icon": "x" }), h, A("dashboard")).await;
    assert!(is_error(&out), "{out}");
}

// ── tasks_update ────────────────────────────────────────────────────────────

#[tokio::test]
async fn tasks_update_requires_a_relationship_to_the_task() {
    let home = Home::new();
    let h = home.path();
    let id = seed_task(h, "sales-rep", "sales-lead", "todo", false, "").await;

    // Unrelated department: refused, nothing written.
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "priority": "urgent" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.priority, "medium");

    // Same-department peer: allowed.
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "priority": "high" }), h, A("sales-rep2")).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.priority, "high");

    // The assignee itself, the manager and an operator.
    for actor in [A("sales-rep"), A("ceo"), OP] {
        let out = handle_tasks_update(
            &serde_json::json!({ "task_id": id, "description": format!("by {}", actor.id()) }),
            h,
            actor,
        )
        .await;
        assert!(!is_error(&out), "{actor:?}: {out}");
    }
}

#[tokio::test]
async fn tasks_update_creator_may_edit_but_not_take_an_unrelated_task() {
    let home = Home::new();
    let h = home.path();
    // Created by mkt-rep, assigned to sales-rep (e.g. via a whitelist pair
    // that has since been removed).
    let id = seed_task(h, "sales-rep", "mkt-rep", "todo", false, "").await;
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "title": "creator edit" }), h, A("mkt-rep")).await;
    assert!(!is_error(&out), "{out}");
    // Taking it for itself needs a relationship with the current assignee.
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "assigned_to": "mkt-rep" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.assigned_to, "sales-rep");
    // A same-department peer may take it.
    let out =
        handle_tasks_update(&serde_json::json!({ "task_id": id, "assigned_to": "sales-rep2" }), h, A("sales-rep2")).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.assigned_to, "sales-rep2");
}

#[tokio::test]
async fn tasks_update_unassigned_task_is_refused_for_an_unrelated_agent() {
    let home = Home::new();
    let h = home.path();
    let id = seed_task(h, "", "dashboard", "todo", false, "").await;
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "title": "x" }), h, A("sales-rep")).await;
    assert!(is_error(&out), "{out}");
    let rows = audit_rows(h, "tasks_update");
    assert!(rows.iter().any(|r| r["reason"] == "owner_unknown"), "{rows:?}");
    // An operator may.
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "title": "x" }), h, OP).await;
    assert!(!is_error(&out), "{out}");
}

#[tokio::test]
async fn tasks_update_goal_title_and_description_are_frozen_for_agents() {
    let home = Home::new();
    let h = home.path();
    let goal = seed_task(h, "sales-rep", "goal:telegram", "todo", true, "").await;
    for field in ["title", "description"] {
        let out = handle_tasks_update(&serde_json::json!({ "task_id": goal, field: "rewritten goal" }), h, A("sales-rep")).await;
        assert!(is_error(&out), "{field}: {out}");
    }
    let t = load_task(h, &goal).await;
    assert_eq!(t.title, "original title");
    assert_eq!(t.description, "original description");
    let rows = audit_rows(h, "tasks_update");
    assert_eq!(rows.iter().filter(|r| r["reason"] == "goal_contract_frozen").count(), 2, "{rows:?}");
    // Other fields still work for the assignee.
    let out = handle_tasks_update(&serde_json::json!({ "task_id": goal, "priority": "high" }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");
    // A plain task's title is not frozen.
    let plain = seed_task(h, "sales-rep", "sales-lead", "todo", false, "").await;
    let out = handle_tasks_update(&serde_json::json!({ "task_id": plain, "title": "new" }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");
    // An operator may edit the goal text.
    let out = handle_tasks_update(&serde_json::json!({ "task_id": goal, "title": "operator edit" }), h, OP).await;
    assert!(!is_error(&out), "{out}");
}

#[tokio::test]
async fn tasks_update_reserved_tags_cannot_be_added_removed_or_reordered_by_agents() {
    let home = Home::new();
    let h = home.path();
    let tags = "outcome:eyJ0IjoidGV4dCJ9,billing,outcome:eyJ0IjoianNvbiJ9";
    let id = seed_task(h, "sales-rep", "sales-lead", "todo", true, tags).await;

    for (what, new_tags) in [
        ("add a grant", format!("{tags},grant:send_email")),
        ("remove an outcome", "billing,outcome:eyJ0IjoianNvbiJ9".to_string()),
        // `OutcomeSpec::from_tags` reads the first `outcome:` tag only.
        ("swap the outcomes", "outcome:eyJ0IjoianNvbiJ9,billing,outcome:eyJ0IjoidGV4dCJ9".to_string()),
        ("duplicate an outcome", format!("{tags},outcome:eyJ0IjoianNvbiJ9")),
    ] {
        let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "tags": new_tags }), h, A("sales-rep")).await;
        assert!(is_error(&out), "{what}: {out}");
    }
    assert_eq!(load_task(h, &id).await.tags, tags);
    assert_eq!(
        audit_rows(h, "tasks_update").iter().filter(|r| r["reason"] == "reserved_tag_change").count(),
        4
    );
    // Ordinary tags change freely (and may move around) while the reserved
    // tags keep their order.
    let out = handle_tasks_update(
        &serde_json::json!({ "task_id": id, "tags": "urgent, outcome:eyJ0IjoidGV4dCJ9 ,my-grant:x,outcome:eyJ0IjoianNvbiJ9" }),
        h,
        A("sales-rep"),
    )
    .await;
    assert!(!is_error(&out), "{out}");
    // An operator may change the reserved tags.
    let out = handle_tasks_update(&serde_json::json!({ "task_id": id, "tags": "grant:send_email" }), h, OP).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.tags, "grant:send_email");
}

// ── tasks_create ────────────────────────────────────────────────────────────

#[tokio::test]
async fn tasks_create_refuses_caller_supplied_reserved_tags() {
    let home = Home::new();
    let h = home.path();
    for tags in ["grant:send_email", "billing, outcome:eyJ0IjoidGV4dCJ9", "auto-research"] {
        let out = handle_tasks_create(&serde_json::json!({ "title": "t", "tags": tags }), h, A("sales-rep")).await;
        assert!(is_error(&out), "{tags}: {out}");
    }
    assert_eq!(
        audit_rows(h, "tasks_create").iter().filter(|r| r["reason"] == "reserved_tag_change").count(),
        3
    );
    // Ordinary tags, and an operator, are unaffected.
    let out = handle_tasks_create(&serde_json::json!({ "title": "t", "tags": "billing,my-grant:x" }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");
    let out = handle_tasks_create(&serde_json::json!({ "title": "t", "tags": "grant:send_email" }), h, OP).await;
    assert!(!is_error(&out), "{out}");
}

#[tokio::test]
async fn tasks_create_goal_keeps_its_server_generated_outcome_tag() {
    let home = Home::new();
    let h = home.path();
    let out = handle_tasks_create(
        &serde_json::json!({
            "title": "月報",
            "kind": "goal",
            "description": "產出月報 JSON",
            "outcome": "json:{\"type\":\"object\"}",
        }),
        h,
        A("sales-rep"),
    )
    .await;
    assert!(!is_error(&out), "{out}");
    let created: Value = serde_json::from_str(&text(&out)).unwrap();
    let id = created["task"]["id"].as_str().unwrap();
    let t = load_task(h, id).await;
    assert!(t.tags.starts_with("outcome:"), "{}", t.tags);
    assert_eq!(t.created_by, "sales-rep");
}

// ── tasks_claim ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn tasks_claim_of_another_agents_task_needs_a_relationship() {
    let home = Home::new();
    let h = home.path();
    let id = seed_task(h, "sales-rep", "sales-lead", "pending", false, "").await;
    let out = handle_tasks_claim(&serde_json::json!({ "task_id": id }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.claimed_by, None);

    let out = handle_tasks_claim(&serde_json::json!({ "task_id": id }), h, A("sales-lead")).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.claimed_by.as_deref(), Some("sales-lead"));

    // Unassigned tasks claim exactly as before, for anyone.
    let open = seed_task(h, "", "dashboard", "pending", false, "").await;
    let out = handle_tasks_claim(&serde_json::json!({ "task_id": open }), h, A("mkt-rep")).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(load_task(h, &open).await.claimed_by.as_deref(), Some("mkt-rep"));
}

// ── tasks_complete / tasks_block ────────────────────────────────────────────

#[tokio::test]
async fn tasks_complete_and_block_need_holder_or_relationship() {
    let home = Home::new();
    let h = home.path();
    let id = seed_task(h, "sales-rep", "sales-lead", "todo", false, "").await;

    let out = handle_tasks_complete(&serde_json::json!({ "task_id": id, "summary": "done" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    let out = handle_tasks_block(&serde_json::json!({ "task_id": id, "reason": "stuck" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.status, "todo");

    // The assignee may block; a same-department peer may complete.
    let out = handle_tasks_block(&serde_json::json!({ "task_id": id, "reason": "waiting on data" }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");
    let out = handle_tasks_complete(&serde_json::json!({ "task_id": id, "summary": "done" }), h, A("sales-rep2")).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(load_task(h, &id).await.status, "done");

    // Nobody owns an unassigned, unclaimed task: an agent must claim it
    // first; an operator may act on it.
    let open = seed_task(h, "", "dashboard", "todo", false, "").await;
    let out = handle_tasks_complete(&serde_json::json!({ "task_id": open }), h, A("sales-rep")).await;
    assert!(is_error(&out), "{out}");
    let out = handle_tasks_complete(&serde_json::json!({ "task_id": open }), h, OP).await;
    assert!(!is_error(&out), "{out}");
}

// ── activity_post ───────────────────────────────────────────────────────────

#[tokio::test]
async fn activity_post_on_another_agents_task_needs_a_relationship() {
    let home = Home::new();
    let h = home.path();
    let id = seed_task(h, "sales-rep", "sales-lead", "in_progress", false, "").await;
    let post = |task: &str| serde_json::json!({ "summary": "progress", "task_id": task });

    let out = handle_activity_post(&post(&id), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    for actor in [A("sales-rep"), A("sales-lead"), A("sales-rep2"), OP] {
        let out = handle_activity_post(&post(&id), h, actor).await;
        assert!(!is_error(&out), "{actor:?}: {out}");
    }
    // The creator counts as a party.
    let created = seed_task(h, "sales-rep", "mkt-rep", "todo", false, "").await;
    let out = handle_activity_post(&post(&created), h, A("mkt-rep")).await;
    assert!(!is_error(&out), "{out}");
    // An unknown task id cannot be checked, so it is refused.
    let out = handle_activity_post(&post("no-such-task"), h, A("sales-rep")).await;
    assert!(is_error(&out), "{out}");
    // Activity without a task is unaffected.
    let out = handle_activity_post(&serde_json::json!({ "summary": "hello" }), h, A("mkt-rep")).await;
    assert!(!is_error(&out), "{out}");
}

// ── cron management ─────────────────────────────────────────────────────────

async fn seed_cron(home: &std::path::Path, name: &str, agent: &str) -> String {
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let row = duduclaw_gateway::cron_store::CronTaskRow::new(
        id.clone(),
        name.into(),
        agent.into(),
        "0 9 * * *".into(),
        "original prompt".into(),
    );
    store.insert(&row).await.unwrap();
    id
}

async fn load_cron(home: &std::path::Path, id: &str) -> Option<duduclaw_gateway::cron_store::CronTaskRow> {
    duduclaw_gateway::cron_store::CronStore::open(home).unwrap().get(id).await.unwrap()
}

#[tokio::test]
async fn cron_management_is_limited_to_the_owner_and_related_agents() {
    let home = Home::new();
    let h = home.path();
    let id = seed_cron(h, "daily-report", "sales-rep").await;

    // Unrelated department: update / pause / delete / run all refused.
    let out = handle_update_cron_task(&serde_json::json!({ "id": id, "task": "exfiltrate" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    let out = handle_pause_cron_task(&serde_json::json!({ "id": id, "enabled": false }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    let out = handle_run_cron_task(&serde_json::json!({ "name": "daily-report" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    let out = handle_delete_cron_task(&serde_json::json!({ "name": "daily-report" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    let row = load_cron(h, &id).await.expect("row must survive");
    assert_eq!(row.task, "original prompt");
    assert!(row.enabled);

    // The owner (by unique name) and its manager (by id).
    let out = handle_update_cron_task(&serde_json::json!({ "name": "daily-report", "task": "new prompt" }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(load_cron(h, &id).await.unwrap().task, "new prompt");
    let out = handle_pause_cron_task(&serde_json::json!({ "id": id, "enabled": false }), h, A("sales-lead")).await;
    assert!(!is_error(&out), "{out}");
    assert!(!load_cron(h, &id).await.unwrap().enabled);
    // An operator.
    let out = handle_delete_cron_task(&serde_json::json!({ "id": id }), h, OP).await;
    assert!(!is_error(&out), "{out}");
    assert!(load_cron(h, &id).await.is_none());
}

#[tokio::test]
async fn cron_name_shared_by_several_rows_is_refused_with_candidate_ids() {
    let home = Home::new();
    let h = home.path();
    let mine = seed_cron(h, "standup", "sales-rep").await;
    let theirs = seed_cron(h, "standup", "mkt-rep").await;

    for (tool, out) in [
        ("update", handle_update_cron_task(&serde_json::json!({ "name": "standup", "task": "x" }), h, A("sales-rep")).await),
        ("pause", handle_pause_cron_task(&serde_json::json!({ "name": "standup", "enabled": false }), h, A("sales-rep")).await),
        ("delete", handle_delete_cron_task(&serde_json::json!({ "name": "standup" }), h, A("sales-rep")).await),
        ("run", handle_run_cron_task(&serde_json::json!({ "name": "standup" }), h, A("sales-rep")).await),
    ] {
        assert!(is_error(&out), "{tool}: {out}");
        let t = text(&out);
        assert!(t.contains(&mine) && t.contains(&theirs), "{tool}: {t}");
    }
    // Even an operator must disambiguate.
    let out = handle_delete_cron_task(&serde_json::json!({ "name": "standup" }), h, OP).await;
    assert!(is_error(&out), "{out}");
    assert!(load_cron(h, &mine).await.is_some() && load_cron(h, &theirs).await.is_some());

    // By id, the owner acts on its own row only.
    let out = handle_pause_cron_task(&serde_json::json!({ "id": mine, "enabled": false }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");
    assert!(!load_cron(h, &mine).await.unwrap().enabled);
    assert!(load_cron(h, &theirs).await.unwrap().enabled);
    let out = handle_pause_cron_task(&serde_json::json!({ "id": theirs, "enabled": false }), h, A("sales-rep")).await;
    assert!(is_error(&out), "{out}");
}

#[tokio::test]
async fn cron_row_owned_by_default_resolves_to_the_main_agent() {
    let home = Home::new();
    let h = home.path();
    // No main agent: the owner cannot be determined → refused.
    let id = seed_cron(h, "morning", "default").await;
    let out = handle_update_cron_task(&serde_json::json!({ "id": id, "task": "x" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    assert!(audit_rows(h, "update_cron_task").iter().any(|r| r["reason"] == "owner_unknown"));

    make_main(h, "sales-lead");
    // The main agent itself and its report may change it; another
    // department may not.
    let out = handle_update_cron_task(&serde_json::json!({ "id": id, "task": "by main" }), h, A("sales-lead")).await;
    assert!(!is_error(&out), "{out}");
    let out = handle_pause_cron_task(&serde_json::json!({ "id": id, "enabled": false }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");
    let out = handle_update_cron_task(&serde_json::json!({ "id": id, "task": "x" }), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    assert_eq!(load_cron(h, &id).await.unwrap().task, "by main");
}

// ── create_reminder ─────────────────────────────────────────────────────────

#[tokio::test]
async fn create_reminder_for_another_agent_needs_a_relationship() {
    let home = Home::new();
    let h = home.path();
    let at = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let params = |agent: Option<&str>| {
        let mut p = serde_json::json!({
            "time": at,
            "message": "ping",
            "channel": "telegram",
            "chat_id": "12345",
        });
        if let Some(a) = agent {
            p["agent_id"] = serde_json::json!(a);
        }
        p
    };
    let out = handle_create_reminder(&params(Some("sales-rep")), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    let t = text(&out);
    assert!(t.contains("提醒") && !t.contains("tasks_claim"), "{t}");
    let out = handle_create_reminder(&params(Some("sales-rep")), h, A("sales-lead")).await;
    assert!(!is_error(&out), "{out}");
    let out = handle_create_reminder(&params(Some("mkt-rep")), h, A("mkt-rep")).await;
    assert!(!is_error(&out), "{out}");
    let out = handle_create_reminder(&params(None), h, A("mkt-rep")).await;
    assert!(!is_error(&out), "omitted agent_id means the caller: {out}");
    let out = handle_create_reminder(&params(Some("sales-rep")), h, OP).await;
    assert!(!is_error(&out), "{out}");
}

// ── agent_update self-authority ─────────────────────────────────────────────

fn budget_of(home: &std::path::Path, agent: &str) -> u64 {
    let cfg: duduclaw_core::types::AgentConfig = toml::from_str(
        &fs::read_to_string(home.join("agents").join(agent).join("agent.toml")).unwrap(),
    )
    .unwrap();
    cfg.budget.monthly_limit_cents
}

#[tokio::test]
async fn agent_update_refuses_self_authority_changes() {
    let home = Home::new();
    let h = home.path();
    for (key, value) in [
        ("budget_cents", serde_json::json!(999_999)),
        ("db_sources_add", serde_json::json!("crm")),
        ("role", serde_json::json!("main")),
        ("reports_to", serde_json::json!("ceo")),
    ] {
        let out = handle_agent_update(&serde_json::json!({ "agent_id": "sales-rep", key: value }), h, A("sales-rep")).await;
        assert!(is_error(&out), "{key}: {out}");
    }
    assert_eq!(budget_of(h, "sales-rep"), 1000);
    let rows = audit_rows(h, "agent_authority_refused");
    // `reports_to` to a non-subtree parent may be refused by the placement
    // gate first; the other three always reach the self-authority guard.
    assert!(rows.len() >= 3, "{rows:?}");
    assert_eq!(rows[0]["reason"], "self_authority_change");

    // Ordinary self-edits are unchanged.
    let out = handle_agent_update(&serde_json::json!({ "agent_id": "sales-rep", "display_name": "小業" }), h, A("sales-rep")).await;
    assert!(!is_error(&out), "{out}");

    // A manager may set a report's budget; an operator may set anyone's.
    let out = handle_agent_update(&serde_json::json!({ "agent_id": "sales-rep", "budget_cents": 2000 }), h, A("sales-lead")).await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(budget_of(h, "sales-rep"), 2000);
    let out = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "budget_cents": 3000 }),
        h,
        RecordActor::Operator("sales-rep"),
    )
    .await;
    assert!(!is_error(&out), "{out}");
    assert_eq!(budget_of(h, "sales-rep"), 3000);
}

#[tokio::test]
async fn agent_update_unproven_internal_caller_cannot_edit_the_default_agent() {
    // Internal key with no DUDUCLAW_AGENT_ID: the actor is the internal
    // client id, which commands nobody, so the config default agent's
    // settings are out of reach.
    let home = Home::new();
    let h = home.path();
    let internal = duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
    let out = handle_agent_update(&serde_json::json!({ "agent_id": "ceo", "budget_cents": 1 }), h, A(internal)).await;
    assert!(is_error(&out), "{out}");
    assert_eq!(budget_of(h, "ceo"), 1000);
}

// ── P2-A round 3 ────────────────────────────────────────────────────────────

/// S-M2: a sub-task can only be hung under a task the caller is related to.
#[tokio::test]
async fn tasks_create_under_a_parent_needs_a_relationship() {
    let home = Home::new();
    let h = home.path();
    let parent = seed_task(h, "sales-rep", "sales-lead", "in_progress", false, "").await;
    let child = |p: &str| serde_json::json!({ "title": "sub", "parent_task_id": p });

    let out = handle_tasks_create(&child(&parent), h, A("mkt-rep")).await;
    assert!(is_error(&out), "{out}");
    for actor in [A("sales-rep"), A("sales-lead"), OP] {
        let out = handle_tasks_create(&child(&parent), h, actor).await;
        assert!(!is_error(&out), "{actor:?}: {out}");
    }
    let out = handle_tasks_create(&child("no-such-task"), h, A("sales-rep")).await;
    assert!(is_error(&out), "{out}");
}

/// E-M4: an employee cannot post Activity rows that pose as the system's
/// responsibility or stop records.
#[tokio::test]
async fn activity_post_refuses_reserved_event_types() {
    let home = Home::new();
    let h = home.path();
    for kind in ["responsibility.notified", "task.stop_reconciled"] {
        let out = handle_activity_post(
            &serde_json::json!({ "summary": "x", "event_type": kind }),
            h,
            A("sales-rep"),
        )
        .await;
        assert!(is_error(&out), "{kind}: {out}");
    }
}
