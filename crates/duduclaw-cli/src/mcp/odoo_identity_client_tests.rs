use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Request-local signing credentials, never process-global environment mutation.
// Shared by local-refusal fixtures so an accidental identity request is observable.
tokio::task_local! { pub(crate) static AUTH: (); }

pub(crate) fn company() -> OdooIdentity {
    interpret(200, br#"{"ok":true,"identity":"company","permission":"engine"}"#, None).unwrap()
}

pub(crate) struct Gateway {
    pub answer: Arc<Mutex<(u16, Value)>>,
    pub requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Gateway { fn drop(&mut self) { self.task.abort(); } }

async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&bytes);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let size: usize = head.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
            }).unwrap();
            if body.len() >= size { return (head.to_owned(), serde_json::from_str(body).unwrap()); }
        }
    }
}

fn set_port(home: &Path, port: u16) {
    let path = home.join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml::Table = raw.parse().unwrap();
    let gateway = doc.entry("gateway").or_insert(toml::Value::Table(Default::default()));
    gateway.as_table_mut().unwrap().insert("port".into(), toml::Value::Integer(port.into()));
    std::fs::write(path, toml::to_string(&doc).unwrap()).unwrap();
}

pub(crate) fn fingerprint(url: &str, db: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{url}|{db}").as_bytes()))
}
fn snapshot(mut answer: Value, url: &str, db: &str) -> Value {
    answer["url"] = json!(url);
    answer["db"] = json!(db);
    answer["server_fp"] = json!(fingerprint(url, db));
    answer
}
fn profile(url: &str, user: &str, rev: i64) -> String {
    format!("person:{user}:{rev}:{}", fingerprint(url, "test"))
}

pub(crate) async fn gateway(home: &Path, status: u16, answer: Value) -> Gateway {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    set_port(home, listener.local_addr().unwrap().port());
    let answer = Arc::new(Mutex::new((status, answer)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    // Freeze the stub's verified server at decision setup, just like the gateway.
    let config: toml::Value = std::fs::read_to_string(home.join("config.toml")).unwrap().parse().unwrap();
    let url = config.get("odoo").and_then(|v| v.get("url")).and_then(toml::Value::as_str).unwrap_or("https://erp.example.com").to_owned();
    let db = config.get("odoo").and_then(|v| v.get("db")).and_then(toml::Value::as_str).unwrap_or("test").to_owned();
    let response = answer.clone();
    let seen = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (head, request) = read_request(&mut socket).await;
            assert!(head.contains("POST /api/internal/odoo-identity "));
            let head = head.to_ascii_lowercase();
            assert!(head.contains("x-duduclaw-signature:") || head.contains("authorization: bearer ddc_member_"));
            assert!(request.get("agent").is_none() && request.get("user_id").is_none());
            seen.lock().unwrap().push(request.clone());
            let (status, mut body) = response.lock().unwrap().clone();
            if body["identity"] == "personal" && body.get("url").is_none() {
                body = snapshot(body, &url, &db);
            }
            if body["identity"] == "personal" && request["have_rev"] == body["rev"] {
                body.as_object_mut().unwrap().remove("secret");
            }
            let body = body.to_string();
            let out = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(out.as_bytes()).await.unwrap();
        }
    });
    Gateway { answer, requests, task }
}

pub(crate) async fn with_decision<F: std::future::Future>(home: &Path, answer: Value, future: F) -> F::Output {
    let _gateway = gateway(home, 200, answer).await;
    AUTH.scope((), future).await
}

pub(crate) async fn with_company<F: std::future::Future>(home: &Path, future: F) -> F::Output {
    let path = home.join("config.toml");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if raw.parse::<toml::Table>().is_ok_and(|table| !table.contains_key("odoo_access")) {
            std::fs::write(&path, format!("{raw}\n[odoo_access]\ncompany_key_agents=['agnes','bot']\n")).unwrap();
        }
    }
    let _gateway = gateway(home, 200, json!({"ok":true,"identity":"company","permission":"engine"})).await;
    AUTH.scope((), future).await
}

