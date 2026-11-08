//! RM-120: generic `odoo_create` / `odoo_write` — explicit grant, gate order,
//! values validation, discoverable ⇔ callable, and the unchanged behaviour of
//! the existing Odoo tools.

use super::*;
use std::sync::{Arc, Mutex};

const NEVER_RESOLVE: &str = "[odoo]\nurl = 'http://localhost:1'\ndb = 'test'\napi_key_enc = 'secret://missing/never-resolve'\n";

fn text_of(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or("").to_string()
}

/// A home whose `agnes` employee carries `agent_odoo` (an `[odoo]` body, or
/// no block at all for `None`). The global config never resolves a secret,
/// so any test that reaches a credential fails loudly.
fn home_with(global: &str, agent_odoo: Option<&str>) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/agnes")).unwrap();
    std::fs::write(home.path().join("config.toml"), global).unwrap();
    let mut agent = "[agent]\nname = \"agnes\"\nstatus = \"active\"\n".to_string();
    if let Some(body) = agent_odoo {
        agent.push_str(&format!("[odoo]\n{body}\n"));
    }
    std::fs::write(home.path().join("agents/agnes/agent.toml"), agent).unwrap();
    home
}

fn pool() -> OdooState {
    Arc::new(crate::odoo_pool::OdooConnectorPool::default())
}

async fn call(tool: &str, params: Value, home: &std::path::Path, pool: &OdooState) -> Value {
    handle_odoo_tool(tool, &params, home, pool, "agnes").await
}

/// Minimal Odoo JSON-RPC mock on 127.0.0.1: authenticates as uid 7, answers
/// `create` with 42 and `write` with true, and records every ORM method.
async fn mock_odoo() -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let methods = Arc::new(Mutex::new(Vec::new()));
    let seen = methods.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut bytes = Vec::new();
            let request = loop {
                let mut buffer = [0u8; 4096];
                let size = socket.read(&mut buffer).await.unwrap_or(0);
                if size == 0 { break Value::Null; }
                bytes.extend_from_slice(&buffer[..size]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some((header, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = header.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                    }).unwrap_or(0);
                    if body.len() >= length { break serde_json::from_str::<Value>(body).unwrap_or(Value::Null); }
                }
            };
            let orm = request["params"]["args"][4].as_str().unwrap_or("").to_string();
            if !orm.is_empty() { seen.lock().unwrap().push(orm.clone()); }
            let result = match request["params"]["method"].as_str().unwrap_or("") {
                "authenticate" => serde_json::json!(7),
                "version" => serde_json::json!({"server_version":"19.0"}),
                _ if orm == "create" => serde_json::json!(42),
                _ if orm == "write" => serde_json::json!(true),
                _ if orm == "fields_get" => serde_json::json!({"id":{"type":"integer"},"name":{"type":"char"}}),
                _ => serde_json::json!([]),
            };
            let body = serde_json::json!({"jsonrpc":"2.0","id":1,"result":result}).to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (url, methods, server)
}

fn mock_global(url: &str) -> String {
    format!("[odoo]\nurl = '{url}'\ndb = 'test'\nusername = 'tester'\napi_key_enc = 'literal-test-key'\n")
}

/// `crm.lead` with one writable field, one read-only stored field, one
/// computed (non-stored) field and one many2many; `access` is its ACL row.
fn lead_snapshot(identity: &str, access: Option<Value>) -> duduclaw_odoo::schema::SchemaSnapshot {
    let field = |name: &str, ttype: &str, readonly: bool, store: bool| serde_json::json!({
        "name": name, "ttype": ttype, "label": name, "relation": null,
        "required": false, "readonly": readonly, "store": store, "custom": false,
    });
    let mut access_rights = serde_json::Map::new();
    if let Some(access) = access {
        access_rights.insert("crm.lead".into(), access);
    }
    serde_json::from_value(serde_json::json!({
        "schema_version": 1, "access_rights": access_rights, "scanned_at": "2026-11-08T00:00:00Z",
        "server_version": "19.0", "connection_identity": identity, "modules": [],
        "total_models": 1, "truncated": false, "truncation_reasons": [],
        "models": [{
            "model": "crm.lead", "name": "Lead", "transient": false, "custom": false,
            "modules": null, "info": null, "total_fields": 4, "truncated": false, "error": null,
            "fields": [
                field("name", "char", false, true),
                field("kanban_state", "selection", true, true),
                field("display_name", "char", false, false),
                field("tag_ids", "many2many", false, true),
            ],
        }],
    })).unwrap()
}

