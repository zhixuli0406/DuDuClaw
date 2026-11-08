//! RM-064: `me.status_tokens.*` / `status_tokens.admin_*` and the
//! `GET /api/v1/status/employees` endpoint, end to end through
//! `MethodHandler::handle` and `external_status::serve` (no socket).
use super::*;

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};

use crate::external_status::{
    Gate, Limits, MAX_CONCURRENT_WORK, RATE_LIMIT_PER_MINUTE, STATUS_PATH, TITLE_MAX_CHARS,
};

/// Admission wide open, the switch re-read on every request; the per-credential
/// limit and the concurrency cap as in production.
fn test_limits() -> Limits {
    Limits {
        global_per_minute: 1_000_000,
        source_per_minute: 1_000_000,
        token_per_minute: RATE_LIMIT_PER_MINUTE,
        concurrency: MAX_CONCURRENT_WORK,
        work_wait: Duration::from_secs(10),
        enabled_ttl: Duration::ZERO,
    }
}

fn seed_agent(home: &Path, name: &str, display: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!(
            r#"[agent]
name = "{name}"
display_name = "{display}"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = "🤖"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["*"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true
"#
        ),
    )
    .unwrap();
}

fn set_enabled(home: &Path, on: bool) {
    std::fs::write(
        home.join("config.toml"),
        format!("[external_status]\nenabled = {on}\n"),
    )
    .unwrap();
}

struct Fx {
    _dir: tempfile::TempDir,
    home: PathBuf,
    db: Arc<UserDb>,
    handler: MethodHandler,
    store: Arc<TaskStore>,
    gate: Gate,
}

async fn fixture(enabled: bool) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    seed_agent(&home, "sales", "業務小幫手");
    seed_agent(&home, "hr", "人資");
    seed_agent(&home, "finance", "財務");
    set_enabled(&home, enabled);
    // Stand-in for an installed on-device PII model that finds no names;
    // without one every title is omitted.
    crate::external_status::title_mask::test_ner::register(
        &home,
        Arc::new(crate::external_status::title_mask::test_ner::People::none()),
    );
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let handler = MethodHandler::new(home.clone()).await;
    handler
        .set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!")))
        .await;
    let store = Arc::new(TaskStore::open(&home).unwrap());
    handler.set_task_store(store.clone()).await;
    Fx { _dir: dir, home, db, handler, store, gate: Gate::new(test_limits()) }
}

fn person(db: &UserDb, name: &str, role: UserRole) -> String {
    db.create_user(&format!("{name}@test.invalid"), name, "isolated-test-password", role)
        .unwrap()
        .id
}

fn live(db: &UserDb, id: &str) -> UserContext {
    db.live_user_context(id).unwrap().unwrap()
}

async fn rpc(fx: &Fx, method: &str, params: Value, user: &str) -> WsFrame {
    let ctx = live(&fx.db, user);
    Box::pin(fx.handler.handle(method, params, &ctx)).await
}

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok response, got {other:?}"),
    }
}

fn error_code(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => {
            e.get("code").and_then(Value::as_str).unwrap_or("").to_string()
        }
        other => panic!("expected error response, got {other:?}"),
    }
}

async fn create(fx: &Fx, user: &str) -> (String, String) {
    let payload = ok(&rpc(fx, "me.status_tokens.create", json!({ "name": "Odoo 卡片" }), user).await);
    (
        payload["secret"].as_str().unwrap().to_string(),
        payload["token"]["id"].as_str().unwrap().to_string(),
    )
}

async fn call(fx: &Fx, method: &str, token: Option<&str>, query: Option<&str>) -> (StatusCode, Value) {
    call_via(fx, &fx.gate, method, token, query, None).await
}

