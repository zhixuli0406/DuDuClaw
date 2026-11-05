use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
use serde_json::json;
fn text<const N: usize>(s: &str) -> Text<N> { Text::new(s.into()).unwrap() }
fn context(binding: EmployeeBinding, principal: Principal, accounts: &[&str]) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal, employee_binding: binding.clone(), lane: Lane::Standard,
        tools: List::new(vec![text("gmail_search")]).unwrap(),
        resources: List::new(accounts.iter().map(|account| ResourceGrant::Connector {
            account: text(account), binding: binding.clone() }).collect()).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    })
}
fn fixture() -> (Arc<Mutex<BindingStore>>, Arc<ConnectorCatalog>, RequestContext) {
    let binding = EmployeeBinding::Personal(text("alice"));
    let principal = Principal::Human { user_id: text("alice"), generation: 1 };
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(EmployeeId([2; 16]), &binding).unwrap();
    for account in ["alice-google", "alice-google-2", "alice-github"] { bindings.register_connector(account, &binding).unwrap(); }
    let catalog = Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap());
    catalog.register("alice-google", "google").unwrap();
    catalog.register("alice-google-2", "google").unwrap();
    catalog.register("alice-github", "github").unwrap();
    (Arc::new(Mutex::new(bindings)), catalog, context(binding, principal, &["alice-google"]))
}
#[test]
fn isolated_connector_rechecks_revocation_and_principal_generation() {
    let (bindings, catalog, ctx) = fixture();
    let selected = SelectedConnector::select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog.clone()).unwrap();
    assert_eq!(selected.account(), "alice-google");
    bindings.lock().unwrap().revoke_connector("alice-google").unwrap();
    assert!(selected.recheck().is_err());
    let (bindings, catalog, ctx) = fixture();
    let selected = SelectedConnector::select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog).unwrap();
    bindings.lock().unwrap().advance_principal(&Principal::Human { user_id: text("alice"), generation: 2 }).unwrap();
    assert!(selected.recheck().is_err());
}
#[test]
fn isolated_connector_refuses_ambiguity_wrong_provider_and_credential_override() {
    let (bindings, catalog, ctx) = fixture();
    for (tool, args) in [("github_status", json!({})), ("gmail_search", json!({"connector_account":"home"})),
        ("gmail_search", json!({"api_key":"not-authority"}))] {
        assert!(SelectedConnector::select(&ctx, tool, &args, bindings.clone(), catalog.clone()).is_err());
    }
    let ctx = context(ctx.record().employee_binding.clone(), ctx.record().principal.clone(), &["alice-google", "alice-google-2"]);
    assert!(SelectedConnector::select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog.clone()).is_err());
    assert!(SelectedConnector::select(&ctx, "gmail_search", &json!({"connector_account":"alice-google-2"}), bindings, catalog).is_ok());
}
#[test]
fn isolated_connector_shared_employee_cannot_select_personal_account() {
    let (bindings, catalog, _) = fixture();
    let principal = Principal::Background { tenant_id: text("company"), generation: 1 };
    let binding = EmployeeBinding::Shared(text("company"));
    let ctx = context(binding.clone(), principal.clone(), &["alice-google"]);
    // A forged Shared principal cannot replace the stored Personal employee.
    assert!(SelectedConnector::select(&ctx, "gmail_search", &json!({}), bindings, catalog.clone()).is_err());
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(EmployeeId([2; 16]), &binding).unwrap();
    bindings.register_connector("alice-google", &EmployeeBinding::Personal(text("alice"))).unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    assert!(SelectedConnector::select(&ctx, "gmail_search", &json!({}), bindings, catalog).is_err());
}
#[test]
fn isolated_connector_route_is_immutable_and_live() {
    let (bindings, catalog, ctx) = fixture();
    let selected = SelectedConnector::select(&ctx, "gmail_search", &json!({}), bindings, catalog.clone()).unwrap();
    assert!(catalog.register("alice-google", "github").is_err());
    catalog.revoke("alice-google").unwrap();
    assert!(selected.recheck().is_err());
    assert!(catalog.register("alice-google", "google").is_err());
}
