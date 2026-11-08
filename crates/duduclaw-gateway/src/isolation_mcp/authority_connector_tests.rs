//! C2: connector-account authorization in the production MCP authority,
//! against the real users.db / apply.db / confirmation stores, the A2
//! lifecycle read model and the production `ConnectorCatalog` (C1) with its
//! C6 details table. Only the root allocator ReadBack and, where a test says
//! so, the product-home switch or the details reader are fixtures.
use super::*;
use std::sync::atomic::AtomicBool;
use crate::isolation_connector_boot::{DetailPin, ProvisionedDetail};
use crate::isolation_connector_details::{AccountDetailRecord, DetailReference};

const TEAM: &str = "team";

/// Product-home connection the test toggles.
struct ProductSwitch(AtomicBool);
impl ProductHome for ProductSwitch {
    fn connected(&self, _: &str) -> Result<bool, VerificationError> {
        Ok(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}
struct BrokenProduct;
impl ProductHome for BrokenProduct {
    fn connected(&self, _: &str) -> Result<bool, VerificationError> { Err(VerificationError::Unavailable) }
}

/// Details reader returning a fixed pin (or an unreadable store).
struct Pinned { pin: Option<DetailPin>, broken: bool }
impl ConnectorAccountDetails for Pinned {
    fn detail(&self, _: &Route) -> Result<Option<ProvisionedDetail>, VerificationError> { Err(VerificationError::Invalid) }
    fn current_pin(&self, _: &AccountId) -> Result<Option<DetailPin>, VerificationError> {
        if self.broken { return Err(VerificationError::Unavailable); }
        Ok(self.pin.clone())
    }
}

/// One home with a Personal employee (owner alice) and a Shared employee
/// (`team`, alice is a member) in the installation tenant.
struct Env { f: Fixture, root: Root, alice_personal: Receipt, team: Receipt, catalog: Arc<ConnectorCatalog> }
impl Env {
    fn alice(&self) -> EmployeeBinding { personal(&self.f.alice) }
    fn tenant(&self) -> EmployeeBinding { EmployeeBinding::Shared(text(&self.f.tenant)) }
    fn stores_with(&self, catalog: &Arc<ConnectorCatalog>, product: Arc<dyn ProductHome>) -> ConnectorStores {
        ConnectorStores { details: Arc::new(CatalogAccountDetails::new(self.f.home().to_owned(), catalog.clone()).unwrap()),
            product, catalog: catalog.clone() }
    }
    /// Real catalog + details reader, product home connected.
    fn connected(&self) -> ConnectorStores {
        self.stores_with(&self.catalog, Arc::new(ProductSwitch(AtomicBool::new(true))))
    }
    fn register(&self, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding) -> (AccountId, u64) {
        let account = self.catalog.register("fixture account", provider, kind, binding).unwrap();
        let generation = self.generation(&account);
        (account, generation)
    }
    fn generation(&self, account: &AccountId) -> u64 { self.catalog.route(account).unwrap().unwrap().generation }
    #[allow(clippy::too_many_arguments)]
    fn use_as(&self, stores: &ConnectorStores, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str, generation: u64, home: Option<&EmployeeBinding>) -> Result<(), VerificationError> {
        connector_with(&self.f.stores(&self.root), stores, agent, employee, binding, principal, account, generation, home)
    }
    /// The Personal employee, used by its owner alice.
    fn personal_use(&self, stores: &ConnectorStores, account: &AccountId, generation: u64, home: Option<&EmployeeBinding>)
        -> Result<(), VerificationError> {
        self.use_as(stores, LABEL, self.alice_personal.employee, &self.alice(), &self.f.human(&self.f.alice),
            account.as_str(), generation, home)
    }
    /// The Shared employee, used by its member alice.
    fn team_use(&self, stores: &ConnectorStores, account: &AccountId, generation: u64, home: Option<&EmployeeBinding>)
        -> Result<(), VerificationError> {
        self.use_as(stores, TEAM, self.team.employee, &self.tenant(), &self.f.human(&self.f.alice),
            account.as_str(), generation, home)
    }
}

fn env() -> Env {
    let f = fixture();
    let alice_personal = personal_employee(&f, 1, &f.alice);
    let item = applied(f.home(), 2, TEAM, &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let team = confirm(f.home(), &item, 2);
    f.users.bind_agent(&f.alice, TEAM, AccessLevel::Operator).unwrap();
    let root = Root(vec![alice_personal.clone(), team.clone()]);
    let catalog = Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap());
    Env { f, root, alice_personal, team, catalog }
}

fn database_record(employee: EmployeeId, agent: &str) -> AccountDetailRecord {
    AccountDetailRecord { employee, agent: agent.into(), reference: DetailReference::Database { source: "crm".into() } }
}

#[test]
fn c2_personal_route_of_owner_authorized() {
    let e = env();
    let stores = e.connected();
    let (google, generation) = e.register("google", ConnectorKind::Personal, &e.alice());
    assert_eq!(e.personal_use(&stores, &google, generation, None), Ok(()));
    // Repeated uses keep succeeding while nothing changes.
    assert_eq!(e.personal_use(&stores, &google, generation, None), Ok(()));
    // A Personal route is never an explicit home fallback.
    assert_eq!(e.personal_use(&stores, &google, generation, Some(&e.tenant())), Err(VerificationError::BindingMismatch));
    // (a) The identity gate applies first: an unreachable root is Unavailable.
    assert_eq!(connector_with(&e.f.stores(&Root(vec![])), &stores, LABEL, e.alice_personal.employee, &e.alice(),
        &e.f.human(&e.f.alice), google.as_str(), generation, None), Err(VerificationError::Unavailable));
    // A stale ACL generation of the owner is Revoked like for any tool.
    let stale = Principal::Human { user_id: text(&e.f.alice), generation: e.f.generation() + 1 };
    assert_eq!(e.use_as(&stores, LABEL, e.alice_personal.employee, &e.alice(), &stale, google.as_str(), generation, None),
        Err(VerificationError::Revoked));
    // Losing the owner's membership mid-run refuses the next use.
    e.f.users.unbind_agent(&e.f.alice, LABEL).unwrap();
    assert!(e.personal_use(&stores, &google, generation, None).is_err());
}

#[test]
fn c2_personal_route_of_other_owner_refused() {
    let e = env();
    let stores = e.connected();
    let (bob_google, bob_generation) = e.register("google", ConnectorKind::Personal, &personal(&e.f.bob));
    assert_eq!(e.personal_use(&stores, &bob_google, bob_generation, None), Err(VerificationError::BindingMismatch));
    // A tenant route is not the Personal employee's account, nor a home route.
    let (tenant_odoo, tenant_generation) = e.register("odoo", ConnectorKind::SharedTenant, &e.tenant());
    assert_eq!(e.personal_use(&stores, &tenant_odoo, tenant_generation, None), Err(VerificationError::BindingMismatch));
    assert_eq!(e.personal_use(&stores, &tenant_odoo, tenant_generation, Some(&e.tenant())),
        Err(VerificationError::BindingMismatch));
    // Bob (an Admin who may assign any agent) is not the owner: Revoked.
    let (alice_google, alice_generation) = e.register("google", ConnectorKind::Personal, &e.alice());
    assert_eq!(e.use_as(&stores, LABEL, e.alice_personal.employee, &e.alice(), &e.f.human(&e.f.bob),
        alice_google.as_str(), alice_generation, None), Err(VerificationError::Revoked));
    // Claiming bob's binding for alice's employee mismatches the lifecycle.
    assert_eq!(e.use_as(&stores, LABEL, e.alice_personal.employee, &personal(&e.f.bob), &e.f.human(&e.f.bob),
        bob_google.as_str(), bob_generation, None), Err(VerificationError::BindingMismatch));
    // A Shared employee never reaches a Personal credential, directly or as home.
    assert_eq!(e.team_use(&stores, &alice_google, alice_generation, None), Err(VerificationError::BindingMismatch));
    assert_eq!(e.team_use(&stores, &alice_google, alice_generation, Some(&e.tenant())),
        Err(VerificationError::BindingMismatch));
    // The owner's own route still works.
    assert_eq!(e.personal_use(&stores, &alice_google, alice_generation, None), Ok(()));
}

#[test]
fn c2_shared_tenant_route_wrong_tenant_refused() {
    let e = env();
    let stores = e.connected();
    let (own, own_generation) = e.register("odoo", ConnectorKind::SharedTenant, &e.tenant());
    assert_eq!(e.team_use(&stores, &own, own_generation, None), Ok(()));
    let other = EmployeeBinding::Shared(text(&uuid::Uuid::new_v4().to_string()));
    let (foreign, foreign_generation) = e.register("odoo", ConnectorKind::SharedTenant, &other);
    assert_eq!(e.team_use(&stores, &foreign, foreign_generation, None), Err(VerificationError::BindingMismatch));
    // Another tenant's home route is neither own nor this installation's home.
    let (foreign_home, home_generation) = e.register("notion", ConnectorKind::SharedHome, &other);
    assert_eq!(e.team_use(&stores, &foreign_home, home_generation, None), Err(VerificationError::BindingMismatch));
    assert_eq!(e.team_use(&stores, &foreign_home, home_generation, Some(&other)), Err(VerificationError::BindingMismatch));
    assert_eq!(e.personal_use(&stores, &foreign_home, home_generation, Some(&other)),
        Err(VerificationError::BindingMismatch));
    // The Shared employee cannot claim the other tenant itself (lifecycle binding).
    assert_eq!(e.use_as(&stores, TEAM, e.team.employee, &other, &e.f.human(&e.f.alice), foreign.as_str(),
        foreign_generation, None), Err(VerificationError::BindingMismatch));
    // Nor treat the Personal employee's binding as its own.
    assert_eq!(e.use_as(&stores, TEAM, e.team.employee, &e.alice(), &e.f.human(&e.f.alice), own.as_str(),
        own_generation, None), Err(VerificationError::BindingMismatch));
}

#[test]
fn c2_shared_home_route_requires_connected_home() {
    let e = env();
    // Production product-home check over the real config.toml.
    let stores = ConnectorStores::production(e.f.home(), e.catalog.clone()).unwrap();
    let config = |raw: &str| std::fs::write(e.f.home().join("config.toml"), raw).unwrap();
    let (notion, notion_generation) = e.register("notion", ConnectorKind::SharedHome, &e.tenant());
    let (github, github_generation) = e.register("github", ConnectorKind::SharedHome, &e.tenant());
    let (odoo, odoo_generation) = e.register("odoo", ConnectorKind::SharedHome, &e.tenant());
    config("");
    // Personal employee via explicit home fallback; Shared employee via its own tenant.
    assert_eq!(e.personal_use(&stores, &notion, notion_generation, Some(&e.tenant())), Ok(()));
    assert_eq!(e.team_use(&stores, &notion, notion_generation, None), Ok(()));
    assert_eq!(e.team_use(&stores, &notion, notion_generation, Some(&e.tenant())), Ok(()));
    assert_eq!(e.team_use(&stores, &odoo, odoo_generation, None), Ok(()));
    // A home route is never the Personal employee's own account.
    assert_eq!(e.personal_use(&stores, &notion, notion_generation, None), Err(VerificationError::BindingMismatch));
    // GitHub is connected only with its explicit enable flag.
    assert_eq!(e.team_use(&stores, &github, github_generation, None), Err(VerificationError::Revoked));
    assert_eq!(e.personal_use(&stores, &github, github_generation, Some(&e.tenant())), Err(VerificationError::Revoked));
    config("[integrations]\ngithub = true\n");
    assert_eq!(e.team_use(&stores, &github, github_generation, None), Ok(()));
    assert_eq!(e.personal_use(&stores, &github, github_generation, Some(&e.tenant())), Ok(()));
    // Disconnected in the product (Notion kill switch): refused for everyone.
    config("[integrations]\ngithub = true\nnotion = false\n");
    assert_eq!(e.personal_use(&stores, &notion, notion_generation, Some(&e.tenant())), Err(VerificationError::Revoked));
    assert_eq!(e.team_use(&stores, &notion, notion_generation, None), Err(VerificationError::Revoked));
    // Unreadable or malformed product state is Unavailable, never "connected".
    for raw in ["[integrations]\nnotion = 'yes'\n", "integrations = 'x'\n", "invalid ["] {
        config(raw);
        assert_eq!(e.team_use(&stores, &notion, notion_generation, None), Err(VerificationError::Unavailable), "{raw}");
    }
    std::fs::remove_file(e.f.home().join("config.toml")).unwrap();
    assert_eq!(e.team_use(&stores, &notion, notion_generation, None), Err(VerificationError::Unavailable));
    // The switch fixture: a home that disconnects between uses.
    let switch = Arc::new(ProductSwitch(AtomicBool::new(true)));
    let switched = e.stores_with(&e.catalog, switch.clone());
    assert_eq!(e.personal_use(&switched, &notion, notion_generation, Some(&e.tenant())), Ok(()));
    switch.0.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(e.personal_use(&switched, &notion, notion_generation, Some(&e.tenant())), Err(VerificationError::Revoked));
    // The product side is consulted only for home routes.
    let (alice_google, alice_generation) = e.register("google", ConnectorKind::Personal, &e.alice());
    assert_eq!(e.personal_use(&switched, &alice_google, alice_generation, None), Ok(()));
    assert_eq!(ConfiguredProductHome::new(e.f.home().to_owned()).unwrap().connected("unknown"), Ok(false));
}

#[test]
fn c2_revoked_route_refused() {
    let e = env();
    let stores = e.connected();
    let (account, generation) = e.register("google", ConnectorKind::Personal, &e.alice());
    assert_eq!(e.personal_use(&stores, &account, generation, None), Ok(()));
    e.catalog.revoke(&account).unwrap();
    let revoked = e.catalog.route(&account).unwrap().unwrap();
    assert!(!revoked.active);
    assert_eq!(e.personal_use(&stores, &account, generation, None), Err(VerificationError::Revoked));
    // Inactive is final even at its current (post-revoke) generation.
    assert_eq!(e.personal_use(&stores, &account, revoked.generation, None), Err(VerificationError::Revoked));
    // Same for a home route, through both paths.
    let (home, home_generation) = e.register("notion", ConnectorKind::SharedHome, &e.tenant());
    assert_eq!(e.team_use(&stores, &home, home_generation, None), Ok(()));
    e.catalog.revoke(&home).unwrap();
    assert_eq!(e.team_use(&stores, &home, home_generation, None), Err(VerificationError::Revoked));
    assert_eq!(e.personal_use(&stores, &home, home_generation, Some(&e.tenant())), Err(VerificationError::Revoked));
    // An id the catalog never issued is refused; a malformed one is Invalid.
    let unknown = AccountId::parse(&"0".repeat(32)).unwrap();
    assert_eq!(e.personal_use(&stores, &unknown, generation, None), Err(VerificationError::Revoked));
    assert_eq!(e.use_as(&stores, LABEL, e.alice_personal.employee, &e.alice(), &e.f.human(&e.f.alice),
        "alice-google", generation, None), Err(VerificationError::Invalid));
    // A same-name relink is a new id; the old selection stays refused.
    let (relinked, relinked_generation) = e.register("google", ConnectorKind::Personal, &e.alice());
    assert_ne!(relinked, account);
    assert_eq!(e.personal_use(&stores, &relinked, relinked_generation, None), Ok(()));
    assert_eq!(e.personal_use(&stores, &account, generation, None), Err(VerificationError::Revoked));
}

#[test]
fn c2_generation_moved_refused() {
    let e = env();
    let stores = e.connected();
    let (account, generation) = e.register("google", ConnectorKind::Personal, &e.alice());
    assert_eq!(e.personal_use(&stores, &account, generation, None), Ok(()));
    let next = e.catalog.advance(&account).unwrap();
    assert!(next > generation);
    assert_eq!(e.personal_use(&stores, &account, generation, None), Err(VerificationError::Revoked));
    // A selection made after the move pins the new generation.
    assert_eq!(e.personal_use(&stores, &account, next, None), Ok(()));
    assert_eq!(e.personal_use(&stores, &account, next + 1, None), Err(VerificationError::Revoked));
    // Home fallback selections are generation-bound too.
    let (home, home_generation) = e.register("notion", ConnectorKind::SharedHome, &e.tenant());
    assert_eq!(e.personal_use(&stores, &home, home_generation, Some(&e.tenant())), Ok(()));
    let moved = e.catalog.advance(&home).unwrap();
    assert_eq!(e.personal_use(&stores, &home, home_generation, Some(&e.tenant())), Err(VerificationError::Revoked));
    assert_eq!(e.team_use(&stores, &home, home_generation, None), Err(VerificationError::Revoked));
    assert_eq!(e.team_use(&stores, &home, moved, None), Ok(()));
}

#[test]
fn c2_detail_pin_changed_refused() {
    let e = env();
    let stores = e.connected();
    let employee = e.alice_personal.employee;
    let account = e.catalog.register_with_details("crm", "database", ConnectorKind::Personal, &e.alice(),
        &database_record(employee, LABEL)).unwrap();
    let generation = e.generation(&account);
    assert_eq!(e.personal_use(&stores, &account, generation, None), Ok(()));
    // Details validated for another employee (e.g. a recreated label) or another agent.
    for record in [database_record(EmployeeId([9; 16]), LABEL), database_record(employee, "other-agent")] {
        let foreign = e.catalog.register_with_details("crm", "database", ConnectorKind::Personal, &e.alice(), &record).unwrap();
        assert_eq!(e.personal_use(&stores, &foreign, e.generation(&foreign), None), Err(VerificationError::Revoked));
    }
    // A detail provider's route without details refuses.
    let (bare, bare_generation) = e.register("database", ConnectorKind::Personal, &e.alice());
    assert_eq!(e.personal_use(&stores, &bare, bare_generation, None), Err(VerificationError::Revoked));
    // A reader whose pin is for another generation, account or agent refuses.
    let route = e.catalog.route(&account).unwrap().unwrap();
    let reference = DetailReference::Database { source: "crm".into() };
    let mut other_generation = route.clone();
    other_generation.generation += 1;
    let mut other_account = route.clone();
    other_account.account = bare.clone();
    for pin in [DetailPin::new(&other_generation, employee, LABEL.into(), reference.clone()),
        DetailPin::new(&other_account, employee, LABEL.into(), reference.clone()),
        DetailPin::new(&route, employee, "other-agent".into(), reference.clone())] {
        let pinned = ConnectorStores { catalog: e.catalog.clone(), details: Arc::new(Pinned { pin: Some(pin), broken: false }),
            product: Arc::new(ProductSwitch(AtomicBool::new(true))) };
        assert_eq!(e.personal_use(&pinned, &account, generation, None), Err(VerificationError::Revoked));
    }
    let exact = ConnectorStores { catalog: e.catalog.clone(),
        details: Arc::new(Pinned { pin: Some(DetailPin::new(&route, employee, LABEL.into(), reference)), broken: false }),
        product: Arc::new(ProductSwitch(AtomicBool::new(true))) };
    assert_eq!(e.personal_use(&exact, &account, generation, None), Ok(()));
    // Advancing the route drops its details with the old generation: the old
    // selection and a new one without re-provisioned details both refuse.
    let next = e.catalog.advance(&account).unwrap();
    assert_eq!(e.personal_use(&stores, &account, generation, None), Err(VerificationError::Revoked));
    assert_eq!(e.personal_use(&stores, &account, next, None), Err(VerificationError::Revoked));
    // Providers without details never consult the reader.
    let (google, google_generation) = e.register("google", ConnectorKind::Personal, &e.alice());
    let empty = ConnectorStores { catalog: e.catalog.clone(), details: Arc::new(Pinned { pin: None, broken: true }),
        product: Arc::new(ProductSwitch(AtomicBool::new(true))) };
    assert_eq!(e.personal_use(&empty, &google, google_generation, None), Ok(()));
}

#[test]
fn c2_unreadable_catalog_unavailable() {
    let e = env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let catalog = Arc::new(ConnectorCatalog::new(Connection::open(&path).unwrap()).unwrap());
    let account = catalog.register("alice google", "google", ConnectorKind::Personal, &e.alice()).unwrap();
    let generation = catalog.route(&account).unwrap().unwrap().generation;
    let stores = e.stores_with(&catalog, Arc::new(ProductSwitch(AtomicBool::new(true))));
    assert_eq!(e.personal_use(&stores, &account, generation, None), Ok(()));
    // Damage the store through another connection: never Revoked/Ok.
    Connection::open(&path).unwrap().execute_batch("DROP TABLE isolation_connector_routes").unwrap();
    assert_eq!(e.personal_use(&stores, &account, generation, None), Err(VerificationError::Unavailable));
    assert_eq!(e.team_use(&stores, &account, generation, Some(&e.tenant())), Err(VerificationError::Unavailable));
    // An unreadable details store and product state are Unavailable as well.
    let database = e.catalog.register_with_details("crm", "database", ConnectorKind::Personal, &e.alice(),
        &database_record(e.alice_personal.employee, LABEL)).unwrap();
    let broken_details = ConnectorStores { catalog: e.catalog.clone(), details: Arc::new(Pinned { pin: None, broken: true }),
        product: Arc::new(ProductSwitch(AtomicBool::new(true))) };
    assert_eq!(e.personal_use(&broken_details, &database, e.generation(&database), None), Err(VerificationError::Unavailable));
    let (home, home_generation) = e.register("notion", ConnectorKind::SharedHome, &e.tenant());
    let broken_product = e.stores_with(&e.catalog, Arc::new(BrokenProduct));
    assert_eq!(e.team_use(&broken_product, &home, home_generation, None), Err(VerificationError::Unavailable));
    // At the trait boundary only Unavailable stays distinguishable.
    assert_eq!(binding_refusal(VerificationError::Unavailable), BindingError::Unavailable);
    assert_eq!(binding_refusal(VerificationError::Invalid), BindingError::Invalid);
    for error in [VerificationError::Revoked, VerificationError::BindingMismatch] {
        assert_eq!(binding_refusal(error), BindingError::Denied);
    }
}
