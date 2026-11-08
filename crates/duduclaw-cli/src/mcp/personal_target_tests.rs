//! v1.81 P1: cross-agent MCP tools refuse a Personal target.
use super::*;
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use serde_json::json;

const PERSONAL: &str = "alice-personal";
const SHARED: &str = "team";

fn args(key: &str, target: &str) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(key.to_owned(), json!(target));
    map.insert("content".to_owned(), json!("x"));
    Value::Object(map)
}

/// A private home (the hardened users.db opener refuses group/other-writable
/// paths) whose users.db records `alice-personal` as alice's Personal employee.
fn home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let path = dir.path().join("users.db");
    let db = UserDb::new(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let alice = db.create_user("alice@example.test", "Alice", "password1", UserRole::Employee).unwrap().id;
    db.bind_agent(&alice, SHARED, AccessLevel::Owner).unwrap();
    let mut conn = rusqlite::Connection::open(&path).unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    assert!(duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, PERSONAL, &alice).unwrap());
    assert!(duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, PERSONAL, &alice).unwrap());
    tx.commit().unwrap();
    drop(conn);
    drop(db);
    for name in ["users.db", "users.db-wal", "users.db-shm", "users.db-journal"] {
        let sidecar = dir.path().join(name);
        if sidecar.exists() {
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    dir
}

#[test]
fn p1_cross_agent_tools_refuse_personal_target() {
    let dir = home();
    let home = dir.path();
    let refused = duduclaw_gateway::personal_visibility::PERSONAL_TARGET_REFUSED;
    // Another AI employee (Shared, internal key) targeting the Personal
    // employee is refused for every listed tool.
    for (tool, key) in PERSONAL_TARGET_TOOLS {
        let arguments = args(key, PERSONAL);
        assert_eq!(personal_target_refusal(home, true, tool, &arguments, SHARED), Some(refused), "{tool}");
        // An external key's client id is never the employee either.
        assert_eq!(personal_target_refusal(home, true, tool, &arguments, "external-client"), Some(refused), "{tool}");
        // The Personal employee acting on itself is left to the tool's own gates.
        assert_eq!(personal_target_refusal(home, true, tool, &arguments, PERSONAL), None, "{tool}");
        // A Shared target is unchanged.
        let shared = args(key, SHARED);
        assert_eq!(personal_target_refusal(home, true, tool, &shared, "other"), None, "{tool}");
    }
    for tool in ["agent_update", "agent_update_soul", "agent_remove"] {
        assert!(PERSONAL_TARGET_TOOLS.iter().any(|(name, _)| *name == tool), "{tool} must be listed");
    }
    // No target argument = the caller itself; unlisted tools are untouched.
    assert_eq!(personal_target_refusal(home, true, "agent_update", &json!({}), SHARED), None);
    assert_eq!(personal_target_refusal(home, true, "agent_update", &json!({"agent_id": "  "}), SHARED), None);
    assert_eq!(personal_target_refusal(home, true, "memory_search", &json!({"agent_id": PERSONAL}), SHARED), None);
    // An unreadable ownership store refuses a cross-agent target (fail closed).
    let missing = tempfile::tempdir().unwrap();
    assert_eq!(
        personal_target_refusal(missing.path(), true, "agent_update", &json!({"agent_id": PERSONAL}), SHARED),
        Some(duduclaw_gateway::personal_visibility::OWNERSHIP_UNAVAILABLE)
    );
}

#[test]
fn p1_cross_agent_tools_unchanged_outside_protected_mode() {
    let dir = home();
    for (tool, key) in PERSONAL_TARGET_TOOLS {
        let arguments = args(key, PERSONAL);
        assert_eq!(personal_target_refusal(dir.path(), false, tool, &arguments, SHARED), None, "{tool}");
    }
}

// ── v1.81 P1b ────────────────────────────────────────────────────────────

const REFUSED: &str = duduclaw_gateway::personal_visibility::PERSONAL_TARGET_REFUSED;
const UNAVAILABLE: &str = duduclaw_gateway::personal_visibility::OWNERSHIP_UNAVAILABLE;
const EPH: &str = "eph-0123456789ab";

/// An ephemeral scaffold whose sidecar names `sidecar_parent` and whose
/// host-owned org store entry reports to `org_parent`.
fn ephemeral(home: &Path, id: &str, sidecar_parent: &str, org_parent: &str) {
    let dir = home.join("agents").join(".ephemeral").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"eph\"\n").unwrap();
    std::fs::write(dir.join("ephemeral.toml"), format!(
        "parent = \"{sidecar_parent}\"\ntier = \"standard\"\ncreated_at = \"2026-11-08T00:00:00Z\"\n\
         expires_at = \"2026-11-09T00:00:00Z\"\n")).unwrap();
    duduclaw_core::org_store::upsert(home, id, duduclaw_core::org_store::OrgEntry::new(org_parent, "")).unwrap();
}

fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn p1b_list_agents_hides_other_personal_employees() {
    let dir = home();
    let home = dir.path();
    let agents = home.join("agents");
    for name in [PERSONAL, SHARED] {
        super::super::tests::create_test_agent(&agents, name, "");
    }
    // "dashboard" is a system sender (unrestricted org view), so only the
    // Personal rule decides what is listed.
    let listed = |acting: &'static str, protected: bool, home: std::path::PathBuf| async move {
        text(&list_agents_hiding(&json!({}), &home, "dashboard", acting, protected).await)
    };
    // Another AI employee: hidden entirely (no row, no metadata).
    let other = listed(SHARED, true, home.to_path_buf()).await;
    assert!(other.contains("(team)") && !other.contains(PERSONAL), "{other}");
    let external = listed("external-client", true, home.to_path_buf()).await;
    assert!(!external.contains(PERSONAL), "{external}");
    // The owner employee itself, and its ephemeral sub-agent, still see it.
    let own = listed(PERSONAL, true, home.to_path_buf()).await;
    assert!(own.contains("(alice-personal)") && own.contains("(team)"), "{own}");
    ephemeral(home, EPH, PERSONAL, PERSONAL);
    let sub = listed(EPH, true, home.to_path_buf()).await;
    assert!(sub.contains("(alice-personal)"), "{sub}");
    // Unprotected install: unchanged.
    let plain = listed(SHARED, false, home.to_path_buf()).await;
    assert!(plain.contains("(alice-personal)") && plain.contains("(team)"), "{plain}");
    // An unreadable ownership store refuses the listing (fail closed).
    let missing = tempfile::tempdir().unwrap();
    super::super::tests::create_test_agent(&missing.path().join("agents"), SHARED, "");
    let refused = list_agents_hiding(&json!({}), missing.path(), "dashboard", SHARED, true).await;
    assert_eq!(refused["isError"], json!(true));
    assert!(text(&refused).contains(UNAVAILABLE));
}

#[test]
fn p1b_delegation_and_response_tools_refuse_personal_target() {
    let dir = home();
    let home = dir.path();
    for (tool, key) in [("spawn_agent", "agent_id"), ("send_to_agent", "agent_id"), ("send_to_agent", "agent"),
        ("check_responses", "agent_id")] {
        assert!(PERSONAL_TARGET_TOOLS.contains(&(tool, key)), "{tool}.{key} must be listed");
        let personal = args(key, PERSONAL);
        // Another AI employee or an external key: refused.
        assert_eq!(personal_target_refusal(home, true, tool, &personal, SHARED), Some(REFUSED), "{tool}.{key}");
        assert_eq!(personal_target_refusal(home, true, tool, &personal, "external-client"), Some(REFUSED));
        // The owner employee on itself (e.g. spawning onto its own queue): allowed.
        assert_eq!(personal_target_refusal(home, true, tool, &personal, PERSONAL), None, "{tool}.{key}");
        // A Shared target is unchanged.
        assert_eq!(personal_target_refusal(home, true, tool, &args(key, SHARED), "other"), None);
    }
    // Every key of a tool is checked: a Shared `agent_id` cannot carry a
    // Personal `agent` past the gate.
    let both = json!({"agent_id": SHARED, "agent": PERSONAL, "prompt": "x"});
    assert_eq!(personal_target_refusal(home, true, "send_to_agent", &both, "other"), Some(REFUSED));
    // create_task steps use the same predicate (bus_tasks.rs).
    let refusal = duduclaw_gateway::personal_visibility::management_refusal;
    assert_eq!(refusal(home, true, SHARED, PERSONAL), Some(REFUSED));
    assert_eq!(refusal(home, true, PERSONAL, PERSONAL), None);
}

#[test]
fn p1b_task_status_refuses_foreign_store_and_hides_personal_step_output() {
    let dir = home();
    let home = dir.path();
    // An external key reading a Personal employee's own task store: refused.
    assert_eq!(process_store_refusal(home, true, "task_status", "external-client", PERSONAL), Some(REFUSED));
    // The employee itself, or a Shared process store: allowed.
    assert_eq!(process_store_refusal(home, true, "task_status", PERSONAL, PERSONAL), None);
    assert_eq!(process_store_refusal(home, true, "task_status", "external-client", SHARED), None);
    assert_eq!(process_store_refusal(home, true, "agent_update", "external-client", PERSONAL), None);
    let pre = pre_dispatch_refusal(home, true, "task_status", &json!({"task_id": "t"}), "external-client", PERSONAL, "x");
    assert_eq!(pre, Some(REFUSED));
    // A Shared employee's task with a step run by the Personal employee:
    // that step's output is hidden, its own steps are not.
    let steps = ["", SHARED, PERSONAL, "unknown-agent"];
    assert_eq!(hidden_step_agents(home, true, SHARED, steps), [PERSONAL.to_string()].into());
    assert!(hidden_step_agents(home, true, PERSONAL, steps).is_empty());
    assert!(hidden_step_agents(home, false, SHARED, steps).is_empty());
}

