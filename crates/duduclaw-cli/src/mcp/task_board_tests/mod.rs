use super::*;
use std::fs;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("duduclaw-tb-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parse_ok(value: &Value) -> Value {
    assert!(
        !value
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "tool returned error: {value}"
    );
    let text = value["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

/// WP21 T5: minimal `agent.toml` fixture so cross-agent `tasks_create` /
/// `tasks_update` in this module's pre-existing tests still pass the
/// department×hierarchy delegation gate — `name` reports to `reports_to`,
/// which under the default `department` policy is enough (rule 3, sender
/// is an ancestor of target). Kept deliberately separate from the
/// `create_test_agent` fixture in `mod tests` (private to that module) —
/// this one only needs to prove a `reports_to` edge, not department peers.
fn create_test_agent(agents_dir: &std::path::Path, name: &str, reports_to: &str) {
    create_test_agent_in_dept(agents_dir, name, reports_to, "");
}

/// WP21 T5/T6: same fixture, plus `[agent] department` — the second axis
/// the delegation predicate and the read-side visibility filter both read.
fn create_test_agent_in_dept(
    agents_dir: &std::path::Path,
    name: &str,
    reports_to: &str,
    department: &str,
) {
    let agent_dir = agents_dir.join(name);
    fs::create_dir_all(&agent_dir).unwrap();
    let toml_content = format!(
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
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
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
    fs::write(agent_dir.join("agent.toml"), toml_content).unwrap();
}

// ── WP21 T5: C3 delegation gate on tasks_create / tasks_update ───────

async fn seed_plan(home: &std::path::Path) -> (String, String, String) {
    // Seed a plan for agent "agnes" with one agent step + one user step,
    // through the same store the MCP handlers open.
    let store = duduclaw_gateway::task_store::TaskStore::open(home).unwrap();
    let plan = duduclaw_gateway::task_store::PlanRow::new(
        "plan-1".into(),
        "Launch week".into(),
        "agnes".into(),
        "louis".into(),
    );
    store.insert_plan(&plan).await.unwrap();
    let agent_step = store
        .add_plan_step(
            "plan-1",
            "st-agent",
            "draft the release notes",
            "agent",
            "agnes",
            None,
        )
        .await
        .unwrap();
    let user_step = store
        .add_plan_step(
            "plan-1",
            "st-user",
            "approve the copy",
            "user",
            "louis",
            None,
        )
        .await
        .unwrap();
    (plan.id, agent_step.id, user_step.id)
}

mod part1;
mod part2;