async fn call_via(
    fx: &Fx,
    gate: &Gate,
    method: &str,
    token: Option<&str>,
    query: Option<&str>,
    peer: Option<IpAddr>,
) -> (StatusCode, Value) {
    let uri = match query {
        Some(q) => format!("{STATUS_PATH}?{q}"),
        None => STATUS_PATH.to_string(),
    };
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    if let Some(ip) = peer {
        builder = builder.extension(ConnectInfo(SocketAddr::new(ip, 40_000)));
    }
    let response = crate::external_status::serve_with(
        gate,
        &fx.handler,
        &fx.db,
        &fx.home,
        builder.body(Body::empty()).unwrap(),
    )
    .await;
    let status = response.status();
    assert!(
        response.headers().get("access-control-allow-origin").is_none(),
        "server-to-server only: no CORS"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn get(fx: &Fx, token: &str) -> (StatusCode, Value) {
    call(fx, "GET", Some(token), None).await
}

fn agent_ids(body: &Value) -> Vec<String> {
    body["employees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["agent_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn disabled_refuses_creation_and_every_request_without_deleting_tokens() {
    let fx = fixture(false).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();

    let refused = rpc(&fx, "me.status_tokens.create", json!({ "name": "odoo" }), &alice).await;
    assert_eq!(error_code(&refused), "external_status_disabled");
    assert_eq!(ok(&rpc(&fx, "me.status_tokens.list", json!({}), &alice).await)["enabled"], false);

    set_enabled(&fx.home, true);
    let (secret, _) = create(&fx, &alice).await;
    assert_eq!(get(&fx, &secret).await.0, StatusCode::OK);

    set_enabled(&fx.home, false);
    for method in ["GET", "POST", "DELETE"] {
        let (status, body) = call(&fx, method, Some(&secret), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(body, json!({ "error": "not_found" }));
    }
    assert_eq!(call(&fx, "GET", None, None).await.0, StatusCode::NOT_FOUND);
    // Off does not delete anything; on again, the same credential works.
    let listed = ok(&rpc(&fx, "me.status_tokens.list", json!({}), &alice).await);
    assert_eq!(listed["tokens"].as_array().unwrap().len(), 1);
    set_enabled(&fx.home, true);
    assert_eq!(get(&fx, &secret).await.0, StatusCode::OK);
}

#[tokio::test]
async fn create_shows_the_secret_once_and_lists_never_do() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    let admin = person(&fx.db, "admin", UserRole::Admin);
    let created = ok(&rpc(&fx, "me.status_tokens.create", json!({ "name": "Odoo 卡片" }), &alice).await);
    let secret = created["secret"].as_str().unwrap();
    assert!(secret.starts_with("ddc_status_"));
    assert_eq!(created["endpoint_path"], STATUS_PATH);
    assert_eq!(created["token"]["name"], "Odoo 卡片");
    assert_eq!(created["token"]["active"], true);

    let mine = ok(&rpc(&fx, "me.status_tokens.list", json!({}), &alice).await);
    let all = ok(&rpc(&fx, "status_tokens.admin_list", json!({}), &admin).await);
    for listing in [&mine, &all] {
        let text = listing.to_string();
        assert!(!text.contains(secret), "{text}");
        assert!(!text.contains(&secret["ddc_status_".len()..]), "{text}");
        assert!(!text.contains("hash"), "{text}");
    }
    assert_eq!(all["tokens"][0]["user_id"], alice.as_str());

    // Admin surfaces stay Admin-only; unknown members of the namespace refuse.
    assert!(matches!(rpc(&fx, "status_tokens.admin_list", json!({}), &alice).await, WsFrame::Response { ok: false, .. }));
    assert!(matches!(rpc(&fx, "me.status_tokens.export", json!({}), &admin).await, WsFrame::Response { ok: false, .. }));
    assert!(matches!(rpc(&fx, "status_tokens.future", json!({}), &admin).await, WsFrame::Response { ok: false, .. }));

    // Six is one too many.
    for i in 0..4 {
        ok(&rpc(&fx, "me.status_tokens.create", json!({ "name": format!("t{i}") }), &alice).await);
    }
    let sixth = rpc(&fx, "me.status_tokens.create", json!({ "name": "t6" }), &alice).await;
    assert_eq!(error_code(&sixth), "status_token_limit_reached");
    let blank = rpc(&fx, "me.status_tokens.create", json!({ "name": "  " }), &admin).await;
    assert_eq!(error_code(&blank), "status_token_invalid_name");
}

#[tokio::test]
async fn revocation_is_immediate_and_owner_scoped() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    let bob = person(&fx.db, "bob", UserRole::Employee);
    let admin = person(&fx.db, "admin", UserRole::Admin);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Viewer).unwrap();
    let (secret, id) = create(&fx, &alice).await;
    assert_eq!(get(&fx, &secret).await.0, StatusCode::OK);

    // Bob cannot revoke Alice's credential.
    let refused = rpc(&fx, "me.status_tokens.revoke", json!({ "id": id }), &bob).await;
    assert_eq!(error_code(&refused), "status_token_not_found");
    assert_eq!(get(&fx, &secret).await.0, StatusCode::OK);

    ok(&rpc(&fx, "me.status_tokens.revoke", json!({ "id": id }), &alice).await);
    let (status, body) = get(&fx, &secret).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({ "error": "unauthorized" }));

    // Admin revocation of someone else's credential.
    let (other, other_id) = create(&fx, &alice).await;
    ok(&rpc(&fx, "status_tokens.admin_revoke", json!({ "id": other_id }), &admin).await);
    assert_eq!(get(&fx, &other).await.0, StatusCode::UNAUTHORIZED);

    // Every attributable call is audited, without response content.
    let audit = fx.db.query_audit_log(Some(alice.as_str()), None, 100).unwrap();
    assert!(audit.iter().any(|e| e.action == "status_token.used" && e.target.as_deref() == Some(id.as_str())));
    assert!(audit.iter().any(|e| e.action == "status_token.refused"
        && e.detail.as_deref().is_some_and(|d| d.contains("revoked"))));
    for entry in &audit {
        let detail = entry.detail.as_deref().unwrap_or("");
        assert!(!detail.contains("業務小幫手") && !detail.contains(&secret), "{detail}");
    }
    let used = fx.db.status_tokens_for_user(&alice).unwrap();
    assert!(used.iter().any(|t| t.id == id && t.last_used_at.is_some()));
}

#[tokio::test]
async fn visibility_follows_the_owners_live_bindings() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    let bob = person(&fx.db, "bob", UserRole::Manager);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();
    fx.db.bind_agent(&alice, "hr", AccessLevel::Viewer).unwrap();
    fx.db.bind_agent(&bob, "finance", AccessLevel::Owner).unwrap();
    let (alice_secret, _) = create(&fx, &alice).await;
    let (bob_secret, _) = create(&fx, &bob).await;

    let (status, body) = get(&fx, &alice_secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent_ids(&body), ["hr", "sales"]);
    assert_eq!(body["employees"][1]["display_name"], "業務小幫手");
    assert_eq!(agent_ids(&get(&fx, &bob_secret).await.1), ["finance"], "another account's credential never sees my employees");

    // The filter narrows within bindings and never widens past them.
    let (status, body) = call(&fx, "GET", Some(&alice_secret), Some("agent_id=sales")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent_ids(&body), ["sales"]);
    let (status, body) = call(&fx, "GET", Some(&alice_secret), Some("agent_id=finance")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({ "error": "not_found" }));
    for bad in ["agent=sales", "agent_id=sales&agent_id=hr", "agent_id=sales&verbose=1"] {
        assert_eq!(call(&fx, "GET", Some(&alice_secret), Some(bad)).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(call(&fx, "POST", Some(&alice_secret), None).await.0, StatusCode::METHOD_NOT_ALLOWED);

    // Unbinding applies on the very next request.
    fx.db.unbind_agent(&alice, "hr").unwrap();
    assert_eq!(agent_ids(&get(&fx, &alice_secret).await.1), ["sales"]);

    // A suspended owner is refused; reinstated, the credential works again.
    fx.db.set_user_status(&alice, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert_eq!(get(&fx, &alice_secret).await.0, StatusCode::UNAUTHORIZED);
    fx.db.set_user_status(&alice, duduclaw_auth::UserStatus::Active).unwrap();
    assert_eq!(get(&fx, &alice_secret).await.0, StatusCode::OK);

    // Garbage and foreign credential types are refused alike.
    for raw in ["ddc_status_nope", "ddc_prod_0123456789abcdef0123456789abcdef", "not-a-token"] {
        assert_eq!(get(&fx, raw).await.0, StatusCode::UNAUTHORIZED, "{raw}");
    }
    assert_eq!(call(&fx, "GET", None, None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn response_carries_only_counts_and_a_truncated_title() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();
    let long_title = format!("整理第四季客戶{}", "報價".repeat(40));
    let mut done = TaskRow::new(
        "t-done".into(),
        long_title,
        "機密描述：客戶 A 的底價".into(),
        "medium".into(),
        "sales".into(),
        "u1".into(),
    );
    done.status = "done".into();
    done.completed_at = Some(chrono::Utc::now().to_rfc3339());
    done.result_summary = Some("對話內容：客戶說可以".into());
    fx.store.insert_task(&done).await.unwrap();
    for (id, status) in [("t-open", "in_progress"), ("t-todo", "todo"), ("t-review", "review")] {
        let mut row = TaskRow::new(id.into(), "x".into(), "機密描述".into(), "medium".into(), "sales".into(), "u1".into());
        row.status = status.into();
        fx.store.insert_task(&row).await.unwrap();
    }
    let broker = crate::approval::ApprovalBroker::open(&fx.home).unwrap();
    broker
        .request("sales", "dynamic_review_kind", "寄出報價單給客戶 A", json!({ "body": "對話內容" }), 3600)
        .await
        .unwrap();

    // The dashboard shows approvals to Managers with Viewer on the employee
    // (`ApprovalVisibility`); an Employee-role owner's count leaves it out.
    let carol = person(&fx.db, "carol", UserRole::Manager);
    fx.db.bind_agent(&carol, "sales", AccessLevel::Viewer).unwrap();
    let (carol_secret, _) = create(&fx, &carol).await;
    let (status, body) = get(&fx, &carol_secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["employees"][0]["awaiting_confirmation"], 2, "review task + readable approval");

    let (secret, _) = create(&fx, &alice).await;
    let (status, body) = get(&fx, &secret).await;
    assert_eq!(status, StatusCode::OK);

    let mut top: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    top.sort();
    assert_eq!(top, ["employees", "generated_at"]);
    let employee = &body["employees"][0];
    let mut keys: Vec<&str> = employee.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        ["agent_id", "awaiting_confirmation", "display_name", "done_7d", "done_today", "in_progress", "latest_done"]
    );
    assert_eq!(employee["in_progress"], 2);
    assert_eq!(employee["awaiting_confirmation"], 1, "review task; the approval is not readable by an Employee");
    assert_eq!(employee["done_today"], 1);
    assert_eq!(employee["done_7d"], 1);
    let latest = employee["latest_done"].as_object().unwrap();
    let mut latest_keys: Vec<&str> = latest.keys().map(String::as_str).collect();
    latest_keys.sort();
    assert_eq!(latest_keys, ["completed_at", "title"]);
    let title = latest["title"].as_str().unwrap();
    assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
    assert!(title.starts_with("整理第四季客戶報價"));

    let text = body.to_string();
    for forbidden in ["機密描述", "對話內容", "寄出報價單", "t-done", "t-open", "description", "summary", "id\":\"t-"] {
        assert!(!text.contains(forbidden), "{forbidden} leaked: {text}");
    }
}

#[tokio::test]
async fn each_credential_is_rate_limited() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();
    let (secret, id) = create(&fx, &alice).await;
    let (other, _) = create(&fx, &alice).await;
    for _ in 0..RATE_LIMIT_PER_MINUTE {
        assert_eq!(get(&fx, &secret).await.0, StatusCode::OK);
    }
    let (status, body) = get(&fx, &secret).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, json!({ "error": "rate_limited" }));
    assert_eq!(get(&fx, &secret).await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(get(&fx, &other).await.0, StatusCode::OK, "limits are per credential");
    // Sixty successful calls in one window: one audit row, not sixty.
    let used = fx
        .db
        .query_audit_log(Some(alice.as_str()), Some("status_token.used"), 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.target.as_deref() == Some(id.as_str()))
        .count();
    assert!((1..=2).contains(&used), "aggregated per 10-minute window: {used}");
    // One audit row for the burst of refusals, not one per call.
    let limited = fx
        .db
        .query_audit_log(Some(alice.as_str()), Some("status_token.refused"), 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.target.as_deref() == Some(id.as_str()))
        .count();
    assert_eq!(limited, 1);
}

fn task(id: &str, title: &str, agent: &str, status: &str) -> TaskRow {
    let mut row = TaskRow::new(
        id.into(),
        title.into(),
        "機密描述".into(),
        "medium".into(),
        agent.into(),
        "u1".into(),
    );
    row.status = status.into();
    row
}

fn done_minutes_ago(mut row: TaskRow, minutes: i64) -> TaskRow {
    row.status = "done".into();
    row.completed_at = Some((chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339());
    row
}

fn packet_dir(home: &Path, task_id: &str) -> PathBuf {
    let dir = home
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join(task_id)
        .join("1");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn limit_audience(home: &Path, task_id: &str, audience: &[String]) {
    let raw = json!({
        "packet_id": "p1",
        "goal_id": task_id,
        "round": 1,
        "from_role": "executor",
        "to_role": "verifier",
        "objective": "Review report",
        "output_format": "files",
        "audience": audience,
    });
    std::fs::write(packet_dir(home, task_id).join("p1.json"), raw.to_string()).unwrap();
}

fn employee<'a>(body: &'a Value, agent: &str) -> &'a Value {
    body["employees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["agent_id"] == agent)
        .unwrap_or_else(|| panic!("{agent} missing from {body}"))
}

#[tokio::test]
async fn two_accounts_bound_to_one_employee_never_see_each_others_private_work() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Manager);
    let bob = person(&fx.db, "bob", UserRole::Manager);
    for user in [&alice, &bob] {
        fx.db.bind_agent(user, "sales", AccessLevel::Viewer).unwrap();
    }
    let principal = |id: &str| format!("user:{id}");

    fx.store.insert_task(&task("t-shared-open", "公開待辦", "sales", "todo")).await.unwrap();
    let mut bob_open = task("t-bob-open", "Bob 私人：議價", "sales", "in_progress");
    bob_open.a2a_owner = Some(principal(&bob));
    fx.store.insert_task(&bob_open).await.unwrap();
    let mut alice_done = done_minutes_ago(task("t-alice-done", "Alice 私人：報價底線", "sales", "done"), 3);
    alice_done.a2a_owner = Some(principal(&alice));
    fx.store.insert_task(&alice_done).await.unwrap();
    fx.store
        .insert_task(&done_minutes_ago(task("t-aud-done", "限定給 Alice 的檢討", "sales", "done"), 1))
        .await
        .unwrap();
    limit_audience(&fx.home, "t-aud-done", &[principal(&alice)]);
    fx.store
        .insert_task(&done_minutes_ago(task("t-shared-done", "公開週報", "sales", "done"), 10))
        .await
        .unwrap();

    let broker = crate::approval::ApprovalBroker::open(&fx.home).unwrap();
    broker.request("sales", "dynamic_review_kind", "一般審核", json!({}), 3600).await.unwrap();
    broker
        .request("sales", "goal_kickoff", "Alice 的私人審核", json!({ "task_id": "t-alice-done" }), 3600)
        .await
        .unwrap();

    let (alice_secret, _) = create(&fx, &alice).await;
    let (bob_secret, _) = create(&fx, &bob).await;

    let (status, body) = get(&fx, &alice_secret).await;
    assert_eq!(status, StatusCode::OK);
    let sales = employee(&body, "sales");
    assert_eq!(sales["in_progress"], 1, "{body}");
    assert_eq!(sales["awaiting_confirmation"], 2, "{body}");
    assert_eq!(sales["done_7d"], 3, "{body}");
    assert_eq!(sales["latest_done"]["title"], "限定給 Alice 的檢討");

    let (status, body) = get(&fx, &bob_secret).await;
    assert_eq!(status, StatusCode::OK);
    let sales = employee(&body, "sales");
    assert_eq!(sales["in_progress"], 2, "shared + own: {body}");
    assert_eq!(sales["awaiting_confirmation"], 1, "only the approval bob may read: {body}");
    assert_eq!(sales["done_7d"], 1, "neither alice's A2A task nor the audience-limited one: {body}");
    assert_eq!(sales["latest_done"]["title"], "公開週報");
    let text = body.to_string();
    for leaked in ["Alice", "限定", "報價底線"] {
        assert!(!text.contains(leaked), "{leaked} leaked to bob: {text}");
    }

    // An unreadable packet set is an ACL read failure: refused, never a guess.
    std::fs::write(packet_dir(&fx.home, "t-aud-done").join("corrupt.json"), "{").unwrap();
    for secret in [&alice_secret, &bob_secret] {
        let (status, body) = get(&fx, secret).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, json!({ "error": "unavailable" }));
    }
}

#[tokio::test]
async fn identity_read_failure_refuses_the_snapshot() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Manager);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Viewer).unwrap();
    fx.store.insert_task(&task("t-open", "x", "sales", "todo")).await.unwrap();
    let ctx = live(&fx.db, &alice);
    assert!(fx.handler.external_status_snapshot(&ctx, &["sales".into()]).await.is_ok());
    // The account went away between the bindings read and the count.
    fx.db.set_user_status(&alice, duduclaw_auth::UserStatus::Suspended).unwrap();
    assert!(fx.handler.external_status_snapshot(&ctx, &["sales".into()]).await.is_err());
    // A reader without an account principal is refused too.
    let mut system = ctx.clone();
    system.user_id = "system".into();
    assert!(fx.handler.external_status_snapshot(&system, &["sales".into()]).await.is_err());
}

