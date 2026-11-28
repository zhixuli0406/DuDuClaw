use super::*;
use crate::computer_use_sessions::{attended, auth as a};
use crate::knowledge::turn_requesters::{self, CURRENT_REQUESTER};
use crate::on_behalf_of::context::{register, ContextGuard};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_memory::feedback::{CURRENT_SESSION_ID, CURRENT_TURN_ID};
use tower::ServiceExt;

const KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";
struct Fixture {
    home: tempfile::TempDir,
    uid: String,
    other: String,
    agent: String,
    turn: String,
    state: Arc<RouteState>,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        duduclaw_core::ensure_identity_key(home.path()).unwrap();
        std::fs::write(home.path().join("config.toml"), format!(
            "[admin_ai]\nenabled=true\n[mcp_keys.\"{KEY}\"]\nclient_id='gateway-internal'\nis_external=false\ncreated_at='{}'\nscopes=['admin']\n",
            chrono::Utc::now().to_rfc3339())).unwrap();
        std::fs::create_dir_all(home.path().join("agents")).unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let uid = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap().id;
        let other = db.create_user("other@test.invalid", "Other", "test-password", UserRole::Employee).unwrap().id;
        let agent = format!("admin-avatar-{}", uuid::Uuid::new_v4().simple());
        db.bind_agent(&uid, &agent, AccessLevel::Operator).unwrap();
        db.set_avatar_as(&uid, &uid, Some(&agent)).unwrap();
        db.bind_channel_identity(&uid, "telegram", "101", true).unwrap();
        let state = Arc::new(RouteState::new(home.path().into()));
        Self { home, uid, other, agent, turn: uuid::Uuid::new_v4().to_string(), state }
    }
    async fn live<F, T>(&self, group: bool, check: impl FnOnce(ContextGuard) -> F) -> T
    where F: std::future::Future<Output = T> {
        use crate::approval::{DecisionContext, TrustedReplyTarget};
        let session = if group { "telegram:group".to_owned() } else { format!("webchat:{}", self.turn) };
        let target = group.then(|| TrustedReplyTarget::new(DecisionContext {
            channel: "telegram".into(), account_id: "bot".into(), conversation_id: "group".into(),
            principal_id: "101".into(),
        }, "test-token".into(), "group".into(), None).unwrap().with_chat_type(Some("group")));
        let decision = target.as_ref().map(|t| t.context.clone());
        let who = if group { RequesterIdentity::ChannelUser { channel: "telegram".into(), chat: "group".into(),
            sender: "101".into(), user_id: Some(self.uid.clone()), attested_private: false }
        } else { RequesterIdentity::DashboardUser { user_id: self.uid.clone() } };
        let body = async {
            assert!(attended::bind_final_route(&session, &session, &self.agent, &self.turn));
            CURRENT_SESSION_ID.scope(Some(session.clone()),
                CURRENT_TURN_ID.scope(Some(self.turn.clone()),
                    CURRENT_REQUESTER.scope(who,
                        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(self.uid.clone(),
                            crate::approval::CURRENT_DECISION_CONTEXT.scope(decision,
                                crate::approval::CURRENT_TRUSTED_REPLY_TARGET.scope(target, async {
                                    let _requester = turn_requesters::register_current_turn(&self.agent).unwrap();
                                    let _turn = crate::turn_context::register(&self.agent, &self.turn, &session).unwrap();
                                    check(register(self.home.path(), &self.agent).unwrap()).await
                                })))))).await
        };
        if group { attended::verified_channel_message(&session, "101", false, body).await }
        else { attended::verified_webchat_message(&session, Some(&self.uid), body).await }
    }
    fn agent(&self, name: &str) {
        let dir = self.home.path().join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), format!(r#"
[agent]
name = "{name}"
display_name = "測試員工"
role = "specialist"
status = "active"
trigger = "@{name}"
reports_to = ""
icon = "🤖"
department = ""
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
skill_auto_activate = false
skill_security_scan = true
[capabilities]
allowed_tools = ["Read"]
"#)).unwrap();
    }
    fn sql(&self, sql: &str) {
        rusqlite::Connection::open(self.home.path().join("users.db")).unwrap().execute_batch(sql).unwrap();
    }
    fn approvals_sql(&self, sql: &str) {
        rusqlite::Connection::open(self.home.path().join("approvals.db")).unwrap().execute_batch(sql).unwrap();
    }
    async fn request(&self, agent: &str, value: Value) -> (u16, Value) {
        request(self.state.clone(), agent, value).await
    }
}
// Same signature and Router::oneshot harness as on_behalf_of/route_tests.rs.
fn headers(home: &Path, agent: &str, body: &[u8], nonce: &str) -> HeaderMap {
    let token = duduclaw_core::mint_identity_token(&duduclaw_core::load_identity_key(home).unwrap(), agent);
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
    let sig = duduclaw_core::internal_request_signature(KEY.as_bytes(), agent, &token, &ts, nonce, body);
    let mut headers = HeaderMap::new();
    for (key, value) in [(a::AGENT_ID_HEADER, agent), (a::TIMESTAMP_HEADER, &ts),
        (a::NONCE_HEADER, nonce), (a::SIGNATURE_HEADER, &sig)] { headers.insert(key, value.parse().unwrap()); }
    headers
}
async fn post(state: Arc<RouteState>, peer: &str, headers: HeaderMap, body: Vec<u8>) -> (u16, Value) {
    let mut req = axum::http::Request::builder().method("POST").uri(ROUTE)
        .body(axum::body::Body::from(body)).unwrap();
    req.headers_mut().extend(headers);
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
    let response = router(state).oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn request(state: Arc<RouteState>, agent: &str, value: Value) -> (u16, Value) {
    let body = serde_json::to_vec(&value).unwrap();
    let headers = headers(&state.home, agent, &body, &uuid::Uuid::new_v4().simple().to_string());
    post(state, "127.0.0.1:1234", headers, body).await
}

#[tokio::test]
async fn route_rejects_body_principal() {
    let f = &Fixture::new();
    f.live(false, |guard| async move {
        for op in ["proof", "read"] {
            for field in ["principal", "user_id", "run_id", "run", "turn", "turn_id", "session", "session_id"] {
                let mut body = json!({"op": op, "context": guard.reference()});
                body[field] = json!("forged");
                assert_eq!(f.request(&f.agent, body).await.0, 400, "{op}: {field}");
            }
        }
        assert_eq!(f.request("wrong-agent", json!({"op":"read", "context":guard.reference()})).await.0, 403);
    }).await;
}

#[tokio::test]
async fn stale_context_after_turn_end_refused() {
    let f = &Fixture::new();
    let reference = f.live(false, |guard| async move {
        let reference = guard.reference().to_owned();
        drop(guard);
        assert_eq!(f.request(&f.agent, json!({"op":"read", "context":reference})).await.0, 403);
        reference
    }).await;
    assert_eq!(f.request(&f.agent, json!({"op":"proof", "context":reference})).await.1,
        json!({"ok":true, "read":false, "write":false}));
}

#[tokio::test]
async fn proof_requires_admin_and_feature_and_read_refusal_is_audited() {
    let f = &Fixture::new();
    f.live(false, |guard| async move {
        let proof = json!({"op":"proof", "context":guard.reference()});
        assert_eq!(f.request(&f.agent, proof.clone()).await,
            (200, json!({"ok":true, "read":true, "write":true})));
        f.sql("UPDATE users SET role='employee' WHERE role='admin'");
        assert_eq!(f.request(&f.agent, proof.clone()).await.1,
            json!({"ok":true, "read":false, "write":false}));
        f.sql(&format!("UPDATE users SET role='admin' WHERE id='{}'", f.uid));
        let path = f.home.path().join("config.toml");
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap().replace("enabled=true", "enabled=false")).unwrap();
        assert_eq!(f.request(&f.agent, proof).await.1, json!({"ok":true, "read":false, "write":false}));
        assert_eq!(f.request(&f.agent, json!({"op":"read", "context":guard.reference()})).await,
            (403, json!({"ok":false, "code":"admin_ai_disabled"})));
        let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
        let event = audit.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["event_type"] == "admin_ai_settings_refused").unwrap();
        assert_eq!(event["details"], json!({"agent":f.agent, "reason":"admin_ai_disabled"}));
        assert!(!audit.contains(guard.reference()));
    }).await;
}

