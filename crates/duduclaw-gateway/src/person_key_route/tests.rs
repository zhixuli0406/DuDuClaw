use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use duduclaw_auth::{AccessLevel, JwtConfig, UserRole, UserStatus};
use crate::task_store::{TaskRow, TaskStore};
use tower::ServiceExt;

struct Fixture {
    home: tempfile::TempDir,
    state: Arc<PersonKeyRouteState>,
    user: String,
    admin: String,
    key_id: String,
    secret: String,
    jwt: String,
}

async fn fixture(scopes: &[&str]) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), &["tasks:read", "agents:status"], 120);
    for (id, display) in [("sales", "Sales"), ("finance", "Finance")] {
        crate::trigger_route::tests::write_agent(
            &home.path().join("agents"), id, display, "", "specialist", "",
        );
    }
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let user = db.create_user("holder@test.invalid", "Holder", "test-password", UserRole::Employee).unwrap();
    let admin = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap();
    db.bind_agent(&user.id, "sales", AccessLevel::Owner).unwrap();
    let key = db.member_key_create(&user.id, "MCP", 30, user.credential_epoch, false,
        &scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    let jwt_config = Arc::new(JwtConfig::new(b"person-key-route-isolated-test-secret-32-bytes"));
    let jwt = jwt_config.issue_access_token(&user, &[]).unwrap();
    let handler = Arc::new(MethodHandler::new(home.path().to_path_buf()).await);
    handler.set_user_db(db.clone(), jwt_config).await;
    handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    let store = Arc::new(TaskStore::open(home.path()).unwrap());
    for (id, agent) in [("visible-task", "sales"), ("hidden-task", "finance")] {
        let row = TaskRow::new(id.into(), id.into(), "Task detail".into(), "normal".into(), agent.into(), "system".into());
        store.insert_task(&row).await.unwrap();
    }
    handler.set_task_store(store).await;
    let state = Arc::new(PersonKeyRouteState::new(home.path().to_path_buf(), db, handler));
    Fixture { home, state, user: user.id, admin: admin.id, key_id: key.info.id, secret: key.secret, jwt }
}

fn write_config(home: &std::path::Path, scopes: &[&str], rate: u32) {
    std::fs::write(home.join("config.toml"), format!(
        "[general]\ndefault_agent='finance'\n[member_api_keys]\nenabled=true\nrate_per_minute={rate}\nallow_mcp_scopes={}",
        serde_json::to_string(scopes).unwrap(),
    )).unwrap();
}

async fn request(f: &Fixture, peer: &str, credential: Option<&str>, body: String) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("POST").uri(ROUTE)
        .header("content-type", "application/json")
        .extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    if let Some(raw) = credential { builder = builder.header("authorization", format!("Bearer {raw}")); }
    let response = router(f.state.clone()).oneshot(builder.body(Body::from(body)).unwrap()).await.unwrap();
    let status = response.status();
    let payload: Value = serde_json::from_slice(&to_bytes(response.into_body(), MAX_BODY_BYTES * 2).await.unwrap()).unwrap();
    assert!(!payload.to_string().contains(&f.secret));
    (status, payload)
}

async fn call(f: &Fixture, tool: &str, args: Value) -> (StatusCode, Value) {
    request(f, "127.0.0.1:12345", Some(&f.secret), json!({"op":"call", "tool":tool, "args":args}).to_string()).await
}

fn assert_refusal(response: (StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(response.0, status, "{}", response.1);
    assert_eq!(response.1["ok"], false);
    assert_eq!(response.1["code"], code);
    assert!(response.1["message"].is_string());
}

#[tokio::test]
async fn authenticate_returns_live_sorted_scopes_and_charges_once() {
    let f = fixture(&["tasks:read", "agents:status"]).await;
    write_config(f.home.path(), &["tasks:read", "agents:status"], 1);
    let response = request(&f, "[::1]:12345", Some(&f.secret), json!({"op":"authenticate"}).to_string()).await;
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1, json!({"ok":true, "key_id":f.key_id, "user_id":f.user,
        "scopes":["agents:status", "tasks:read"]}));
    assert_refusal(request(&f, "127.0.0.1:1", Some(&f.secret), json!({"op":"authenticate"}).to_string()).await,
        StatusCode::TOO_MANY_REQUESTS, "member_key_rate_limited");
}

