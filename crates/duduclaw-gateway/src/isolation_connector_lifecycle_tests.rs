use super::*;
use std::sync::Mutex;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane, Principal,
    ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{bindings::{BindingAuthority, BindingStore}, RequestContext};
use rusqlite::Connection;
use crate::isolation_connector_clients::{AccountCredential, AccountCredentialProvider};
use crate::isolation_connectors::{LiveConnectorSelectionPolicy, ProductConnectorAccount, SelectedConnector};

struct Live;
impl duduclaw_isolation::capability::LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
struct Policy(Vec<ProductConnectorAccount>);
impl LiveConnectorSelectionPolicy for Policy {
    fn supports(&self, tool: &str) -> bool { tool == "gmail_search" }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Ok(self.0.clone())
    }
}
fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn acquired(vault: &AccountCredentialVault, account: &AccountId) -> bool {
    tokio::runtime::Builder::new_current_thread().build().unwrap()
        .block_on(vault.acquire(account.as_str(), "google")).is_ok()
}
fn alice() -> EmployeeBinding { EmployeeBinding::Personal(text("alice")) }
fn actor() -> AdminActor { AdminActor::verified("0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10").unwrap() }

/// A real selection pinned through the boot catalog instance; the binding
/// double keeps authorizing, so only the catalog route can fence it.
fn selection(home: &Path, account: &AccountId) -> Arc<SelectedConnector> {
    let catalog = Arc::new(ConnectorCatalog::open(home).unwrap());
    let principal = Principal::Human { user_id: text("alice"), generation: 1 };
    let mut store = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    store.register_principal(&principal).unwrap();
    store.create_employee(EmployeeId([2; 16]), &alice()).unwrap();
    store.register_connector(account.as_str(), &alice()).unwrap();
    let bindings: Arc<dyn BindingAuthority> = Arc::new(Mutex::new(store));
    let context = RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal, employee_binding: alice(), lane: Lane::Standard,
        tools: List::new(vec![text("gmail_search")]).unwrap(),
        resources: List::new(vec![ResourceGrant::Connector { account: text(account.as_str()), binding: alice() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Live));
    let policy = Arc::new(Policy(vec![ProductConnectorAccount::Personal {
        account: account.to_string(), owner: "alice".into() }]));
    let selected = SelectedConnector::select_with_policy(&context, "gmail_search", &serde_json::json!({}),
        bindings, catalog, Some(policy)).unwrap();
    selected.recheck().unwrap();
    Arc::new(selected)
}

/// Records what a pinned selection observed at the moment material removal
/// started, then drops the real vault entry.
struct Probe {
    selected: Arc<SelectedConnector>,
    vault: AccountCredentialVault,
    observed: Mutex<Vec<Result<(), VerificationError>>>,
}
impl CredentialMaterial for Probe {
    fn drop_material(&self, account: &AccountId) -> Result<(), VerificationError> {
        self.observed.lock().unwrap().push(self.selected.recheck());
        // Material is still present here: only the route fence is refusing.
        assert!(acquired(&self.vault, account));
        self.vault.drop_material(account)
    }
}

#[cfg(unix)]
#[test]
fn revoke_fences_selected_connector_before_effect() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let account = provision(home.path(), "google", ConnectorKind::Personal, &alice(), "alice work", None, &actor()).unwrap();
    let selected = selection(home.path(), &account);
    let vault = AccountCredentialVault::default();
    vault.install(AccountCredential::new(account.to_string(), "google".into(), "fixture-token".into(), u64::MAX).unwrap())
        .unwrap();
    let probe = Probe { selected: selected.clone(), vault, observed: Mutex::new(Vec::new()) };

    let route = revoke_with(home.path(), &account, &actor(), Some(&probe)).unwrap();
    assert!(!route.active);
    // While removal was still pending, the selection already refused.
    assert_eq!(*probe.observed.lock().unwrap(), vec![Err(VerificationError::BindingMismatch)]);
    // Every later effect checkpoint refuses; nothing would be sent.
    let mut effects = 0;
    for _ in 0..3 { if selected.recheck().is_ok() { effects += 1; } }
    assert_eq!(effects, 0);
    assert!(!acquired(&probe.vault, &account));
    // Revocation is final and idempotent; it never revives the route.
    revoke_with(home.path(), &account, &actor(), None).unwrap();
    assert!(!ConnectorCatalog::open(home.path()).unwrap().route(&account).unwrap().unwrap().active);
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("isolation.connector_revoked"));
    assert!(!audit.contains("fixture-token") && !audit.contains("alice work"));
}

#[cfg(unix)]
#[test]
fn revoke_keeps_fence_when_material_removal_fails() {
    struct Broken;
    impl CredentialMaterial for Broken {
        fn drop_material(&self, _: &AccountId) -> Result<(), VerificationError> { Err(VerificationError::Unavailable) }
    }
    let home = crate::fs_safe::private_test_home().unwrap();
    let account = provision(home.path(), "google", ConnectorKind::Personal, &alice(), "alice", None, &actor()).unwrap();
    let selected = selection(home.path(), &account);
    assert_eq!(revoke_with(home.path(), &account, &actor(), Some(&Broken)), Err(LifecycleError::MaterialPending));
    assert!(selected.recheck().is_err());
    let unknown = AccountId::parse(&"f".repeat(32)).unwrap();
    assert_eq!(revoke_with(home.path(), &unknown, &actor(), None).map(|_| ()),
        Err(LifecycleError::Catalog(CatalogError::NotFound)));
}

