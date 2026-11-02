//! `on_behalf_of` through the MCP server: read from the host env only,
//! written onto what the employee hands on, and (enforce mode) narrowing.

use super::*;

/// Serializes the tests that set `DUDUCLAW_ON_BEHALF_OF` (process-global).
static OBO_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct HostPrincipal;

impl HostPrincipal {
    fn set(value: &str) -> Self {
        unsafe { std::env::set_var(duduclaw_core::ENV_ON_BEHALF_OF, value) };
        Self
    }
}

impl Drop for HostPrincipal {
    fn drop(&mut self) {
        unsafe { std::env::remove_var(duduclaw_core::ENV_ON_BEHALF_OF) };
    }
}

fn home_with_queue() -> TempDir {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");
    // The gateway's own schema (it migrates the queue at start).
    duduclaw_gateway::message_queue::MessageQueue::open(tmp.path()).unwrap();
    tmp
}

fn queued_principal(home: &std::path::Path) -> Vec<Option<String>> {
    let conn = rusqlite::Connection::open(home.join("message_queue.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT on_behalf_of FROM message_queue")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

fn ctx() -> DelegationContext {
    DelegationContext {
        depth: 0,
        origin: None,
    }
}

fn audit_events(home: &std::path::Path, event: &str) -> Vec<serde_json::Value> {
    fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event_type"] == event)
        .collect()
}

#[tokio::test]
async fn send_to_agent_stamps_the_host_principal_and_ignores_arguments() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let _host = HostPrincipal::set("user:u-emp1");
    let params = serde_json::json!({
        "agent_id": "worker",
        "prompt": "整理報價單",
        "on_behalf_of": "user:someone-else",
    });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(
        queued_principal(home),
        vec![Some("user:u-emp1".to_string())]
    );
}

#[tokio::test]
async fn send_to_agent_without_a_principal_writes_none() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let params =
        serde_json::json!({ "agent_id": "worker", "prompt": "x", "on_behalf_of": "user:u-1" });
    let result = send_to_agent_with_ctx(&params, tmp.path(), "main", ctx()).await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(queued_principal(tmp.path()), vec![None]);
}

#[tokio::test]
async fn a_system_sender_name_is_refused_as_a_principal() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let _host = HostPrincipal::set("user:dashboard");
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "x" });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert!(result.get("isError").is_none(), "{result}");
    use duduclaw_gateway::on_behalf_of::PrincipalState;
    let carried = queued_principal(home);
    assert_eq!(carried, vec![PrincipalState::Unknown.to_wire()], "a rejected principal stays unknown, never background work");
    assert_eq!(PrincipalState::from_wire(carried[0].as_deref()), PrincipalState::Unknown);
    assert_eq!(duduclaw_core::host_principal_checked().unwrap_err().reason, "system_sender");
    assert!(duduclaw_core::host_principal().is_none());
}

#[tokio::test]
async fn spawn_agent_carries_the_principal_on_the_bus_line() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let _host = HostPrincipal::set("unmapped:line:0123456789abcdef");
    let params =
        serde_json::json!({ "agent_id": "worker", "task": "背景工作", "on_behalf_of": "user:x" });
    let result = spawn_agent_with_ctx(&params, home, "main", ctx()).await;
    assert!(result.get("isError").is_none(), "{result}");
    let line = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(line.lines().last().unwrap()).unwrap();
    assert_eq!(v["on_behalf_of"], "unmapped:line:0123456789abcdef");
}