#[tokio::test]
async fn next_call_refuses_revocation_offboarding_password_epoch_and_expiry() {
    for cause in ["revoke", "offboard", "password", "epoch", "expire"] {
        let f = fixture(&["tasks:read"]).await;
        assert_eq!(call(&f, "my_tasks_list", json!({})).await.0, StatusCode::OK, "{cause}");
        match cause {
            "revoke" => f.state.user_db.member_key_revoke_own(&f.user, &f.key_id).unwrap(),
            "offboard" => f.state.user_db.set_user_status(&f.user, UserStatus::Offboarded).unwrap(),
            other => {
                let conn = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
                match other {
                    "password" => { conn.execute("UPDATE users SET password_hash='changed-test-hash' WHERE id=?1", [&f.user]).unwrap(); }
                    "epoch" => { conn.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&f.user]).unwrap(); }
                    "expire" => { conn.execute("UPDATE member_api_keys SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1", [&f.key_id]).unwrap(); }
                    _ => unreachable!(),
                }
            }
        }
        assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::UNAUTHORIZED, "person_key_invalid");
    }
}

#[tokio::test]
async fn scope_and_admin_narrowing_are_live_for_calls_and_authentication() {
    let f = fixture(&["agents:status"]).await;
    assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::FORBIDDEN, "person_key_scope");
    assert_eq!(call(&f, "my_agents_status", json!({})).await.0, StatusCode::OK);
    f.state.user_db.member_key_narrow_admin(&f.admin, &f.key_id, None, Some(&[])).unwrap();
    assert_refusal(call(&f, "my_agents_status", json!({})).await, StatusCode::FORBIDDEN, "person_key_scope");
    let response = request(&f, "127.0.0.1:1", Some(&f.secret), json!({"op":"authenticate"}).to_string()).await;
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["scopes"], json!([]));
}

#[tokio::test]
async fn config_allowlist_removal_takes_effect_on_next_call() {
    let f = fixture(&["tasks:read", "agents:status"]).await;
    assert_eq!(call(&f, "my_tasks_list", json!({})).await.0, StatusCode::OK);
    write_config(f.home.path(), &["agents:status"], 120);
    assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::FORBIDDEN, "person_key_scope");
    let response = request(&f, "127.0.0.1:1", Some(&f.secret), json!({"op":"authenticate"}).to_string()).await;
    assert_eq!(response.1["scopes"], json!(["agents:status"]));
}

#[tokio::test]
async fn malformed_requests_and_identity_injection_never_execute() {
    let f = fixture(&["tasks:read", "agents:status"]).await;
    let bodies = vec![
        json!({"op":"call", "tool":"my_tasks_list", "args":{}, "user_id":f.admin}).to_string(),
        json!({"op":"call", "tool":"my_tasks_list", "args":{}, "agent_id":"finance"}).to_string(),
        json!({"op":"authenticate", "args":{}}).to_string(),
        json!({"op":"unknown"}).to_string(),
        json!({"op":"call", "tool":"tasks_list", "args":{}}).to_string(),
        json!({"op":"call", "tool":"my_tasks_list", "args":{"user_id":f.admin}}).to_string(),
        json!({"op":"call", "tool":"my_tasks_list", "args":{"agent_id":"finance"}}).to_string(),
        json!({"op":"call", "tool":"my_agents_status", "args":{"limit":1}}).to_string(),
        json!({"op":"call", "tool":"my_tasks_list", "args":[]}).to_string(),
        json!({"op":"call", "tool":"my_tasks_list"}).to_string(),
        "{malformed".into(),
        " ".repeat(MAX_BODY_BYTES + 1),
    ];
    for body in bodies {
        assert_refusal(request(&f, "127.0.0.1:1", Some(&f.secret), body).await,
            StatusCode::BAD_REQUEST, "person_key_bad_request");
    }
    // Neither authentication charging nor an RPC gate was reached.
    assert!(f.state.user_db.member_api_keys_for_user(&f.user).unwrap()[0].last_used_at.is_none());
}

