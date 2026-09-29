use super::*;
use std::fs;

/// Create a temporary test directory that is cleaned up on drop.
struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("duduclaw-test-{}", uuid::Uuid::new_v4()));
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

// ── WP6: channel receipt for routine creation ────────────────

/// Create a minimal agent directory for testing.
/// Create the `message_queue` schema in `<home>/message_queue.db` for
/// tests. Mirrors `duduclaw-gateway::message_queue::MessageQueue::init_schema`
/// including the v1.8.16 `reply_channel` column, so `send_to_agent`'s
/// INSERT has a table to write into.
///
/// In production, the gateway creates this table on startup via
/// `MessageQueue::open`. Tests that bypass the gateway need to set it
/// up themselves since MCP subprocesses assume the schema already
/// exists.
fn init_message_queue_schema(home: &std::path::Path) {
    let db_path = home.join("message_queue.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open message_queue.db");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_queue (
                 id              TEXT PRIMARY KEY,
                 sender          TEXT NOT NULL,
                 target          TEXT NOT NULL,
                 payload         TEXT NOT NULL,
                 status          TEXT NOT NULL DEFAULT 'pending',
                 retry_count     INTEGER NOT NULL DEFAULT 0,
                 delegation_depth INTEGER NOT NULL DEFAULT 0,
                 origin_agent    TEXT,
                 sender_agent    TEXT,
                 error           TEXT,
                 response        TEXT,
                 created_at      TEXT NOT NULL,
                 acked_at        TEXT,
                 completed_at    TEXT,
                 reply_channel   TEXT
             );",
    )
    .expect("init message_queue schema");
}

fn create_test_agent(agents_dir: &std::path::Path, name: &str, reports_to: &str) {
    create_test_agent_in_dept(agents_dir, name, reports_to, "");
}

/// WP21: same fixture, plus `[agent] department` — the second axis the
/// delegation predicate reads.
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

// ── WP21 C2: the delegation gate at the MCP front door ───────────
//
// These replace the pre-WP21 `check_supervisor_relation` tests. The rule
// itself is unit-tested in `duduclaw-core::delegation_policy`; what is
// proven here is the *wiring* — that agent.toml on disk (both `reports_to`
// and `department`) reaches the core predicate correctly, including the two
// shapes the old direct-parent-only check got wrong: skip-level command and
// same-department peers.

/// ceo ─ sales-lead ─ sales-rep / sales-rep2   (department 業務)
///     └ mkt-lead   ─ mkt-rep                  (department 行銷)
/// plus two department-less siblings under ceo.
fn write_delegation_org(agents_dir: &std::path::Path) {
    create_test_agent(agents_dir, "ceo", "");
    create_test_agent_in_dept(agents_dir, "sales-lead", "ceo", "業務");
    create_test_agent_in_dept(agents_dir, "sales-rep", "sales-lead", "業務");
    create_test_agent_in_dept(agents_dir, "sales-rep2", "sales-lead", "業務");
    create_test_agent_in_dept(agents_dir, "mkt-lead", "ceo", "行銷");
    create_test_agent_in_dept(agents_dir, "mkt-rep", "mkt-lead", "行銷");
    create_test_agent(agents_dir, "researcher", "ceo");
    create_test_agent(agents_dir, "writer", "ceo");
}

fn delegation_home() -> TempDir {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_delegation_org(&agents_dir);
    tmp
}

/// Rewrite one agent's `[agent]` org fields the way a tampering agent
/// would (Bash redirect, or a runtime with no `PreToolUse` hook).
fn tamper_agent_toml(home: &std::path::Path, agent: &str, reports_to: &str, department: &str) {
    let path = home.join("agents").join(agent).join("agent.toml");
    let mut table = fs::read_to_string(&path)
        .unwrap()
        .parse::<toml::Table>()
        .unwrap();
    let section = table.get_mut("agent").unwrap().as_table_mut().unwrap();
    section.insert("reports_to".into(), toml::Value::String(reports_to.into()));
    section.insert("department".into(), toml::Value::String(department.into()));
    fs::write(&path, toml::to_string_pretty(&table).unwrap()).unwrap();
}

/// Pull the bare agent ids out of `list_agents`' text table via the
/// `(name)` marker each line renders — precise enough that "sales-rep"
/// cannot match the "sales-rep2" line.
fn listed_names(res: &Value) -> Vec<String> {
    let text = res["content"][0]["text"].as_str().unwrap_or("");
    text.lines()
        .filter_map(|line| {
            let start = line.find('(')?;
            let end = line[start..].find(')')? + start;
            Some(line[start + 1..end].to_string())
        })
        .collect()
}

