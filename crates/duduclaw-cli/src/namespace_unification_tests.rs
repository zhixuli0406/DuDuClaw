//! v1.68.0 memory namespace unification: an employee's MCP memory tools and
//! the gateway's own memory paths (distillation, prompt injection, dashboard
//! RPCs) now read and write the same rows. These tests drive the real MCP
//! handlers with a namespace resolved from a token-verified identity and the
//! real gateway entry points on the same `memory.db`.

use std::collections::HashSet;
use std::path::Path;

use duduclaw_memory::SqliteMemoryEngine;
use serde_json::{json, Value};

use crate::mcp_auth::Principal;
use crate::mcp_memory_handlers as h;
use crate::mcp_namespace::{
    resolve_for_caller, shared_internal_pool, verified_employee, CallerIdentity, NamespaceContext,
};

const USER: &str = "tg-424242";

fn home_with(agents: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for a in agents {
        let d = tmp.path().join("agents").join(a);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("agent.toml"), format!("[agent]\nname = \"{a}\"\n")).unwrap();
    }
    tmp
}

fn internal_principal() -> Principal {
    Principal {
        client_id: duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID.to_string(),
        scopes: HashSet::new(),
        is_external: false,
        created_at: chrono::Utc::now(),
    }
}

/// The namespace a gateway-spawned employee's MCP server resolves: internal
/// key + `DUDUCLAW_AGENT_ID`/`DUDUCLAW_AGENT_TOKEN` verified against
/// `identity.key`.
fn employee_ns(home: &Path, agent: &str) -> NamespaceContext {
    let key = duduclaw_core::ensure_identity_key(home).unwrap();
    let token = duduclaw_core::mint_identity_token(&key, agent);
    let verified = verified_employee(home, agent, &token);
    assert_eq!(verified.as_deref(), Some(agent));
    resolve_for_caller(
        &internal_principal(),
        CallerIdentity { verified_agent: verified.as_deref(), client_is_agent: false },
    )
    .unwrap()
}

fn engine(home: &Path) -> SqliteMemoryEngine {
    SqliteMemoryEngine::new(&home.join("memory.db")).unwrap()
}