#[tokio::test]
async fn remote_peer_and_non_member_credentials_are_refused() {
    let f = fixture(&["tasks:read"]).await;
    let body = json!({"op":"call", "tool":"my_tasks_list", "args":{}}).to_string();
    assert_refusal(request(&f, "192.0.2.1:1", Some(&f.secret), body.clone()).await,
        StatusCode::FORBIDDEN, "person_key_loopback");
    for raw in [None, Some(f.jwt.as_str()), Some("ddc_mcp_not-a-member-key"), Some("ddc_status_token")] {
        assert_refusal(request(&f, "127.0.0.1:1", raw, body.clone()).await,
            StatusCode::UNAUTHORIZED, "person_key_invalid");
    }
    let req = Request::builder().method("POST").uri(ROUTE)
        .extension(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()))
        .header("authorization", format!("Bearer {}", f.secret))
        .header("authorization", format!("Bearer {}", f.jwt))
        .body(Body::from(body)).unwrap();
    assert_eq!(router(f.state.clone()).oneshot(req).await.unwrap().status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tasks_use_holder_acl_and_one_rpc_charge_with_status_and_limit() {
    let f = fixture(&["tasks:read"]).await;
    write_config(f.home.path(), &["tasks:read"], 1);
    let response = call(&f, "my_tasks_list", json!({"status":"todo", "limit":1})).await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    let tasks = response.1["result"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], "visible-task");
    assert!(!response.1.to_string().contains("hidden-task"));
    assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::TOO_MANY_REQUESTS, "member_key_rate_limited");
}

#[tokio::test]
async fn task_status_filter_and_argument_types_are_enforced() {
    let f = fixture(&["tasks:read"]).await;
    let response = call(&f, "my_tasks_list", json!({"status":"completed"})).await;
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["result"]["tasks"], json!([]));
    for args in [json!({"limit":0}), json!({"limit":201}), json!({"limit":u64::MAX}),
        json!({"limit":-1}), json!({"limit":"1"}),
        json!({"limit":null}), json!({"status":null}), json!({"status":1})] {
        assert_refusal(call(&f, "my_tasks_list", args).await, StatusCode::BAD_REQUEST, "person_key_bad_request");
    }
}

#[tokio::test]
async fn member_tasks_list_limit_validates_and_returns_newest_rows() {
    let f = fixture(&["tasks:read"]).await;
    let live = authenticate(&f.state, f.secret.clone()).await
        .unwrap_or_else(|e| panic!("authenticate: {}", e.message));
    let store = TaskStore::open(f.home.path()).unwrap();
    let conn = rusqlite::Connection::open(f.home.path().join("tasks.db")).unwrap();
    conn.execute("UPDATE tasks SET created_at='2000-01-01T00:00:00Z', pinned=1", []).unwrap();
    for index in 1..=4 {
        let id = format!("rpc-{index}");
        let mut row = TaskRow::new(id.clone(), id, String::new(), "normal".into(), "sales".into(), "system".into());
        row.created_at = format!("2026-01-0{index}T00:00:00Z");
        row.updated_at = format!("2026-02-0{}T00:00:00Z", 5 - index);
        store.insert_task(&row).await.unwrap();
    }
    for limit in [1, 2, 1000] {
        let frame = f.state.handler.handle("tasks.list", json!({"agent_id":"sales", "limit":limit}), &live.context).await;
        let WsFrame::Response { ok: true, payload: Some(payload), .. } = frame else {
            panic!("member tasks.list refused a valid limit");
        };
        let rows = payload["tasks"].as_array().unwrap();
        assert_eq!(rows.len(), (limit as usize).min(5));
        assert_eq!(rows[0]["id"], "rpc-4");
        if limit >= 2 { assert_eq!(rows[1]["id"], "rpc-3"); }
    }
    let frame = f.state.handler.handle("tasks.list", json!({"agent_id":"sales"}), &live.context).await;
    let WsFrame::Response { ok: true, payload: Some(payload), .. } = frame else {
        panic!("member tasks.list refused the unlimited default");
    };
    assert_eq!(payload["tasks"].as_array().unwrap().len(), 5);
    assert_eq!(payload["tasks"][0]["id"], "visible-task");
    for limit in [json!(0), json!(1001), json!(u64::MAX), json!(-1), json!(1.5),
        json!("1"), json!(true), json!(null), json!([]), json!({})] {
        let frame = f.state.handler.handle("tasks.list", json!({"agent_id":"sales", "limit":limit}), &live.context).await;
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{limit}");
    }
}

