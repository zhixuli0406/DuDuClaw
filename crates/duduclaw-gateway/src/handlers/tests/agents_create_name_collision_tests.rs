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

/// No `agents/<x>` and no staging leftovers (`.creating-*`) for `name`.
fn assert_nothing_published(home: &std::path::Path, name: &str) {
    let agents = home.join("agents");
    assert!(!agents.join(name).exists(), "agents/{name} must not exist");
    let leftovers: Vec<String> = std::fs::read_dir(&agents)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.starts_with(duduclaw_core::org_store::AGENT_STAGING_PREFIX))
                .collect()
        })
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
}

fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error, .. } => {
            error.as_ref().and_then(|v| v.as_str()).unwrap_or("").to_string()
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// RM-027 round 4, finding 1: the org lock is taken at the start of the
/// creation — while it is held elsewhere, the new employee is not visible
/// at all (no directory, no staging dir, no row); once released the create
/// publishes the directory together with its authority row.
#[tokio::test]
async fn agents_create_waits_for_the_org_lock_before_anything_is_visible() {
    let home = tempfile::tempdir().unwrap();
    write_agent_toml(&home.path().join("agents").join("ceo"), "ceo", "");
    duduclaw_core::org_store::seed_if_absent(home.path()).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let held = duduclaw_core::org_store::lock_org_mutations(home.path()).unwrap();
    let path = home.path().to_path_buf();
    let checker = async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_nothing_published(&path, "waiter");
        assert!(duduclaw_core::org_store::load(&path).get("waiter").is_none());
        drop(held);
    };
    let (frame, ()) = tokio::join!(
        handler.handle_agents_create(json!({ "name": "waiter", "display_name": "Waiter", "reports_to": "ceo" })),
        checker
    );
    assert!(matches!(frame, WsFrame::Response { ok: true, .. }), "{frame:?}");
    assert!(home.path().join("agents/waiter/agent.toml").is_file());
    assert!(home.path().join("agents/waiter/SOUL.md").is_file());
    assert_eq!(duduclaw_core::org_store::load(home.path()).get("waiter").unwrap().reports_to, "ceo");
}

/// An unusable authority (strict parser refusal) fails the whole create —
/// no directory remains and the bytes are untouched. Before, the create
/// warned, reported success and left the employee governed by its mirror.
#[tokio::test]
async fn agents_create_fails_when_the_authority_refuses_and_leaves_no_directory() {
    let home = tempfile::tempdir().unwrap();
    write_agent_toml(&home.path().join("agents").join("ceo"), "ceo", "");
    duduclaw_core::org_store::seed_if_absent(home.path()).unwrap();
    let store = duduclaw_core::org_store::org_store_path(home.path());
    std::fs::write(&store, "schema = 1\n[agents.ceo]\nreports_to = \"\"\n").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_create(json!({ "name": "orphan", "display_name": "Orphan", "reports_to": "ceo" }))
        .await;
    let msg = error_text(&frame);
    assert!(msg.contains("AI 員工沒有建立"), "{msg}");
    assert_nothing_published(home.path(), "orphan");
    assert_eq!(std::fs::read_to_string(&store).unwrap(), "schema = 1\n[agents.ceo]\nreports_to = \"\"\n");
}

/// A create whose placement would close a loop in the authority (a stale
/// row says the supervisor reports to the new id) is refused by the cycle
/// check, with nothing published.
#[tokio::test]
async fn agents_create_refuses_a_placement_that_closes_a_cycle() {
    let home = tempfile::tempdir().unwrap();
    write_agent_toml(&home.path().join("agents").join("ceo"), "ceo", "");
    duduclaw_core::org_store::seed_if_absent(home.path()).unwrap();
    duduclaw_core::org_store::with_org_lock(home.path(), |org| {
        duduclaw_core::org_store::upsert(org, "ceo", duduclaw_core::OrgEntry::new("ghost", ""))
    })
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_create(json!({ "name": "ghost", "display_name": "Ghost", "reports_to": "ceo" }))
        .await;
    let msg = error_text(&frame);
    assert!(msg.contains("循環"), "{msg}");
    assert_nothing_published(home.path(), "ghost");
    assert!(duduclaw_core::org_store::load(home.path()).get("ghost").is_none());
}

#[tokio::test]
async fn agents_create_evolution_is_explicitly_enabled() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler.handle_agents_create(json!({ "name": "learner", "display_name": "Learner" })).await;
    assert!(matches!(frame, WsFrame::Response { ok: true, .. }), "{frame:?}");
    let dir = home.path().join("agents/learner");
    let raw = std::fs::read_to_string(dir.join("agent.toml")).unwrap();
    let config: toml::Table = raw.parse().unwrap();
    assert_eq!(config["evolution"]["enabled"].as_bool(), Some(true));
    assert_eq!(config["evolution"]["gvu_enabled"].as_bool(), Some(true));
    assert!(crate::gvu::trigger::agent_gvu_enabled(&dir));
}
