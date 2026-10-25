//! Cross-crate HTTP composition with isolated homes and owned loopback servers.
//! The CLI dependency is a normal library, so its signed clients are not mocks.
use super::*;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request};
use axum::middleware::Next;
use axum::response::Response;
use duduclaw_cli::mcp_dispatch::McpDispatcher;
use duduclaw_cli::mcp_http_server::{HttpServerConfig, build_router};
use duduclaw_cli::mcp_redaction::McpRedactionLayer;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tower::ServiceExt;

const EXTERNAL_KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";
const INTERNAL_KEY: &str = "ddc_prod_fedcba9876543210fedcba9876543210";
const ALIAS: &str = "<REDACT:EMAIL:0123456789abcdef0123456789abcdef>";
const SESSION: &str = "owned-composed-cli-session";
const ORIGINAL_ID: &str = "unit-message-a";

#[derive(Clone, Debug)]
struct SignedExchange {
    path: String,
    headers: HeaderMap,
    request: Vec<u8>,
    status: u16,
    response: Value,
}
impl SignedExchange {
    fn arguments(&self) -> Value {
        serde_json::from_slice(&self.request).unwrap()
    }
}
type Exchanges = Arc<Mutex<Vec<SignedExchange>>>;

async fn capture_signed_exchange(
    State(exchanges): State<Exchanges>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    let headers = parts.headers.clone();
    let request_bytes = to_bytes(body, crate::google_identity::execute::MAX_BODY)
        .await
        .unwrap();
    let response = next
        .run(Request::from_parts(
            parts,
            Body::from(request_bytes.clone()),
        ))
        .await;
    let (parts, body) = response.into_parts();
    let status = parts.status.as_u16();
    let response_bytes = to_bytes(body, crate::google_identity::execute::MAX_RESPONSE)
        .await
        .unwrap();
    let response_value = serde_json::from_slice(&response_bytes).unwrap();
    {
        let mut saved = exchanges.lock().unwrap();
        assert!(saved.len() < 20, "owned fixture exchange bound");
        saved.push(SignedExchange {
            path,
            headers,
            request: request_bytes.to_vec(),
            status,
            response: response_value,
        });
    }
    Response::from_parts(parts, Body::from(response_bytes))
}

struct OwnedTask<T>(Option<tokio::task::JoinHandle<T>>);
impl<T> OwnedTask<T> {
    fn new(task: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(task))
    }
    fn is_finished(&self) -> bool {
        self.0.as_ref().unwrap().is_finished()
    }
    async fn finish(mut self) -> T {
        // Keep the handle in self during the await, so cancelling this
        // finish future also invokes the aborting Drop implementation.
        match tokio::time::timeout(Duration::from_secs(30), self.0.as_mut().unwrap()).await {
            Ok(result) => {
                drop(self.0.take());
                result.unwrap()
            }
            Err(_) => {
                let task = self.0.take().unwrap();
                task.abort();
                let _ = task.await;
                panic!("owned CLI HTTP call did not finish within its original limit");
            }
        }
    }
    async fn cancel(mut self) {
        let task = self.0.take().unwrap();
        task.abort();
        match task.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("owned CLI call finished before cancellation"),
        }
    }
}
impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

fn alias_layer(home: &Path) -> Arc<McpRedactionLayer> {
    let mut config = duduclaw_redaction::RedactionConfig::default();
    config.enabled = true;
    config.profiles = vec![];
    config.tool_egress.insert(
        "gmail_read".into(),
        duduclaw_redaction::ToolEgressRule {
            restore_args: duduclaw_redaction::RestoreArgsMode::Restore,
            audit_reveal: false,
        },
    );
    let manager = Arc::new(
        duduclaw_redaction::RedactionManager::open(
            config,
            duduclaw_redaction::ManagerPaths::under_home(home),
        )
        .unwrap(),
    );
    manager
        .vault()
        .insert_mapping(
            ALIAS,
            ORIGINAL_ID,
            "external",
            Some(SESSION),
            "EMAIL",
            "unit",
            &duduclaw_redaction::RestoreScope::AllScopes { scopes: vec![] },
            false,
            24,
        )
        .unwrap();
    Arc::new(McpRedactionLayer {
        manager,
        agent_id: "not-the-authenticated-employee".into(),
        session_id: SESSION.into(),
    })
}

