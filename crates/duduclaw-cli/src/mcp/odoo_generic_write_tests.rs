//! RM-120: generic `odoo_create` / `odoo_write` — explicit grant and separate
//! opt-in, never-writable list, gate order, values validation (x2many by
//! field type, nested commands on related models, related fields,
//! archiving), discoverable ⇔ callable, audit, and the unchanged behaviour of
//! the existing Odoo tools.

use super::*;
use std::sync::{Arc, Mutex};

const NEVER_RESOLVE: &str = "[odoo]\nurl = 'http://localhost:1'\ndb = 'test'\napi_key_enc = 'secret://missing/never-resolve'\n";

/// The regular grant plus the per-table opt-in for both tools on `crm.lead`.
const LEAD_FULL: &str = "allowed_models = ['crm.lead']\nallowed_actions = ['read', 'search', 'create', 'write', 'generic_create:crm.lead', 'generic_write:crm.lead']";

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
/// `create` with 42 and `write` with true (or with an RPC error carrying
/// `fault_text` when set), and records every ORM method.
async fn mock_odoo_with(fault_text: Option<&'static str>) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
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
            let mutating = orm == "create" || orm == "write";
            let body = match fault_text {
                Some(text) if mutating => serde_json::json!({"jsonrpc":"2.0","id":1,"error":{
                    "code": 200, "message": "Odoo Server Error",
                    "data": {"name": "odoo.exceptions.ValidationError", "message": text},
                }}),
                _ => {
                    let result = match request["params"]["method"].as_str().unwrap_or("") {
                        "authenticate" => serde_json::json!(7),
                        "version" => serde_json::json!({"server_version":"19.0"}),
                        _ if orm == "create" => serde_json::json!(42),
                        _ if orm == "write" => serde_json::json!(true),
                        _ if orm == "fields_get" => serde_json::json!({"id":{"type":"integer"},"name":{"type":"char"}}),
                        _ => serde_json::json!([]),
                    };
                    serde_json::json!({"jsonrpc":"2.0","id":1,"result":result})
                }
            }.to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (url, methods, server)
}

async fn mock_odoo() -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    mock_odoo_with(None).await
}

fn mock_global(url: &str) -> String {
    format!("[odoo]\nurl = '{url}'\ndb = 'test'\nusername = 'tester'\napi_key_enc = 'literal-test-key'\n")
}

fn field(name: &str, ttype: &str, relation: Option<&str>, readonly: bool, store: bool, related: Option<bool>) -> Value {
    let mut field = serde_json::json!({
        "name": name, "ttype": ttype, "label": name, "relation": relation,
        "required": false, "readonly": readonly, "store": store, "custom": false,
    });
    if let Some(related) = related {
        field["related"] = Value::Bool(related);
    }
    field
}

fn plain(name: &str, ttype: &str) -> Value {
    field(name, ttype, None, false, true, Some(false))
}

fn rel(name: &str, ttype: &str, relation: &str) -> Value {
    field(name, ttype, Some(relation), false, true, Some(false))
}

fn scanned_model(model: &str, fields: Vec<Value>) -> Value {
    serde_json::json!({
        "model": model, "name": model, "transient": false, "custom": false,
        "modules": null, "info": null, "total_fields": fields.len(), "truncated": false, "error": null,
        "fields": fields,
    })
}