/// `delegation_home()` plus a `config.toml` declaring two real sources.
fn db_grant_home() -> TempDir {
    let tmp = delegation_home();
    fs::write(
        tmp.path().join("config.toml"),
        "[db_sources.crm]\n\
             label = \"客戶 CRM\"\n\
             driver = \"sqlite\"\n\
             url = \"/tmp/duduclaw-test-crm.sqlite\"\n\
             allowed_tables = [\"customers\"]\n\
             \n\
             [db_sources.hr]\n\
             driver = \"sqlite\"\n\
             url = \"/tmp/duduclaw-test-hr.sqlite\"\n\
             allowed_tables = [\"*\"]\n",
    )
    .unwrap();
    tmp
}

fn read_db_grants(home: &std::path::Path, agent: &str) -> Vec<String> {
    let path = home.join("agents").join(agent).join("agent.toml");
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    cfg.capabilities.db_sources
}

fn update_text(res: &serde_json::Value) -> String {
    res["content"][0]["text"].as_str().unwrap_or("").to_string()
}

// Mutex to serialize env-var-mutating tests (env is process-global).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn clear_delegation_env() {
    unsafe {
        std::env::remove_var(duduclaw_core::ENV_DELEGATION_DEPTH);
        std::env::remove_var(duduclaw_core::ENV_DELEGATION_ORIGIN);
        std::env::remove_var(duduclaw_core::ENV_DELEGATION_SENDER);
    }
}

/// Shared driver for the end-to-end BUG-1 regression tests below:
/// dispatches one state-changing tool call (`pairing_manage action=list`,
/// which deterministically returns a fixed reply against a fresh/empty
/// `access_control.json` — no external dependency, see
/// `state_changing_call_result_text_activates_grounding_evidence` in
/// `audit_input_tests`) through the REAL `handle_tools_call`
/// dispatch path (not a hand-written fixture) and returns the resulting
/// `tool_calls.jsonl` line. Caller sets/clears `DUDUCLAW_DELEGATION_SENDER`
/// under `ENV_LOCK` around the call.
async fn dispatch_pairing_manage_list_and_read_audit_line(default_agent: &str) -> String {
    let tmp = TempDir::new();
    let memory = SqliteMemoryEngine::new(&tmp.path().join("memory.db")).expect("memory engine");
    let odoo: OdooState = std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let ns = crate::mcp_namespace::NamespaceContext {
        write_namespace: format!("internal/{default_agent}"),
        read_namespaces: vec![format!("internal/{default_agent}")],
    };
    let quota = crate::mcp_memory_quota::DailyQuota::new();
    let http = reqwest::Client::new();

    let params = serde_json::json!({
        "name": "pairing_manage",
        "arguments": { "action": "list" }
    });
    let _ = handle_tools_call(
        &serde_json::json!(1),
        &params,
        tmp.path(),
        &http,
        &memory,
        default_agent,
        &odoo,
        &ns,
        &quota,
        "default",
        true,
    )
    .await;

    let body = fs::read_to_string(tmp.path().join("tool_calls.jsonl"))
        .expect("audit record must be written for a state-changing tool");
    body.lines()
        .find(|l| l.contains("pairing_manage"))
        .expect("pairing_manage audit line")
        .to_string()
}

/// F2: overwrite an agent's `[agent].status` in its fixture agent.toml.
fn set_agent_status(agents_dir: &std::path::Path, name: &str, status: &str) {
    let toml_path = agents_dir.join(name).join("agent.toml");
    let content = fs::read_to_string(&toml_path).unwrap();
    let updated = content.replace("status = \"active\"", &format!("status = \"{status}\""));
    assert_ne!(
        updated, content,
        "fixture must contain status = \"active\" to replace"
    );
    fs::write(&toml_path, updated).unwrap();
}

/// Like `create_test_agent` but with a restricted `allowed_tools` list
/// (edits the empty allowlist inside the fixture's existing
/// `[capabilities]` section).
fn create_test_agent_with_caps(
    agents_dir: &std::path::Path,
    name: &str,
    allowed_tools: &[&str],
) {
    create_test_agent(agents_dir, name, "");
    let toml_path = agents_dir.join(name).join("agent.toml");
    let content = fs::read_to_string(&toml_path).unwrap();
    let list = allowed_tools
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let updated = content.replace("allowed_tools = []", &format!("allowed_tools = [{list}]"));
    assert_ne!(
        updated, content,
        "fixture must contain the empty allowlist to replace"
    );
    fs::write(&toml_path, updated).unwrap();
}

mod part1;
mod part2;
mod part3;
mod part4;
mod part5;
