//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! WP22 T4 — `agents.create` must reject a `name` that collides with any
//! *existing* agent's directory name or `[agent] name` field, mirroring
//! MCP `create_agent` (`crates/duduclaw-cli/src/mcp.rs`). Without this,
//! `map.insert` in `AgentRegistry::scan` (last-wins) or the delegation
//! `name → dir` resolver can silently pick the wrong one of two agents
//! sharing a registry key.
use super::*;

/// A full, deserializable `agent.toml` — the collision check reads
/// through `self.registry`, which only ever indexes agents that parse
/// cleanly into `AgentConfig`. An `[agent]`-only stub (as `agents.create`
/// itself never writes) would silently fail to load and vanish from
/// `reg.list()`, defeating the very fixture meant to exercise the check.
fn write_agent_toml(dir: &std::path::Path, name: &str, reports_to: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!(
            r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = "@{name}"
reports_to = "{reports_to}"
icon = "🤖"
department = ""

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["*"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true
"#
        ),
    )
    .unwrap();
}

/// Plain case: the new agent's directory name matches an existing
/// directory exactly. `create_dir` would already fail this, but the
/// name-collision check runs first and must give the same zh-TW message.
#[tokio::test]
async fn rejects_name_matching_existing_directory() {
    let home = tempfile::tempdir().unwrap();
    write_agent_toml(&home.path().join("agents").join("ceo"), "ceo", "");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_create(json!({ "name": "ceo", "display_name": "另一個 CEO" }))
        .await;
    assert!(
        !matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );
}

/// The gap WP22 T4 closes: an existing agent's directory name and its
/// `[agent] name` field have drifted apart. A new `agents.create` whose
/// `name` matches that *field* — not any directory — must still be
/// refused, even though `agents/<name>` does not exist yet.
#[tokio::test]
async fn rejects_name_matching_existing_name_field_in_other_dir() {
    let home = tempfile::tempdir().unwrap();
    write_agent_toml(
        &home.path().join("agents").join("legacy-sales"),
        "sales-alias",
        "",
    );
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_create(json!({ "name": "sales-alias", "display_name": "新業務" }))
        .await;
    match frame {
        WsFrame::Response {
            ok: false, error, ..
        } => {
            let msg = error.as_ref().and_then(|v| v.as_str()).unwrap_or("");
            assert!(msg.contains("已有同名的 AI 員工"), "{msg}");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(
        !home.path().join("agents").join("sales-alias").exists(),
        "a rejected create must not scaffold a directory"
    );
}

/// Control: a fresh, non-colliding name still creates normally.
#[tokio::test]
async fn allows_non_colliding_name() {
    let home = tempfile::tempdir().unwrap();
    write_agent_toml(&home.path().join("agents").join("ceo"), "ceo", "");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_create(json!({ "name": "brand-new-agent", "display_name": "全新員工" }))
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );
    assert!(home.path().join("agents").join("brand-new-agent").exists());
}