#[test]
fn response_contract_is_closed_and_debug_redacts() {
    let personal = snapshot(json!({"ok":true,"identity":"personal","permission":"follow",
        "user_id":"alice","odoo_uid":7,"login":"alice","rev":1,"secret":"DO-NOT-PRINT"}), "https://erp.example.com", "test");
    let identity = interpret(200, &serde_json::to_vec(&personal).unwrap(), None).unwrap();
    assert!(!format!("{identity:?}").contains("DO-NOT-PRINT"));
    assert_eq!(identity.permission, IdentityPermission::Follow);
    for field in ["user_id", "login", "odoo_uid", "rev", "secret", "url", "db", "server_fp"] {
        let mut bad = personal.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(interpret(200, &serde_json::to_vec(&bad).unwrap(), None).is_err(), "{field}");
    }
    for (field, value) in [("server_fp", json!("0".repeat(64))), ("url", json!("https://evil.example")), ("db", json!("wrong")), ("identity", json!("bogus")), ("permission", json!("bogus")),
        ("odoo_uid", json!(0)), ("rev", json!(0)), ("secret", Value::Null), ("login", json!(""))] {
        let mut bad = personal.clone(); bad[field] = value;
        assert!(interpret(200, &serde_json::to_vec(&bad).unwrap(), None).is_err(), "{field}");
    }
    let mut cached = personal;
    cached.as_object_mut().unwrap().remove("secret");
    assert!(interpret(200, &serde_json::to_vec(&cached).unwrap(), Some(1)).is_ok());
    assert!(interpret(200, &serde_json::to_vec(&cached).unwrap(), Some(2)).is_err());
    for kind in ["agent", "company"] {
        for secret in [json!("DO-NOT-PRINT"), Value::Null] {
            let bad = json!({"ok":true,"identity":kind,"permission":"engine","secret":secret});
            let error = interpret(200, &serde_json::to_vec(&bad).unwrap(), None).unwrap_err();
            assert!(!error.message.contains("DO-NOT-PRINT"));
        }
    }
    assert!(interpret(502, b"<html>", None).is_err());
    assert!(interpret(403, br#"{"ok":true,"identity":"company","permission":"engine"}"#, None).is_err());
    let refusal = interpret(403, "{\"ok\":false,\"code\":\"odoo_not_connected\",\"message\":\"尚未連接\"}".as_bytes(), None).unwrap_err();
    assert_eq!(refusal.message, "尚未連接");
}

fn personal(user: &str, uid: i64, rev: i64) -> Value {
    json!({"ok":true,"identity":"personal","permission":"follow","user_id":user,
        "odoo_uid":uid,"login":user,"rev":rev,"secret":format!("{user}-key-{rev}")})
}

struct OdooStub {
    url: String,
    calls: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for OdooStub { fn drop(&mut self) { self.task.abort(); } }
async fn odoo_stub() -> OdooStub {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (_, request) = read_request(&mut socket).await;
            let result = match request["params"]["method"].as_str().unwrap_or("") {
                "authenticate" => match request["params"]["args"][1].as_str() {
                    Some("alice") => json!(11), Some("bob") => json!(22), _ => json!(7),
                },
                "version" => json!({"server_version":"19.0"}),
                _ => match request["params"]["args"][4].as_str() {
                    Some("fields_get") => json!({"id":{"type":"integer"}, "name":{"type":"char"}, "key":{"type":"char"}, "value":{"type":"char"}}),
                    Some("search_read") if request["params"]["args"][3] == "ir.config_parameter" =>
                        json!([{"id":1,"key":"smtp.password","value":"smtp-secret-must-not-persist"}]),
                    Some("search_read") => json!([{"id":1,"name":"Record"}]),
                    Some("create" | "activity_schedule" | "message_post") => json!(44),
                    Some("write") => json!(true),
                    _ => json!([]),
                },
            };
            seen.lock().unwrap().push(request);
            let body = json!({"result":result}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    OdooStub { url, calls, task }
}
fn home(url: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[odoo]\nmember_credentials=true\nurl='{url}'\ndb='test'\nusername='company-user'\napi_key_enc='company-secret'\n")).unwrap();
    home
}
fn pool() -> crate::mcp::OdooState { Arc::new(crate::odoo_pool::OdooConnectorPool::default()) }

#[tokio::test]
async fn unreachable_and_not_connected_refuse_without_dialing_odoo() {
    let odoo = odoo_stub().await;
    let home = home(&odoo.url);
    let pool = pool();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    set_port(home.path(), listener.local_addr().unwrap().port());
    drop(listener);
    let result = AUTH.scope((), super::super::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await;
    assert!(result.is_err());
    assert!(odoo.calls.lock().unwrap().is_empty());
    let gateway = gateway(home.path(), 403, json!({"ok":false,"code":"odoo_not_connected","message":"沒有本人憑證"})).await;
    let result = AUTH.scope((), super::super::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await;
    assert_eq!(result.err().unwrap(), "沒有本人憑證");
    assert!(pool.slots().await.is_empty());
    assert!(odoo.calls.lock().unwrap().is_empty());
    assert_eq!(gateway.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn company_decision_connects_only_global_account_and_records_identity() {
    let odoo = odoo_stub().await;
    let home = home(&odoo.url);
    let pool = pool();
    pool.register_agent("bot", duduclaw_odoo::AgentOdooConfig {
        username: Some("wrong-agent".into()), api_key_enc: Some("wrong-secret".into()), ..Default::default()
    }).await;
    std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"),
        "[odoo]\nusername='wrong-agent'\napi_key_enc='wrong-secret'\nurl='http://localhost:1'\ndb='wrong_db'\n").unwrap();
    let result = with_company(home.path(), crate::mcp::odoo::handle_odoo_tool(
        "odoo_connect", &json!({}), home.path(), &pool, "bot")).await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(pool.slots().await, vec![("bot".into(), "company".into(), true)]);
    let calls = odoo.calls.lock().unwrap();
    let auth = calls.iter().find(|v| v["params"]["method"] == "authenticate").unwrap();
    assert_eq!(auth["params"]["args"][1], "company-user");
    assert_eq!(auth["params"]["args"][2], "company-secret");
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .any(|v| v["tool_name"] == "odoo_connect" && v["odoo_identity"]["kind"] == "company"));
    assert!(!audit.contains("company-secret"));
}

#[tokio::test]
async fn personal_users_and_revisions_never_share_a_slot() {
    let odoo = odoo_stub().await;
    let home = home(&odoo.url);
    let pool = pool();
    let gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    AUTH.scope((), async {
        let (alice, _) = crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search").await.unwrap();
        assert_eq!(alice.status().uid, Some(11));
        let (again, decision) = crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search").await.unwrap();
        assert!(Arc::ptr_eq(&alice, &again));
        assert_eq!(decision.permission, IdentityPermission::Follow);
        assert_eq!(gateway.requests.lock().unwrap().last().unwrap()["have_rev"], 1);
        *gateway.answer.lock().unwrap() = (200, personal("bob", 22, 1));
        let (bob, _) = crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search").await.unwrap();
        assert_eq!(bob.status().uid, Some(22));
        assert!(!Arc::ptr_eq(&alice, &bob));
        assert_eq!(pool.slots().await.len(), 2);
        // Same rev, different person: must retry without have_rev, not use Alice's secret.
        assert_eq!(gateway.requests.lock().unwrap().last().unwrap()["have_rev"], Value::Null);
        *gateway.answer.lock().unwrap() = (200, personal("alice", 11, 2));
        let (rotated, _) = crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search").await.unwrap();
        assert!(!Arc::ptr_eq(&alice, &rotated));
        let slots = pool.slots().await;
        assert_eq!(slots.len(), 2);
        assert!(slots.contains(&("bot".into(), profile(&odoo.url, "alice", 2), true)));
        assert!(slots.contains(&("bot".into(), profile(&odoo.url, "bob", 1), true)));
        let cache = pool.identity_cache.lock().await;
        assert!(!cache.entries.iter().any(|(_, user, rev, _, _)| user == "alice" && *rev == 1));
    }).await;
    let calls = odoo.calls.lock().unwrap();
    let credentials: Vec<_> = calls.iter().filter(|v| v["params"]["method"] == "authenticate")
        .map(|v| (v["params"]["args"][1].clone(), v["params"]["args"][2].clone())).collect();
    assert!(credentials.contains(&(json!("alice"), json!("alice-key-1"))));
    assert!(credentials.contains(&(json!("bob"), json!("bob-key-1"))));
    assert!(credentials.contains(&(json!("alice"), json!("alice-key-2"))));
    assert!(!credentials.iter().any(|(user, _)| user == "company-user"));
}

#[tokio::test]
async fn person_key_has_only_bearer_body_and_cannot_accept_company() {
    let home = tempfile::tempdir().unwrap();
    let gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    let raw = format!("ddc_member_{}", "a".repeat(64));
    let identity = request_identity_for_person_key(home.path(), &raw, "odoo_search", None).await.unwrap();
    assert_eq!(identity.user_id.as_deref(), Some("alice"));
    let body = gateway.requests.lock().unwrap()[0].clone();
    assert_eq!(body, json!({"tool":"odoo_search","have_rev":null}));
    *gateway.answer.lock().unwrap() = (200, json!({"ok":true,"identity":"company","permission":"engine"}));
    assert!(request_identity_for_person_key(home.path(), &raw, "odoo_search", None).await.is_err());
}

#[tokio::test]
async fn malformed_decision_never_dials_even_with_a_company_slot() {
    let odoo = odoo_stub().await;
    let home = home(&odoo.url);
    // Reach decision validation rather than the local no-possible-identity gate.
    let path = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, format!("{raw}\n[odoo_access]\ncompany_key_agents=['bot']\n")).unwrap();
    let pool = pool();
    let gateway = gateway(home.path(), 200, json!({"ok":true,"identity":"company","permission":"engine"})).await;
    AUTH.scope((), async {
        crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search").await.unwrap();
        let count = odoo.calls.lock().unwrap().len();
        for kind in ["agent", "company"] {
            *gateway.answer.lock().unwrap() = (200, json!({"ok":true,"identity":kind,"permission":"engine","secret":"never-log-this"}));
            let decisions = gateway.requests.lock().unwrap().len();
            let result = crate::mcp::odoo::handle_odoo_tool("odoo_search", &json!({"model":"crm.lead"}), home.path(), &pool, "bot").await;
            assert_eq!(result["isError"], true);
            assert_eq!(gateway.requests.lock().unwrap().len(), decisions + 1,
                "the malformed decision must be requested and rejected");
            assert!(!result.to_string().contains("never-log-this"));
            assert_eq!(odoo.calls.lock().unwrap().len(), count);
        }
    }).await;
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(!audit.contains("never-log-this"));
    assert_eq!(audit.lines().count(), 2);
    for row in audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap()) {
        assert_eq!(row["params_summary"], "identity refused");
        assert_eq!(row["success"], false);
    }
}

#[tokio::test]
async fn revoked_person_is_refused_and_cached_secret_and_slot_are_removed() {
    let odoo = odoo_stub().await;
    let home = home(&odoo.url);
    let pool = pool();
    let gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    AUTH.scope((), async {
        crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search").await.unwrap();
        let count = odoo.calls.lock().unwrap().len();
        *gateway.answer.lock().unwrap() = (403, json!({"ok":false,"code":"odoo_not_connected","message":"請重新連接"}));
        let result = crate::mcp::odoo::handle_odoo_tool("odoo_status", &json!({}), home.path(), &pool, "bot").await;
        assert_eq!(result["isError"], true);
        assert!(result.to_string().contains("請重新連接"));
        assert!(pool.slots().await.is_empty());
        assert!(pool.identity_cache.lock().await.entries.is_empty());
        assert_eq!(odoo.calls.lock().unwrap().len(), count);
    }).await;
}

#[tokio::test]
async fn personal_ignores_agent_endpoint_and_never_decrypts_config_secret() {
    let odoo = odoo_stub().await;
    let home = home(&odoo.url);
    let path = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap().replace("company-secret", "secret://missing/must-not-resolve");
    std::fs::write(path, raw).unwrap();
    let pool = pool();
    pool.register_agent("bot", duduclaw_odoo::AgentOdooConfig {
        url: Some("http://localhost:1".into()), db: Some("wrong_db".into()),
        username: Some("wrong-login".into()), api_key_enc: Some("secret://missing/agent".into()), ..Default::default()
    }).await;
    let _gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    let (conn, _) = AUTH.scope((), crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await.unwrap();
    assert_eq!(conn.status().url, odoo.url);
    assert_eq!(conn.status().db, "test");
    assert_eq!(conn.status().uid, Some(11));
}

#[tokio::test]
async fn agent_decision_resolves_only_its_own_password() {
    let odoo = odoo_stub().await;
    let home = home(&odoo.url);
    std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"),
        "[odoo]\nusername='agent-user'\npassword_enc='agent-password'\nprofile='own-profile'\n").unwrap();
    let pool = pool();
    let _gateway = gateway(home.path(), 200, json!({"ok":true,"identity":"agent","permission":"engine"})).await;
    AUTH.scope((), crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await.unwrap();
    assert_eq!(pool.slots().await, vec![("bot".into(), "own-profile".into(), true)]);
    let calls = odoo.calls.lock().unwrap();
    let auth = calls.iter().find(|v| v["params"]["method"] == "authenticate").unwrap();
    assert_eq!(auth["params"]["args"][1], "agent-user");
    assert_eq!(auth["params"]["args"][2], "agent-password");
}

// P10 exercises the actual person-tools entry and shared Odoo handlers. The
// gateway fixture stands in for already authenticated member-key validation.
async fn person_call(home: &Path, pool: &crate::mcp::OdooState, user: &str, raw: &str,
    tool: &str, args: Value) -> Value {
    use crate::mcp_auth::{AuthenticatedCall, Principal, PrincipalKind, with_authenticated_call};
    let principal = Principal { client_id: format!("person:{user}"),
        scopes: crate::mcp_auth::parse_scopes("odoo:read,odoo:write").unwrap(),
        is_external: true, created_at: chrono::Utc::now() };
    let call = AuthenticatedCall::without_reference(principal.clone())
        .with_kind(PrincipalKind::PersonKey { key_id: format!("key-{user}"), user_id: user.into() })
        .with_person_key(raw.into());
    with_authenticated_call(call, crate::mcp::person_tools::call(home, pool, &principal, tool, &args)).await
}

fn person_home(url: &str) -> tempfile::TempDir {
    let home = home(url);
    let path = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, format!("[general]\ndefault_agent='bot'\n{raw}\n[odoo_access]\ncompany_key_agents=['bot']\npermission_mode='follow_odoo_account'\npermission_mode_ack={{by='owner',at='2026-11-01T00:00:00Z'}}\nalways_refuse=['unlink:*']\n")).unwrap();
    // Neither this file nor a synthetic employee's config is usable. Person
    // calls must still work without parsing them or the employee autonomy chain.
    for name in ["bot", "person-key", "person:alice"] {
        let dir = home.path().join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "not valid TOML [").unwrap();
    }
    home
}

const ALICE_KEY: &str = "ddc_member_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const BOB_KEY: &str = "ddc_member_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

#[tokio::test]
async fn person_missing_or_invalid_identity_never_uses_company_even_when_cached() {
    let odoo = odoo_stub().await;
    let home = person_home(&odoo.url);
    let pool = pool();
    let gateway = gateway(home.path(), 403, json!({"ok":false,"code":"odoo_not_connected","message":"Odoo not connected"})).await;
    let result = person_call(home.path(), &pool, "alice", ALICE_KEY, "odoo_search", json!({"model":"res.partner"})).await;
    assert_eq!(result["isError"], true);
    assert!(result.to_string().contains("not connected"));
    assert!(pool.slots().await.is_empty());
    assert!(odoo.calls.lock().unwrap().is_empty());

    *gateway.answer.lock().unwrap() = (200, json!({"ok":true,"identity":"company","permission":"engine"}));
    AUTH.scope((), crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await.unwrap();
    let calls = odoo.calls.lock().unwrap().len();
    let mut engine = personal("alice", 11, 1); engine["permission"] = json!("engine");
    for response in [json!({"ok":true,"identity":"company","permission":"engine"}),
        json!({"ok":true,"identity":"agent","permission":"read_only"}), engine, personal("bob", 22, 1)] {
        *gateway.answer.lock().unwrap() = (200, response);
        let result = person_call(home.path(), &pool, "alice", ALICE_KEY, "odoo_search", json!({"model":"res.partner"})).await;
        assert_eq!(result["isError"], true, "{result}");
        assert_eq!(odoo.calls.lock().unwrap().len(), calls);
    }
    for code in ["odoo_not_connected", "odoo_member_credentials_off"] {
        *gateway.answer.lock().unwrap() = (403, json!({"ok":false,"code":code,"message":code}));
        let result = person_call(home.path(), &pool, "alice", ALICE_KEY, "odoo_status", json!({})).await;
        assert_eq!(result["isError"], true);
        assert_eq!(odoo.calls.lock().unwrap().len(), calls);
    }
    assert_eq!(pool.slots().await, vec![("bot".into(), "company".into(), true)]);
}

#[tokio::test]
async fn person_modes_fixed_refusals_uid_and_metadata_only_audit() {
    let odoo = odoo_stub().await;
    let home = person_home(&odoo.url);
    let pool = pool();
    let mut read_only = personal("alice", 11, 1); read_only["permission"] = json!("read_only");
    let gateway = gateway(home.path(), 200, read_only).await;
    let writes = [
        ("odoo_create", json!({"model":"res.partner","values":{"name":"never-audit-values"}})),
        ("odoo_write", json!({"model":"res.partner","ids":[1],"values":{"name":"never-audit-values"}})),
        ("odoo_activity_create", json!({"model":"res.partner","res_id":1,"summary":"never-audit-values"})),
        ("odoo_note_post", json!({"model":"res.partner","res_id":1,"body":"never-audit-values"})),
    ];
    for (tool, args) in &writes {
        let result = person_call(home.path(), &pool, "alice", ALICE_KEY, tool, args.clone()).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
    }
    assert!(odoo.calls.lock().unwrap().is_empty());
    for tool in ["odoo_search", "odoo_schema_fields", "odoo_status"] {
        let result = person_call(home.path(), &pool, "alice", ALICE_KEY, tool, json!({"model":"res.partner"})).await;
        assert_ne!(result["isError"], true, "{tool}: {result}");
    }
    *gateway.answer.lock().unwrap() = (200, personal("alice", 11, 1));
    for (tool, args) in &writes {
        let result = person_call(home.path(), &pool, "alice", ALICE_KEY, tool, args.clone()).await;
        assert_ne!(result["isError"], true, "{tool}: {result}");
    }
    let before = odoo.calls.lock().unwrap().len();
    for (tool, args) in [
        ("odoo_write", json!({"model":"ir.model.fields","ids":[1],"values":{"name":"x"}})),
        ("odoo_create", json!({"model":"res.partner","values":{"child_ids":[[2,1,0]]}})),
        ("odoo_execute", json!({"model":"res.partner","method":"unlink","ids":"1"})),
        ("odoo_message_send", json!({"model":"res.partner","res_id":1,"body":"x"})),
        ("odoo_attachment_add", json!({"model":"res.partner","res_id":1,"path":"x"})),
    ] {
        let result = person_call(home.path(), &pool, "alice", ALICE_KEY, tool, args).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
    }
    assert_eq!(odoo.calls.lock().unwrap().len(), before);
    let calls = odoo.calls.lock().unwrap();
    for method in ["create", "write", "activity_schedule", "message_post"] {
        let call = calls.iter().find(|v| v["params"]["args"][4] == method).unwrap();
        assert_eq!(call["params"]["args"][1], 11);
        assert_eq!(call["params"]["args"][2], "alice-key-1");
    }
    drop(calls);
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    for secret in [ALICE_KEY, "alice-key-1", "company-secret", "never-audit-values"] {
        assert!(!audit.contains(secret));
    }
    let rows: Vec<Value> = audit.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(rows.iter().all(|row| row["agent_id"] == "person:alice"));
    assert!(rows.iter().any(|row| row["odoo_identity"]["user_id"] == "alice"));
    // Four refused writes + three reads + four writes + two locally refused
    // generic writes. Tools outside the table never enter the Odoo handler.
    assert_eq!(rows.len(), 13);
}

#[tokio::test]
async fn person_keys_and_employee_personal_pool_entries_are_disjoint() {
    use crate::mcp::odoo_actor::OdooActor;
    let odoo = odoo_stub().await;
    let home = person_home(&odoo.url);
    let pool = pool();
    let gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    let (employee, _) = AUTH.scope((), crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await.unwrap();
    let alice = OdooActor::Person { user_id:"alice", raw_key:ALICE_KEY };
    let bob = OdooActor::Person { user_id:"bob", raw_key:BOB_KEY };
    let args = json!({"model":"res.partner"});
    let (first, decision) = alice.connect(home.path(), &pool, "odoo_search", &args).await.unwrap();
    assert!(!Arc::ptr_eq(&employee, &first));
    assert!(pool.key_for_identity(crate::odoo_pool::PERSON_KEY_SLOT, &decision).await.is_err());
    let (again, _) = alice.connect(home.path(), &pool, "odoo_search", &args).await.unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    *gateway.answer.lock().unwrap() = (200, personal("bob", 22, 1));
    let (second, _) = bob.connect(home.path(), &pool, "odoo_search", &args).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(second.status().uid, Some(22));
    let slots = pool.slots().await;
    for key in [("bot", "person:alice:1"), (crate::odoo_pool::PERSON_KEY_SLOT, "person:alice:1"),
        (crate::odoo_pool::PERSON_KEY_SLOT, "person:bob:1")] {
        assert!(slots.contains(&(key.0.into(), format!("{}:{}", key.1, fingerprint(&odoo.url, "test")), true)));
    }
    *gateway.answer.lock().unwrap() = (200, personal("alice", 11, 2));
    let (rotated, _) = alice.connect(home.path(), &pool, "odoo_search", &args).await.unwrap();
    assert!(!Arc::ptr_eq(&rotated, &first));
    assert!(pool.slots().await.contains(&("bot".into(), profile(&odoo.url, "alice", 1), true)));
    *gateway.answer.lock().unwrap() = (403, json!({"ok":false,"code":"odoo_not_connected","message":"not connected"}));
    assert!(alice.connect(home.path(), &pool, "odoo_search", &args).await.is_err());
    let slots = pool.slots().await;
    assert_eq!(slots.len(), 2);
    assert!(slots.contains(&("bot".into(), profile(&odoo.url, "alice", 1), true)));
    assert!(slots.contains(&(crate::odoo_pool::PERSON_KEY_SLOT.into(), profile(&odoo.url, "bob", 1), true)));
}


#[tokio::test]
async fn personal_endpoint_snapshot_survives_config_change_before_connect() {
    let decided = odoo_stub().await;
    let changed = odoo_stub().await;
    let home = home(&decided.url);
    let pool = pool();
    let gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    let decision = AUTH.scope((), request_identity(home.path(), "bot", "odoo_search", None)).await.unwrap();
    let path = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap().replace(&decided.url, &changed.url);
    std::fs::write(path, raw).unwrap();
    pool.load_global_for_gate(home.path()).await.unwrap();
    let (employee, cached) = AUTH.scope((), crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await.unwrap();
    assert_eq!(cached.url.as_deref(), Some(decided.url.as_str()));
    let person = pool.connect_person_key(duduclaw_odoo::OdooConfig {
        url: changed.url.clone(), db: "wrong".into(), ..Default::default()
    }, &decision).await.unwrap();
    assert_eq!(employee.status().url, decided.url);
    assert_eq!(person.status().url, decided.url);
    assert_eq!(employee.status().uid, Some(11));
    assert_eq!(person.status().uid, Some(11));
    assert!(!Arc::ptr_eq(&employee, &person), "employee and person-key slots stay separate");
    assert!(changed.calls.lock().unwrap().is_empty());
    // On Unix the separate person-key slot reuses the same credential-bound
    // on-disk connection metadata; off Unix that cache is disabled.
    assert_eq!(decided.calls.lock().unwrap().iter().filter(|r| r["params"]["method"] == "authenticate").count(), if cfg!(unix) { 1 } else { 2 });
    // Same user/revision at a newly verified server cannot reuse the old slot.
    *gateway.answer.lock().unwrap() = (200, snapshot(personal("alice", 11, 1), &changed.url, "test"));
    let (new, _) = AUTH.scope((), crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search")).await.unwrap();
    assert!(!Arc::ptr_eq(&employee, &new));
    assert_eq!(new.status().url, changed.url);
    assert_eq!(new.status().uid, Some(11));
    assert_eq!(changed.calls.lock().unwrap().iter().filter(|r| r["params"]["method"] == "authenticate").count(), 1);
    assert_eq!(pool.identity_cache.lock().await.entries.len(), 1);
    let slots = pool.slots().await;
    assert!(!slots.contains(&("bot".into(), profile(&decided.url, "alice", 1), true)));
}

#[tokio::test]
async fn follow_sensitive_read_never_persists_values() {
    let odoo = odoo_stub().await;
    let home = person_home(&odoo.url);
    std::fs::write(home.path().join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[odoo]\nact_as_requester=true\n").unwrap();
    super::super::odoo_access_tests::personal_db(home.path(), "bot", true);
    let pool = pool();
    let _gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    let args = json!({"model":"ir.config_parameter","fields":"key,value"});
    let result = AUTH.scope((), crate::mcp::odoo::handle_odoo_tool("odoo_search", &args, home.path(), &pool, "bot")).await;
    assert_ne!(result["isError"], true, "{result}");
    assert!(result.to_string().contains("smtp-secret-must-not-persist"));
    let person = person_call(home.path(), &pool, "alice", ALICE_KEY, "odoo_search", args).await;
    assert_ne!(person["isError"], true, "{person}");
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(!audit.contains("smtp-secret-must-not-persist") && !audit.contains("smtp.password"));
    assert_audit_chain(&audit, 1);
    for row in audit.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()) {
        assert!(row.get("result_text").is_none());
        assert_eq!(row["odoo_result"]["model"], "ir.config_parameter");
        assert_eq!(row["odoo_result"]["record_count"], 1);
        assert_metadata_fields_only(&row);
    }
}

#[test]
fn sensitive_models_are_metadata_only_even_without_follow() {
    let home = tempfile::tempdir().unwrap();
    for model in ["ir.config_parameter", "res.users.apikeys", "res.users.apikeys.audit", "ir.mail_server",
        "fetchmail.server", "payment.provider", "res.users", "ir.attachment", "custom.secret.store", "oauth_token"] {
        assert!(sensitive_model(model), "{model}");
        audit_result(home.path(), "bot", "odoo_search", &json!({"model":model}), true,
            r#"[{"value":"secret-never-log"}]"#, false);
    }
    for model in ["crm.lead", "secretary.notes", "keynote.slide", "tokenizer.result"] { assert!(!sensitive_model(model)); }
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(!audit.contains("secret-never-log") && !audit.contains("result_text"));
    assert_audit_chain(&audit, 10);
    for row in audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap()) {
        assert_metadata_fields_only(&row);
    }
}

#[tokio::test]
async fn person_listing_and_dispatch_share_local_policy_and_follow_ack() {
    use crate::mcp_auth::{AuthenticatedCall, Principal, PrincipalKind, with_authenticated_call};
    let odoo = odoo_stub().await;
    let home = person_home(&odoo.url);
    let pool = pool();
    let _gateway = gateway(home.path(), 200, personal("alice", 11, 1)).await;
    let path = home.path().join("config.toml");
    let original: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    for mode in ["broken", "engine", "no_ack", "follow"] {
        let mut config = original.clone();
        match mode {
            "broken" => config["odoo_access"] = toml::Value::String("broken".into()),
            "engine" => config["odoo_access"]["permission_mode"] = toml::Value::String("engine".into()),
            "no_ack" => { config["odoo_access"].as_table_mut().unwrap().remove("permission_mode_ack"); },
            _ => (),
        }
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        let principal = Principal { client_id:"person:alice".into(),
            scopes:crate::mcp_auth::parse_scopes("tasks:read,agents:status,odoo:read,odoo:write").unwrap(),
            is_external:true, created_at:chrono::Utc::now() };
        let authenticated = AuthenticatedCall::without_reference(principal.clone())
            .with_kind(PrincipalKind::PersonKey { key_id:"key".into(), user_id:"alice".into() })
            .with_person_key(ALICE_KEY.into());
        with_authenticated_call(authenticated, async {
            let listed = crate::mcp::tools_list::visible_tools_with(&principal, home.path(), "bot", false).await;
            assert!(listed.iter().any(|t| t.name == "my_tasks_list"));
            assert_eq!(listed.iter().any(|t| t.name == "odoo_search"), mode != "broken");
            for tool in ["odoo_create", "odoo_write", "odoo_activity_create", "odoo_note_post"] {
                assert_eq!(listed.iter().any(|t| t.name == tool), mode == "follow", "{mode}: {tool}");
                if mode != "follow" {
                    let args = match tool {
                        "odoo_create" => json!({"model":"res.partner","values":{"name":"x"}}),
                        "odoo_write" => json!({"model":"res.partner","values":{"name":"x"},"ids":[1]}),
                        "odoo_activity_create" => json!({"model":"res.partner","res_id":1,"summary":"x"}),
                        _ => json!({"model":"res.partner","res_id":1,"body":"x"}),
                    };
                    let result = crate::mcp::person_tools::call(home.path(), &pool, &principal, tool, &args).await;
                    assert_eq!(result["isError"], true, "{mode}: {tool}: {result}");
                }
            }
            if mode == "broken" {
                let result = crate::mcp::person_tools::call(home.path(), &pool, &principal,
                    "odoo_search", &json!({"model":"res.partner"})).await;
                assert_eq!(result["isError"], true);
            }
        }).await;
    }
    assert!(odoo.calls.lock().unwrap().is_empty());
}

fn assert_audit_chain(audit: &str, expected_rows: usize) {
    assert_eq!(audit.lines().count(), expected_rows);
    let mut previous = duduclaw_security::audit_chain::genesis_hash();
    for line in audit.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        // This is a hash of the previous persisted metadata record, never
        // a digest of the Odoo response (including low-entropy secrets).
        assert_eq!(row["_prev_hash"].as_str(), Some(previous.as_str()));
        previous = format!("{:x}", Sha256::digest(line.as_bytes()));
    }
}

fn assert_metadata_fields_only(row: &Value) {
    let metadata = row["odoo_result"].as_object().unwrap();
    assert_eq!(metadata.len(), 5);
    for key in ["model", "method", "record_count", "ids_count", "byte_length"] {
        assert!(metadata.contains_key(key), "missing {key}: {metadata:?}");
    }
    assert!(row.get("result_text").is_none());
    // Only the verified top-level audit-chain field is exempt. Metadata and
    // every other top-level field still cannot carry a response digest.
    for key in row.as_object().unwrap().keys().filter(|key| key.as_str() != "_prev_hash").chain(metadata.keys()) {
        let key = key.to_ascii_lowercase();
        assert!(!key.contains("hash") && !key.contains("digest") && !key.contains("sha256"));
    }
}

#[test]
fn follow_audit_keeps_counts_and_lengths_without_content_or_digest_even_on_error() {
    let home = tempfile::tempdir().unwrap();
    let raw = r#"[{"value":"low-entropy-pin-1234"}]"#;
    for success in [true, false] {
        audit_result(home.path(), "bot", "odoo_execute",
            &json!({"model":"crm.lead","method":"read","ids":[1,2]}), success, raw, true);
    }
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(!audit.contains("low-entropy-pin-1234"));
    assert!(!audit.contains(&format!("{:x}", Sha256::digest(raw.as_bytes()))));
    assert_audit_chain(&audit, 2);
    for row in audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap()) {
        assert_metadata_fields_only(&row);
        assert_eq!(row["odoo_result"]["model"], "crm.lead");
        assert_eq!(row["odoo_result"]["method"], "read");
        assert_eq!(row["odoo_result"]["ids_count"], 2);
        assert_eq!(row["odoo_result"]["record_count"], 1);
        assert_eq!(row["odoo_result"]["byte_length"], raw.len());
    }
}
