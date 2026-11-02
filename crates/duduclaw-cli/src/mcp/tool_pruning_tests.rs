use super::*;
use serde_json::json;

fn setup() -> (tempfile::TempDir, crate::mcp_auth::Principal) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/alice");
    std::fs::create_dir_all(&dir).unwrap();
    mode(home.path(), "off");
    let lines: Vec<_> = (0..10)
        .map(|i| {
            json!({
                "timestamp":format!("2026-11-01T00:00:{i:02}Z"),
                "agent_id":"alice", "tool_name":"memory_search", "success":true,
                "session_id":format!("s{}",i%2), "turn_id":format!("t{i}")
            })
            .to_string()
        })
        .collect();
    std::fs::write(
        home.path().join("tool_calls.jsonl"),
        lines.join("\n") + "\n",
    )
    .unwrap();
    (
        home,
        crate::mcp_auth::Principal {
            client_id: "alice".into(),
            scopes: crate::mcp_auth::parse_scopes("admin").unwrap(),
            is_external: false,
            created_at: chrono::Utc::now(),
        },
    )
}
fn mode(home: &Path, mode: &str) {
    std::fs::write(home.join("agents/alice/agent.toml"), format!("[agent]\nid='alice'\nname='alice'\nstatus='active'\n[capabilities]\ntool_pruning='{mode}'\n")).unwrap();
}
async fn listing(home: &Path, principal: &crate::mcp_auth::Principal) -> Value {
    let defs: Vec<_> = visible_tools_with(principal, home, "alice", true)
        .await
        .into_iter()
        .map(build_tool_schema)
        .collect();
    json!({"tools":defs})
}
fn has(list: &Value, name: &str) -> bool {
    list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == name)
}

#[tokio::test(flavor = "current_thread")]
async fn shadow_is_byte_identical_on_hides_and_reset_restores() {
    let (home, p) = setup();
    let off = listing(home.path(), &p).await;
    assert!(has(&off, "skill_list"));
    mode(home.path(), "shadow");
    assert_eq!(
        serde_json::to_vec(&off).unwrap(),
        serde_json::to_vec(&listing(home.path(), &p).await).unwrap()
    );
    mode(home.path(), "on");
    let on = listing(home.path(), &p).await;
    assert!(!has(&on, "skill_list"));
    assert!(has(&on, "memory_search"));
    std::fs::write(
        home.path().join("agents/alice/tool_pruning_reset_at"),
        "2026-11-02T00:00:00Z",
    )
    .unwrap();
    assert_eq!(off, listing(home.path(), &p).await);
}

#[tokio::test(flavor = "current_thread")]
async fn hidden_tool_is_callable_through_real_dispatch_and_returns() {
    let (home, p) = setup();
    mode(home.path(), "on");
    let before = listing(home.path(), &p).await;
    assert!(!has(&before, "skill_list"));
    let memory =
        std::sync::Arc::new(SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap());
    let dispatcher = crate::mcp_dispatch::McpDispatcher::new(
        home.path().to_path_buf(),
        reqwest::Client::new(),
        memory,
        "alice".into(),
        std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
        crate::mcp_rate_limit::RateLimiter::new(),
        crate::mcp_memory_quota::DailyQuota::new(),
    );
    let ns = crate::mcp_namespace::resolve(&p).unwrap();
    let result = dispatcher
        .dispatch_tool_call(
            &p,
            &ns,
            &json!({"name":"skill_list","arguments":{}}),
            &json!(1),
        )
        .await;
    assert!(result.get("error").is_none(), "{result}");
    assert_ne!(result["result"]["isError"], json!(true), "{result}");
    let after = listing(home.path(), &p).await;
    assert!(has(&after, "skill_list"));
    assert_ne!(
        advertised_tool_names(&json!({"result":before})),
        advertised_tool_names(&json!({"result":after}))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn active_scoped_grant_overrides_pruning() {
    let (home, principal) = setup();
    std::fs::write(home.path().join("agents/alice/agent.toml"), "[agent]\nid='alice'\nname='alice'\n[capabilities]\ntool_pruning='on'\nscoped_tools=['skill_list']\n").unwrap();
    assert!(!has(&listing(home.path(), &principal).await, "skill_list"));
    let store =
        duduclaw_gateway::capability_grants::CapabilityGrantStore::open(home.path()).unwrap();
    store
        .grant("alice", None, "skill_list", "test-operator", 3600)
        .await
        .unwrap();
    assert!(has(&listing(home.path(), &principal).await, "skill_list"));
}
