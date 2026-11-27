use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Request-local signing credentials, never process-global environment mutation.
tokio::task_local! { pub(super) static AUTH: (); }

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

pub(crate) async fn gateway(home: &Path, status: u16, answer: Value) -> Gateway {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    set_port(home, listener.local_addr().unwrap().port());
    let answer = Arc::new(Mutex::new((status, answer)));
    let requests = Arc::new(Mutex::new(Vec::new()));
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
    let personal = json!({"ok":true,"identity":"personal","permission":"follow",
        "user_id":"alice","odoo_uid":7,"login":"alice","rev":1,"secret":"DO-NOT-PRINT"});
    let identity = interpret(200, &serde_json::to_vec(&personal).unwrap(), None).unwrap();
    assert!(!format!("{identity:?}").contains("DO-NOT-PRINT"));
    assert_eq!(identity.permission, IdentityPermission::Follow);
    for field in ["user_id", "login", "odoo_uid", "rev", "secret"] {
        let mut bad = personal.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(interpret(200, &serde_json::to_vec(&bad).unwrap(), None).is_err(), "{field}");
    }
    for (field, value) in [("identity", json!("bogus")), ("permission", json!("bogus")),
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
                _ => json!([]),
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
        "[odoo]\nurl='{url}'\ndb='test'\nusername='company-user'\napi_key_enc='company-secret'\n")).unwrap();
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
        assert!(slots.contains(&("bot".into(), "person:alice:2".into(), true)));
        assert!(slots.contains(&("bot".into(), "person:bob:1".into(), true)));
        let cache = pool.identity_cache.lock().await;
        assert!(!cache.entries.iter().any(|(_, user, rev, _)| user == "alice" && *rev == 1));
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
    let pool = pool();
    let gateway = gateway(home.path(), 200, json!({"ok":true,"identity":"company","permission":"engine"})).await;
    AUTH.scope((), async {
        crate::mcp::odoo::connect_odoo_pool(home.path(), &pool, "bot", "odoo_search").await.unwrap();
        let count = odoo.calls.lock().unwrap().len();
        for kind in ["agent", "company"] {
            *gateway.answer.lock().unwrap() = (200, json!({"ok":true,"identity":kind,"permission":"engine","secret":"never-log-this"}));
            let result = crate::mcp::odoo::handle_odoo_tool("odoo_search", &json!({"model":"crm.lead"}), home.path(), &pool, "bot").await;
            assert_eq!(result["isError"], true);
            assert!(!result.to_string().contains("never-log-this"));
            assert_eq!(odoo.calls.lock().unwrap().len(), count);
        }
    }).await;
    let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert!(!audit.contains("never-log-this"));
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