fn text(v: &Value) -> String {
    v["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn is_error(v: &Value) -> bool {
    v["isError"].as_bool().unwrap_or(false)
}

async fn profile_value(mem: &SqliteMemoryEngine, ns: &NamespaceContext, predicate: &str) -> Option<String> {
    let got = h::handle_user_profile_get(&json!({ "user_id": USER }), mem, ns).await;
    let payload: Value = serde_json::from_str(&text(&got)).unwrap();
    payload["traits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["predicate"] == predicate)
        .and_then(|t| t["value"].as_str().map(str::to_string))
}

async fn distill(home: &Path, agent: &str, said: &str) {
    duduclaw_gateway::profile_distill::run_profile_distill(
        said,
        agent,
        USER,
        &home.join("memory.db"),
        home,
    )
    .await;
}

#[tokio::test]
async fn employee_tool_writes_land_where_the_gateway_reads() {
    let home = home_with(&["agnes"]);
    let ns = employee_ns(home.path(), "agnes");
    assert_eq!(ns.write_namespace, "agnes");
    let mem = engine(home.path());
    let quota = crate::mcp_memory_quota::DailyQuota::new();

    let stored = h::handle_memory_store(&json!({ "content": "客戶偏好週五開會" }), &mem, &ns, &quota).await;
    assert!(!is_error(&stored), "{stored}");
    let id = stored["memory_id"].as_str().unwrap().to_string();

    // The gateway (prompt injection, dashboard) reads by the bare id.
    let row = mem.get_by_id("agnes", &id).await.unwrap().expect("gateway sees it");
    assert_eq!(row.agent_id, "agnes");

    // And the employee's search sees what the gateway distils.
    distill(home.path(), "agnes", "call me Sam").await;
    assert_eq!(profile_value(&mem, &ns, "preferred_name").await.as_deref(), Some("Sam"));
}

#[tokio::test]
async fn internal_key_without_identity_cannot_reach_an_employee_pool() {
    let home = home_with(&["agnes"]);
    let mem = engine(home.path());
    let quota = crate::mcp_memory_quota::DailyQuota::new();
    let agnes = employee_ns(home.path(), "agnes");
    let anonymous = resolve_for_caller(&internal_principal(), CallerIdentity::default()).unwrap();
    assert_eq!(anonymous.write_namespace, shared_internal_pool());

    let mine = h::handle_memory_store(&json!({ "content": "agnes secret plan" }), &mem, &agnes, &quota).await;
    let mine_id = mine["memory_id"].as_str().unwrap().to_string();
    // No identity: cannot read it by id, and its writes do not enter agnes's pool.
    let read = h::handle_memory_read(&json!({ "id": mine_id }), &mem, &anonymous).await;
    assert!(is_error(&read));
    assert_eq!(read["error_code"], 403);
    let search = h::handle_memory_search(&json!({ "query": "secret" }), &mem, &anonymous).await;
    assert!(text(&search).contains("\"total\":0"), "{search}");
    h::handle_memory_store(&json!({ "content": "pool note" }), &mem, &anonymous, &quota).await;
    let search = h::handle_memory_search(&json!({ "query": "pool" }), &mem, &agnes).await;
    assert!(text(&search).contains("\"total\":0"), "{search}");
}

#[tokio::test]
async fn ai_record_and_distilled_trait_correct_each_other() {
    let home = home_with(&["agnes"]);
    let ns = employee_ns(home.path(), "agnes");
    let mem = engine(home.path());

    let rec = h::handle_user_profile_record(
        &json!({ "user_id": USER, "predicate": "preferred_name", "value": "Samuel" }),
        &mem,
        &ns,
    )
    .await;
    assert!(!is_error(&rec), "{rec}");
    // A distilled statement (user_profile 0.6) corrects the AI record (0.6).
    distill(home.path(), "agnes", "call me Sammy").await;
    assert_eq!(profile_value(&mem, &ns, "preferred_name").await.as_deref(), Some("Sammy"));
    // And the AI can correct the distilled value back.
    let rec = h::handle_user_profile_record(
        &json!({ "user_id": USER, "predicate": "preferred_name", "value": "Sam" }),
        &mem,
        &ns,
    )
    .await;
    assert!(!is_error(&rec), "{rec}");
    assert_eq!(profile_value(&mem, &ns, "preferred_name").await.as_deref(), Some("Sam"));
}

#[tokio::test]
async fn operator_approved_value_is_protected_from_both_sides() {
    let home = home_with(&["agnes"]);
    let ns = employee_ns(home.path(), "agnes");
    let mem = engine(home.path());
    duduclaw_memory::user_profile::record_trait_with_origin(
        &mem,
        "agnes",
        USER,
        "preferred_name",
        "Mr. Lee",
        duduclaw_memory::origin::OPERATOR.name,
        1.0,
    )
    .await
    .unwrap();

    let rec = h::handle_user_profile_record(
        &json!({ "user_id": USER, "predicate": "preferred_name", "value": "Sam" }),
        &mem,
        &ns,
    )
    .await;
    assert!(is_error(&rec));
    assert!(text(&rec).contains("more trusted value already exists"), "{rec}");

    distill(home.path(), "agnes", "call me Sammy").await;
    assert_eq!(profile_value(&mem, &ns, "preferred_name").await.as_deref(), Some("Mr. Lee"));
    // The distilled claim was held for review, not dropped.
    let broker = duduclaw_gateway::approval::ApprovalBroker::open(home.path()).unwrap();
    let pending = broker.list_pending(Some("agnes")).await.unwrap();
    assert!(
        pending.iter().any(|r| r.action_kind == "knowledge_quarantine"),
        "distilled claim should be held with a review card"
    );
}

#[tokio::test]
async fn invalidate_by_origin_acts_on_the_callers_own_pool_only() {
    let home = home_with(&["agnes", "bob"]);
    let agnes = employee_ns(home.path(), "agnes");
    let mem = engine(home.path());
    for agent in ["agnes", "bob"] {
        let meta = duduclaw_memory::TemporalMeta {
            subject: Some("weather".into()),
            predicate: Some("today".into()),
            object: Some("rain".into()),
            origin: Some(duduclaw_memory::origin::CHANNEL_DISTILL.name.into()),
            ..Default::default()
        };
        let e = duduclaw_core::types::MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.into(),
            content: "it rains today".into(),
            timestamp: chrono::Utc::now(),
            tags: vec![],
            embedding: None,
            layer: duduclaw_core::types::MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "test".into(),
        };
        mem.store_temporal(agent, e, meta).await.unwrap();
    }
    let acting = h::ai_employee_caller(
        duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID,
        Some("agnes"),
        false,
    );
    let out = h::handle_memory_invalidate_by_origin(
        &json!({ "origin": "channel" }),
        &mem,
        &agnes,
        acting.as_deref(),
        home.path(),
    )
    .await;
    assert!(!is_error(&out), "{out}");
    assert!(text(&out).contains("\"expired\":1"), "{out}");
    assert!(mem.get_at("agnes", "weather", "today", chrono::Utc::now()).await.unwrap().is_none());
    assert!(mem.get_at("bob", "weather", "today", chrono::Utc::now()).await.unwrap().is_some());
    // The 1.67.1 restriction still applies.
    let refused = h::handle_memory_invalidate_by_origin(
        &json!({ "origin": "operator" }),
        &mem,
        &agnes,
        acting.as_deref(),
        home.path(),
    )
    .await;
    assert!(is_error(&refused));
}

#[tokio::test]
async fn dashboard_forget_deletes_a_tool_written_row() {
    let home = home_with(&["agnes"]);
    let ns = employee_ns(home.path(), "agnes");
    let mem = engine(home.path());
    let quota = crate::mcp_memory_quota::DailyQuota::new();
    let stored = h::handle_memory_store(&json!({ "content": "temporary note" }), &mem, &ns, &quota).await;
    let id = stored["memory_id"].as_str().unwrap().to_string();

    let handler = duduclaw_gateway::handlers::MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "memory.forget",
            json!({ "agent_id": "agnes", "memory_id": id }),
            &duduclaw_auth::acl::UserContext::admin_fallback(),
        )
        .await;
    let v = serde_json::to_value(&frame).unwrap();
    assert_eq!(v["payload"]["forgotten"], true, "{v}");
    let read = h::handle_memory_read(&json!({ "id": id }), &mem, &ns).await;
    assert!(is_error(&read), "forgotten row is gone from the tool too");
}
