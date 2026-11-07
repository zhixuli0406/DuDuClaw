use super::*;
use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{bindings::BindingStore, tool_policy::{ResourceResolver, ToolPolicy, authorize}, RequestContext};
use crate::isolation_connectors::{ConnectorCatalog, LiveConnectorSelectionPolicy, ProductConnectorAccount};
use rusqlite::Connection;
struct FixtureAuthority;
impl duduclaw_isolation::capability::LiveRequestVerifier for FixtureAuthority {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
struct FixtureSelection;
impl LiveConnectorSelectionPolicy for FixtureSelection {
    fn supports(&self, tool: &str) -> bool { tool == "google_status" }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Ok(vec![ProductConnectorAccount::Personal { account: "alice-google".into(), owner: "alice".into() }])
    }
}
fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
struct Fixed;
impl ResourceResolver for Fixed {
    fn resolve(&self, context: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(context.record().resources.as_slice().to_vec())
    }
}
fn fixture() -> (Arc<Mutex<BindingStore>>, SelectedConnector, AuthorizedToolCall) {
    let principal = Principal::Human { user_id: text("alice"), generation: 1 };
    let binding = EmployeeBinding::Personal(text("alice"));
    let employee = EmployeeId([2; 16]);
    let mut store = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    store.register_principal(&principal).unwrap();
    store.create_employee(employee, &binding).unwrap();
    store.register_connector("alice-google", &binding).unwrap();
    let bindings = Arc::new(Mutex::new(store));
    let catalog = Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap());
    catalog.register("alice-google", "google").unwrap();
    let context = RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee, run: RunId([3; 16]), task: None,
        principal, employee_binding: binding.clone(), lane: Lane::Standard,
        tools: List::new(vec![text("google_status")]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: employee }, ResourceGrant::Connector {
            account: text("alice-google"), binding }]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(FixtureAuthority));
    let selected = SelectedConnector::select_with_policy(&context, "google_status", &json!({}), bindings.clone(), catalog,
        Some(Arc::new(FixtureSelection))).unwrap();
    let call = authorize(&context, "google_status", &json!({}), &Fixed).unwrap();
    (bindings, selected, call)
}
struct SpyProvider {
    calls: AtomicUsize,
    returned_account: &'static str,
    revoke_during_acquire: Option<Arc<Mutex<BindingStore>>>,
}
impl SpyProvider {
    fn new(account: &'static str) -> Self { Self { calls: AtomicUsize::new(0), returned_account: account, revoke_during_acquire: None } }
}
impl AccountCredentialProvider for SpyProvider {
    fn acquire<'a>(&'a self, account: &'a str, provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(account, "alice-google");
            assert_eq!(provider, "google");
            tokio::task::yield_now().await;
            if let Some(bindings) = &self.revoke_during_acquire { bindings.lock().unwrap().revoke_connector(account).unwrap(); }
            AccountCredential::new(self.returned_account.into(), provider.into(), "fixture-secret-token".into(), u64::MAX)
        })
    }
}
struct ClientFixture { home: tempfile::TempDir, client: NativeConnectorExecutor }
impl std::ops::Deref for ClientFixture {
    type Target = NativeConnectorExecutor;
    fn deref(&self) -> &Self::Target { &self.client }
}
fn executor(provider: Arc<dyn AccountCredentialProvider>) -> ClientFixture {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
    std::fs::write(home.path().join("config.toml"), "[integrations]\ngoogle_workspace=true\ngithub=true\n").unwrap();
    std::fs::write(home.path().join("agents/employee-a/agent.toml"), "[google]\nidentity='home'\n").unwrap();
    let client = NativeConnectorExecutor::new(home.path().to_owned(), "employee-a".into(), provider).unwrap();
    ClientFixture { home, client }
}
#[tokio::test]
async fn isolated_native_connector_revocation_prevents_credential_acquisition() {
    let (bindings, selected, call) = fixture();
    bindings.lock().unwrap().revoke_connector("alice-google").unwrap();
    let provider = Arc::new(SpyProvider::new("alice-google"));
    assert!(executor(provider.clone()).execute(selected, call).await.is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn isolated_native_connector_rechecks_after_await_and_returned_account() {
    let (_, selected, call) = fixture();
    let provider = Arc::new(SpyProvider::new("different-account"));
    assert!(executor(provider.clone()).execute(selected, call).await.is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let (bindings, selected, call) = fixture();
    let mut provider = SpyProvider::new("alice-google");
    provider.revoke_during_acquire = Some(bindings);
    assert!(executor(Arc::new(provider)).execute(selected, call).await.is_err());
}
#[tokio::test]
async fn isolated_native_connector_status_is_account_specific_without_token_output() {
    let (_, selected, call) = fixture();
    let provider = Arc::new(SpyProvider::new("alice-google"));
    let result = executor(provider).execute(selected, call).await.unwrap().to_string();
    assert!(result.contains("alice-google"));
    assert!(!result.contains("fixture-secret-token"));
    assert!(result.contains("provider_verified"));
}
#[tokio::test]
async fn isolated_native_connector_every_http_hop_rechecks_binding_origin_and_expiry() {
    let (bindings, selected, _) = fixture();
    let client = executor(Arc::new(AccountCredentialVault::default()));
    CLIENT_POLICY.scope(client.policy.clone(), BOUND_CONNECTOR.scope(Arc::new(selected), CREDENTIAL_EXPIRY.scope(u64::MAX, async {
        assert!(before_request("google", "https://gmail.googleapis.com/gmail/v1/users/me/messages").is_ok());
        for url in ["http://gmail.googleapis.com/", "https://gmail.googleapis.com.evil.test/", "https://user@gmail.googleapis.com/", "https://127.0.0.1/"] {
            assert!(before_request("google", url).is_err());
        }
        assert!(before_request("github", "https://api.github.com/").is_err());
        CREDENTIAL_EXPIRY.scope(1, async { assert!(before_request("google", "https://gmail.googleapis.com/").is_err()); }).await;
        bindings.lock().unwrap().revoke_connector("alice-google").unwrap();
        assert!(before_request("google", "https://gmail.googleapis.com/").is_err());
    }))).await;
    assert!(!active());
}
#[test]
fn isolated_native_connector_support_is_exact_and_has_no_other_provider_fallback() {
    let client = executor(Arc::new(AccountCredentialVault::default()));
    let supported: Vec<_> = duduclaw_mcp_boundary::tool_policy::POLICIES.iter().filter(|p| client.supports(p.name)).collect();
    assert_eq!(supported.len(), 28);
    for tool in ["google_status_extra", "gmail_search_extra", "github_status_extra", "notion_status_extra", "odoo_search", "db_query", "send_message", "web_search"] {
        assert!(!client.supports(tool));
    }
}
#[tokio::test]
async fn isolated_native_connector_vault_is_exact_and_expires_closed() {
    let vault = AccountCredentialVault::default();
    assert!(vault.acquire("missing", "google").await.is_err());
    vault.install(AccountCredential::new("alice-google".into(), "google".into(), "token".into(), 1).unwrap()).unwrap();
    let (_, selected, _) = fixture();
    assert!(credential(&selected, &vault).await.is_err());
    assert!(vault.acquire("alice-google", "github").await.is_err());
    assert!(vault.install(AccountCredential::new("alice-google".into(), "github".into(), "token".into(), u64::MAX).unwrap()).is_err());
}

#[test]
fn isolated_native_credential_rejects_json_escaping_and_non_bearer_alphabet() {
    for token in ["quote\"token", "slash\\token", "white space", "new\nline", "中文", "token:colon"] {
        assert!(AccountCredential::new("account".into(), "google".into(), token.into(), u64::MAX).is_err());
    }
    assert!(AccountCredential::new("account".into(), "google".into(), "aZ09-._~+/=".into(), u64::MAX).is_ok());
}

#[tokio::test]
async fn isolated_native_connector_disabled_or_invalid_integration_never_acquires_credential() {
    for config in ["[integrations]\ngoogle_workspace=false\n", "[integrations]\ngoogle_workspace='true'\n", "invalid ["] {
        let (_, selected, call) = fixture();
        let provider = Arc::new(SpyProvider::new("alice-google"));
        let client = executor(provider.clone());
        std::fs::write(client.home.path().join("config.toml"), config).unwrap();
        assert!(client.execute(selected, call).await.is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
    let (_, selected, call) = fixture();
    let provider = Arc::new(SpyProvider::new("alice-google"));
    let client = executor(provider.clone());
    std::fs::remove_file(client.home.path().join("config.toml")).unwrap();
    assert!(client.execute(selected, call).await.is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_native_connector_google_none_or_invalid_employee_never_acquires_credential() {
    for employee in ["[google]\nidentity='none'\n", "[google]\nidentity=12\n", "invalid ["] {
        let (_, selected, call) = fixture();
        let provider = Arc::new(SpyProvider::new("alice-google"));
        let client = executor(provider.clone());
        std::fs::write(client.home.path().join("agents/employee-a/agent.toml"), employee).unwrap();
        assert!(client.execute(selected, call).await.is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn isolated_native_connector_policy_change_fences_later_http_hop() {
    let (_, selected, _) = fixture();
    let client = executor(Arc::new(AccountCredentialVault::default()));
    CLIENT_POLICY.scope(client.policy.clone(), BOUND_CONNECTOR.scope(Arc::new(selected), CREDENTIAL_EXPIRY.scope(u64::MAX, async {
        assert!(before_request("google", "https://gmail.googleapis.com/").is_ok());
        std::fs::write(client.home.path().join("agents/employee-a/agent.toml"), "[google]\nidentity='none'\n").unwrap();
        assert!(before_request("google", "https://gmail.googleapis.com/").is_err());
    }))).await;
}

#[test]
fn isolated_native_google_recipients_are_single_strings_not_silently_ignored_arrays() {
    for key in ["to", "cc", "attendees"] {
        for value in [json!(["one@example.org"]), json!({"email":"one@example.org"}), Value::Null,
            json!("one@example.org,two@example.org"), json!("one@example.org;two@example.org"), json!("one@example.org\n")] {
            assert!(validate_google_recipients(&json!({(key):value})).is_err());
        }
        assert!(validate_google_recipients(&json!({(key):"one@example.org"})).is_ok());
    }
}

#[tokio::test]
async fn search_output_bounded_and_no_redirect() {
    use crate::isolation_connector_search::{format_search_items, search_http_client};
    use duduclaw_isolation::worker::ParsedItem;
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};
    let snippets: Vec<String> = (0..8).map(|index| format!("搜尋結果{index}：{}", "繁體中文🦀".repeat(100))).collect();
    let items: Vec<_> = snippets.iter().map(|snippet| ParsedItem::Html { input_slot:0,
        query:text("search"), value:text(&serde_json::to_string(snippet).unwrap()) }).collect();
    let rendered = format_search_items(&items).unwrap();
    // SearchHtml's documented cap is five results. Preserve complete Unicode
    // snippets while discarding results past the cap, including their canaries.
    assert_eq!(rendered, snippets.iter().take(5).enumerate()
        .map(|(index, snippet)| format!("{}. {snippet}", index + 1)).collect::<Vec<_>>().join("\n\n"));
    assert!(!rendered.contains("搜尋結果5"));
    assert!(!rendered.contains('\u{fffd}'));
    assert_eq!(format_search_items(&[]).unwrap(), "No results found.");
    let invalid = ParsedItem::Html { input_slot:0, query:text("other"), value:text("\"private\"") };
    assert!(format_search_items(&[invalid]).is_err());

    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", origin.local_addr().unwrap());
    let redirect = format!("http://localhost:{}/must-not-receive", destination.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        let (mut socket, _) = origin.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let mut buffer = [0;1024];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0 && request.len() < 16_384);
            request.extend_from_slice(&buffer[..count]);
        }
        socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    });
    // This is the production search client's redirect policy, not a separately
    // configured test client. No worker or external search endpoint is used.
    let response = search_http_client().unwrap().get(&url).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    assert_eq!(response.url().as_str(), url);
    assert!(!response.status().is_success());
    server.await.unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(100), destination.accept()).await.is_err());
}

#[tokio::test]
async fn odoo_disallowed_operation_refused_before_auth() {
    use crate::isolation_connector_odoo::OdooConnectorProvider;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[odoo]\nurl='http://{}'\ndb='demo'\nusername='shared'\napi_key_enc='unused-reference'\nfeatures_crm=true\n",
        listener.local_addr().unwrap())).unwrap();
    // Deliberately unusable secret: refusal must come from operation policy,
    // not credential decoding or an authentication handshake.
    std::fs::write(home.path().join("agents/employee-a/agent.toml"),
        "[odoo]\nusername='alice'\napi_key_enc='invalid-secret-reference'\nallowed_models=['crm.lead']\nallowed_actions=['read','search']\n").unwrap();
    let provider = OdooConnectorProvider::new(home.path().into(), "employee-a".into(), EmployeeId([2;16]),
        ProductConnectorAccount::Personal { account:"alice-odoo".into(), owner:"alice".into() }).unwrap();
    assert!(provider.check_operation(Some(("search", "crm.lead".into()))).is_ok());
    for (verb, model) in [("write", "crm.lead"), ("create", "crm.lead"), ("unlink", "crm.lead"),
        ("read", "res.partner"), ("search", "res.users")] {
        assert!(matches!(provider.check_operation(Some((verb, model.into()))), Err(VerificationError::BindingMismatch)),
            "{verb} {model}");
    }
    assert!(tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept()).await.is_err());
}