#[tokio::test]
async fn task_limit_default_and_upper_boundary_are_accepted() {
    let f = fixture(&["tasks:read"]).await;
    assert_eq!(default_limit(), 50);
    assert_eq!(MAX_TASK_LIMIT * TASK_ROW_MULTIPLIER, 1000);
    for args in [json!({}), json!({"limit":200})] {
        let response = call(&f, "my_tasks_list", args).await;
        assert_eq!(response.0, StatusCode::OK, "{}", response.1);
        assert_eq!(response.1["result"]["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(response.1["result"]["truncated"], false);
    }
}

fn add_visible_agent(f: &Fixture, id: &str) {
    crate::trigger_route::tests::write_agent(
        &f.home.path().join("agents"), id, id, "", "specialist", "",
    );
    f.state.user_db.bind_agent(&f.user, id, AccessLevel::Owner).unwrap();
}

#[tokio::test]
async fn task_fan_out_stops_at_first_fifty_employee_ids() {
    let f = fixture(&["tasks:read"]).await;
    write_config(f.home.path(), &["tasks:read"], 50);
    // Reverse insertion order must not affect which employees are queried.
    for index in (0..51).rev() {
        add_visible_agent(&f, &format!("cap-{index:03}"));
    }
    let store = TaskStore::open(f.home.path()).unwrap();
    for (id, agent) in [("last-included", "cap-049"), ("first-excluded", "cap-050")] {
        let row = TaskRow::new(id.into(), id.into(), "".into(), "normal".into(), agent.into(), "system".into());
        store.insert_task(&row).await.unwrap();
    }
    let response = call(&f, "my_tasks_list", json!({})).await;
    // A 51st RPC would exhaust the configured rate and refuse the whole call.
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    let tasks = response.1["result"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], "last-included");
    assert_eq!(response.1["result"]["truncated"], true);
}

#[tokio::test]
async fn task_row_ceiling_bounds_collection_and_stops_further_rpcs() {
    let f = fixture(&["tasks:read"]).await;
    write_config(f.home.path(), &["tasks:read"], 1);
    add_visible_agent(&f, "cap-000");
    let store = TaskStore::open(f.home.path()).unwrap();
    for index in 1..=6 {
        let id = format!("bounded-{index}");
        let mut row = TaskRow::new(id.clone(), id, "".into(), "normal".into(), "cap-000".into(), "system".into());
        // Bounded tasks.list must use created_at, regardless of update order.
        row.updated_at = format!("2026-01-0{index}T00:00:00Z");
        row.created_at = format!("2026-01-0{}T00:00:00Z", 7 - index);
        store.insert_task(&row).await.unwrap();
    }
    let conn = rusqlite::Connection::open(f.home.path().join("tasks.db")).unwrap();
    conn.execute("UPDATE tasks SET title=X'FF' WHERE id='bounded-6'", []).unwrap();
    let response = call(&f, "my_tasks_list", json!({"limit":1})).await;
    // Querying sales after reaching five rows would fail the one-call quota.
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    let tasks = response.1["result"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], "bounded-1");
    assert_eq!(response.1["result"]["truncated"], true);
}

#[tokio::test]
async fn task_bridge_passes_remaining_budget_to_each_employee() {
    let f = fixture(&["tasks:read"]).await;
    add_visible_agent(&f, "cap-000");
    let store = TaskStore::open(f.home.path()).unwrap();
    for (agent, count, year) in [("cap-000", 2, 2029), ("sales", 3, 2030)] {
        for index in 1..=count {
            let id = format!("{agent}-{index}");
            let mut row = TaskRow::new(id.clone(), id, String::new(), "normal".into(), agent.into(), "system".into());
            row.created_at = format!("{year}-01-0{index}T00:00:00Z");
            store.insert_task(&row).await.unwrap();
        }
    }
    let conn = rusqlite::Connection::open(f.home.path().join("tasks.db")).unwrap();
    conn.execute("UPDATE tasks SET title=X'FF', created_at='2000-01-01T00:00:00Z' WHERE id='visible-task'", []).unwrap();
    // cap-000 uses two of five rows. Reading more than three from sales
    // would materialize its invalid old row and fail this request.
    let response = call(&f, "my_tasks_list", json!({"limit":1})).await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    let rows = response.1["result"]["tasks"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "sales-3");
    assert_eq!(response.1["result"]["truncated"], true);
}

#[tokio::test]
async fn agents_include_only_visible_employees_and_live_callability() {
    let f = fixture(&["agents:status"]).await;
    write_config(f.home.path(), &["agents:status"], 2);
    let response = call(&f, "my_agents_status", json!({})).await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    assert_eq!(response.1["result"], json!({"agents":[{"id":"sales", "name":"Sales", "status":"active", "can_call":true}]}));
    assert_refusal(call(&f, "my_agents_status", json!({})).await, StatusCode::TOO_MANY_REQUESTS, "member_key_rate_limited");
}

