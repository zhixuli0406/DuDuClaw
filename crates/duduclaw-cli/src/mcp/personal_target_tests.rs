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