#[tokio::test]
async fn group_read_redacts_schedule_text() {
    let f = &Fixture::new();
    f.agent("worker");
    std::fs::write(f.home.path().join("runtime_models.json"), serde_json::to_vec(&json!({
        "fetched_at":"2026-11-27T00:00:00Z", "providers": {
            "claude":{"source":"fallback", "fetched_at":"2026-11-27T00:00:00Z", "models":[
                {"id":"claude-sonnet-4-6", "label":"Sonnet", "provider":"claude"}]},
            "codex":{"source":"fallback", "fetched_at":"2026-11-27T00:00:00Z", "models":[
                {"id":"gpt-5", "label":"GPT", "provider":"codex"}]}
        }
    })).unwrap()).unwrap();
    let store = crate::cron_store::CronStore::open(f.home.path()).unwrap();
    let mut schedule = crate::cron_store::CronTaskRow::new("plain".into(), "Morning".into(),
        "worker".into(), "0 9 * * *".into(), "PRIVATE SCHEDULE TEXT".into());
    schedule.cron_timezone = Some("Asia/Taipei".into());
    store.insert(&schedule).await.unwrap();
    let mut bound = schedule.clone(); bound.id = "bound".into(); bound.enabled = false;
    store.insert(&bound).await.unwrap();
    store.bind_workflow("bound", "activation", "material-hash").await.unwrap();
    let mut scripted = schedule.clone(); scripted.id = "scripted".into(); scripted.condition_script = Some("SECRET SCRIPT".into());
    store.insert(&scripted).await.unwrap();
    let dir = f.home.path().join("agents/worker/SKILLS");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.md"), "---\nname: report\ndescription: PRIVATE SKILL DESCRIPTION\n---\nSECRET BODY").unwrap();
    duduclaw_agent::skill_state::set_disabled(f.home.path(), "worker", &["report".into()], true).unwrap();
    for group in [false, true] {
        f.live(group, |guard| async move {
            let (status, value) = f.request(&f.agent, json!({"op":"read", "context":guard.reference(), "agent_ids":["worker"]})).await;
            assert_eq!(status, 200, "{value}");
            let worker = &value["agents"][0];
            assert_eq!(worker["fields"]["display_name"], json!({"label":"顯示名稱", "value":"測試員工"}));
            assert_eq!(worker["engine"], "claude");
            assert_eq!(worker["selectable_models"], json!([{"id":"claude-sonnet-4-6", "label":"Sonnet"}]));
            assert!(worker["global_limits"]["turn_limit"].is_number());
            assert_eq!(worker["schedules"].as_array().unwrap().len(), 2);
            for row in worker["schedules"].as_array().unwrap() {
                assert_eq!(row["admin_ai_may_modify"], row["id"] == "plain");
                assert_eq!(row.get("task").is_some(), !group);
            }
            assert_eq!(worker["skills"][0]["enabled"], false);
            assert_eq!(worker["skills"][0].get("description").is_some(), !group);
            assert!(worker["skills"][0].get("content").is_none());
            assert_no_sensitive_keys(&value);
            if group {
                assert!(!value.to_string().contains("PRIVATE"));
                assert_eq!(f.request(&f.agent, json!({"op":"proof", "context":guard.reference()})).await.1,
                    json!({"ok":true, "read":true, "write":false}));
            }
        }).await;
    }
}
fn assert_no_sensitive_keys(value: &Value) {
    match value {
        Value::Object(map) => for (key, value) in map {
            assert!(!["budget", "permissions", "capabilities", "channels", "google", "odoo", "soul", "memory", "conversations", "account_pool"].contains(&key.as_str()), "{key}");
            assert_no_sensitive_keys(value);
        },
        Value::Array(values) => for value in values { assert_no_sensitive_keys(value); },
        _ => {},
    }
}