#[test]
fn p1b_memory_namespace_of_another_personal_employee_refused() {
    let dir = home();
    let home = dir.path();
    for requested in [PERSONAL, "alice-personal/notes", "internal/alice-personal"] {
        assert_eq!(namespace_refusal(home, true, SHARED, requested, SHARED), Some(REFUSED), "{requested}");
        let pre = pre_dispatch_refusal(home, true, "memory_search", &json!({"namespace": requested}), SHARED, SHARED, SHARED);
        assert_eq!(pre, Some(REFUSED), "{requested}");
    }
    // Own namespace (and below), the shared pool, external pools, Shared ids.
    for requested in [SHARED, "team/notes", "shared/public", "external/x", "internal/team"] {
        assert_eq!(namespace_refusal(home, true, SHARED, requested, SHARED), None, "{requested}");
    }
    assert_eq!(namespace_refusal(home, true, PERSONAL, PERSONAL, PERSONAL), None);
    assert_eq!(namespace_refusal(home, false, SHARED, PERSONAL, SHARED), None);
}

#[test]
fn p1b_self_targets_never_read_users_db() {
    // No users.db at all, as inside an isolated run whose MCP process
    // cannot open it: self-targets still work, cross targets fail closed.
    let missing = tempfile::tempdir().unwrap();
    let home = missing.path();
    for (tool, key) in PERSONAL_TARGET_TOOLS {
        assert_eq!(personal_target_refusal(home, true, tool, &args(key, PERSONAL), PERSONAL), None, "{tool}");
        assert_eq!(personal_target_refusal(home, true, tool, &args(key, PERSONAL), SHARED), Some(UNAVAILABLE));
    }
    assert_eq!(process_store_refusal(home, true, "task_status", PERSONAL, PERSONAL), None);
    assert_eq!(namespace_refusal(home, true, PERSONAL, "alice-personal/x", PERSONAL), None);
    assert_eq!(namespace_refusal(home, true, "internal/gateway-internal", "shared/public", "x"), None);
    assert!(hidden_step_agents(home, true, PERSONAL, ["", PERSONAL]).is_empty());
    assert_eq!(hidden_step_agents(home, true, PERSONAL, [SHARED]), [SHARED.to_string()].into(),
        "unreadable store hides other employees' step output");
}

#[test]
fn p1b_ephemeral_subagent_acts_as_its_parent() {
    let dir = home();
    let home = dir.path();
    ephemeral(home, EPH, PERSONAL, PERSONAL);
    for (tool, key) in PERSONAL_TARGET_TOOLS {
        assert_eq!(personal_target_refusal(home, true, tool, &args(key, PERSONAL), EPH), None, "{tool}");
    }
    assert_eq!(process_store_refusal(home, true, "task_status", EPH, PERSONAL), None);
    // A scaffold whose sidecar claims the Personal parent but whose host
    // org record does not agree is not that employee.
    const FORGED: &str = "eph-ffffffffffff";
    ephemeral(home, FORGED, PERSONAL, SHARED);
    assert_eq!(personal_target_refusal(home, true, "agent_update", &args("agent_id", PERSONAL), FORGED), Some(REFUSED));
    // An unknown ephemeral id has no parent at all.
    assert_eq!(personal_target_refusal(home, true, "spawn_agent", &args("agent_id", PERSONAL), "eph-000000000000"),
        Some(REFUSED));
}

#[test]
fn p1b_unprotected_install_unchanged() {
    let dir = home();
    let home = dir.path();
    let missing = Path::new("/nonexistent/duduclaw-home");
    for home in [home, missing] {
        assert_eq!(pre_dispatch_refusal(home, false, "spawn_agent", &args("agent_id", PERSONAL), SHARED, SHARED, SHARED), None);
        assert_eq!(pre_dispatch_refusal(home, false, "task_status", &json!({}), "external-client", PERSONAL, "x"), None);
        assert_eq!(pre_dispatch_refusal(home, false, "memory_search", &json!({"namespace": PERSONAL}), SHARED, SHARED, SHARED), None);
        assert!(hidden_step_agents(home, false, SHARED, [PERSONAL]).is_empty());
    }
}