/// `crm.lead` and the tables its x2many fields reach. `access` is the ACL
/// row of `crm.lead`; `crm.tag` and `crm.lead.line` are fully writable.
fn lead_snapshot(identity: &str, access: Option<Value>) -> duduclaw_odoo::schema::SchemaSnapshot {
    let mut access_rights = serde_json::Map::new();
    if let Some(access) = access {
        access_rights.insert("crm.lead".into(), access);
    }
    for model in ["crm.tag", "crm.lead.line", "mail.followers", "mail.message", "res.users", "res.partner"] {
        access_rights.insert(model.into(), full_access(serde_json::json!(true), serde_json::json!(true)));
    }
    serde_json::from_value(serde_json::json!({
        "schema_version": 1, "access_rights": access_rights, "scanned_at": "2026-11-08T00:00:00Z",
        "server_version": "19.0", "connection_identity": identity, "modules": [],
        "total_models": 7, "truncated": false, "truncation_reasons": [],
        "models": [
            scanned_model("crm.lead", vec![
                plain("name", "char"),
                plain("active", "boolean"),
                plain("lead_props", "properties"),
                plain("lead_props_def", "properties_definition"),
                field("kanban_state", "selection", None, true, true, Some(false)),
                field("display_name", "char", None, false, false, Some(false)),
                field("partner_email", "char", None, false, true, Some(true)),
                field("legacy_note", "char", None, false, true, None),
                rel("tag_ids", "many2many", "crm.tag"),
                rel("line_ids", "one2many", "crm.lead.line"),
                rel("message_follower_ids", "one2many", "mail.followers"),
                rel("message_ids", "one2many", "mail.message"),
                rel("user_ids", "many2many", "res.users"),
                rel("partner_ids", "many2many", "res.partner"),
                // The scan dropped the relation (blocked / switched-off table).
                field("hidden_ids", "many2many", None, false, true, Some(false)),
            ]),
            scanned_model("crm.tag", vec![plain("name", "char"), plain("color", "integer")]),
            scanned_model("crm.lead.line", vec![
                plain("name", "char"),
                plain("line_props", "properties"),
                plain("active", "boolean"),
                field("price", "float", None, true, true, Some(false)),
                rel("tag_ids", "many2many", "crm.tag"),
                rel("child_ids", "one2many", "crm.lead.line"),
            ]),
            scanned_model("mail.followers", vec![plain("res_model", "char"), plain("res_id", "integer"), rel("partner_id", "many2one", "res.partner")]),
            scanned_model("mail.message", vec![plain("body", "html")]),
            scanned_model("res.users", vec![plain("name", "char"), rel("groups_id", "many2many", "res.groups")]),
        ],
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

fn lead_of(snapshot: &duduclaw_odoo::schema::SchemaSnapshot) -> &duduclaw_odoo::schema::ScannedModel {
    snapshot.models.iter().find(|m| m.model == "crm.lead").unwrap()
}

/// Schema check with an authorizer that records every related use and
/// allows it.
fn schema_check(values: Value) -> (std::result::Result<(), String>, Vec<(String, &'static str)>) {
    let snapshot = lead_snapshot("id", None);
    let mut uses = Vec::new();
    let mut record = |model: &str, use_verb: &'static str| -> std::result::Result<(), String> {
        uses.push((model.to_string(), use_verb));
        Ok(())
    };
    let result = check_values_against_schema(&snapshot, lead_of(&snapshot), values.as_object().unwrap(), &mut record);
    (result, uses)
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
        // The opt-in alone grants nothing.
        "allowed_models = ['crm.lead']\nallowed_actions = ['generic_create:crm.lead']",
        // An empty model list grants nothing either.
        "allowed_actions = ['create', 'generic_create:crm.lead']",
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
        "allowed_models = ['sale.order']\nallowed_actions = ['write', 'generic_write:crm.lead']",
        // A wildcard is not a model name.
        "allowed_models = ['*']\nallowed_actions = ['write', 'generic_write:crm.lead']",
        // The qualified verb names another model.
        "allowed_models = ['crm.lead']\nallowed_actions = ['write:sale.order', 'generic_write:crm.lead']",
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

// ── separate opt-in (owner decision 2026-11-08) ─────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn existing_full_grants_do_not_gain_generic_tools_without_opt_in() {
    for (agent, tool) in [
        // A configuration that existed before RM-120: a full grant, no opt-in.
        ("allowed_models = ['crm.lead']\nallowed_actions = ['read', 'search', 'create', 'write']", "odoo_create"),
        ("allowed_models = ['crm.lead']\nallowed_actions = ['read', 'search', 'create', 'write']", "odoo_write"),
        // The opt-in for the other tool / another table / no table.
        ("allowed_models = ['crm.lead']\nallowed_actions = ['create', 'write', 'generic_write:crm.lead']", "odoo_create"),
        ("allowed_models = ['crm.lead']\nallowed_actions = ['create', 'write', 'generic_write:sale.order']", "odoo_write"),
        ("allowed_models = ['crm.lead']\nallowed_actions = ['create', 'write', 'generic_write', 'generic_write:*']", "odoo_write"),
    ] {
        let home = home_with(NEVER_RESOLVE, Some(agent));
        let pool = pool();
        let result = call(tool, serde_json::json!({"model":"crm.lead","ids":[1],"values":{"name":"x"}}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{agent} {tool}: {text}");
        assert!(text.contains("generic write opt-in gate"), "{agent} {tool}: {text}");
        assert!(text.contains("允許通用新增／修改"), "{agent} {tool}: {text}");
        assert!(pool.slots().await.is_empty(), "{agent} {tool}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_allowed_with_explicit_grant() {
    let (url, methods, server) = mock_odoo().await;
    let home = home_with(&mock_global(&url), Some(
        "allowed_models = ['crm.lead', 'crm.lead.line']\nallowed_actions = ['read', 'search', 'create', 'write:crm.lead', \
         'generic_create:crm.lead', 'generic_write:crm.lead', 'generic_create:crm.lead.line']",
    ));
    let pool = pool();
    connect_and_snapshot(home.path(), &pool, Some(full_access(serde_json::json!(true), serde_json::json!(true)))).await;

    let result = call("odoo_create", serde_json::json!({
        "model": "crm.lead",
        "values": {
            "name": "Deal",
            "active": true,
            "tag_ids": [[4, 1], [4, 2]],
            "line_ids": [[0, 0, {"name": "Line", "tag_ids": [[4, 3]]}]],
        },
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
    assert!(!audit.contains("Renamed") && !audit.contains("Deal") && !audit.contains("Line"), "{audit}");
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_error_is_audited_as_class_and_length_only() {
    let (url, _methods, server) = mock_odoo_with(Some("Lead Zebra-Unicorn rejected for customer Quokka")).await;
    let home = home_with(&mock_global(&url), Some(LEAD_FULL));
    let pool = pool();
    connect_and_snapshot(home.path(), &pool, Some(full_access(serde_json::json!(true), serde_json::json!(true)))).await;
    let result = call("odoo_create", serde_json::json!({"model":"crm.lead","values":{"name":"x"}}), home.path(), &pool).await;
    let text = text_of(&result);
    assert!(result["isError"].as_bool().unwrap(), "{text}");
    assert!(text.contains("ValidationError"), "the caller still sees the screened error: {text}");
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(audit.contains("error class=ValidationError; length="), "{audit}");
    assert!(!audit.contains("Zebra-Unicorn") && !audit.contains("Quokka"), "remote text never reaches the audit: {audit}");
    server.abort();
}

// ── values validation ───────────────────────────────────────────────────────

#[test]
fn properties_fields_are_refused() {
    for values in [
        serde_json::json!({"lead_props": [{"name": "a", "definition_changed": true}]}),
        serde_json::json!({"lead_props_def": [{"name": "a"}]}),
        serde_json::json!({"line_ids": [[0, 0, {"name": "x", "line_props": [{"name": "a", "definition_changed": true}]}]]}),
    ] {
        let (result, _) = schema_check(values.clone());
        let error = result.unwrap_err();
        assert!(error.contains("properties"), "{values}: {error}");
    }
    assert!(schema_check(serde_json::json!({"name": "ok"})).0.is_ok());
}

#[test]
fn denial_audit_strips_control_characters() {
    let home = tempfile::tempdir().unwrap();
    deny(home.path(), "agnes", "odoo_write", "crm.lead\u{1b}[31m\nFORGED", "write", "bad 'x\r\ny\u{7f}z'");
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert_eq!(audit.lines().count(), 1, "{audit}");
    for escaped in ["\\u001b", "\\n", "\\r", "\\u007f"] {
        assert!(!audit.contains(escaped), "{escaped} in {audit}");
    }
    assert!(audit.contains("crm.lead?[31m?FORGED") && audit.contains("bad 'x??y?z'"), "{audit}");
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_write_rejects_unlink_commands() {
    let home = home_with(NEVER_RESOLVE, Some(LEAD_FULL));
    let pool = pool();
    for values in [
        serde_json::json!({"tag_ids": [[3, 5]]}),
        serde_json::json!({"tag_ids": [[2, 5]]}),
        serde_json::json!({"tag_ids": [[5]]}),
        serde_json::json!({"tag_ids": [[4, 1], [3, 5]]}),
        // Command 6 replaces the whole set: refused on every x2many.
        serde_json::json!({"tag_ids": [[6, 0, [1, 2]]]}),
        serde_json::json!({"line_ids": [[6, 0, [1]]]}),
        // Nested inside a create command.
        serde_json::json!({"order_line": [[0, 0, {"tax_ids": [[5, 0, 0]]}]]}),
        serde_json::json!({"order_line": [[0, 0, {"tax_ids": [[6, 0, [1]]]}]]}),
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

#[test]
fn x2many_values_are_judged_by_field_type() {
    // false / null clear, an empty list may clear, a plain id list replaces.
    for name in ["tag_ids", "line_ids"] {
        for (value, expected) in [
            (Value::Bool(false), "false / null"),
            (Value::Null, "false / null"),
            (serde_json::json!([]), "empty list"),
            (serde_json::json!([1]), "plain id list"),
            (serde_json::json!([1, 2]), "plain id list"),
            (serde_json::json!([[4, 1], 2]), "plain id list"),
            (serde_json::json!("1,2"), "list of x2many commands"),
            (serde_json::json!([[6, 0, [1]]]), "x2many command 6"),
        ] {
            let mut values = serde_json::Map::new();
            values.insert(name.to_string(), value.clone());
            let (result, _) = schema_check(Value::Object(values));
            let error = result.unwrap_err();
            assert!(error.contains(expected), "{name} = {value}: {error}");
        }
    }
    // Malformed commands.
    for value in [
        serde_json::json!([[4]]),
        serde_json::json!([[4, 0]]),
        serde_json::json!([[4, "x"]]),
        serde_json::json!([[1, 0, {"name": "x"}]]),
        serde_json::json!([[0, 0]]),
        serde_json::json!([[0, 0, [1]]]),
    ] {
        assert!(schema_check(serde_json::json!({"tag_ids": value})).0.is_err(), "{value}");
    }
    // An x2many whose related table the scan dropped is refused.
    let (result, _) = schema_check(serde_json::json!({"hidden_ids": [[4, 1]]}));
    assert!(result.unwrap_err().contains("not in the scanned schema"));
    // Linking: many2many 4 is a link, one2many 4 moves the record (a write).
    let (result, uses) = schema_check(serde_json::json!({"tag_ids": [[4, 1]], "line_ids": [[4, 9]]}));
    result.unwrap();
    assert!(uses.contains(&("crm.tag".to_string(), LINK_USE)), "{uses:?}");
    assert!(uses.contains(&("crm.lead.line".to_string(), "write")), "{uses:?}");
    // A field that is not an x2many keeps its normal values, false included.
    schema_check(serde_json::json!({"name": false})).0.unwrap();
}

#[test]
fn nested_commands_are_checked_against_the_related_model() {
    let (result, uses) = schema_check(serde_json::json!({
        "line_ids": [[0, 0, {"name": "a", "tag_ids": [[4, 1], [0, 0, {"name": "t"}]]}], [1, 7, {"name": "b"}]],
    }));
    result.unwrap();
    for expected in [("crm.lead.line", "create"), ("crm.lead.line", "write"), ("crm.tag", LINK_USE), ("crm.tag", "create")] {
        assert!(uses.contains(&(expected.0.to_string(), expected.1)), "{expected:?} in {uses:?}");
    }
    // The related model's own field rules apply.
    for (values, expected) in [
        (serde_json::json!({"line_ids": [[0, 0, {"price": 1}]]}), "read-only field of 'crm.lead.line'"),
        (serde_json::json!({"line_ids": [[0, 0, {"nope": 1}]]}), "is not a field of 'crm.lead.line'"),
        (serde_json::json!({"line_ids": [[1, 3, {"active": false}]]}), "archives the record"),
        (serde_json::json!({"line_ids": [[0, 0, {"tag_ids": false}]]}), "false / null"),
        (serde_json::json!({"line_ids": [[0, 0, {"Bad Key": 1}]]}), "not an Odoo field name"),
    ] {
        let error = schema_check(values.clone()).0.unwrap_err();
        assert!(error.contains(expected), "{values}: {error}");
    }
    // The authorizer runs before the related values are looked at.
    let snapshot = lead_snapshot("id", None);
    let mut refuse = |model: &str, use_verb: &'static str| -> std::result::Result<(), String> {
        Err(format!("refused {use_verb} on {model}"))
    };
    let values = serde_json::json!({"line_ids": [[0, 0, {"nope": 1}]]});
    let error = check_values_against_schema(&snapshot, lead_of(&snapshot), values.as_object().unwrap(), &mut refuse).unwrap_err();
    assert!(error.contains("refused create on crm.lead.line"), "{error}");
    // Nesting is bounded.
    let deep = serde_json::json!({"line_ids": [[0, 0, {"child_ids": [[0, 0, {"child_ids": [[0, 0, {"child_ids": [[0, 0, {"name": "x"}]]}]]}]]}]]});
    let error = schema_check(deep.clone()).0.unwrap_err();
    assert!(error.contains("nested more than 3 levels"), "{error}");
    assert!(check_x2many_commands("line_ids", &deep["line_ids"]).unwrap_err().contains("nested more than 3 levels"));
    let three = serde_json::json!({"line_ids": [[0, 0, {"child_ids": [[0, 0, {"child_ids": [[0, 0, {"name": "x"}]]}]]}]]});
    schema_check(three).0.unwrap();
}

#[test]
fn related_model_gate_refuses_ungranted_blocked_and_never_writable_tables() {
    let global = duduclaw_odoo::OdooConfig::default();
    let cfg = duduclaw_odoo::AgentOdooConfig::parse_agent_toml(&format!("[odoo]\n{LEAD_FULL}\n")).unwrap().unwrap();
    let gate = |verb: &str, model: &str| generic_write_model_gate(Some(&cfg), &global, &[], verb, model);
    assert_eq!(gate("create", "crm.lead"), Ok(()));
    // Ungranted related table: create / update need its own grant + opt-in.
    assert!(matches!(gate("create", "crm.lead.line"), Err(GenericWriteDenial::Grant(r)) if r.contains("explicit grant gate")));
    // A link needs no grant of its own …
    assert_eq!(gate(LINK_USE, "crm.tag"), Ok(()));
    // … but still the block list and the never-writable list.
    assert!(matches!(gate(LINK_USE, "res.partner"), Err(GenericWriteDenial::Block(duduclaw_odoo::BlockDenial::SecurityDefault))));
    for model in ["mail.followers", "mail.message", "res.users", "res.groups"] {
        assert_eq!(gate(LINK_USE, model), Err(GenericWriteDenial::NeverWritable), "{model}");
        assert_eq!(gate("create", model), Err(GenericWriteDenial::NeverWritable), "{model}");
    }
    // A switched-off module.
    let crm_off = duduclaw_odoo::OdooConfig { features_crm: false, ..Default::default() };
    assert_eq!(generic_write_model_gate(Some(&cfg), &crm_off, &[], LINK_USE, "crm.tag"), Err(GenericWriteDenial::Module("crm")));
}

#[tokio::test(flavor = "current_thread")]
async fn nested_commands_on_ungranted_or_never_writable_related_models_are_refused() {
    let (url, methods, server) = mock_odoo().await;
    let home = home_with(&mock_global(&url), Some(LEAD_FULL));
    let pool = pool();
    connect_and_snapshot(home.path(), &pool, Some(full_access(serde_json::json!(true), serde_json::json!(true)))).await;
    for (values, related, expected) in [
        // Subscribe someone to the lead's chatter.
        (serde_json::json!({"message_follower_ids": [[0, 0, {"res_model": "crm.lead", "res_id": 1, "partner_id": 3}]]}), "mail.followers", "This capability is not available"),
        (serde_json::json!({"message_follower_ids": [[4, 3]]}), "mail.followers", "This capability is not available"),
        // Post a message.
        (serde_json::json!({"message_ids": [[0, 0, {"body": "hello"}]]}), "mail.message", "This capability is not available"),
        // Create a user with groups, or link users.
        (serde_json::json!({"user_ids": [[0, 0, {"name": "u", "groups_id": [[4, 1]]}]]}), "res.users", "This capability is not available"),
        (serde_json::json!({"user_ids": [[4, 2]]}), "res.users", "This capability is not available"),
        // A blocked table, linked without unblocking.
        (serde_json::json!({"partner_ids": [[4, 2]]}), "res.partner", "block list"),
        // A writable but ungranted table.
        (serde_json::json!({"line_ids": [[0, 0, {"name": "l"}]]}), "crm.lead.line", "explicit grant gate"),
        (serde_json::json!({"line_ids": [[1, 4, {"name": "l"}]]}), "crm.lead.line", "explicit grant gate"),
        (serde_json::json!({"line_ids": [[4, 4]]}), "crm.lead.line", "explicit grant gate"),
    ] {
        for (tool, ids) in [("odoo_write", serde_json::json!([1])), ("odoo_create", Value::Null)] {
            let result = call(tool, serde_json::json!({"model":"crm.lead","ids":ids,"values":values}), home.path(), &pool).await;
            let text = text_of(&result);
            assert!(result["isError"].as_bool().unwrap(), "{tool} {values}: {text}");
            assert!(text.contains(&format!("related table '{related}'")), "{tool} {values}: {text}");
            assert!(text.contains(expected), "{tool} {values}: {text}");
        }
    }
    let seen = methods.lock().unwrap().clone();
    assert!(!seen.iter().any(|m| m == "create" || m == "write"), "no create / write RPC was sent: {seen:?}");
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn odoo_create_rejects_readonly_related_archiving_or_unknown_field() {
    let (url, methods, server) = mock_odoo().await;
    let home = home_with(&mock_global(&url), Some(LEAD_FULL));
    let pool = pool();
    connect_and_snapshot(home.path(), &pool, Some(full_access(serde_json::json!(true), serde_json::json!(true)))).await;
    for (values, expected) in [
        (serde_json::json!({"name": "ok", "kanban_state": "done"}), "read-only field"),
        (serde_json::json!({"nope": 1}), "is not a field of 'crm.lead'"),
        (serde_json::json!({"display_name": "x"}), "computed (non-stored) field"),
        (serde_json::json!({"partner_email": "a@b.c"}), "related field"),
        (serde_json::json!({"legacy_note": "x"}), "re-scan the Odoo schema"),
        (serde_json::json!({"tag_ids": false}), "false / null"),
        (serde_json::json!({"line_ids": [1, 2]}), "plain id list"),
    ] {
        let result = call("odoo_create", serde_json::json!({"model":"crm.lead","values":values}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{values}: {text}");
        assert!(text.contains(expected), "{values}: {text}");
    }
    assert!(!methods.lock().unwrap().iter().any(|m| m == "create"), "no create RPC was sent");
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn archiving_is_refused_before_connecting() {
    let home = home_with(NEVER_RESOLVE, Some(LEAD_FULL));
    let pool = pool();
    for active in [Value::Bool(false), Value::Null, serde_json::json!(0), serde_json::json!("")] {
        let result = call("odoo_write", serde_json::json!({"model":"crm.lead","ids":[1],"values":{"active":active}}), home.path(), &pool).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{active}: {text}");
        assert!(text.contains("archives the record") && text.contains("這項功能不提供"), "{active}: {text}");
        assert!(pool.slots().await.is_empty(), "{active}");
    }
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
        serde_json::json!({"model":"crm.lead","ids":[1],"values":{"active":false}}),
        serde_json::json!({"model":"crm.lead","ids":[1],"values":{"line_ids":[[1, 2, {"active": false}]]}}),
    ] {
        assert!(parse_generic_write_args("odoo_write", &params).is_err(), "{params}");
    }
    // Restoring (active = true) is an ordinary write.
    assert!(parse_generic_write_args("odoo_write", &serde_json::json!({"model":"crm.lead","ids":[1],"values":{"active":true}})).is_ok());
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
        let home = home_with(&mock_global(&url), Some(LEAD_FULL));
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
    let home = home_with(&mock_global(&url), Some(LEAD_FULL));
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
        "allowed_models = ['ir.model']\nallowed_actions = ['write', 'generic_write:ir.model']\nunblock_models = ['ir.model']",
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

#[tokio::test(flavor = "current_thread")]
async fn never_writable_models_are_refused_whatever_the_settings() {
    for model in [
        "res.users", "res.groups", "res.company", "res.users.apikeys", "mail.mail", "mail.followers",
        "mail.message", "mail.template", "mail.compose.message", "discuss.channel", "mail.channel",
        "ir.actions.server", "ir.ui.view", "ir.config_parameter", "ir.rule",
    ] {
        assert!(never_generically_writable(model), "{model}");
        // Every grant, the opt-in and an unblock: still refused.
        let agent = format!(
            "allowed_models = ['{model}']\nallowed_actions = ['create', 'write', 'generic_create:{model}', 'generic_write:{model}']\nunblock_models = ['{model}']"
        );
        let home = home_with(NEVER_RESOLVE, Some(&agent));
        let pool = pool();
        for tool in ["odoo_create", "odoo_write"] {
            let result = call(tool, serde_json::json!({"model":model,"ids":[1],"values":{"name":"x"}}), home.path(), &pool).await;
            let text = text_of(&result);
            assert!(result["isError"].as_bool().unwrap(), "{model} {tool}: {text}");
            assert!(text.contains("This capability is not available") && text.contains("這項功能不提供"), "{model} {tool}: {text}");
            assert!(!text.to_lowercase().contains("admin"), "{model} {tool}: {text}");
        }
        assert!(pool.slots().await.is_empty(), "{model}");
    }
    for model in ["crm.lead", "res.partner", "sale.order", "mail.activity", "res.users_like"] {
        assert!(!never_generically_writable(model), "{model}");
    }
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
    // No credential: a call that passes every policy gate stops at the local
    // connection precheck, without resolving a secret or dialling.
    const NO_CREDENTIAL: &str = "[odoo]\nurl = 'http://localhost:1'\ndb = 'test'\n";
    let crm_off = format!("{NO_CREDENTIAL}features_crm = false\n");
    for (global, agent, model, create, write) in [
        (NO_CREDENTIAL,None, "crm.lead", false, false),
        (NO_CREDENTIAL,Some("profile = 'p'"), "crm.lead", false, false),
        (NO_CREDENTIAL,Some("allowed_models = ['crm.lead']"), "crm.lead", false, false),
        (NO_CREDENTIAL,Some("allowed_actions = ['create', 'write', 'generic_create:crm.lead', 'generic_write:crm.lead']"), "crm.lead", false, false),
        (NO_CREDENTIAL,Some("allowed_models = ['*']\nallowed_actions = ['create', 'write', 'generic_create:*', 'generic_write:*']"), "crm.lead", false, false),
        // Existing grants without the opt-in: not listed.
        (NO_CREDENTIAL,Some("allowed_models = ['crm.lead', 'sale.order']\nallowed_actions = ['create', 'write']"), "crm.lead", false, false),
        (NO_CREDENTIAL,Some("allowed_models = ['crm.lead']\nallowed_actions = ['create', 'generic_create:crm.lead']"), "crm.lead", true, false),
        (NO_CREDENTIAL,Some("allowed_models = ['crm.lead']\nallowed_actions = ['write:crm.lead', 'generic_write:crm.lead']"), "crm.lead", false, true),
        (NO_CREDENTIAL,Some("allowed_models = ['crm.lead']\nallowed_actions = ['write:sale.order', 'generic_write:crm.lead']"), "crm.lead", false, false),
        (NO_CREDENTIAL,Some(LEAD_FULL), "crm.lead", true, true),
        // 功能模組 switched off.
        (crm_off.as_str(), Some(LEAD_FULL), "crm.lead", false, false),
        // Security block list, without and with an unblock.
        (NO_CREDENTIAL,Some("allowed_models = ['res.partner']\nallowed_actions = ['create', 'generic_create:res.partner']"), "res.partner", false, false),
        (NO_CREDENTIAL,Some("allowed_models = ['res.partner']\nallowed_actions = ['create', 'generic_create:res.partner']\nunblock_models = ['res.partner']"), "res.partner", true, false),
        // Never-writable, whatever the settings.
        (NO_CREDENTIAL,Some("allowed_models = ['res.users']\nallowed_actions = ['create', 'write', 'generic_create:res.users', 'generic_write:res.users']\nunblock_models = ['res.users']"), "res.users", false, false),
    ] {
        let home = home_with(global, agent);
        let names = visible_tool_names(&principal, home.path(), "agnes").await;
        assert_eq!(names.contains(&"odoo_create"), create, "{global} {agent:?}");
        assert_eq!(names.contains(&"odoo_write"), write, "{global} {agent:?}");
        assert!(names.contains(&"odoo_crm_create_lead"), "existing tools stay listed: {agent:?}");
        // Listed ⇔ the call passes every policy gate and stops only at the
        // connection precheck (this home has no credential).
        for (tool, listed) in [("odoo_create", create), ("odoo_write", write)] {
            let pool = pool();
            let text = text_of(&call(tool, serde_json::json!({"model":model,"ids":[1],"values":{"name":"x"}}), home.path(), &pool).await);
            assert_eq!(text.contains("Odoo not connected"), listed, "{tool} {global} {agent:?}: {text}");
        }
    }
    // An unreadable [odoo] block hides both (fail closed).
    let home = home_with(NEVER_RESOLVE, Some("allowed_models = 42"));
    let names = visible_tool_names(&principal, home.path(), "agnes").await;
    assert!(!names.contains(&"odoo_create") && !names.contains(&"odoo_write"));
    // So does an unreadable global config.
    let home = home_with("not toml [[[", Some(LEAD_FULL));
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

// ── protected mode: the employee's own isolation connector (decision C) ─────

fn write_agnes_odoo(home: &std::path::Path, body: &str) {
    std::fs::write(home.join("agents/agnes/agent.toml"),
        format!("[agent]\nname = \"agnes\"\nstatus = \"active\"\n[odoo]\n{body}\n")).unwrap();
}

/// The isolation-bound path (`handle_bound_odoo_tool`, what the Odoo
/// isolation connector calls with the connector it selected and bound) now
/// serves `odoo_create` / `odoo_write` — with every RM-120 gate, and without
/// touching the pool (no account resolution, no auto-connect).
#[tokio::test(flavor = "current_thread")]
async fn bound_generic_write_runs_every_rm120_gate_on_the_isolation_connector() {
    let (url, methods, server) = mock_odoo().await;
    let granted = "allowed_models = ['crm.lead', 'crm.lead.line']\nallowed_actions = ['read', 'search', 'create', 'write:crm.lead', \
                   'generic_create:crm.lead', 'generic_write:crm.lead', 'generic_create:crm.lead.line']";
    let home = home_with(&mock_global(&url), Some(granted));
    // Stand-in for the provider's bound connector: authenticated against the
    // mock, with a snapshot of its connection identity.
    let setup = pool();
    connect_and_snapshot(home.path(), &setup, Some(full_access(serde_json::json!(true), serde_json::json!(true)))).await;
    let connector = setup.cached_connector("agnes").await.unwrap();
    let bound = |tool: &'static str, params: Value| {
        let connector = connector.clone();
        let home = home.path().to_path_buf();
        async move {
            // A fresh pool per call, as the isolation executor builds one.
            let isolated = pool();
            let result = handle_bound_odoo_tool(tool, &params, &home, &isolated, "agnes", connector).await;
            assert!(isolated.slots().await.is_empty(), "the bound path never connects through the pool");
            result
        }
    };
    let creates = || methods.lock().unwrap().iter().filter(|m| *m == "create" || *m == "write").count();

    // Granted: the create and write run on the bound connector.
    let result = bound("odoo_create", serde_json::json!({"model": "crm.lead", "values": {
        "name": "Deal", "tag_ids": [[4, 1]], "line_ids": [[0, 0, {"name": "Line"}]]}})).await;
    assert!(!result["isError"].as_bool().unwrap_or(false), "{result}");
    assert_eq!(serde_json::from_str::<Value>(&text_of(&result)).unwrap()["id"], 42);
    let result = bound("odoo_write", serde_json::json!({"model": "crm.lead", "ids": [5], "values": {"name": "R"}})).await;
    assert!(!result["isError"].as_bool().unwrap_or(false), "{result}");
    assert_eq!(creates(), 2);

    // Every gate still refuses on the bound path, before any create / write RPC.
    for (tool, params, expected) in [
        // Never-writable list.
        ("odoo_create", serde_json::json!({"model": "res.users", "values": {"name": "x"}}), "never created or changed by the generic tools"),
        // x2many command rules.
        ("odoo_write", serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"tag_ids": [[3, 5]]}}), "x2many command"),
        ("odoo_write", serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"tag_ids": false}}), "false / null"),
        // Related-model authorization (no write grant on crm.lead.line).
        ("odoo_write", serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"line_ids": [[1, 4, {"name": "l"}]]}}), "related table 'crm.lead.line'"),
        // Field checks against the snapshot.
        ("odoo_create", serde_json::json!({"model": "crm.lead", "values": {"nope": 1}}), "is not a field of 'crm.lead'"),
        ("odoo_create", serde_json::json!({"model": "crm.lead", "values": {"kanban_state": "done"}}), "read-only field"),
        ("odoo_create", serde_json::json!({"model": "crm.lead", "values": {"partner_email": "a@b.c"}}), "related field"),
        // Archiving.
        ("odoo_write", serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"active": false}}), "archives the record"),
    ] {
        let result = bound(tool, params.clone()).await;
        let text = text_of(&result);
        assert!(result["isError"].as_bool().unwrap(), "{tool} {params}: {text}");
        assert!(text.contains(expected), "{tool} {params}: {text}");
    }
    // Explicit grant and the per-table opt-in, read from the employee's own file.
    write_agnes_odoo(home.path(), "allowed_models = ['crm.lead']\nallowed_actions = ['read', 'search', 'create', 'write']");
    for tool in ["odoo_create", "odoo_write"] {
        let text = text_of(&bound(tool, serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"name": "x"}})).await);
        assert!(text.contains("generic write opt-in gate"), "{tool}: {text}");
    }
    write_agnes_odoo(home.path(), "allowed_models = ['crm.lead']\nallowed_actions = ['generic_create:crm.lead']");
    let text = text_of(&bound("odoo_create", serde_json::json!({"model": "crm.lead", "values": {"name": "x"}})).await);
    assert!(text.contains("explicit grant gate"), "{text}");
    assert_eq!(creates(), 2, "no refused call reached a create / write RPC");
    server.abort();
}

/// The isolation executor's pre-credential gate: steps 1–5 refuse locally
/// (no credential, no connection) and pass exactly the calls the full chain
/// would let through to the connection step.
#[test]
fn generic_write_local_gate_refuses_before_credentials() {
    let home = home_with(NEVER_RESOLVE, Some(LEAD_FULL));
    let ok = serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"name": "x", "tag_ids": [[4, 1]]}});
    for tool in ["odoo_create", "odoo_write"] {
        assert!(generic_write_local_gate(tool, &ok, home.path(), "agnes").is_ok(), "{tool}");
    }
    assert_eq!(classify_generic_write_call("odoo_create", &ok), Some(("create", "crm.lead".to_string())));
    assert_eq!(classify_generic_write_call("odoo_write", &ok), Some(("write", "crm.lead".to_string())));
    assert_eq!(classify_generic_write_call("odoo_search", &ok), None);
    assert_eq!(classify_generic_write_call("odoo_create", &serde_json::json!({"model": "Bad Model"})), None);
    for (tool, params, expected) in [
        ("odoo_create", serde_json::json!({"values": {"name": "x"}}), "model is required"),
        ("odoo_create", serde_json::json!({"model": "res.users", "values": {"name": "x"}}), "never created or changed"),
        ("odoo_create", serde_json::json!({"model": "sale.order", "values": {"name": "x"}}), "explicit grant gate"),
        ("odoo_write", serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"tag_ids": [[5]]}}), "x2many command"),
        ("odoo_write", serde_json::json!({"model": "crm.lead", "values": {"name": "x"}}), "ids is required"),
        ("odoo_write", serde_json::json!({"model": "crm.lead", "ids": [1], "values": {"active": false}}), "archives the record"),
        ("odoo_search", serde_json::json!({"model": "crm.lead"}), "is not offered"),
    ] {
        let refused = generic_write_local_gate(tool, &params, home.path(), "agnes").unwrap_err();
        assert!(refused["isError"].as_bool().unwrap(), "{tool} {params}");
        assert!(text_of(&refused).contains(expected), "{tool} {params}: {}", text_of(&refused));
    }
    // The opt-in, the module switch and the block list are read from the files too.
    write_agnes_odoo(home.path(), "allowed_models = ['crm.lead']\nallowed_actions = ['create', 'write']");
    assert!(text_of(&generic_write_local_gate("odoo_create", &ok, home.path(), "agnes").unwrap_err())
        .contains("generic write opt-in gate"));
    write_agnes_odoo(home.path(), LEAD_FULL);
    std::fs::write(home.path().join("config.toml"), format!("{NEVER_RESOLVE}features_crm = false\n")).unwrap();
    assert!(text_of(&generic_write_local_gate("odoo_create", &ok, home.path(), "agnes").unwrap_err())
        .contains("switched off"));
    std::fs::write(home.path().join("config.toml"), "not toml [[[").unwrap();
    assert!(generic_write_local_gate("odoo_create", &ok, home.path(), "agnes").is_err(), "unreadable global fails closed");
}