#[tokio::test]
async fn personal_employee_excluded() {
    let f = &Fixture::new();
    for id in ["owned", "other-personal", "terminated", "role-member", "eph-123456789abc", "worker"] { f.agent(id); }
    let mut db = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    let tx = db.transaction().unwrap();
    duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, "owned", &f.uid).unwrap();
    duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, "other-personal", &f.other).unwrap();
    tx.commit().unwrap();
    let path = f.home.path().join("agents/terminated/agent.toml");
    std::fs::write(&path, std::fs::read_to_string(&path).unwrap().replace("status = \"active\"", "status = \"terminated\"")).unwrap();
    let path = f.home.path().join("agents/role-member/agent.toml");
    std::fs::write(&path, format!("{}\n[team_member]\n", std::fs::read_to_string(&path).unwrap())).unwrap();
    f.live(false, |guard| async move {
        let (status, value) = f.request(&f.agent, json!({"op":"read", "context":guard.reference(),
            "agent_ids":["owned", "other-personal", "terminated", "role-member", "eph-123456789abc", "missing", "worker"], "sections":["profile"]})).await;
        assert_eq!(status, 200, "{value}");
        assert_eq!(value["agents"].as_array().unwrap().len(), 2);
        assert_eq!(value["not_available"], json!(["other-personal", "terminated", "role-member", "eph-123456789abc", "missing"]));
        assert!(value["agents"][0].get("schedules").is_none());
        let (_, all) = f.request(&f.agent, json!({"op":"read", "context":guard.reference(), "sections":["profile"]})).await;
        assert_eq!(all["agents"].as_array().unwrap().len(), 2);
        assert_eq!(all["not_available"], json!([]));
    }).await;
}

