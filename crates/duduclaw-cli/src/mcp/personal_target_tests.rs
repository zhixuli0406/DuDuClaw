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
    {
        let org = duduclaw_core::org_store::lock_org_mutations(home).unwrap();
        duduclaw_core::org_store::upsert(&org, id, duduclaw_core::org_store::OrgEntry::new(org_parent, "")).unwrap();
    }
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

// ── v1.81 P1c ────────────────────────────────────────────────────────────

/// Record `agent` as `owner`'s Personal employee (the apply path's write)
/// and keep the store private for the hardened opener.
fn record_personal(home: &Path, agent: &str, owner: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = home.join("users.db");
    let mut conn = rusqlite::Connection::open(&path).unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    assert!(duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, agent, owner).unwrap());
    assert!(duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, agent, owner).unwrap());
    tx.commit().unwrap();
    drop(conn);
    for name in ["users.db", "users.db-wal", "users.db-shm", "users.db-journal"] {
        let sidecar = home.join(name);
        if sidecar.exists() {
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

/// Keys under which a tool schema names another employee.
const TARGET_KEYS: &[&str] = &["agent_id", "agent", "assigned_to", "target_agent", "reports_to"];

#[test]
fn p1c_every_schema_target_key_is_in_the_table() {
    // Scan the shared schema registry: a tool parameter that names an
    // employee is checked by the pre-dispatch gate (listing ⇔ calling).
    let mut missing = Vec::new();
    for tool in duduclaw_core::mcp_tool_defs::tools() {
        for param in tool.params.iter().filter(|p| TARGET_KEYS.contains(&p.name)) {
            if !PERSONAL_TARGET_TOOLS.contains(&(tool.name, param.name)) {
                missing.push(format!("{}.{}", tool.name, param.name));
            }
        }
    }
    assert!(missing.is_empty(), "tools naming an employee but not in PERSONAL_TARGET_TOOLS: {missing:?}");
    // Delegation tools are a subset of the table.
    for tool in DELEGATION_TOOLS {
        assert!(PERSONAL_TARGET_TOOLS.iter().any(|(name, _)| name == tool), "{tool}");
    }
}

#[tokio::test]
async fn p1c_delegation_and_target_tools_refuse_another_owners_personal_employee() {
    let dir = home();
    let home = dir.path();
    for (tool, key) in [("tasks_create", "assigned_to"), ("tasks_update", "assigned_to"),
        ("discovery_catalog", "agent_id"), ("create_agent", "reports_to"), ("agent_update", "reports_to"),
        ("tasks_list", "assigned_to"), ("activity_list", "agent_id"), ("discovery_list", "agent_id"),
        ("list_cron_tasks", "agent_id"), ("list_reminders", "agent_id"), ("submit_feedback", "agent_id"),
        ("audit_trail_query", "agent_id"), ("reliability_summary", "agent_id"), ("cost_summary", "agent_id")] {
        let personal = args(key, PERSONAL);
        // Another AI employee or an external key: refused.
        assert_eq!(personal_target_refusal(home, true, tool, &personal, SHARED), Some(REFUSED), "{tool}.{key}");
        assert_eq!(personal_target_refusal(home, true, tool, &personal, "external-client"), Some(REFUSED));
        // The employee itself: allowed (left to the tool's own gates).
        assert_eq!(personal_target_refusal(home, true, tool, &personal, PERSONAL), None, "{tool}.{key}");
        // Shared targets: unchanged.
        assert_eq!(personal_target_refusal(home, true, tool, &args(key, SHARED), "other"), None);
    }

    // The owner's own work: another Personal employee of the same owner may
    // hand work over; another person's Personal employee may not.
    let users = UserDb::new(&home.join("users.db")).unwrap();
    let bob = users.create_user("bob@example.test", "Bob", "password1", UserRole::Employee).unwrap().id;
    drop(users);
    let alice: String = rusqlite::Connection::open(home.join("users.db")).unwrap()
        .query_row("SELECT id FROM users WHERE email = ?1", ["alice@example.test"], |row| row.get(0))
        .unwrap();
    record_personal(home, "alice-second", &alice);
    record_personal(home, "bob-personal", &bob);
    for tool in DELEGATION_TOOLS {
        let key = PERSONAL_TARGET_TOOLS.iter().find(|(name, _)| name == tool).unwrap().1;
        let personal = args(key, PERSONAL);
        assert_eq!(personal_target_refusal(home, true, tool, &personal, "alice-second"), None, "{tool}");
        assert_eq!(personal_target_refusal(home, true, tool, &personal, "bob-personal"), Some(REFUSED), "{tool}");
    }
    // Reads and management stay "the employee itself only".
    assert_eq!(personal_target_refusal(home, true, "agent_update", &args("agent_id", PERSONAL), "alice-second"),
        Some(REFUSED));
    assert_eq!(personal_target_refusal(home, true, "activity_list", &args("agent_id", PERSONAL), "alice-second"),
        Some(REFUSED));

    // Work items named by id resolve their employee from tasks.db.
    let store = duduclaw_gateway::task_store::TaskStore::open(home).unwrap();
    store.insert_task(&duduclaw_gateway::task_store::TaskRow::new("personal-task".into(), "t".into(), String::new(),
        "normal".into(), PERSONAL.into(), "system".into())).await.unwrap();
    store.insert_plan(&duduclaw_gateway::task_store::PlanRow::new("personal-plan".into(), "p".into(),
        PERSONAL.into(), "system".into())).await.unwrap();
    drop(store);
    let task = json!({"task_id": "personal-task", "title": "x"});
    let plan = json!({"plan_id": "personal-plan"});
    for (tool, arguments) in [("tasks_update", &task), ("activity_list", &task), ("plan_get", &plan)] {
        assert_eq!(id_target_refusal(home, true, tool, arguments, SHARED), Some(REFUSED), "{tool}");
        assert_eq!(pre_dispatch_refusal(home, true, tool, arguments, SHARED, SHARED, SHARED), Some(REFUSED), "{tool}");
        assert_eq!(id_target_refusal(home, true, tool, arguments, PERSONAL), None, "{tool}");
        assert_eq!(id_target_refusal(home, true, tool, arguments, "bob-personal"), Some(REFUSED), "{tool}");
    }
    // Same owner: delegation (tasks_update) yes, reads no.
    assert_eq!(id_target_refusal(home, true, "tasks_update", &task, "alice-second"), None);
    assert_eq!(id_target_refusal(home, true, "plan_get", &plan, "alice-second"), Some(REFUSED));
    // Unknown ids are left to the tool; unlisted tools are untouched.
    assert_eq!(id_target_refusal(home, true, "plan_get", &json!({"plan_id": "missing"}), SHARED), None);
    assert_eq!(id_target_refusal(home, true, "memory_search", &task, SHARED), None);
    // No readable task store: refused (fail closed).
    let missing = tempfile::tempdir().unwrap();
    assert_eq!(id_target_refusal(missing.path(), true, "plan_get", &plan, SHARED), Some(UNAVAILABLE));
    // Unprotected: unchanged.
    assert_eq!(id_target_refusal(home, false, "plan_get", &plan, SHARED), None);
    assert_eq!(personal_target_refusal(home, false, "tasks_create", &args("assigned_to", PERSONAL), SHARED), None);
}

// ── v1.81 P1d ────────────────────────────────────────────────────────────

fn data(result: &Value) -> Value {
    serde_json::from_str(&text(result)).unwrap_or_else(|_| panic!("not a JSON result: {result}"))
}

fn activity_row(id: &str, kind: &str, agent: &str, summary: &str, metadata: Option<String>)
    -> duduclaw_gateway::task_store::ActivityRow {
    duduclaw_gateway::task_store::ActivityRow {
        id: id.into(),
        event_type: kind.into(),
        agent_id: agent.into(),
        task_id: None,
        summary: summary.into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata,
    }
}

#[tokio::test]
async fn p1d_wildcard_listings_hide_other_personal_rows_and_totals() {
    use super::super::personal_wildcard as wildcard;
    use super::super::RecordActor;
    use duduclaw_gateway::task_store::{GoalRow, TaskRow, TaskStore};
    let dir = home();
    let home = dir.path();
    let store = TaskStore::open(home).unwrap();
    for (id, agent) in [("personal-task", PERSONAL), ("shared-task", SHARED)] {
        store.insert_task(&TaskRow::new(id.into(), "t".into(), String::new(), "normal".into(), agent.into(),
            agent.into())).await.unwrap();
    }
    for (id, agent) in [("a-personal", PERSONAL), ("a-shared", SHARED)] {
        store.append_activity(&activity_row(id, "agent_comment", agent, "secret", None)).await.unwrap();
    }
    // Goals: created by the Personal employee (metadata, and a legacy row
    // matched by title), by the Shared one, and a company goal nobody created.
    for (id, title, creator, recorded) in [("g-personal", "Personal goal", Some(PERSONAL), true),
        ("g-legacy", "Legacy goal", Some(PERSONAL), false), ("g-shared", "Shared goal", Some(SHARED), true),
        ("g-company", "Company goal", None, false)] {
        store.insert_goal(&GoalRow::new(id.into(), title.into(), String::new())).await.unwrap();
        if let Some(agent) = creator {
            let metadata = recorded.then(|| json!({"goal_id": id}).to_string());
            store.append_activity(&activity_row(&format!("created-{id}"), "goal_created", agent,
                &format!("Created goal: {title}"), metadata)).await.unwrap();
        }
    }
    drop(store);
    let all_tasks = json!({"assigned_to": "*"});
    let all_activity = json!({"agent_id": "*"});
    let ids = |value: &Value, list: &str, key: &str| -> Vec<String> {
        value[list].as_array().unwrap().iter().map(|row| row[key].as_str().unwrap().to_owned()).collect()
    };

    // Another AI employee: no row of the Personal employee, totals match.
    let tasks = data(&wildcard::tasks_list(&all_tasks, home, SHARED, RecordActor::Agent(SHARED), SHARED, true).await);
    assert!(!ids(&tasks, "tasks", "assigned_to").iter().any(|agent| agent == PERSONAL), "{tasks}");
    assert!(ids(&tasks, "tasks", "id").contains(&"shared-task".to_string()), "{tasks}");
    assert_eq!(tasks["total"].as_u64().unwrap() as usize, tasks["tasks"].as_array().unwrap().len());
    let activity = data(&wildcard::activity_list(&all_activity, home, SHARED, RecordActor::Agent(SHARED), SHARED,
        true).await);
    assert!(!ids(&activity, "activities", "agent_id").iter().any(|agent| agent == PERSONAL), "{activity}");
    assert_eq!(activity["total"], json!(2), "a-shared + created-g-shared: {activity}");
    let goals = data(&wildcard::goals_list(&json!({}), home, SHARED, true).await);
    let mut listed = ids(&goals, "goals", "id");
    listed.sort();
    assert_eq!(listed, ["g-company", "g-shared"], "{goals}");
    assert_eq!(goals["total"], json!(2));
    // An external key is not the employee its client id spells.
    let external = data(&wildcard::goals_list(&json!({}), home, "external:alice-personal", true).await);
    assert_eq!(external["total"], json!(2));

    // The Personal employee itself sees its own rows.
    let own = data(&wildcard::tasks_list(&all_tasks, home, PERSONAL, RecordActor::Agent(PERSONAL), PERSONAL,
        true).await);
    assert!(ids(&own, "tasks", "id").contains(&"personal-task".to_string()), "{own}");
    let own_goals = data(&wildcard::goals_list(&json!({}), home, PERSONAL, true).await);
    assert_eq!(own_goals["total"], json!(4));
    // Unprotected: unchanged.
    let plain = data(&wildcard::tasks_list(&all_tasks, home, SHARED, RecordActor::Operator(SHARED), SHARED,
        false).await);
    assert!(ids(&plain, "tasks", "id").contains(&"personal-task".to_string()), "{plain}");
    let plain_activity = data(&wildcard::activity_list(&all_activity, home, SHARED, RecordActor::Operator(SHARED),
        SHARED, false).await);
    assert_eq!(plain_activity["total"], json!(5));

    // A self-scoped listing reads no users.db; a wildcard fails closed.
    let missing = tempfile::tempdir().unwrap();
    assert_eq!(wildcard::wildcard_hidden(missing.path(), true, SHARED, false), Ok(Default::default()));
    assert_eq!(wildcard::wildcard_hidden(missing.path(), true, SHARED, true), Err(UNAVAILABLE));
    let refused = wildcard::goals_list(&json!({}), missing.path(), SHARED, true).await;
    assert_eq!(refused["isError"], json!(true));
}

#[tokio::test]
async fn p1d_task_id_tools_and_link_fields_resolve_their_employee() {
    use duduclaw_gateway::task_store::{GoalRow, TaskRow, TaskStore};
    let dir = home();
    let home = dir.path();
    let users = UserDb::new(&home.join("users.db")).unwrap();
    let bob = users.create_user("bob@example.test", "Bob", "password1", UserRole::Employee).unwrap().id;
    drop(users);
    let alice: String = rusqlite::Connection::open(home.join("users.db")).unwrap()
        .query_row("SELECT id FROM users WHERE email = ?1", ["alice@example.test"], |row| row.get(0))
        .unwrap();
    record_personal(home, "alice-second", &alice);
    record_personal(home, "bob-personal", &bob);
    let store = TaskStore::open(home).unwrap();
    for (id, agent) in [("personal-task", PERSONAL), ("unassigned-task", "")] {
        store.insert_task(&TaskRow::new(id.into(), "t".into(), String::new(), "normal".into(), agent.into(),
            "system".into())).await.unwrap();
    }
    store.insert_goal(&GoalRow::new("g-personal".into(), "Personal goal".into(), String::new())).await.unwrap();
    store.append_activity(&activity_row("created", "goal_created", PERSONAL, "Created goal: Personal goal",
        Some(json!({"goal_id": "g-personal"}).to_string()))).await.unwrap();
    drop(store);

    // Every tool naming a task by id: the employee itself only.
    let task = json!({"task_id": "personal-task", "summary": "x", "reason": "x", "tool": "x"});
    for tool in ["tasks_claim", "tasks_renew", "tasks_complete", "tasks_block", "activity_post", "capability_request"] {
        assert!(PERSONAL_ID_TARGETS.iter().any(|(name, key, _)| *name == tool && *key == "task_id"), "{tool}");
        assert_eq!(id_target_refusal(home, true, tool, &task, SHARED), Some(REFUSED), "{tool}");
        assert_eq!(pre_dispatch_refusal(home, true, tool, &task, SHARED, SHARED, SHARED), Some(REFUSED), "{tool}");
        assert_eq!(id_target_refusal(home, true, tool, &task, "alice-second"), Some(REFUSED), "{tool}");
        assert_eq!(id_target_refusal(home, true, tool, &task, PERSONAL), None, "{tool}");
        // An unassigned task belongs to nobody; unknown ids are left to the tool.
        assert_eq!(id_target_refusal(home, true, tool, &json!({"task_id": "unassigned-task"}), SHARED), None);
        assert_eq!(id_target_refusal(home, true, tool, &json!({"task_id": "missing"}), SHARED), None);
    }
    // `tasks_create` link fields (a string, an array, or a JSON-array string).
    for arguments in [json!({"title": "x", "parent_task_id": "personal-task"}),
        json!({"title": "x", "depends_on": ["missing", "personal-task"]}),
        json!({"title": "x", "depends_on": "[\"personal-task\"]"}),
        json!({"title": "x", "goal_id": "g-personal"})] {
        assert_eq!(id_target_refusal(home, true, "tasks_create", &arguments, SHARED), Some(REFUSED), "{arguments}");
        assert_eq!(id_target_refusal(home, true, "tasks_create", &arguments, "bob-personal"), Some(REFUSED));
        // Same owner: the owner's own work (delegation rule).
        assert_eq!(id_target_refusal(home, true, "tasks_create", &arguments, "alice-second"), None, "{arguments}");
    }
    let rewire = json!({"task_id": "unassigned-task", "depends_on": ["personal-task"]});
    assert_eq!(id_target_refusal(home, true, "tasks_update", &rewire, SHARED), Some(REFUSED));
    let child_goal = json!({"title": "x", "parent_goal_id": "g-personal"});
    assert_eq!(id_target_refusal(home, true, "goals_create", &child_goal, SHARED), Some(REFUSED));
    assert_eq!(id_target_refusal(home, false, "tasks_create", &json!({"parent_task_id": "personal-task"}), SHARED),
        None);
}

/// Keys under which a tool schema names a work item by id.
const ID_KEYS: &[&str] = &["task_id", "plan_id", "parent_task_id", "goal_id", "depends_on", "parent_goal_id"];

/// Id parameters that do not name a DuDuClaw work item of another employee.
const ID_ALLOWLIST: &[(&str, &str, &str)] = &[
    ("task_status", "task_id", "a create_task bus task in the process's own store (PROCESS_STORE_TOOLS)"),
    ("gtasks_complete", "task_id", "a Google Tasks id in the employee's own Google account"),
];

#[test]
fn p1d_every_schema_id_key_is_in_the_table() {
    let mut missing = Vec::new();
    for tool in duduclaw_core::mcp_tool_defs::tools() {
        for param in tool.params.iter().filter(|p| ID_KEYS.contains(&p.name)) {
            let listed = PERSONAL_ID_TARGETS.iter().any(|(name, key, _)| *name == tool.name && *key == param.name);
            let allowed = ID_ALLOWLIST.iter().any(|(name, key, _)| *name == tool.name && *key == param.name);
            if !listed && !allowed {
                missing.push(format!("{}.{}", tool.name, param.name));
            }
        }
    }
    assert!(missing.is_empty(), "tools naming a work item by id but not checked: {missing:?}");
    for (tool, key, reason) in ID_ALLOWLIST {
        assert!(!reason.is_empty(), "{tool}.{key}");
    }
}

#[test]
fn p1d_external_client_id_is_never_an_employee() {
    use super::super::personal_wildcard::{EXTERNAL_ID_IS_EMPLOYEE, external_identity_refusal, personal_acting_id};
    let dir = home();
    let home = dir.path();
    super::super::tests::create_test_agent(&home.join("agents"), PERSONAL, "");
    let principal = |id: &str, external: bool| crate::mcp_auth::Principal {
        client_id: id.into(),
        scopes: Default::default(),
        is_external: external,
        created_at: chrono::Utc::now(),
    };
    // An external key named like the Personal employee is judged as itself.
    let external = principal(PERSONAL, true);
    let acting = personal_acting_id(&external, SHARED);
    assert_eq!(acting, "external:alice-personal");
    assert!(!duduclaw_gateway::personal_visibility::same_employee(home, &acting, PERSONAL));
    assert_eq!(pre_dispatch_refusal(home, true, "spawn_agent", &args("agent_id", PERSONAL), &acting, SHARED, "x"),
        Some(REFUSED));
    // … and refused outright in protected mode, since other gates would
    // resolve that client id to the employee.
    assert_eq!(external_identity_refusal(home, true, &external), Some(EXTERNAL_ID_IS_EMPLOYEE));
    assert_eq!(external_identity_refusal(home, false, &external), None);
    assert_eq!(external_identity_refusal(home, true, &principal("crm-client", true)), None);
    // An external key spelled like the internal client id is not the
    // process's employee either.
    let internal_id = duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
    assert_eq!(personal_acting_id(&principal(internal_id, true), PERSONAL), format!("external:{internal_id}"));
    // Internal and per-agent keys are unchanged.
    assert_eq!(personal_acting_id(&principal(internal_id, false), PERSONAL), PERSONAL);
    assert_eq!(personal_acting_id(&principal(PERSONAL, false), SHARED), PERSONAL);
    assert_eq!(external_identity_refusal(home, true, &principal(PERSONAL, false)), None);
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
