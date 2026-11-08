use super::*;
use std::sync::{Arc, Mutex};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    Lane, LiveRequestVerifier, Principal, ResourceGrant}, ids::{RequestId, RunId, Sha256Digest},
    migration::{ProposalBinding, ProposalOperationId}};
use duduclaw_mcp_boundary::{bindings::BindingStore, RequestContext};
use rusqlite::Connection;
use crate::isolation_connectors::{LiveConnectorSelectionPolicy, SelectedConnector};

fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(Text::new(owner.into()).unwrap()) }
fn shared(tenant: &str) -> EmployeeBinding { EmployeeBinding::Shared(Text::new(tenant.into()).unwrap()) }
pub(crate) fn catalog() -> Arc<ConnectorCatalog> {
    Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap())
}
pub(crate) fn lifecycle(binding: EmployeeBinding) -> EmployeeLifecycle {
    EmployeeLifecycle { employee: EmployeeId([1; 16]), label: "bot".into(), uid: 70_001, binding,
        allocation: ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([2; 16]), revision: 1 },
            proposal_digest: Sha256Digest([3; 32]) } }
}
pub(crate) fn routes_for(catalog: &ConnectorCatalog, lifecycle: &EmployeeLifecycle)
    -> Result<GoogleAccountRoutes, GoogleRoutesError> {
    GoogleAccountRoutes::read_with(&lifecycle.label, lifecycle.employee, &lifecycle.binding,
        catalog, || Ok(lifecycle.clone()))
}

#[test]
fn google_routes_personal_uses_owner_account_only() {
    let catalog = catalog();
    let alice = catalog.register("same-label", "google", ConnectorKind::Personal, &personal("alice")).unwrap();
    catalog.register("same-label", "google", ConnectorKind::Personal, &personal("bob")).unwrap();
    catalog.register("home", "google", ConnectorKind::SharedHome, &shared("company")).unwrap();
    catalog.register("other-provider", "github", ConnectorKind::Personal, &personal("alice")).unwrap();
    let routes = routes_for(&catalog, &lifecycle(personal("alice"))).unwrap();
    assert_eq!(routes.personal.keys().map(String::as_str).collect::<Vec<_>>(), vec!["alice"]);
    assert_eq!(routes.personal["alice"].product.account(), alice.as_str());
    assert_eq!(routes.personal["alice"].generation(), 1);
    assert!(routes.home.is_none());
}

#[test]
fn google_routes_personal_refuses_other_users_account() {
    let catalog = catalog();
    catalog.register("bob", "google", ConnectorKind::Personal, &personal("bob")).unwrap();
    catalog.register("home", "google", ConnectorKind::SharedHome, &shared("company")).unwrap();
    let alice = lifecycle(personal("alice"));
    assert_eq!(routes_for(&catalog, &alice).err(), Some(GoogleRoutesError::SetupRequired));
    // A capability claiming Bob cannot replace Alice's root lifecycle owner.
    assert_eq!(GoogleAccountRoutes::read_with("bot", alice.employee, &personal("bob"),
        &catalog, || Ok(alice.clone())).err(), Some(GoogleRoutesError::BindingMismatch));
}

#[test]
fn google_routes_shared_uses_home_routes_only() {
    let catalog = catalog();
    let home = catalog.register("home", "google", ConnectorKind::SharedHome, &shared("company")).unwrap();
    // A tenant route alongside the home route is ignored, not ambiguous.
    catalog.register("tenant", "google", ConnectorKind::SharedTenant, &shared("company")).unwrap();
    catalog.register("other-home", "google", ConnectorKind::SharedHome, &shared("other")).unwrap();
    catalog.register("personal", "google", ConnectorKind::Personal, &personal("alice")).unwrap();
    let routes = routes_for(&catalog, &lifecycle(shared("company"))).unwrap();
    assert!(routes.personal.is_empty());
    let route = routes.home.unwrap();
    assert_eq!(route.product.account(), home.as_str());
    assert!(matches!(&route.product, ProductConnectorAccount::SharedHome { tenant, .. } if tenant == "company"));
    assert_eq!(route.kind(), ConnectorKind::SharedHome);
}

#[test]
fn google_routes_shared_tenant_route_not_admitted_until_tenant_sources_exist() {
    // Until E4 adds tenant-scoped credential sources, a SharedTenant route must
    // never be served from the shared home credential.
    let catalog = catalog();
    catalog.register("tenant", "google", ConnectorKind::SharedTenant, &shared("company")).unwrap();
    assert_eq!(routes_for(&catalog, &lifecycle(shared("company"))).err(), Some(GoogleRoutesError::SetupRequired));
}

struct Live;
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
// Keep the product candidate unchanged so C1 must detect the route generation
// independently, including while using the sealed owner's binding I/O context.
struct Product(ProductConnectorAccount);
impl LiveConnectorSelectionPolicy for Product {
    fn supports(&self, tool: &str) -> bool { tool == "gmail_read" }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> { Ok(vec![self.0.clone()]) }
}