#[tokio::test]
async fn disabled_config_is_401_but_unreadable_config_or_database_is_503() {
    let f = fixture(&["tasks:read"]).await;
    std::fs::write(f.home.path().join("config.toml"), "[member_api_keys]\nenabled=false").unwrap();
    assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::UNAUTHORIZED, "person_key_invalid");
    for config in ["broken [", "[member_api_keys]\nenabled=true\nallow_mcp_scopes=['invalid']"] {
        std::fs::write(f.home.path().join("config.toml"), config).unwrap();
        assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::SERVICE_UNAVAILABLE, "person_key_unavailable");
    }
    std::fs::remove_file(f.home.path().join("config.toml")).unwrap();
    assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::SERVICE_UNAVAILABLE, "person_key_unavailable");
    write_config(f.home.path(), &["tasks:read"], 120);
    rusqlite::Connection::open(f.home.path().join("users.db")).unwrap()
        .execute("ALTER TABLE member_api_keys RENAME TO unavailable_member_keys", []).unwrap();
    assert_refusal(call(&f, "my_tasks_list", json!({})).await, StatusCode::SERVICE_UNAVAILABLE, "person_key_unavailable");
}

#[tokio::test]
async fn call_audit_contains_identity_tool_and_outcome_without_credentials() {
    let f = fixture(&["tasks:read"]).await;
    assert_eq!(call(&f, "my_tasks_list", json!({})).await.0, StatusCode::OK);
    assert_eq!(call(&f, "my_agents_status", json!({})).await.0, StatusCode::FORBIDDEN);
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(!audit.contains(&f.secret));
    assert!(!audit.contains(&f.jwt));
    let events: Vec<Value> = audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event_type"] == "person_key_mcp_call").collect();
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(event["details"]["key_id"], f.key_id);
        assert_eq!(event["details"]["user_id"], f.user);
    }
    assert_eq!(events[0]["details"]["tool"], "my_tasks_list");
    assert_eq!(events[0]["details"]["outcome"], "ok");
    assert_eq!(events[1]["details"]["outcome"], "person_key_scope");
}

#[tokio::test]
async fn unwritable_audit_target_refuses_instead_of_releasing_tasks() {
    let f = fixture(&["tasks:read"]).await;
    let control = call(&f, "my_tasks_list", json!({})).await;
    assert_eq!(control.0, StatusCode::OK);
    assert_eq!(control.1["result"]["tasks"][0]["id"], "visible-task");
    let audit_path = f.home.path().join("security_audit.jsonl");
    std::fs::remove_file(&audit_path).unwrap();
    // A directory cannot be opened as an append target, including as root.
    std::fs::create_dir(&audit_path).unwrap();
    let response = call(&f, "my_tasks_list", json!({})).await;
    assert!(response.1.get("result").is_none());
    assert!(!response.1.to_string().contains("visible-task"));
    assert_refusal(response, StatusCode::SERVICE_UNAVAILABLE, "person_key_unavailable");
}

#[tokio::test]
async fn id_operations_with_gateway_internal_bearer_are_refused_without_execution() {
    let f = fixture(&["tasks:read", "agents:status"]).await;
    let proof = "ddc_prod_11111111111111111111111111111111";
    let config_path = f.home.path().join("config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str(&format!("\n[mcp_keys.{proof}]\nclient_id='gateway-internal'\nis_external=false\nscopes=['admin']\ncreated_at='{}'\n",
        chrono::Utc::now().to_rfc3339()));
    std::fs::write(config_path, config).unwrap();
    duduclaw_auth::mcp_keys::McpKeyStore::open(f.home.path()).unwrap()
        .record_internal_key(proof, chrono::Utc::now()).unwrap();
    assert!(crate::mcp_internal_key::verify_internal_key(f.home.path(), proof));
    let audit_path = f.home.path().join("security_audit.jsonl");
    let audit_before = std::fs::read(&audit_path).ok();
    for body in [
        json!({"op":"authenticate_id", "key_id":f.key_id, "user_id":f.user}),
        json!({"op":"call_id", "key_id":f.key_id, "user_id":f.user,
            "tool":"my_tasks_list", "args":{}}),
    ] {
        assert_refusal(request(&f, "127.0.0.1:1", Some(proof), body.to_string()).await,
            StatusCode::BAD_REQUEST, "person_key_bad_request");
    }
    // No authentication charge, member RPC or tool-call audit was reached.
    assert!(f.state.user_db.member_api_keys_for_user(&f.user).unwrap()[0].last_used_at.is_none());
    assert_eq!(std::fs::read(audit_path).ok(), audit_before);
}
