//! Isolated HTTP and browser acceptance fixtures. Never starts production runtimes.
use super::*;
use axum::http::{Request, StatusCode, header};
use calamine::{Reader, Xlsx};
use duduclaw_auth::UserRole;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::PathBuf;
use tower::ServiceExt;

const AGENT: &str = "export-fixture";
const STAMP: &str = "2026-10-11T03:00:00Z";
const PASSWORD: &str = "isolated-browser-fixture-password";

struct Fixture {
    _directory: tempfile::TempDir,
    state: Arc<AppState>,
    admin_id: String,
    admin_token: String,
    second_token: String,
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().to_path_buf();
    std::fs::write(
        home.join("config.toml"),
        "[product]\nprofile = \"restricted\"\n",
    )
    .unwrap();
    let agent_dir = home.join("agents").join(AGENT);
    std::fs::create_dir_all(agent_dir.join("state")).unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        format!(
            r#"
[agent]
name = "{AGENT}"
display_name = "示範資料助理（隔離測試）"
role = "specialist"
status = "active"
trigger = "@{AGENT}"
reports_to = ""
icon = "🐾"
department = "示範資料"
[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = []
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
monthly_limit_cents = 0
warn_threshold_percent = 80
hard_stop = true
[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []
[evolution]
skill_auto_activate = false
skill_security_scan = true
"#
        ),
    )
    .unwrap();

    let user_db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let admin = user_db
        .create_user(
            "export-fixture@example.invalid",
            "隔離測試管理員",
            PASSWORD,
            UserRole::Admin,
        )
        .unwrap();
    let second = user_db
        .create_user(
            "second-fixture@example.invalid",
            "第二位隔離測試管理員",
            PASSWORD,
            UserRole::Admin,
        )
        .unwrap();
    let jwt_config = Arc::new(JwtConfig::new(b"export-browser-isolated-test-secret-32-bytes"));
    let admin_token = jwt_config.issue_access_token(&admin, &[]).unwrap();
    let second_token = jwt_config.issue_access_token(&second, &[]).unwrap();

    let tasks = Arc::new(crate::task_store::TaskStore::open(&home).unwrap());
    for (id, title, stamp) in [
        ("export-task-1", "整理產品操作說明（隔離示範資料）", STAMP),
        ("export-task-2", "核對產品操作說明與匯出資料", STAMP),
        (
            "export-old-task",
            "日期篩選應排除的舊任務",
            "2026-09-01T00:00:00Z",
        ),
    ] {
        let mut row = crate::task_store::TaskRow::new(
            id.into(),
            title.into(),
            "這是隔離測試資料，沒有執行模型或外部操作。".into(),
            "medium".into(),
            AGENT.into(),
            "fixture".into(),
        );
        row.created_at = stamp.into();
        row.updated_at = stamp.into();
        tasks.insert_task(&row).await.unwrap();
    }
    let sessions = crate::session::SessionManager::new(&home.join("sessions.db")).unwrap();
    sessions
        .get_or_create("webchat:export-fixture", AGENT)
        .await
        .unwrap();
    sessions
        .append_message(
            "webchat:export-fixture",
            "user",
            "請整理使用者操作說明（隔離示範對話）",
            16,
        )
        .await
        .unwrap();
    sessions
        .append_message(
            "webchat:export-fixture",
            "assistant",
            "可以，示範內容包含任務與知識匯出。\n這段文字有換行與\"引號\"。",
            22,
        )
        .await
        .unwrap();
    let conn = Connection::open(home.join("sessions.db")).unwrap();
    conn.execute("UPDATE session_messages SET timestamp=?1", params![STAMP])
        .unwrap();
    drop(conn);
    let telemetry =
        crate::cost_telemetry::CostTelemetry::new(&home.join("cost_telemetry.db")).unwrap();
    telemetry
        .record(
            AGENT,
            crate::cost_telemetry::RequestType::Chat,
            "claude-sonnet-4-6",
            &crate::cost_telemetry::TokenUsage {
                input_tokens: 120,
                cache_read_tokens: 20,
                cache_creation_tokens: 0,
                output_tokens: 60,
            },
        )
        .await;
    let conn = Connection::open(home.join("cost_telemetry.db")).unwrap();
    conn.execute("UPDATE token_usage SET created_at=?1", params![STAMP])
        .unwrap();
    drop(conn);
    let memory_path = agent_dir.join("state/memory.db");
    let _memory = duduclaw_memory::SqliteMemoryEngine::new(&memory_path).unwrap();
    let conn = Connection::open(memory_path).unwrap();
    // SQL fixture rows use the real initialized store schema, without claiming source lineage.
    conn.execute("INSERT INTO memories(id,agent_id,content,timestamp,source_event) VALUES ('export-memory-1',?1,'示範記憶：匯出時保留繁體中文、換行與引號。',?2,'isolated_fixture')", params![AGENT, STAMP]).unwrap();
    conn.execute("INSERT INTO key_facts(id,agent_id,fact,source_session,timestamp) VALUES ('export-fact-1',?1,'示範事實：匯出資料包含繁體中文。','webchat:export-fixture',?2)", params![AGENT, STAMP]).unwrap();
    drop(conn);
    let wiki = duduclaw_memory::WikiStore::new(agent_dir.join("wiki"));
    wiki.write_page("操作說明.md", &format!("---\ntitle: 操作說明（隔離示範）\ncreated: {STAMP}\nupdated: {STAMP}\n---\n# 操作說明（隔離示範）\n任務、對話與知識資料可匯出 CSV 或 Excel。\n")).unwrap();
    let shared = duduclaw_memory::WikiStore::new_shared(&home);
    shared.write_page("產品操作資料.md", &format!("---\ntitle: 產品操作資料（隔離示範）\ncreated: {STAMP}\nupdated: {STAMP}\n---\n# 產品操作資料（隔離示範）\n此為隔離測試，資料僅供產品匯出功能驗證。\n")).unwrap();
    crate::data_exports::initialize(&home).unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    handler
        .set_user_db(user_db.clone(), jwt_config.clone())
        .await;
    handler.set_task_store(tasks).await;
    handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
        .await;
    let (tx, _) = broadcast::channel(16);
    let (event_tx, _) = broadcast::channel(16);
    let state = Arc::new(AppState {
        auth: AuthManager::new(None),
        handler,
        tx,
        event_tx,
        user_db,
        jwt_config,
        otp_delivery: Arc::new(crate::otp_delivery::ConfigOtpDeliverer::new(
            home.clone(),
            reqwest::Client::new(),
        )),
        home_dir: home,
    });
    Fixture {
        _directory: directory,
        state,
        admin_id: admin.id,
        admin_token,
        second_token,
    }
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(value),
            ..
        } => value,
        other => panic!("expected successful real RPC: {other:?}"),
    }
}

