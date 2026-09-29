//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// WP-A — `agents.update` / `agents.inspect` side of the per-agent
/// `[capabilities] db_sources` grants. The grant *matrix* RPCs
/// (`db_sources.grants.*`) are tested in `crate::db_source_grants`; these
/// cover the single-agent editor path, which is the one that has to reject an
/// id no `config.toml [db_sources.*]` block declares.
use super::*;

/// A complete, registry-scannable agent.toml.
fn seed_agent(home: &std::path::Path, id: &str, grants: &[&str]) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let caps = if grants.is_empty() {
        "[capabilities]\ncomputer_use = false\n".to_string()
    } else {
        let list = grants
            .iter()
            .map(|g| format!("\"{g}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[capabilities]\ncomputer_use = false\ndb_sources = [{list}]\n")
    };
    let toml = format!(
        r#"[agent]
name = "{id}"
display_name = "{id}"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = "🤖"

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

{caps}"#
    );
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

fn seed_config(home: &std::path::Path, names: &[&str]) {
    let mut out = String::new();
    for name in names {
        out.push_str(&format!(
            "[db_sources.{name}]\ndriver = \"sqlite\"\nurl = \"/tmp/{name}.db\"\nallowed_tables = [\"t\"]\n\n"
        ));
    }
    std::fs::write(home.join("config.toml"), out).unwrap();
}

fn frame_ok(f: &WsFrame) -> bool {
    matches!(f, WsFrame::Response { ok: true, .. })
}

fn frame_error(f: &WsFrame) -> String {
    match f {
        WsFrame::Response { error: Some(e), .. } => e
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| e.to_string()),
        _ => String::new(),
    }
}

fn grants_on_disk(home: &std::path::Path, id: &str) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(home.join("agents").join(id).join("agent.toml")).unwrap();
    let table: toml::Table = raw.parse().unwrap();
    table
        .get("capabilities")?
        .as_table()?
        .get("db_sources")?
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
}

#[tokio::test]
async fn agents_update_rejects_an_unconfigured_db_source_id() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), &["crm_pg"]);
    seed_agent(home.path(), "alpha", &[]);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "capabilities": { "db_sources": ["crm_pg", "ghost"] },
        }))
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
    let err = frame_error(&frame);
    assert!(err.contains("ghost"), "{err}");
    assert!(
        err.contains("crm_pg"),
        "must list the configured ids: {err}"
    );
    // Fail-closed: nothing written, not even the valid half.
    assert_eq!(grants_on_disk(home.path(), "alpha"), None);
}

#[tokio::test]
async fn agents_update_writes_a_valid_grant_list_with_replace_semantics() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), &["crm_pg", "erp_my"]);
    seed_agent(home.path(), "alpha", &["crm_pg"]);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // REPLACE, trimmed + de-duplicated, first-seen order preserved.
    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "capabilities": { "db_sources": [" erp_my ", "crm_pg", "erp_my"] },
        }))
        .await;
    assert!(frame_ok(&frame), "{}", frame_error(&frame));
    assert_eq!(
        grants_on_disk(home.path(), "alpha"),
        Some(vec!["erp_my".to_string(), "crm_pg".to_string()])
    );

    // An empty array revokes everything.
    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "capabilities": { "db_sources": [] },
        }))
        .await;
    assert!(frame_ok(&frame), "{}", frame_error(&frame));
    assert_eq!(grants_on_disk(home.path(), "alpha"), Some(vec![]));
}

/// An `agents.update` that does not carry `capabilities.db_sources` must
/// behave exactly as before — in particular it must not touch the key,
/// and it must not consult `config.toml` at all (there is none here).
#[tokio::test]
async fn agents_update_without_db_sources_is_unchanged() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", &["stale_source"]);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "capabilities": { "computer_use": true },
        }))
        .await;
    assert!(frame_ok(&frame), "{}", frame_error(&frame));
    assert_eq!(
        grants_on_disk(home.path(), "alpha"),
        Some(vec!["stale_source".to_string()]),
        "an untouched grant list must survive, even when it is stale"
    );
}

#[tokio::test]
async fn agents_inspect_always_carries_db_sources_as_an_array() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), &["crm_pg"]);
    seed_agent(home.path(), "alpha", &[]);
    seed_agent(home.path(), "bravo", &["crm_pg"]);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for (id, expected) in [("alpha", json!([])), ("bravo", json!(["crm_pg"]))] {
        let frame = handler
            .handle_agents_inspect(json!({ "agent_id": id }))
            .await;
        let WsFrame::Response {
            payload: Some(p), ..
        } = &frame
        else {
            panic!("inspect {id} failed: {frame:?}");
        };
        assert_eq!(p["capabilities"]["db_sources"], expected, "agent {id}");
    }
}

/// `db_sources.remove` revokes the source from every agent holding it and
/// reports who lost it.
#[tokio::test]
async fn db_sources_remove_revokes_the_grant_everywhere() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), &["crm_pg", "erp_my"]);
    seed_agent(home.path(), "alpha", &["crm_pg", "erp_my"]);
    seed_agent(home.path(), "bravo", &["erp_my"]);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = crate::db_sources_rpc::dispatch(
        &handler.registry,
        home.path(),
        "db_sources.remove",
        json!({ "name": "crm_pg" }),
    )
    .await;
    let WsFrame::Response {
        payload: Some(p), ..
    } = &frame
    else {
        panic!("remove failed: {frame:?}");
    };
    assert_eq!(p["success"], json!(true));
    assert_eq!(p["revoked_from"], json!(["alpha"]));
    assert_eq!(
        grants_on_disk(home.path(), "alpha"),
        Some(vec!["erp_my".to_string()]),
        "the agent's other grant must survive"
    );
    assert_eq!(
        grants_on_disk(home.path(), "bravo"),
        Some(vec!["erp_my".to_string()])
    );

    // Removing a source nobody holds reports an empty list, not a missing key.
    let frame = crate::db_sources_rpc::dispatch(
        &handler.registry,
        home.path(),
        "db_sources.remove",
        json!({ "name": "erp_my" }),
    )
    .await;
    let WsFrame::Response {
        payload: Some(p), ..
    } = &frame
    else {
        panic!("remove failed: {frame:?}");
    };
    assert_eq!(p["revoked_from"], json!(["alpha", "bravo"]));
}

/// The two new methods are reachable only behind the admin gate, in the
/// same `db_sources.*` arm as the five that manage the sources themselves.
#[tokio::test]
async fn grant_rpcs_require_admin() {
    let home = tempfile::tempdir().unwrap();
    seed_config(home.path(), &["crm_pg"]);
    seed_agent(home.path(), "alpha", &[]);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let viewer = UserContext {
        user_id: "u1".to_string(),
        email: "u1@test.local".to_string(),
        role: UserRole::Employee,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    };
    for method in ["db_sources.grants.list", "db_sources.grants.set"] {
        let frame = handler.handle(method, json!({}), &viewer).await;
        assert!(!frame_ok(&frame), "{method} must be admin-gated: {frame:?}");
    }

    let frame = handler
        .handle(
            "db_sources.grants.list",
            json!({}),
            &UserContext::admin_fallback(),
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
}
