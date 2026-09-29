//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! WP22 T1 follow-up — confirms the dashboard off-board paths
//! (`agents.remove` / `agents.archive`, both driven by
//! `offboard_agent_toml`) leave the `org.toml` authority record alone.
//!
//! Unlike MCP `agent_remove` (which relocates the agent directory to
//! `_trash` and only then calls `org_store::remove`, and the ephemeral GC
//! sweep, which does the same after `remove_dir_all`), the dashboard
//! handlers never move or delete the agent's directory — `agents.remove`
//! is a **soft** delete (`status = "deleted"`, `data_retained: true`,
//! see the RPC catalog entry "Soft-delete an agent (hidden, data
//! retained)") and `agents.archive` is explicitly a "recoverable
//! off-board". The agent keeps existing on disk with a live, writable
//! `agent.toml` in both cases, so clearing its `org_store` entry here
//! would reopen exactly the hole WP22 T1 closed: a still-live agent
//! would fall back to being governed by its own mirror again (see the
//! module doc on `duduclaw_core::org_store`, "Authority rule"). These
//! tests lock in that the entry survives both RPCs, so a future edit
//! that "fixes" the perceived orphan-record gap by adding
//! `org_store::remove` to these handlers gets caught immediately.
use super::*;

fn frame_ok(f: &WsFrame) -> bool {
    matches!(f, WsFrame::Response { ok: true, .. })
}

fn frame_data(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response { payload, .. } => payload.clone().unwrap_or(Value::Null),
        other => panic!("expected response, got {other:?}"),
    }
}

/// A full, deserializable, non-main `agent.toml` — mirrors the fixture
/// in `agents_create_name_collision_tests::write_agent_toml`. The RPCs
/// under test parse the registry entry (for the main-agent guard) and
/// then rewrite this same file via `update_agent_toml`, so it must be a
/// complete, valid `AgentConfig`.
fn write_offboardable_agent_toml(dir: &std::path::Path, name: &str) {
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
reports_to = "root-lead"
icon = "🤖"
department = "銷售部"

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

/// `agents.remove` (dashboard soft-delete): the agent directory and its
/// `agent.toml` are left in place — only `status` flips to `"deleted"`
/// and the freeze kill-switch trips. The `org_store` entry seeded before
/// the call must still be there afterwards, byte-identical.
#[tokio::test]
async fn remove_preserves_org_store_entry() {
    let home = tempfile::tempdir().unwrap();
    let agent_id = "org-remove-test";
    write_offboardable_agent_toml(&home.path().join("agents").join(agent_id), agent_id);

    let entry = duduclaw_core::org_store::OrgEntry::new("root-lead", "銷售部");
    duduclaw_core::org_store::upsert(home.path(), agent_id, entry.clone()).unwrap();
    assert!(
        duduclaw_core::org_store::load(home.path()).contains(agent_id),
        "fixture sanity: entry must exist before the call"
    );

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_agents_remove(json!({ "agent_id": agent_id }))
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(
        frame_data(&frame)["status"].as_str(),
        Some("deleted"),
        "agents.remove is a soft-delete, not a hard delete"
    );

    // The directory (and its agent.toml) is untouched — this is the
    // "data_retained: true" contract, not a relocation to _trash.
    assert!(
        home.path()
            .join("agents")
            .join(agent_id)
            .join("agent.toml")
            .exists(),
        "soft-delete must not remove the agent directory"
    );

    let store = duduclaw_core::org_store::load(home.path());
    assert!(
        store.contains(agent_id),
        "org_store entry must survive agents.remove — the agent still \
             exists on disk with a writable agent.toml, so clearing the \
             authority record would hand it back control of its own \
             reports_to/department"
    );
    assert_eq!(store.get(agent_id), Some(&entry));
}

/// `agents.archive` is explicitly documented as a "recoverable
/// off-board" — the org_store entry must survive it too.
#[tokio::test]
async fn archive_preserves_org_store_entry() {
    let home = tempfile::tempdir().unwrap();
    let agent_id = "org-archive-test";
    write_offboardable_agent_toml(&home.path().join("agents").join(agent_id), agent_id);

    let entry = duduclaw_core::org_store::OrgEntry::new("root-lead", "銷售部");
    duduclaw_core::org_store::upsert(home.path(), agent_id, entry.clone()).unwrap();

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_agents_archive(json!({ "agent_id": agent_id }))
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(frame_data(&frame)["status"].as_str(), Some("archived"));

    let store = duduclaw_core::org_store::load(home.path());
    assert!(
        store.contains(agent_id),
        "org_store entry must survive agents.archive"
    );
    assert_eq!(store.get(agent_id), Some(&entry));
}