async fn create_ready(fixture: &Fixture, dataset: &str, format: &str) -> String {
    let ctx = authenticate_jwt(&fixture.state, &fixture.admin_token).unwrap();
    let created = payload(fixture.state.handler.handle("data_exports.create", json!({"dataset":dataset,"format":format,"agent_id":AGENT,"from":"2026-10-10T16:00:00Z","until":"2026-10-11T16:00:00Z"}), &ctx).await);
    let id = created["export_id"].as_str().unwrap().to_owned();
    for _ in 0..250 {
        let status = payload(
            fixture
                .state
                .handler
                .handle("data_exports.get", json!({"export_id":id}), &ctx)
                .await,
        );
        if status["state"] == "ready" {
            return id;
        }
        assert_ne!(status["state"], "failed", "{status}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("real export did not complete within five seconds");
}

fn download_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/api/data-exports/{id}/download",
            get(super::data_exports_http::download),
        )
        .with_state(state)
}

async fn http_get(router: &Router, uri: &str, token: Option<&str>) -> axum::response::Response {
    let mut request = Request::builder().uri(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    router
        .clone()
        .oneshot(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn export_http_requires_bearer_owner_and_current_role_and_returns_native_files() {
    let fixture = fixture().await;
    let router = download_router(fixture.state.clone());
    let csv_id = create_ready(&fixture, "tasks", "csv").await;
    let url = format!("/api/data-exports/{csv_id}/download");
    assert_eq!(
        http_get(&router, &url, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http_get(
            &router,
            &format!(
                "{url}?token={}&jwt={}",
                fixture.admin_token, fixture.admin_token
            ),
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http_get(&router, &url, Some(&fixture.second_token))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = http_get(&router, &url, Some(&fixture.admin_token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/csv; charset=utf-8"
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains(".csv")
    );
    let csv = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(csv.starts_with(&[0xef, 0xbb, 0xbf]));
    let mut csv_reader = csv::Reader::from_reader(&csv[3..]);
    let csv_headers = csv_reader.headers().unwrap().clone();
    let csv_rows: Vec<Vec<String>> = csv_reader
        .records()
        .map(|r| r.unwrap().iter().map(str::to_owned).collect())
        .collect();
    // The in-process body retains its download permit until these bytes drop.
    drop(csv_reader);
    drop(csv);
    assert_eq!(csv_rows.len(), 2, "date filter must exclude September task");
    assert!(
        csv_rows
            .iter()
            .flatten()
            .any(|v| v.contains("整理產品操作說明"))
    );

    let xlsx_id = create_ready(&fixture, "tasks", "xlsx").await;
    let response = http_get(
        &router,
        &format!("/api/data-exports/{xlsx_id}/download"),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    );
    let xlsx = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(xlsx.starts_with(b"PK"), "must be a native ZIP/XLSX file");
    let mut workbook: Xlsx<_> = Xlsx::new(std::io::Cursor::new(xlsx.to_vec())).unwrap();
    let range = workbook.worksheet_range("Sheet1").unwrap();
    let workbook_rows: Vec<Vec<String>> = range
        .rows()
        .map(|r| r.iter().map(ToString::to_string).collect())
        .collect();
    assert_eq!(
        workbook_rows[0],
        csv_headers.iter().map(str::to_owned).collect::<Vec<_>>()
    );
    assert_eq!(&workbook_rows[1..], &csv_rows);
    drop(xlsx);
    let usage_id = create_ready(&fixture, "usage", "csv").await;
    fixture
        .state
        .user_db
        .update_user(&fixture.admin_id, None, Some(UserRole::Employee), None)
        .unwrap();
    assert_eq!(
        http_get(
            &router,
            &format!("/api/data-exports/{usage_id}/download"),
            Some(&fixture.admin_token)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN,
        "an old Admin JWT must not preserve cost export access"
    );
    fixture
        .state
        .user_db
        .update_user(&fixture.admin_id, None, Some(UserRole::Admin), None)
        .unwrap();
    let ctx = authenticate_jwt(&fixture.state, &fixture.admin_token).unwrap();
    let cancelled = payload(
        fixture
            .state
            .handler
            .handle("data_exports.cancel", json!({"export_id":xlsx_id}), &ctx)
            .await,
    );
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(
        http_get(
            &router,
            &format!("/api/data-exports/{xlsx_id}/download"),
            Some(&fixture.admin_token)
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
}

/// Only incidental dashboard RPCs use fixture payloads. Export and product-policy
/// RPCs below always dispatch through the actual MethodHandler and source stores.
async fn fixture_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !origin_is_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |mut socket| async move {
        let mut context = None;
        while let Some(Ok(Message::Text(text))) = socket.recv().await {
            let Ok(WsFrame::Request { id, method, params }) = serde_json::from_str(&text) else { continue; };
            let mut response = if method == "connect" {
                match params.get("jwt").and_then(Value::as_str).ok_or_else(|| "JWT required".to_string()).and_then(|jwt| authenticate_jwt(&state, jwt)) {
                    Ok(ctx) => { context = Some(ctx); WsFrame::ok_response("", json!({"must_change_password":false})) }
                    Err(error) => WsFrame::error_response("", &error),
                }
            } else if let Some(ctx) = &context {
                if method.starts_with("data_exports.") || matches!(method.as_str(), "system.product_profile" | "tasks.list" | "tasks.flow_metrics" | "agents.list" | "agents.get" | "agents.inspect" | "channels.available" | "channels.catalog" | "runtime.catalog" | "models.registry" | "wiki.list" | "wiki.pages" | "wiki.stats" | "shared_wiki.pages" | "shared_wiki.read" | "shared_wiki.stats" | "wiki_scope.get" | "memory.browse" | "memory.key_facts" | "wiki.read" | "memory.search") {
                    state.handler.handle(&method, params, ctx).await
                } else {
                    let value = match method.as_str() {
                        "system.status" => json!({"version":"隔離測試","uptime_seconds":0,"agents_count":1,"channels_connected":0,"gateway_address":"127.0.0.1","edition_profile":"enterprise","is_appliance":false}),
                        "license.status" => json!({"tier":"open_source","mode":"opensource","installed":false}),
                        "approvals.list" => json!({"approvals":[],"total":0}),
                        "chat.sessions.list" => json!({"sessions":[]}),
                        "channels.status" => json!({"channels":[],"config":{},"access_policy":{}}),
                        "accounts.list" => json!({"accounts":[]}),
                        "models.providers" => json!({"providers":[]}),
                        "models.list" => json!({"models":[{"id":"claude-sonnet-4-6","label":"Claude Sonnet（隔離選項）","type":"cloud","provider":"anthropic","source":"fallback"},{"id":"gpt-4o","label":"OpenAI（測試限制）","type":"cloud","provider":"openai","source":"fallback"}],"default_local":null,"discovered_at":"2026-10-11T03:00:00Z"}),
                        "system.get_config" => json!({"product":{"profile":"restricted"}}),
                        "system.version" => json!({"current_version":"隔離測試","latest_version":"隔離測試","update_available":false}),
                        "tasks.stats" => json!({"todo":3,"in_progress":0,"done":0,"blocked":0,"total":3}),
                        "audit.unified_log" => json!({"entries":[],"total":0}),
                        "budget.incidents" => json!({"incidents":[]}),
                        "growth.daily_report" => json!({"agents":[],"summary":{}}),
                        "growth.snapshot" => json!({"agents":[],"total":0,"level":0}),
                        "fork.list" => json!({"forks":[]}),
                        "memory.stats" => json!({"agents":[],"total_memories":0,"total_facts":0}),
                        _ => json!({"items":[],"agents":[],"entries":[],"channels":[],"providers":[],"models":[],"runtimes":[],"accounts":[],"sessions":[],"notifications":[],"tasks":[],"widgets":[],"users":[],"layout":null}),
                    };
                    WsFrame::ok_response("", value)
                }
            } else { WsFrame::error_response("", "authentication required") };
            if let WsFrame::Response { id: response_id, .. } = &mut response { *response_id = id; }
            if socket.send(Message::Text(serde_json::to_string(&response).unwrap().into())).await.is_err() { break; }
        }
    }).into_response()
}

#[cfg(feature = "dashboard")]
#[tokio::test]
#[ignore = "manual local browser fixture; bounded at 12 minutes and never starts providers"]
async fn data_export_browser_fixture() {
    let fixture = fixture().await;
    let state = fixture.state.clone();
    let router = Router::new()
        .route("/api/login", post(handle_login))
        .route("/api/me", get(handle_me))
        .route("/api/refresh", post(handle_refresh))
        .route("/api/first-run/status", get(handle_first_run_status))
        .route(
            "/api/data-exports/{id}/download",
            get(super::data_exports_http::download),
        )
        .route("/ws", get(fixture_ws))
        .with_state(state)
        .merge(duduclaw_dashboard::dashboard_router());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18091")
        .await
        .unwrap();
    let artifacts = PathBuf::from("/private/tmp/duduclaw-export-browser-fixture");
    std::fs::create_dir_all(&artifacts).unwrap();
    let stop = artifacts.join("stop");
    let _ = std::fs::remove_file(&stop);
    let ready = artifacts.join("fixture-ready.json");
    std::fs::write(&ready, serde_json::to_vec_pretty(&json!({"base_url":"http://127.0.0.1:18091","email":"export-fixture@example.invalid","password":PASSWORD,"agent_id":AGENT,"from_date":"2026-10-11","through_date":"2026-10-11","isolated_fixture":true,"production_rpc":["data_exports.create","data_exports.get","data_exports.cancel","system.product_profile"],"expected_logical_rows":{"tasks":2,"conversations":2,"usage":1,"memories":2,"wiki":1}})).unwrap()).unwrap();
    println!("DATA_EXPORT_BROWSER_FIXTURE_READY {}", ready.display());
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12 * 60);
        while !stop.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });
    // Graceful shutdown alone waits indefinitely for open WebSockets. The
    // outer timeout bounds this test fixture even when a browser stops responding.
    match tokio::time::timeout(std::time::Duration::from_secs(12 * 60 + 5), async {
        server.await
    })
    .await
    {
        Ok(result) => result.unwrap(),
        Err(_) => println!("DATA_EXPORT_BROWSER_FIXTURE_STOPPED_AT_HARD_DEADLINE"),
    }
    let _ = std::fs::remove_file(ready);
}
