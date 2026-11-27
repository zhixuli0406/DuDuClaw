//! Exercise the real bound executor and legacy dispatcher before credentials.
use super::*;
use duduclaw_gateway::{isolation_connector_odoo::OdooConnectorProvider,
    isolation_connectors::{BoundConnectorExecutor, ConnectorCatalog, ConnectorKind,
        ProductConnectorAccount, SelectedConnector}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{bindings::BindingStore, RequestContext, VerificationError,
    resources::ResourceRoots, tool_policy::{self, ResourceResolver}};
use std::{path::PathBuf, result::Result, sync::atomic::{AtomicUsize, Ordering}, time::Duration};

struct SecretSpy {
    url: String,
    resolutions: Arc<AtomicUsize>,
    replace: Arc<Mutex<Option<(PathBuf, Vec<u8>)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for SecretSpy { fn drop(&mut self) { self.task.abort(); } }
impl SecretSpy {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let resolutions = Arc::new(AtomicUsize::new(0));
        let replace = Arc::new(Mutex::new(None::<(PathBuf, Vec<u8>)>));
        let seen = resolutions.clone();
        let replacement = replace.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                seen.fetch_add(1, Ordering::SeqCst);
                let mut request = Vec::new();
                loop {
                    let mut buf = [0; 1024];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                }
                assert!(String::from_utf8_lossy(&request).starts_with("GET /v1/secret/data/odoo "));
                // Simulate a path replacement exactly at secret resolution,
                // after local validation but before Odoo authentication.
                if let Some((path, bytes)) = replacement.lock().unwrap().take() {
                    std::fs::remove_file(&path).unwrap();
                    std::fs::write(path, bytes).unwrap();
                }
                let body = r#"{"data":{"data":{"value":"agent-test-key"}}}"#;
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self { url, resolutions, replace, task }
    }
}

fn fixture(url: &str, secret: &SecretSpy) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/bot/attachments")).unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[odoo]\nurl='{url}'\ndb='test'\nusername='company'\napi_key_enc='secret://vault/odoo'\nmember_credentials=true\n\
        [odoo_access]\npermission_mode='follow_odoo_account'\npermission_mode_ack={{by='owner',at='2026-11-01T00:00:00Z'}}\nalways_refuse=[]\nattachment_max_mb=1\n\
        [secret_manager]\nbackend='vault'\nvault_addr='{}'\nvault_token='test-token'\n", secret.url)).unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\nstatus='active'\n[odoo]\nusername='agent'\napi_key_enc='secret://vault/odoo'\nallowed_models=[]\nallowed_actions=[]\n").unwrap();
    home
}

fn set(home: &Path, file: &str, table: &str, key: &str, value: toml::Value) {
    let path = home.join(file);
    let mut config: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    config[table][key] = value;
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
}

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
struct Live;
impl duduclaw_isolation::capability::LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
struct Resolver { account: String, binding: EmployeeBinding }
impl ResourceResolver for Resolver {
    fn resolve(&self, context: &RequestContext, _: &tool_policy::ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Connector { account: text(&self.account), binding: self.binding.clone() }])
    }
}

async fn bound_call(home: &Path, tool: &str, params: &Value) -> Value {
    let employee = EmployeeId([2; 16]);
    let binding = EmployeeBinding::Personal(text("alice"));
    let principal = Principal::Human { user_id: text("alice"), generation: 1 };
    let catalog = Arc::new(ConnectorCatalog::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap());
    let account = catalog.register("Odoo", "odoo", ConnectorKind::Personal, &binding).unwrap();
    let mut bindings = BindingStore::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(employee, &binding).unwrap();
    bindings.register_connector(account.as_str(), &binding).unwrap();
    let context = RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee, run: RunId([3; 16]),
        task: None, principal, employee_binding: binding.clone(), lane: Lane::Standard,
        tools: List::new(vec![text(tool)]).unwrap(),
        resources: List::new(vec![ResourceGrant::EmployeeScope { id: employee }]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Live));
    let provider = Arc::new(OdooConnectorProvider::new(home.into(), "bot".into(), employee,
        ProductConnectorAccount::Personal { account: account.as_str().into(), owner: "alice".into() }).unwrap());
    let selected = SelectedConnector::select_with_policy(&context, tool, params,
        Arc::new(Mutex::new(bindings)), catalog, Some(provider.clone())).unwrap();
    let call = tool_policy::authorize(&context, tool, params,
        &Resolver { account: account.as_str().into(), binding }).unwrap();
    let isolated = Arc::new(crate::mcp_isolation::IsolatedCall { home: home.into(), context,
        agent: "bot".into(), roots: Arc::new(ResourceRoots::default()), parser: None, services: None,
        worker: None, execution: None, forks: None, record_resolver: None });
    let executor = crate::mcp_isolation_connector_odoo::OdooBoundExecutor::new(provider);
    crate::mcp_isolation::ISOLATED_CALL.scope(isolated, executor.execute(selected, call)).await
        .expect("local refusal must return its policy error, not fail in provider.prepare")
}

async fn run(home: &Path, protected: bool, tool: &str, params: &Value) -> Value {
    crate::mcp::with_token_verified_caller("bot", async {
        tokio::time::timeout(Duration::from_secs(5), async {
            if protected { bound_call(home, tool, params).await }
            else { call(home, "agent", false, tool, params).await }
        }).await.expect("preconnect regression hung on network I/O")
    }).await
}