#[tokio::test]
async fn body_limits_replay_and_transport_authentication() {
    let f = &Fixture::new();
    for (op, limit) in [("proof", PROOF_LIMIT), ("read", READ_LIMIT)] {
        let mut body = serde_json::to_vec(&json!({"op":op, "context":"invalid"})).unwrap();
        body.resize(limit + 1, b' ');
        let signed = headers(f.home.path(), &f.agent, &body, &uuid::Uuid::new_v4().simple().to_string());
        assert_eq!(post(f.state.clone(), "127.0.0.1:1234", signed, body).await.0, 413);
    }
    let body = serde_json::to_vec(&json!({"op":"proof", "context":"invalid"})).unwrap();
    let signed = headers(f.home.path(), &f.agent, &body, &uuid::Uuid::new_v4().simple().to_string());
    assert_eq!(post(f.state.clone(), "192.0.2.1:1234", signed.clone(), body.clone()).await.0, 401);
    assert_eq!(post(f.state.clone(), "127.0.0.1:1234", HeaderMap::new(), body.clone()).await.0, 401);
    assert_eq!(post(f.state.clone(), "127.0.0.1:1234", signed.clone(), body.clone()).await.0, 200);
    assert_eq!(post(f.state.clone(), "127.0.0.1:1234", signed, body).await.0, 401);
}

#[tokio::test]
async fn proof_rate_bucket_independent_from_read() {
    let f = &Fixture::new();
    f.live(false, |guard| async move {
        // Fill the same production limiter without spending a minute on HMAC/DB calls.
        for _ in 0..a::RATE_LIMIT_PER_MINUTE { assert!(f.state.proof_rate.allow(&f.agent, Instant::now())); }
        assert_eq!(f.request(&f.agent, json!({"op":"proof", "context":guard.reference()})).await.0, 429);
        assert_eq!(f.request(&f.agent, json!({"op":"read", "context":guard.reference(), "sections":[]})).await.0, 200);
        let second = Arc::new(RouteState::new(f.home.path().into()));
        for _ in 0..a::RATE_LIMIT_PER_MINUTE { assert!(second.read_rate.allow(&f.agent, Instant::now())); }
        assert_eq!(request(second.clone(), &f.agent, json!({"op":"read", "context":guard.reference()})).await.0, 429);
        assert_eq!(request(second, &f.agent, json!({"op":"proof", "context":guard.reference()})).await.0, 200);
    }).await;
}

#[tokio::test]
async fn external_callers_never_list_admin_tools() {
    let f = &Fixture::new();
    assert_eq!(f.request("dashboard", json!({"op":"proof", "context":"invalid"})).await.0, 403);
    assert_eq!(f.request(&f.agent, json!({"op":"proof", "context":""})).await.1,
        json!({"ok":true, "read":false, "write":false}));
    f.live(false, |guard| async move {
        let path = f.home.path().join("config.toml");
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap().replace("is_external=false", "is_external=true")).unwrap();
        for op in ["proof", "read"] {
            assert_eq!(f.request(&f.agent, json!({"op":op, "context":guard.reference()})).await.0, 401);
        }
    }).await;
}

#[tokio::test]
async fn read_rejects_unknown_sections_and_excess_targets() {
    let f = &Fixture::new();
    for body in [json!({"op":"read", "context":"invalid", "sections":["budget"]}),
        json!({"op":"read", "context":"invalid", "agent_ids":vec!["worker"; 201]}),
        json!({"op":"proof", "context":"invalid", "agent_ids":[]}),
        json!({"op":"propose", "context":"invalid"})] {
        assert_eq!(f.request(&f.agent, body).await.0, 400);
    }
}

#[test]
fn read_projection_rejects_nested_values_at_whitelisted_leaves() {
    let table: toml::Value = "[agent.display_name]\nbudget='hidden'\n".parse().unwrap();
    assert!(fields(&table, None).is_err());
    let table: toml::Value = "[agent]\ndisplay_name='員工'\n[budget]\nmonthly_limit_cents=5000\n[permissions]\ncan_create_agents=true\n[capabilities]\ncomputer_use=true\n".parse().unwrap();
    let projection = fields(&table, None).unwrap();
    assert_eq!(projection.as_object().unwrap().len(), AdminAiField::ALL.len());
    assert_no_sensitive_keys(&projection);
}

#[path = "proposal_tests.rs"]
mod proposal_tests;