fn dispatcher(home: &Path, layer: Arc<McpRedactionLayer>) -> McpDispatcher {
    McpDispatcher::new(
        home.to_path_buf(),
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        Arc::new(duduclaw_memory::SqliteMemoryEngine::new(&home.join("cli-memory.db")).unwrap()),
        "not-the-authenticated-employee".into(),
        Arc::new(duduclaw_cli::odoo_pool::OdooConnectorPool::default()),
        duduclaw_cli::mcp_rate_limit::RateLimiter::new(),
        duduclaw_cli::mcp_memory_quota::DailyQuota::new(),
    )
    .with_redaction(Some(layer))
}

struct Composed {
    fixture: Fixture,
    provider: D1Wire,
    _native: Option<crate::google_workspace::native_call::testing::Guard>,
    gateway: OwnedTask<()>,
    gateway_url: String,
    exchanges: Exchanges,
    dispatcher: McpDispatcher,
    cli: axum::Router,
    redaction: Arc<McpRedactionLayer>,
}
impl Composed {
    async fn new(home_fallback: bool, native_fixture: bool) -> Self {
        // Do not overwrite global environment or accidentally target port18789.
        assert!(std::env::var_os("DUDUCLAW_PORT").is_none());
        let fixture = d1_fixture(home_fallback);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config_path = fixture.home.path().join("config.toml");
        let original = std::fs::read_to_string(&config_path).unwrap();
        assert!(original.contains("scopes=['google:read','google:write']"));
        let config = original.replace(
            "scopes=['google:read','google:write']",
            "scopes=['google:read','google:write']\nis_external=true",
        );
        std::fs::write(&config_path, format!(
            "{config}\n[gateway]\nbind='127.0.0.1'\nport={}\n[mcp_keys.{INTERNAL_KEY}]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n",
            address.port(), chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        let agent = fixture.home.path().join("agents/bot/agent.toml");
        let policy = std::fs::read_to_string(&agent).unwrap();
        std::fs::write(
            agent,
            format!("{policy}\n[[capabilities.policy]]\ntool='gmail_read'\neffect='ask'\n",),
        )
        .unwrap();
        duduclaw_core::ensure_identity_key(fixture.home.path()).unwrap();
        assert_eq!(
            duduclaw_core::gateway_port_for_home(fixture.home.path()).0,
            address.port()
        );

        let provider = d1_wire(|_, _| {
            D1Reply::json(json!({
                "id": ORIGINAL_ID, "snippet": "composed owned result", "ignored": D1_SECRET,
            }))
        })
        .await;
        let native = native_fixture.then(|| {
            crate::google_workspace::native_call::testing::install(
                fixture.home.path(),
                &provider.endpoint,
            )
        });
        let exchanges = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(crate::google_identity::execute::ExecuteState::new(
            fixture.home.path().to_path_buf(),
            Some(fixture.db.clone()),
        ));
        let gateway_app = crate::google_identity::execute::router(state).layer(
            axum::middleware::from_fn_with_state(exchanges.clone(), capture_signed_exchange),
        );
        let gateway = OwnedTask::new(tokio::spawn(async move {
            axum::serve(
                listener,
                gateway_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        }));
        let redaction = alias_layer(fixture.home.path());
        let dispatcher = dispatcher(fixture.home.path(), redaction.clone());
        let cli_config = HttpServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            fixture.home.path().to_path_buf(),
        );
        let cli = build_router(&cli_config, dispatcher.clone());
        Self {
            fixture,
            provider,
            _native: native,
            gateway,
            gateway_url: format!("http://{address}"),
            exchanges,
            dispatcher,
            cli,
            redaction,
        }
    }

    fn params() -> Value {
        json!({"name":"gmail_read",
            "_meta":{"selection_stamp":"model-metadata-cannot-authorize"},
            "host_google_account":{"selected_google_account":"forged@unit.test"},
            "arguments":{"message_id":ALIAS,"agent_id":"forged-agent",
                "namespace":"forged-namespace","selection_stamp":"model-cannot-authorize"}})
    }
    fn start_call(&self) -> OwnedTask<(u16, Value)> {
        let app = self.cli.clone();
        OwnedTask::new(tokio::spawn(async move {
            cli_request(app, EXTERNAL_KEY, Self::params()).await
        }))
    }
    fn captured(&self) -> Vec<SignedExchange> {
        self.exchanges.lock().unwrap().clone()
    }
    fn effect_count(&self) -> usize {
        self.provider.hits.load(Ordering::SeqCst)
    }
    async fn shutdown(mut self) {
        if let Some(task) = self.gateway.0.take() {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        self.provider.task.abort();
        assert!((&mut self.provider.task).await.unwrap_err().is_cancelled());
        drop(self._native.take());
    }
    async fn pending(
        &self,
        call: &OwnedTask<(u16, Value)>,
    ) -> (
        crate::approval::ApprovalBroker,
        crate::approval::ApprovalRecord,
    ) {
        let broker = crate::approval::ApprovalBroker::open(self.fixture.home.path()).unwrap();
        for _ in 0..400 {
            if let Some(record) = broker.list_pending(None).await.unwrap().into_iter().next() {
                return (broker, record);
            }
            assert!(
                !call.is_finished(),
                "CLI returned before creating the real Policy Ask: {:?}",
                self.captured()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("real Policy Ask did not become pending");
    }
    fn assert_before_approval(
        &self,
        record: &crate::approval::ApprovalRecord,
        home: bool,
    ) -> Value {
        let exchanges = self.captured();
        assert_eq!(
            exchanges.len(),
            1,
            "only signed selection is permitted before Ask"
        );
        let selection = &exchanges[0];
        assert_eq!(
            selection.path,
            duduclaw_core::google_identity::GOOGLE_SELECTION_ROUTE
        );
        assert_eq!(selection.status, 200);
        assert_eq!(selection.arguments()["purpose"], "select_google_account");
        let stamp = selection.response["selection"].clone();
        assert!(
            stamp["opaque_id"]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        );
        assert_eq!(
            stamp["approval"]["selected_identity"],
            if home {
                "home_oauth_fallback"
            } else {
                "personal"
            }
        );
        assert_eq!(record.agent_id, "bot");
        assert_eq!(record.payload["arguments"]["message_id"], ALIAS);
        assert!(!record.payload.to_string().contains(ORIGINAL_ID));
        assert!(
            !record
                .payload
                .to_string()
                .contains(stamp["opaque_id"].as_str().unwrap())
        );
        assert!(!record.payload.to_string().contains(D1_SECRET));
        if home {
            assert!(record.summary.contains("公司共用") && record.summary.contains("主體未驗證"));
            assert!(!record.summary.contains("alice@unit.test"));
        } else {
            assert!(record.summary.contains("alice@unit.test"));
            assert!(record.summary.contains("未核准改用公司帳號"));
        }
        assert_eq!(self.effect_count(), 0);
        stamp
    }
}
impl Drop for Composed {
    fn drop(&mut self) {
        // Abort this fixture's listener before its private home and registry drop.
        if let Some(task) = self.gateway.0.as_ref() {
            task.abort();
        }
    }
}

async fn cli_request(app: axum::Router, bearer: &str, params: Value) -> (u16, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/v1/call")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::from(
                    json!({"jsonrpc":"2.0","id":73,"method":"tools/call","params":params})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(
        response.into_body(),
        crate::google_identity::execute::MAX_RESPONSE,
    )
    .await
    .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn fresh_headers(home: &Path, signed_bytes: &[u8]) -> HeaderMap {
    use crate::computer_use_sessions::auth;
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let identity =
        duduclaw_core::mint_identity_token(&duduclaw_core::load_identity_key(home).unwrap(), "bot");
    let signature = duduclaw_core::internal_request_signature(
        INTERNAL_KEY.as_bytes(),
        "bot",
        &identity,
        &timestamp,
        &nonce,
        signed_bytes,
    );
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (auth::AGENT_ID_HEADER, "bot"),
        (auth::TIMESTAMP_HEADER, timestamp.as_str()),
        (auth::NONCE_HEADER, nonce.as_str()),
        (auth::SIGNATURE_HEADER, signature.as_str()),
    ] {
        headers.insert(name, value.parse().unwrap());
    }
    headers
}
async fn post_signed(f: &Composed, bytes: Vec<u8>, headers: HeaderMap) -> (u16, Value) {
    let mut response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .post(format!(
            "{}{}",
            f.gateway_url,
            crate::google_identity::execute::ROUTE
        ))
        .header("content-type", "application/json")
        .headers(headers)
        .body(bytes)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let mut raw = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        assert!(raw.len() + chunk.len() <= crate::google_identity::execute::MAX_RESPONSE);
        raw.extend_from_slice(&chunk);
    }
    (status, serde_json::from_slice(&raw).unwrap())
}
fn assert_real_mac(home: &Path, exchange: &SignedExchange) {
    use crate::computer_use_sessions::auth;
    let get = |name| exchange.headers.get(name).unwrap().to_str().unwrap();
    let identity =
        duduclaw_core::mint_identity_token(&duduclaw_core::load_identity_key(home).unwrap(), "bot");
    assert_eq!(get(auth::AGENT_ID_HEADER), "bot");
    assert_eq!(get(auth::NONCE_HEADER).len(), 32);
    assert_eq!(
        get(auth::SIGNATURE_HEADER),
        duduclaw_core::internal_request_signature(
            INTERNAL_KEY.as_bytes(),
            "bot",
            &identity,
            get(auth::TIMESTAMP_HEADER),
            get(auth::NONCE_HEADER),
            &exchange.request,
        )
    );
}

#[tokio::test]
async fn d1_cli_composed_approved_ask_restores_and_consumes_one_real_host_claim() {
    for home in [false, true] {
        let f = Composed::new(home, true).await;
        let call = f.start_call();
        let (broker, record) = f.pending(&call).await;
        let stamp = f.assert_before_approval(&record, home);
        // A synthetic API decision, not Dashboard authentication or a human.
        broker
            .decide(&record.id, true, "synthetic-admin-fixture")
            .await
            .unwrap();
        let (status, result) = call.finish().await;
        assert_eq!(status, 200);
        assert!(result.get("error").is_none(), "{result}");
        assert_ne!(result["result"]["isError"], true, "{result}");
        assert!(
            result["result"]["content"]
                .as_array()
                .is_some_and(|parts| !parts.is_empty())
        );
        assert!(result.to_string().contains("composed owned result"));
        for secret in [D1_SECRET, EXTERNAL_KEY, INTERNAL_KEY] {
            assert!(!result.to_string().contains(secret));
        }
        if home {
            assert!(result.to_string().contains("公司共用"));
        }
        assert_eq!(f.effect_count(), 1);
        assert_eq!(f.provider.requests.lock().unwrap().len(), 1);
        assert!(
            f.provider.requests.lock().unwrap()[0]
                .1
                .contains(ORIGINAL_ID)
        );
        let exchanges = f.captured();
        let binds: Vec<_> = exchanges
            .iter()
            .filter(|r| r.arguments()["purpose"] == "bind_google_arguments")
            .collect();
        assert_eq!(binds.len(), 2);
        for bind in binds {
            assert_eq!(bind.status, 200);
            assert_eq!(bind.arguments()["opaque_id"], stamp["opaque_id"]);
            assert_eq!(bind.arguments()["arguments"]["message_id"], ORIGINAL_ID);
        }
        let executes: Vec<_> = exchanges
            .iter()
            .filter(|r| r.path == crate::google_identity::execute::ROUTE)
            .collect();
        assert_eq!(executes.len(), 1);
        let execute = executes[0];
        assert_eq!(execute.arguments()["selection_stamp"], stamp["opaque_id"]);
        assert_eq!(execute.arguments()["arguments"]["message_id"], ORIGINAL_ID);
        assert!(execute.arguments().get("host_google_account").is_none());
        for exchange in &exchanges {
            assert_real_mac(f.fixture.home.path(), exchange);
        }
        let audit = d1_r2_execute_audit_rows(&f.fixture);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0]["agent_id"], "bot");
        assert_eq!(audit[0]["success"], true);
        assert_eq!(audit[0]["fallback"], home);
        assert!(!audit[0].to_string().contains(D1_SECRET));
        assert_eq!(
            crate::google_workspace::native_call::testing::observations(f.fixture.home.path())
                .len(),
            1
        );

        // The first valid fresh nonce below has not been used by the CLI.
        // Its signature authenticates original bytes, not the tampered bytes.
        let headers = fresh_headers(f.fixture.home.path(), &execute.request);
        let mut changed = execute.arguments();
        changed["arguments"]["message_id"] = json!("unit-message-b");
        let (status, refused) =
            post_signed(&f, serde_json::to_vec(&changed).unwrap(), headers.clone()).await;
        assert_eq!(status, 401);
        assert_eq!(refused["code"], "no_verified_host");
        // MAC failure must not consume this fresh nonce. Unchanged bytes now
        // pass auth but fail the already-consumed private selection claim.
        let (status, refused) = post_signed(&f, execute.request.clone(), headers).await;
        assert_eq!(status, 403);
        assert_eq!(refused["ok"], false);
        assert_ne!(refused["code"], "no_verified_host");
        let mut changed_stamp = execute.arguments();
        changed_stamp["selection_stamp"] = json!(uuid::Uuid::new_v4().to_string());
        let bytes = serde_json::to_vec(&changed_stamp).unwrap();
        let (status, refused) = post_signed(
            &f,
            bytes.clone(),
            fresh_headers(f.fixture.home.path(), &bytes),
        )
        .await;
        assert_eq!(status, 403);
        assert_eq!(refused["ok"], false);
        assert_eq!(f.effect_count(), 1);
        f.shutdown().await;
    }
}

#[tokio::test]
async fn d1_cli_composed_mapping_or_original_source_changes_during_ask_never_send() {
    for home in [false, true] {
        for mutation in ["mapping", "consent", "source"] {
            let f = Composed::new(home, true).await;
            let call = f.start_call();
            let (broker, record) = f.pending(&call).await;
            f.assert_before_approval(&record, home);
            match mutation {
                "mapping" => f
                    .redaction
                    .manager
                    .vault()
                    .insert_mapping(
                        ALIAS,
                        "unit-message-b",
                        "external",
                        Some(SESSION),
                        "EMAIL",
                        "unit",
                        &duduclaw_redaction::RestoreScope::AllScopes { scopes: vec![] },
                        false,
                        24,
                    )
                    .unwrap(),
                "consent" => {
                    f.fixture
                        .db
                        .set_google_consent(&f.fixture.user, "bot", false, &f.fixture.user)
                        .unwrap();
                }
                "source" => {
                    let path = f.fixture.home.path().join("config.toml");
                    let raw = std::fs::read_to_string(&path).unwrap();
                    assert!(raw.contains("google_workspace=true"));
                    std::fs::write(
                        path,
                        raw.replace("google_workspace=true", "google_workspace=false"),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            broker
                .decide(&record.id, true, "synthetic-admin-fixture")
                .await
                .unwrap();
            let (status, result) = call.finish().await;
            assert_eq!(status, 403, "{mutation}: {result}");
            assert_eq!(result["error"]["code"], -32003, "{mutation}: {result}");
            let exchanges = f.captured();
            assert_eq!(
                exchanges
                    .iter()
                    .filter(|r| r.arguments()["purpose"] == "select_google_account")
                    .count(),
                1
            );
            assert!(
                exchanges
                    .iter()
                    .all(|r| r.path != crate::google_identity::execute::ROUTE)
            );
            if mutation == "mapping" {
                assert_eq!(exchanges.len(), 1);
            }
            assert_eq!(f.effect_count(), 0);
            assert!(
                crate::google_workspace::native_call::testing::observations(f.fixture.home.path())
                    .is_empty()
            );
            f.shutdown().await;
        }
    }
}

#[tokio::test]
async fn d1_cli_composed_denied_or_cancelled_ask_has_no_final_bind_or_effect() {
    for home in [false, true] {
        for cancel in [false, true] {
            let f = Composed::new(home, true).await;
            let call = f.start_call();
            let (broker, record) = f.pending(&call).await;
            f.assert_before_approval(&record, home);
            if cancel {
                call.cancel().await;
                // A late decision cannot resurrect the cancelled CLI future.
                broker
                    .decide(&record.id, true, "synthetic-admin-after-cancel")
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(2100)).await;
            } else {
                broker
                    .decide(&record.id, false, "synthetic-admin-fixture")
                    .await
                    .unwrap();
                let (status, result) = call.finish().await;
                assert_eq!(status, 403);
                assert_eq!(result["error"]["code"], -32003);
            }
            assert_eq!(f.captured().len(), 1);
            assert_eq!(f.effect_count(), 0);
            assert!(
                crate::google_workspace::native_call::testing::observations(f.fixture.home.path())
                    .is_empty()
            );
            f.shutdown().await;
        }
    }
}

#[tokio::test]
async fn d1_cli_composed_public_principal_or_wrong_bearer_cannot_create_a_receipt() {
    let f = Composed::new(false, true).await;
    let (status, result) = cli_request(
        f.cli.clone(),
        "ddc_prod_00000000000000000000000000000000",
        Composed::params(),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(result["error"]["code"], -32003);
    let principal =
        duduclaw_cli::mcp_auth::authenticate_with_key(EXTERNAL_KEY, f.fixture.home.path()).unwrap();
    let namespace = duduclaw_cli::mcp_namespace::resolve_for_caller(
        &principal,
        duduclaw_cli::mcp_namespace::CallerIdentity {
            verified_agent: None,
            client_is_agent: true,
        },
    )
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        f.dispatcher
            .dispatch_tool_call(&principal, &namespace, &Composed::params(), &json!(74)),
    )
    .await
    .unwrap();
    assert_eq!(result["error"]["code"], -32601);
    assert!(f.captured().is_empty());
    assert_eq!(f.effect_count(), 0);
    assert!(
        crate::approval::ApprovalBroker::open(f.fixture.home.path())
            .unwrap()
            .list_pending(None)
            .await
            .unwrap()
            .is_empty()
    );
    f.shutdown().await;
}

#[tokio::test]
async fn d1_cli_composed_missing_owned_native_fixture_refuses_after_real_execute() {
    for home in [false, true] {
        let f = Composed::new(home, false).await;
        let call = f.start_call();
        let (broker, record) = f.pending(&call).await;
        f.assert_before_approval(&record, home);
        broker
            .decide(&record.id, true, "synthetic-admin-fixture")
            .await
            .unwrap();
        let (status, result) = call.finish().await;
        assert_eq!(status, 200);
        assert_eq!(result["result"]["isError"], true, "{result}");
        let exchanges = f.captured();
        let executes: Vec<_> = exchanges
            .iter()
            .filter(|r| r.path == crate::google_identity::execute::ROUTE)
            .collect();
        assert_eq!(executes.len(), 1);
        assert_eq!(executes[0].status, 403);
        assert_eq!(
            executes[0].response["code"],
            "personal_external_unsupported"
        );
        for exchange in &exchanges {
            assert_real_mac(f.fixture.home.path(), exchange);
        }
        assert_eq!(f.effect_count(), 0);
        f.shutdown().await;
    }
}