#[cfg(unix)]
#[test]
fn provision_never_scans_credentials() {
    let home = crate::fs_safe::private_test_home().unwrap();
    // Legacy and product credential artifacts that a scanner could turn into
    // routes. Provision must neither read them nor derive routes from them.
    let artifacts = [
        ("config.toml", "[integrations]\ngoogle_workspace = true\ngithub = true\nnotion_token = \"legacy-notion\"\n"),
        ("google_oauth_tokens.json", "{\"refresh_token\":\"legacy-google\",\"email\":\"owner@example.com\"}"),
        ("mcp_oauth_tokens.json", "{\"github\":{\"access_token\":\"legacy-github\"}}"),
    ];
    for (name, body) in artifacts { std::fs::write(home.path().join(name), body).unwrap(); }
    std::fs::create_dir(home.path().join("secrets")).unwrap();
    std::fs::write(home.path().join("secrets/odoo.json"), "{\"password\":\"legacy-odoo\"}").unwrap();

    let company = EmployeeBinding::Shared(text("company"));
    let first = provision(home.path(), "github", ConnectorKind::SharedTenant, &company, "org bot", None, &actor()).unwrap();
    // A refused provision creates nothing and still scans nothing.
    assert!(provision(home.path(), "google", ConnectorKind::SharedHome, &alice(), "x", None, &actor()).is_err());
    assert!(provision(home.path(), "dropbox", ConnectorKind::Personal, &alice(), "x", None, &actor()).is_err());
    // C6: a detail provider without details is refused before the catalog.
    assert_eq!(provision(home.path(), "database", ConnectorKind::SharedTenant, &company, "x", None, &actor()),
        Err(LifecycleError::Detail(DetailError::Required)));

    let catalog = ConnectorCatalog::open(home.path()).unwrap();
    let routes = catalog.list(None, 200).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!((routes[0].account.clone(), routes[0].provider, routes[0].kind), (first.clone(), "github", ConnectorKind::SharedTenant));
    assert!(catalog.active_routes_for(&alice()).unwrap().is_empty());
    for (name, body) in artifacts { assert_eq!(std::fs::read_to_string(home.path().join(name)).unwrap(), body); }

    // The same label again is a new account, never the old id.
    let second = provision(home.path(), "github", ConnectorKind::SharedTenant, &company, "org bot", None, &actor()).unwrap();
    assert_ne!(first, second);
    assert_eq!(catalog.list(None, 200).unwrap().len(), 2);
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap_or_default();
    for secret in ["legacy-", "owner@example.com", "org bot", "company"] { assert!(!audit.contains(secret), "{secret}"); }
}

#[cfg(unix)]
#[test]
fn e4_google_shared_tenant_provision_requires_tenant_source() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let company = EmployeeBinding::Shared(text("company"));
    // Home OAuth only: a tenant route would be served from the home credential.
    std::fs::write(home.path().join("config.toml"), "[integrations]\ngoogle_workspace = true\n").unwrap();
    assert_eq!(provision(home.path(), "google", ConnectorKind::SharedTenant, &company, "tenant mail", None, &actor()),
        Err(LifecycleError::Detail(DetailError::Unknown)));
    // A configured service account whose key cannot be read refuses as unavailable.
    std::fs::write(home.path().join("config.toml"), "[integrations]\ngoogle_workspace = true\n\
        [integrations.google_service_account]\nkey_file = \"sa.json\"\nsubject = \"support@customer.com\"\n").unwrap();
    assert_eq!(provision(home.path(), "google", ConnectorKind::SharedTenant, &company, "tenant mail", None, &actor()),
        Err(LifecycleError::Detail(DetailError::Unavailable)));
    assert!(ConnectorCatalog::open(home.path()).unwrap().list(None, 200).unwrap().is_empty());
    std::fs::write(home.path().join("sa.json"), "{\"client_email\":\"sa@p\",\"private_key\":\"k\"}").unwrap();
    let account = provision(home.path(), "google", ConnectorKind::SharedTenant, &company, "tenant mail", None, &actor()).unwrap();
    let route = ConnectorCatalog::open(home.path()).unwrap().route(&account).unwrap().unwrap();
    assert_eq!((route.provider, route.kind, route.binding), ("google", ConnectorKind::SharedTenant, company));
    // Other providers' tenant routes are unaffected by the Google gate.
    std::fs::remove_file(home.path().join("config.toml")).unwrap();
    assert!(provision(home.path(), "github", ConnectorKind::SharedTenant,
        &EmployeeBinding::Shared(text("company")), "org bot", None, &actor()).is_ok());
}

#[test]
fn admin_actor_refuses_system_and_malformed_ids() {
    for id in ["", "system", "a\nb"] { assert!(AdminActor::verified(id).is_err()); }
    assert!(AdminActor::verified(&"a".repeat(129)).is_err());
    assert_eq!(actor().user_id(), "0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10");
}