fn no_connection(listener: &std::net::TcpListener, secret: &SecretSpy) {
    assert_eq!(secret.resolutions.load(Ordering::SeqCst), 0, "credential was resolved before local refusal");
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock,
        "Odoo was contacted before local refusal");
}

#[tokio::test]
async fn protected_execute_refuses_local_policy_before_secret_or_connection() {
    let secret = SecretSpy::start().await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    for (model, method, rule, expected) in [
        ("crm.lead", "unlink", None, "blocked"),
        ("res.partner", "search_read", None, "block list gate"),
        ("ir.model", "write", None, "block list gate"),
        ("crm.lead", "write", Some("write:crm.lead"), "一律不允許"),
    ] {
        let home = fixture(&format!("http://{}", listener.local_addr().unwrap()), &secret);
        // A possible Follow identity must not bypass Engine in isolation.
        crate::mcp::odoo_access_tests::personal_db(home.path(), "bot", true);
        if let Some(rule) = rule {
            set(home.path(), "config.toml", "odoo_access", "always_refuse", toml::Value::Array(vec![rule.into()]));
        }
        let result = run(home.path(), true, "odoo_execute",
            &json!({"model":model,"method":method,"args":[[9]],"kwargs":{}})).await;
        assert_eq!(result["isError"], true, "{result}");
        assert!(result.to_string().contains(expected), "{result}");
        no_connection(&listener, &secret);
    }
}

#[tokio::test]
async fn attachment_refusals_precede_secret_and_connection_on_both_paths() {
    let secret = SecretSpy::start().await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    for protected in [true, false] {
        let home = fixture(&format!("http://{}", listener.local_addr().unwrap()), &secret);
        set(home.path(), "agents/bot/agent.toml", "odoo", "allowed_actions",
            toml::Value::Array(vec!["attach:crm.lead".into()]));
        let root = home.path().join("agents/bot/attachments");
        std::fs::write(root.join("large.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        std::fs::write(root.join("bad.exe"), b"MZ").unwrap();
        std::fs::write(root.join("bad.png"), b"not an image").unwrap();
        std::fs::write(root.join("bad.pdf"), b"%PDF-1.7 /JavaScript").unwrap();
        std::fs::write(home.path().join("outside.txt"), b"outside secret").unwrap();
        let cases = vec![("../../../outside.txt", "attachments"), ("large.txt", "attachment_max_mb"),
            ("bad.exe", "副檔名"), ("bad.png", "圖片"), ("bad.pdf", "PDF")];
        #[cfg(unix)]
        let cases = {
            std::os::unix::fs::symlink(home.path().join("outside.txt"), root.join("link.txt")).unwrap();
            let mut cases = cases;
            cases.push(("link.txt", "attachments"));
            cases
        };
        for (file, expected) in cases {
            let result = run(home.path(), protected, "odoo_attachment_add",
                &json!({"model":"crm.lead","res_id":9,"file":file})).await;
            assert_eq!(result["isError"], true, "protected={protected}, file={file}: {result}");
            assert!(result.to_string().contains(expected), "{result}");
            no_connection(&listener, &secret);
        }
    }
}

#[tokio::test]
async fn attachment_upload_keeps_validated_bytes_after_path_replacement_on_both_paths() {
    use base64::Engine;
    use crate::mcp::odoo_attachment::tests::{clean_pdf, image_bytes};
    for protected in [true, false] {
        for image in [false, true] {
            let secret = SecretSpy::start().await;
            let stub = Stub::start(true).await;
            let home = fixture(&stub.url, &secret);
            set(home.path(), "agents/bot/agent.toml", "odoo", "allowed_actions",
                toml::Value::Array(vec!["attach:crm.lead".into()]));
            let (file, input, expected, name, mime) = if image {
                let input = image_bytes(image::ImageFormat::WebP, 2, 2);
                let mut output = Vec::new();
                image::load_from_memory(&input).unwrap().write_with_encoder(
                    image::codecs::png::PngEncoder::new(&mut output)).unwrap();
                ("photo.webp", input, output, "photo.png", "image/png")
            } else {
                let pdf = clean_pdf();
                ("x.pdf", pdf.clone(), pdf, "x.pdf", "application/pdf")
            };
            let path = home.path().join("agents/bot/attachments").join(file);
            std::fs::write(&path, input).unwrap();
            *secret.replace.lock().unwrap() = Some((path.clone(), b"replacement must never upload".to_vec()));
            let result = run(home.path(), protected, "odoo_attachment_add",
                &json!({"model":"crm.lead","res_id":9,"file":file})).await;
            assert_ne!(result["isError"], true, "protected={protected}: {result}");
            assert_eq!(secret.resolutions.load(Ordering::SeqCst), 1);
            assert_eq!(std::fs::read(path).unwrap(), b"replacement must never upload");
            let writes = attachment_requests(&stub);
            assert_eq!(writes.len(), 1);
            let values = &writes[0][5][0];
            assert_eq!(values["name"], name);
            assert_eq!(values["mimetype"], mime);
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(values["datas"].as_str().unwrap()).unwrap(), expected);
        }
    }
}