#[tokio::test]
async fn tasks_create_records_the_principal_from_the_host_only() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = TempDir::new();
    let home = tmp.path();
    create_test_agent(&home.join("agents"), "agnes", "");
    let _host = HostPrincipal::set("user:u-mgr");
    let args = serde_json::json!({
        "title": "追款",
        "assigned_to": "agnes",
        "on_behalf_of": "user:someone-else",
        "created_via": "line",
    });
    let result = crate::mcp::caller_shims::handle_tasks_create(&args, home, "agnes").await;
    assert!(result.get("isError").is_none(), "{result}");
    let store = duduclaw_gateway::task_store::TaskStore::open(home).unwrap();
    let rows = store
        .list_by_principal("user:u-mgr", false, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{result}");
    assert_eq!(rows[0].created_via.as_deref(), Some("agent"));
    assert!(
        store
            .list_by_principal("user:someone-else", false, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn enforce_requires_host_context_even_after_binding_while_record_does_not() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    // A real account with no binding on `worker`.
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let u = db
        .create_user(
            "v@test.local",
            "訪客",
            "TeamPass-12345",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap()
        .id;
    let _host = HostPrincipal::set(&format!("user:{u}"));
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "x" });

    // record (default): allowed and recorded.
    let ok = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert!(ok.get("isError").is_none(), "{ok}");

    fs::write(
        home.join("config.toml"),
        "[delegation]\nhuman_principal = \"enforce\"\n",
    )
    .unwrap();
    let refused = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert_eq!(refused["isError"], true, "{refused}");
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("發出這個指令的人沒有權限"), "{text}");
    assert_eq!(queued_principal(home).len(), 1, "nothing queued on refusal");
    // The denial row names the person (tool_calls.jsonl host stamp).
    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).unwrap();
    let row = audit
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["tool_name"] == "delegation_denied")
        .expect("delegation_denied row");
    assert_eq!(row["reason"], "human_principal_not_permitted");
    assert_eq!(row["on_behalf_of"], format!("user:{u}"));

    // A binding cannot replace the gateway's live host context. Positive
    // authenticated cases run through the real route in native MCP tests.
    db.bind_agent(&u, "worker", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    let refused = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(
        queued_principal(home).len(),
        1,
        "no context means no new queue row even after binding"
    );

    // Self-assignment also needs host context, including for a Viewer.
    let viewer = db
        .create_user(
            "w@test.local",
            "看看",
            "TeamPass-12345",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap()
        .id;
    db.bind_agent(&viewer, "worker", duduclaw_auth::AccessLevel::Viewer)
        .unwrap();
    drop(_host);
    let _host = HostPrincipal::set(&format!("user:{viewer}"));
    let args = serde_json::json!({ "title": "自己做", "assigned_to": "worker" });
    let refused = crate::mcp::caller_shims::handle_tasks_create(&args, home, "worker").await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(audit_events(home, "on_behalf_of_rejected").is_empty());
}

#[tokio::test]
async fn spawn_ephemeral_carries_host_principal_through_admission_replay() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    create_test_agent_with_caps(&home.join("agents"), "main", &["Read"]);
    fs::write(home.join("config.toml"), "[dispatch]\nephemeral_max_active = 1\n").unwrap();
    let args = serde_json::json!({
        "instruction": "Read the report.", "context": "Review the report.", "tools": ["Read"],
        "on_behalf_of": "user:forged",
    });
    {
        let _host = HostPrincipal::set("user:owner");
        for _ in 0..2 {
            let result = spawn_ephemeral_with_ctx_and_upstream(
                &args, home, "main", ctx(),
                (Some("origin-turn".into()), Some("webchat:owner#agent:main".into())),
            ).await;
            assert_ne!(result["isError"], true, "{result}");
        }
    }
    let bus = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let first: serde_json::Value = serde_json::from_str(bus.trim()).unwrap();
    assert_eq!(first["on_behalf_of"], "user:owner");
    let dir = duduclaw_gateway::ephemeral::resolve_agent_dir(
        home, first["agent_id"].as_str().unwrap(),
    ).unwrap();
    fs::remove_dir_all(dir).unwrap();
    assert_eq!(duduclaw_gateway::ephemeral::drain_admission_queue(home).await.admitted, 1);
    let bus = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let replayed: serde_json::Value = serde_json::from_str(bus.lines().last().unwrap()).unwrap();
    assert_eq!(replayed["on_behalf_of"], "user:owner", "the sweeper has no host principal");
    assert_eq!(replayed["turn_id"], "origin-turn");
    assert_eq!(replayed["session_id"], "webchat:owner#agent:main");
}
