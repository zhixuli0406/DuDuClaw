//! Unit tests for [`super`], moved verbatim out of the former `ephemeral.rs`.
//!
//! Shared fixtures live here; the cases are split across the sibling
//! files for size only.

mod gc_cases;
mod ids_cases;
mod role_member_cases;
mod scaffold_cases;

use super::*;

fn caps(allowed: &[&str], denied: &[&str]) -> CapabilitiesConfig {
    let mut c = CapabilitiesConfig::default();
    c.allowed_tools = allowed.iter().map(|s| s.to_string()).collect();
    c.denied_tools = denied.iter().map(|s| s.to_string()).collect();
    c
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn write_parent(home: &Path, name: &str, extra: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!(
            r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = "@{name}"
reports_to = ""
icon = "X"

[model]
preferred = "parent-preferred-model"
fallback = ""
account_pool = []
utility = "parent-utility-model"
standard = "parent-standard-model"

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

{extra}
"#
        ),
    )
    .unwrap();
}

/// Read a scaffold's `.mcp.json` as raw JSON.
fn member_mcp_json(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(".mcp.json")).unwrap()).unwrap()
}

fn queued_ephemeral_payload(
    parent: &str,
    instruction: &str,
    context: &str,
) -> serde_json::Value {
    serde_json::json!({
        "parent": parent,
        "instruction": instruction,
        "tools": ["Read"],
        "tier": "standard",
        "context": context,
        "origin": parent,
        "outgoing_depth": 1,
        "incoming_hop": 0,
    })
}

// ─────────────────────────────────────────────────────────────────────
// Team role members (WP-2)
// ─────────────────────────────────────────────────────────────────────

fn role_spec(parent: &str, role: Role, runtime: &str, model: &str) -> RoleMemberSpec {
    RoleMemberSpec {
        parent_agent: parent.to_string(),
        task_id: "task-abc".to_string(),
        round: 2,
        role,
        runtime: runtime.to_string(),
        model: model.to_string(),
        effort: None,
        instruction: "You verify the executor's output. 只審查，不改稿。".to_string(),
        tools: strs(&["Read"]),
    }
}

/// Read the member's `agent.toml` back as a raw table (the scaffold writes
/// it through the TOML serializer, so this is the on-disk truth).
fn member_toml(dir: &Path) -> toml::Value {
    std::fs::read_to_string(dir.join("agent.toml"))
        .unwrap()
        .parse()
        .unwrap()
}

/// No scaffold directory was left behind by a rejected request.
fn assert_no_scaffold(home: &Path) {
    let root = ephemeral_root(home);
    let count = std::fs::read_dir(&root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0);
    assert_eq!(count, 0, "a rejected request must not scaffold anything");
}