#[tokio::test]
async fn titles_are_pii_masked_and_omitted_when_unmaskable() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();
    fx.db.bind_agent(&alice, "hr", AccessLevel::Owner).unwrap();
    fx.store
        .insert_task(&done_minutes_ago(
            task(
                "t-pii",
                "回電給 王先生 ０９１２－３４５－６７８，寄 alice.chen@example.com，token=abcdefghijklmnopqrstuvwxyz123456",
                "sales",
                "done",
            ),
            1,
        ))
        .await
        .unwrap();
    fx.store
        .insert_task(&done_minutes_ago(task("t-huge", &"客".repeat(2000), "hr", "done"), 1))
        .await
        .unwrap();
    let (secret, _) = create(&fx, &alice).await;
    let (status, body) = get(&fx, &secret).await;
    assert_eq!(status, StatusCode::OK);

    let title = employee(&body, "sales")["latest_done"]["title"].as_str().unwrap().to_string();
    assert!(title.starts_with("回電") && title.contains('＊'), "{title}");
    for secret in ["王", "３４５", "６７８", "345", "678", "alice.chen", "example.com", "abcdefghijklmnop"] {
        assert!(!title.contains(secret), "{secret} leaked: {title}");
    }
    assert!(title.chars().count() <= TITLE_MAX_CHARS);

    let hr = &employee(&body, "hr")["latest_done"];
    assert_eq!(hr["title"], Value::Null, "a title that cannot be masked is omitted: {body}");
    assert!(hr["completed_at"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn titles_need_the_on_device_model() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();
    fx.store
        .insert_task(&done_minutes_ago(task("t-visit", "拜訪王小明確認續約", "sales", "done"), 1))
        .await
        .unwrap();
    let (secret, _) = create(&fx, &alice).await;
    use crate::external_status::title_mask::test_ner;

    // The model finds the name no rule knows.
    test_ner::register(&fx.home, Arc::new(test_ner::People::of(&["王小明"])));
    let (status, body) = get(&fx, &secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(employee(&body, "sales")["latest_done"]["title"], "拜訪＊＊＊確認續約");

    // A failing model, then none installed: the title goes, the time stays.
    for failing in [true, false] {
        if failing {
            test_ner::register(&fx.home, Arc::new(test_ner::Failing));
        } else {
            test_ner::unregister(&fx.home);
        }
        let (status, body) = get(&fx, &secret).await;
        assert_eq!(status, StatusCode::OK);
        let latest = &employee(&body, "sales")["latest_done"];
        assert_eq!(latest["title"], Value::Null, "{body}");
        assert!(latest["completed_at"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(!body.to_string().contains("王小明"), "{body}");
    }
}

#[tokio::test]
async fn random_credential_flood_is_refused_before_any_lookup() {
    let fx = fixture(true).await;
    let gate = Gate::new(Limits { source_per_minute: 5, global_per_minute: 8, ..test_limits() });
    let flood: IpAddr = "203.0.113.7".parse().unwrap();
    let random = |i: usize| format!("ddc_status_{i:043}");
    for i in 0..5 {
        let (status, _) = call_via(&fx, &gate, "GET", Some(&random(i)), None, Some(flood)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(gate.lookups(), 5);
    for i in 5..40 {
        let (status, body) = call_via(&fx, &gate, "GET", Some(&random(i)), None, Some(flood)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body, json!({ "error": "rate_limited" }));
    }
    assert_eq!(gate.lookups(), 5, "no credential lookup past the source limit");

    // Other sources share what is left of the global budget (8).
    for i in 0..3 {
        let ip = IpAddr::from([198, 51, 100, i as u8 + 1]);
        assert_eq!(call_via(&fx, &gate, "GET", Some(&random(100 + i)), None, Some(ip)).await.0, StatusCode::UNAUTHORIZED);
    }
    let late = IpAddr::from([192, 0, 2, 1]);
    assert_eq!(call_via(&fx, &gate, "GET", Some(&random(200)), None, Some(late)).await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(gate.lookups(), 8, "no credential lookup past the global limit");
    // Unattributable refusals write no audit rows.
    assert!(fx.db.query_audit_log(None, Some("status_token.refused"), 100).unwrap().is_empty());
}

#[tokio::test]
async fn a_full_work_pool_refuses_without_a_lookup() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();
    let (secret, _) = create(&fx, &alice).await;
    let gate = Gate::new(Limits { concurrency: 1, work_wait: Duration::from_millis(50), ..test_limits() });
    let held = gate.acquire_work().await.unwrap();
    let (status, body) = call_via(&fx, &gate, "GET", Some(&secret), None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({ "error": "unavailable" }));
    assert_eq!(gate.lookups(), 0);
    drop(held);
    assert_eq!(call_via(&fx, &gate, "GET", Some(&secret), None, None).await.0, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_never_exceed_the_work_cap() {
    let fx = fixture(true).await;
    let alice = person(&fx.db, "alice", UserRole::Employee);
    fx.db.bind_agent(&alice, "sales", AccessLevel::Owner).unwrap();
    fx.store.insert_task(&task("t-open", "x", "sales", "todo")).await.unwrap();
    let (secret, _) = create(&fx, &alice).await;
    let gate = Gate::new(Limits { concurrency: 2, ..test_limits() });
    let requests = (0..12).map(|_| call_via(&fx, &gate, "GET", Some(&secret), None, None));
    for (status, _) in futures_util::future::join_all(requests).await {
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(gate.lookups(), 12);
    assert!((1..=2).contains(&gate.peak_in_flight()), "peak {}", gate.peak_in_flight());
}

#[test]
fn namespace_roles_are_explicit() {
    use super::super::status_tokens_rpc::minimum_role;
    for m in ["me.status_tokens.list", "me.status_tokens.create", "me.status_tokens.revoke"] {
        assert_eq!(minimum_role(m), Some(UserRole::Employee), "{m}");
    }
    for m in ["status_tokens.admin_list", "status_tokens.admin_revoke", "status_tokens.anything", "me.status_tokens.future"] {
        assert_eq!(minimum_role(m), Some(UserRole::Admin), "{m}");
    }
    for m in ["me.google.status", "status.get", "me.status_tokensx.list", "users.me"] {
        assert_eq!(minimum_role(m), None, "{m}");
    }
}