#[test]
fn google_routes_revoked_route_refused_by_generation() {
    for revoke in [false, true] {
        let catalog = catalog();
        let lifecycle = lifecycle(personal("alice"));
        let id = catalog.register("alice", "google", ConnectorKind::Personal, &lifecycle.binding).unwrap();
        let route = routes_for(&catalog, &lifecycle).unwrap().personal.remove("alice").unwrap();
        let principal = Principal::Human { user_id: Text::new("alice".into()).unwrap(), generation: 1 };
        let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
        bindings.register_principal(&principal).unwrap();
        bindings.create_employee(lifecycle.employee, &lifecycle.binding).unwrap();
        bindings.register_connector(id.as_str(), &lifecycle.binding).unwrap();
        let context = RequestContext::with_live_verifier(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: lifecycle.employee, run: RunId([3; 16]),
            task: None, principal, employee_binding: lifecycle.binding.clone(), lane: Lane::Standard,
            tools: List::new(vec![Text::new("gmail_read".into()).unwrap()]).unwrap(),
            resources: List::new(vec![ResourceGrant::Connector { account: Text::new(id.as_str().into()).unwrap(),
                binding: lifecycle.binding.clone() }]).unwrap(),
            policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
            nonce: [5; 32], jti: [6; 16],
        }, Arc::new(Live));
        let selected = Arc::new(SelectedConnector::select_with_policy(&context, "gmail_read", &serde_json::json!({}),
            Arc::new(Mutex::new(bindings)), catalog.clone(), Some(Arc::new(Product(route.product.clone())))).unwrap());
        assert_eq!(selected.generation(), route.generation());
        assert_eq!(selected.recheck(), Ok(()));
        assert_eq!(route.recheck(&catalog), Ok(()));
        if revoke { catalog.revoke(&id).unwrap(); } else { catalog.advance(&id).unwrap(); }
        assert_eq!(route.recheck(&catalog), Err(VerificationError::BindingMismatch));
        assert_eq!(selected.recheck(), Err(VerificationError::BindingMismatch));
        assert_eq!(selected.binding_io_context().verify_live(1), Err(VerificationError::BindingMismatch));
    }
}

#[test]
fn google_routes_unreadable_catalog_unavailable() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("catalog.db");
    let catalog = ConnectorCatalog::new(Connection::open(&path).unwrap()).unwrap();
    // Corrupt a real SQLite store after opening, rather than substituting an
    // empty route result for a failed catalog query.
    Connection::open(path).unwrap().execute_batch("DROP TABLE isolation_connector_routes;").unwrap();
    assert_eq!(routes_for(&catalog, &lifecycle(personal("alice"))).err(), Some(GoogleRoutesError::Unavailable));
}

#[test]
fn google_routes_no_route_setup_required() {
    let catalog = catalog();
    let lifecycle = lifecycle(personal("alice"));
    assert_eq!(routes_for(&catalog, &lifecycle).err(), Some(GoogleRoutesError::SetupRequired));
    let id = catalog.register("alice", "google", ConnectorKind::Personal, &lifecycle.binding).unwrap();
    catalog.revoke(&id).unwrap();
    catalog.register("alice", "github", ConnectorKind::Personal, &lifecycle.binding).unwrap();
    let error = routes_for(&catalog, &lifecycle).err().unwrap();
    assert_eq!(error.code(), "connector_setup_required");
    assert_eq!(VerificationError::from(error), VerificationError::Unavailable);
    assert_eq!(error.to_string(), "connector_setup_required: google");
}

#[test]
fn google_routes_unreadable_or_changed_lifecycle_refused() {
    let home = tempfile::tempdir().unwrap();
    let catalog = catalog();
    let lifecycle = lifecycle(personal("alice"));
    catalog.register("alice", "google", ConnectorKind::Personal, &lifecycle.binding).unwrap();
    assert_eq!(GoogleAccountRoutes::read(home.path(), "bot", lifecycle.employee, &lifecycle.binding,
        &catalog).err(), Some(GoogleRoutesError::Unavailable));
    let reads = std::cell::Cell::new(0);
    assert_eq!(GoogleAccountRoutes::read_with("bot", lifecycle.employee, &lifecycle.binding, &catalog, || {
        reads.set(reads.get() + 1);
        let mut current = lifecycle.clone();
        if reads.get() > 1 { current.binding = personal("bob"); }
        Ok(current)
    }).err(), Some(GoogleRoutesError::BindingMismatch));
}

#[test]
fn google_routes_ambiguous_accounts_require_setup() {
    let catalog = catalog();
    for binding in [personal("alice"), shared("company")] {
        let kind = if matches!(&binding, EmployeeBinding::Personal(_)) {
            ConnectorKind::Personal
        } else { ConnectorKind::SharedHome };
        for _ in 0..2 { catalog.register("same-label", "google", kind, &binding).unwrap(); }
        assert_eq!(routes_for(&catalog, &lifecycle(binding)).err(), Some(GoogleRoutesError::SetupRequired));
    }
}