fn full_access(create: Value, write: Value) -> Value {
    serde_json::json!({"read": true, "create": create, "write": write, "unlink": false, "checked_at": "2026-11-08T00:00:00Z"})
}

/// Connect through `odoo_connect`, then store a snapshot for exactly this
/// connection identity — what the gateway's background scan would write.
async fn connect_and_snapshot(home: &std::path::Path, pool: &OdooState, access: Option<Value>) {
    let connected = call("odoo_connect", serde_json::json!({}), home, pool).await;
    assert!(!connected["isError"].as_bool().unwrap_or(false), "{connected}");
    let conn = pool.cached_connector("agnes").await.unwrap();
    let profile = conn.schema_profile(&pool.pool_key("agnes").await.1);
    duduclaw_odoo::schema::save_snapshot(home, &profile, &lead_snapshot(&conn.connection_identity(), access)).unwrap();
}

// ── explicit grant gate ─────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_refused_without_override() {
    let home = home_with(NEVER_RESOLVE, None);
    let pool = pool();
    let result = call("odoo_create", serde_json::json!({"model":"crm.lead","values":{"name":"x"}}), home.path(), &pool).await;
    let text = text_of(&result);
    assert!(result["isError"].as_bool().unwrap(), "{text}");
    assert!(text.contains("explicit grant gate"), "{text}");
    assert!(text.contains("no own Odoo settings"), "{text}");
    assert!(text.contains("員工 → 編輯 → 整合 → Odoo"), "{text}");
    assert!(pool.slots().await.is_empty(), "refused before any connection");
    // No agent.toml at all is the same answer.
    std::fs::remove_file(home.path().join("agents/agnes/agent.toml")).unwrap();
    let text = text_of(&call("odoo_create", serde_json::json!({"model":"crm.lead","values":{"name":"x"}}), home.path(), &pool).await);
    assert!(text.contains("explicit grant gate"), "{text}");
    assert!(pool.slots().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_refused_with_empty_allowed_actions() {
    for agent in [
        "allowed_models = ['crm.lead']",
        "allowed_models = ['crm.lead']\nallowed_actions = []",
        "allowed_models = ['crm.lead']\nallowed_actions = ['read', 'search', 'write']",
        // An empty model list grants nothing either.
        "allowed_actions = ['create']",
    ] {
        let home = home_with(NEVER_RESOLVE, Some(agent));
        let pool = pool();
        let result = call("odoo_create", serde_json::json!({"model":"crm.lead","values":{"name":"x"}}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{agent}: {text}");
        assert!(text.contains("explicit grant gate"), "{agent}: {text}");
        assert!(text.contains("does not grant"), "{agent}: {text}");
        assert!(pool.slots().await.is_empty(), "{agent}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_write_refused_when_model_not_listed() {
    for agent in [
        "allowed_models = ['sale.order']\nallowed_actions = ['write']",
        // A wildcard is not a model name.
        "allowed_models = ['*']\nallowed_actions = ['write']",
        // The qualified verb names another model.
        "allowed_models = ['crm.lead']\nallowed_actions = ['write:sale.order']",
    ] {
        let home = home_with(NEVER_RESOLVE, Some(agent));
        let pool = pool();
        let result = call("odoo_write", serde_json::json!({"model":"crm.lead","ids":[1],"values":{"name":"x"}}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{agent}: {text}");
        assert!(text.contains("explicit grant gate"), "{agent}: {text}");
        assert!(text.contains("retrying the same call will not succeed"), "{agent}: {text}");
        assert!(pool.slots().await.is_empty(), "{agent}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_allowed_with_explicit_grant() {
    let (url, methods, server) = mock_odoo().await;
    let home = home_with(&mock_global(&url), Some("allowed_models = ['crm.lead']\nallowed_actions = ['read', 'search', 'create', 'write:crm.lead']"));
    let pool = pool();
    connect_and_snapshot(home.path(), &pool, Some(full_access(serde_json::json!(true), serde_json::json!(true)))).await;

    let result = call("odoo_create", serde_json::json!({
        "model": "crm.lead",
        "values": {"name": "Deal", "tag_ids": [[6, 0, [1, 2]], [4, 3]]},
    }), home.path(), &pool).await;
    let text = text_of(&result);
    assert!(!result["isError"].as_bool().unwrap_or(false), "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["id"], 42);
    assert_eq!(body["model"], "crm.lead");

    // Stringified arguments and duplicate ids are accepted for write.
    let result = call("odoo_write", serde_json::json!({
        "model": "crm.lead", "ids": "[5, 5, 6]", "values": "{\"name\": \"Renamed\"}",
    }), home.path(), &pool).await;
    let text = text_of(&result);
    assert!(!result["isError"].as_bool().unwrap_or(false), "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["ids"], serde_json::json!([5, 6]));
    let seen = methods.lock().unwrap().clone();
    assert_eq!(seen.iter().filter(|m| *m == "create").count(), 1, "{seen:?}");
    assert_eq!(seen.iter().filter(|m| *m == "write").count(), 1, "{seen:?}");
    // Values themselves never reach the audit line, field names do.
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(audit.contains("odoo_write") && audit.contains("fields=name"), "{audit}");
    assert!(!audit.contains("Renamed") && !audit.contains("Deal"), "{audit}");
    server.abort();
}

// ── values validation ───────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn odoo_write_rejects_unlink_commands() {
    let home = home_with(NEVER_RESOLVE, Some("allowed_models = ['crm.lead']\nallowed_actions = ['write']"));
    let pool = pool();
    for values in [
        serde_json::json!({"tag_ids": [[3, 5]]}),
        serde_json::json!({"tag_ids": [[2, 5]]}),
        serde_json::json!({"tag_ids": [[5]]}),
        serde_json::json!({"tag_ids": [[4, 1], [3, 5]]}),
        // Nested inside a create command.
        serde_json::json!({"order_line": [[0, 0, {"tax_ids": [[5, 0, 0]]}]]}),
    ] {
        let result = call("odoo_write", serde_json::json!({"model":"crm.lead","ids":[1],"values":values}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{values}: {text}");
        assert!(text.contains("x2many command"), "{values}: {text}");
        assert!(text.contains("This capability is not available"), "{values}: {text}");
        assert!(text.contains("這項功能不提供"), "{values}: {text}");
        assert!(!text.to_lowercase().contains("ask an admin") && !text.contains("管理員"), "{text}");
        assert!(pool.slots().await.is_empty(), "refused before connecting");
    }
    // An unrecognised command is refused too (fail closed).
    let text = text_of(&call("odoo_write", serde_json::json!({"model":"crm.lead","ids":[1],"values":{"tag_ids":[[9, 1]]}}), home.path(), &pool).await);
    assert!(text.contains("unrecognised x2many command"), "{text}");
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_rejects_readonly_or_unknown_field() {
    let (url, methods, server) = mock_odoo().await;
    let home = home_with(&mock_global(&url), Some("allowed_models = ['crm.lead']\nallowed_actions = ['create']"));
    let pool = pool();
    connect_and_snapshot(home.path(), &pool, Some(full_access(serde_json::json!(true), serde_json::json!(true)))).await;
    for (values, expected) in [
        (serde_json::json!({"name": "ok", "kanban_state": "done"}), "read-only field"),
        (serde_json::json!({"nope": 1}), "is not a field of 'crm.lead'"),
        (serde_json::json!({"display_name": "x"}), "computed (non-stored) field"),
    ] {
        let result = call("odoo_create", serde_json::json!({"model":"crm.lead","values":values}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{values}: {text}");
        assert!(text.contains(expected), "{values}: {text}");
    }
    assert!(!methods.lock().unwrap().iter().any(|m| m == "create"), "no create RPC was sent");
    server.abort();
}

#[test]
fn generic_write_argument_bounds() {
    let ok = parse_generic_write_args("odoo_write", &serde_json::json!({"model":"crm.lead","ids":[3, "3", 4],"values":{"name":"x"}})).unwrap();
    assert_eq!(ok.ids, vec![3, 4]);
    let too_many: Vec<i64> = (1..=101).collect();
    for params in [
        serde_json::json!({"model":"Bad Model","ids":[1],"values":{"name":"x"}}),
        serde_json::json!({"model":"crm.lead","ids":[0],"values":{"name":"x"}}),
        serde_json::json!({"model":"crm.lead","ids":[-1],"values":{"name":"x"}}),
        serde_json::json!({"model":"crm.lead","ids":[1.5],"values":{"name":"x"}}),
        serde_json::json!({"model":"crm.lead","ids":[],"values":{"name":"x"}}),
        serde_json::json!({"model":"crm.lead","ids":too_many,"values":{"name":"x"}}),
        serde_json::json!({"model":"crm.lead","values":{"name":"x"}}),
        serde_json::json!({"model":"crm.lead","ids":[1],"values":{}}),
        serde_json::json!({"model":"crm.lead","ids":[1],"values":[1]}),
        serde_json::json!({"model":"crm.lead","ids":[1],"values":"not json"}),
        serde_json::json!({"model":"crm.lead","ids":[1],"values":{"Bad Field":1}}),
        serde_json::json!({"model":"crm.lead","ids":[1],"values":{"name":"x".repeat(GENERIC_WRITE_MAX_VALUES_BYTES)}}),
    ] {
        assert!(parse_generic_write_args("odoo_write", &params).is_err(), "{params}");
    }
    // 100 distinct ids are fine; duplicates do not count twice.
    let many: Vec<i64> = (1..=100).chain(1..=100).collect();
    assert_eq!(parse_generic_write_args("odoo_write", &serde_json::json!({"model":"crm.lead","ids":many,"values":{"name":"x"}})).unwrap().ids.len(), 100);
    // odoo_create takes no ids.
    assert!(parse_generic_write_args("odoo_create", &serde_json::json!({"model":"crm.lead","values":{"name":"x"}})).unwrap().ids.is_empty());
}

// ── account ACL ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_refused_when_account_acl_unknown() {
    for (access, expected) in [
        (None, "unknown"),
        (Some(full_access(Value::Null, serde_json::json!(true))), "unknown"),
        (Some(full_access(serde_json::json!(false), serde_json::json!(true))), "has no 'create' right"),
    ] {
        let (url, methods, server) = mock_odoo().await;
        let home = home_with(&mock_global(&url), Some("allowed_models = ['crm.lead']\nallowed_actions = ['create']"));
        let pool = pool();
        connect_and_snapshot(home.path(), &pool, access.clone()).await;
        let result = call("odoo_create", serde_json::json!({"model":"crm.lead","values":{"name":"x"}}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{access:?}: {text}");
        assert!(text.contains("Odoo account permission"), "{access:?}: {text}");
        assert!(text.contains(expected), "{access:?}: {text}");
        assert!(!methods.lock().unwrap().iter().any(|m| m == "create"), "no create RPC for {access:?}");
        server.abort();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_without_schema_snapshot_asks_to_wait_for_the_scan() {
    let (url, methods, server) = mock_odoo().await;
    let home = home_with(&mock_global(&url), Some("allowed_models = ['crm.lead']\nallowed_actions = ['create']"));
    let pool = pool();
    let result = call("odoo_create", serde_json::json!({"model":"crm.lead","values":{"name":"x"}}), home.path(), &pool).await;
    let text = text_of(&result);
    assert!(result["isError"].as_bool().unwrap(), "{text}");
    assert!(text.contains("結構正在背景準備中"), "{text}");
    assert!(!methods.lock().unwrap().iter().any(|m| m == "create"));
    server.abort();
}

// ── hard-unavailable wording ────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn odoo_write_system_readonly_model_says_capability_unavailable() {
    for agent in [
        "allowed_models = ['ir.model']\nallowed_actions = ['write']",
        "allowed_models = ['ir.model']\nallowed_actions = ['write']\nunblock_models = ['ir.model']",
    ] {
        let home = home_with(NEVER_RESOLVE, Some(agent));
        let pool = pool();
        let result = call("odoo_write", serde_json::json!({"model":"ir.model","ids":[1],"values":{"name":"x"}}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{agent}: {text}");
        assert!(text.contains("This capability is not available"), "{agent}: {text}");
        assert!(text.contains("這項功能不提供"), "{agent}: {text}");
        assert!(!text.to_lowercase().contains("admin"), "{agent}: must not send the user to an admin: {text}");
        assert!(pool.slots().await.is_empty(), "{agent}");
    }
    // The same rule now covers odoo_execute writes on a read-only system model.
    let home = home_with(NEVER_RESOLVE, Some("profile = 'p'"));
    let text = text_of(&call("odoo_execute", serde_json::json!({"model":"ir.model","method":"write","ids":"[1]"}), home.path(), &pool()).await);
    assert!(text.contains("這項功能不提供") && !text.to_lowercase().contains("admin"), "{text}");
}

// ── discoverable ⇔ callable ─────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn tools_list_hides_generic_write_without_grant() {
    let principal = crate::mcp_auth::Principal {
        client_id: "agnes".into(),
        scopes: std::collections::HashSet::new(),
        is_external: false,
        created_at: chrono::Utc::now(),
    };
    for (agent, create, write) in [
        (None, false, false),
        (Some("profile = 'p'"), false, false),
        (Some("allowed_models = ['crm.lead']"), false, false),
        (Some("allowed_actions = ['create', 'write']"), false, false),
        (Some("allowed_models = ['*']\nallowed_actions = ['create', 'write']"), false, false),
        (Some("allowed_models = ['crm.lead']\nallowed_actions = ['create']"), true, false),
        (Some("allowed_models = ['crm.lead']\nallowed_actions = ['write:crm.lead']"), false, true),
        (Some("allowed_models = ['crm.lead']\nallowed_actions = ['write:sale.order']"), false, false),
        (Some("allowed_models = ['crm.lead', 'sale.order']\nallowed_actions = ['create', 'write']"), true, true),
    ] {
        let home = home_with(NEVER_RESOLVE, agent);
        let names = visible_tool_names(&principal, home.path(), "agnes").await;
        assert_eq!(names.contains(&"odoo_create"), create, "{agent:?}");
        assert_eq!(names.contains(&"odoo_write"), write, "{agent:?}");
        assert!(names.contains(&"odoo_crm_create_lead"), "existing tools stay listed: {agent:?}");
        // Same predicate as the dispatch gate.
        let cfg = load_generic_write_override(home.path(), "agnes").unwrap();
        for (tool, verb, listed) in [("odoo_create", "create", create), ("odoo_write", "write", write)] {
            let callable = cfg.as_ref().is_some_and(|c| c.allowed_models.iter().any(|m| generic_write_grant(Some(c), verb, m).is_ok()));
            assert_eq!(callable, listed, "{tool}: {agent:?}");
        }
    }
    // An unreadable [odoo] block hides both (fail closed).
    let home = home_with(NEVER_RESOLVE, Some("allowed_models = 42"));
    let names = visible_tool_names(&principal, home.path(), "agnes").await;
    assert!(!names.contains(&"odoo_create") && !names.contains(&"odoo_write"));
}

#[test]
fn generic_write_tools_are_modify_and_need_odoo_write() {
    for tool in ["odoo_create", "odoo_write"] {
        assert_eq!(duduclaw_core::effect_of_builtin(tool), Some(duduclaw_core::ToolEffect::Modify), "{tool}");
        assert_eq!(crate::mcp_auth::tool_requires_scope(tool), Some(crate::mcp_auth::Scope::OdooWrite), "{tool}");
        assert_eq!(duduclaw_core::tool_effect::reads_private(tool), Some(false), "{tool}");
        assert_eq!(duduclaw_core::tool_effect::egress_capable(tool), Some(true), "{tool}");
    }
    let schema = build_tool_schema(tools().find(|t| t.name == "odoo_write").unwrap());
    assert_eq!(schema["inputSchema"]["properties"]["values"]["type"], "object");
    assert_eq!(schema["inputSchema"]["properties"]["ids"]["items"]["type"], "integer");
    assert_eq!(schema["inputSchema"]["required"], serde_json::json!(["model", "ids", "values"]));
}

// ── existing tools unchanged ────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn existing_tools_behaviour_unchanged_without_override() {
    // No override and empty lists stay permissive for the existing tools.
    assert!(crate::odoo_pool::check_action_permission(None, "create", "crm.lead").is_ok());
    let empty = duduclaw_odoo::AgentOdooConfig { profile: Some("p".into()), ..Default::default() };
    assert!(crate::odoo_pool::check_action_permission(Some(&empty), "write", "crm.lead").is_ok());
    // No credential configured: every policy gate runs, then the local
    // connection precheck stops the call without resolving a secret.
    let no_credential = "[odoo]\nurl = 'http://localhost:1'\ndb = 'test'\n";
    for agent in [None, Some("profile = 'p'")] {
        let home = home_with(no_credential, agent);
        let pool = pool();
        for (tool, params) in [
            ("odoo_crm_create_lead", serde_json::json!({"name":"lead"})),
            ("odoo_sale_create_quotation", serde_json::json!({"partner_id":1,"product_id":2})),
            ("odoo_crm_update_stage", serde_json::json!({"lead_id":1,"stage_name":"Won"})),
        ] {
            let text = text_of(&call(tool, params, home.path(), &pool).await);
            assert!(!text.contains("explicit grant gate"), "{tool} {agent:?}: {text}");
            assert!(!text.contains("permission denied"), "{tool} {agent:?}: {text}");
            assert!(text.contains("not connected") && text.contains("no Odoo credential"), "{tool} {agent:?}: {text}");
            assert!(pool.slots().await.is_empty());
        }
    }
}
