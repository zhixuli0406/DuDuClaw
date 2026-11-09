//! v1.81 C4: product credential events keep the connector catalog in step.
use super::*;
use std::sync::{Arc, Mutex};
use duduclaw_isolation::{bounded::List, capability::{CapabilityId, CapabilityRecord, Lane, Principal, ResourceGrant},
    ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, bindings::{BindingAuthority, BindingStore}};
use duduclaw_security::credential_protection::ProtectedKey;
use rusqlite::Connection;
use crate::google_personal::lifecycle::{disconnect_material, publish_connection};
use crate::isolation_connectors::{AccountId, LiveConnectorSelectionPolicy, ProductConnectorAccount, SelectedConnector};
use crate::person_credentials::{self as vault, Metadata, Secret, Status};

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(text(owner)) }
fn company() -> EmployeeBinding { EmployeeBinding::Shared(text("company")) }

struct Fixture { home: tempfile::TempDir, key: ProtectedKey }
fn fixture() -> Fixture {
    let home = crate::fs_safe::private_test_home().unwrap();
    duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
    crate::handlers::credential_protection_rpc::protection_for(home.path()).enable(false).unwrap();
    let key = crate::google_identity::protection_gate(home.path()).unwrap();
    Fixture { home, key }
}
impl Fixture {
    fn path(&self) -> &Path { self.home.path() }
    fn catalog(&self) -> ConnectorCatalog { ConnectorCatalog::open(self.path()).unwrap() }
    fn register(&self, label: &str, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding) -> AccountId {
        self.catalog().register(label, provider, kind, binding).unwrap()
    }
    fn route(&self, account: &AccountId) -> Route { self.catalog().route(account).unwrap().unwrap() }
    fn active(&self, target: &Target) -> Vec<Route> { self.catalog().active_in_scope(target.scope()).unwrap() }
    fn pending(&self, target: &Target) -> usize { self.catalog().pending_relinks(target.scope()).unwrap() }
    /// The real Personal Google publication path (`flow::finish` minus OAuth).
    fn connect(&self, user: &str, sub: &str) -> Metadata {
        let metadata = metadata(user, sub);
        let epoch = vault::begin_connection(self.path(), user).unwrap();
        publish_connection(self.path(), user, &metadata, || {
            vault::connect_with_previous(self.path(), &self.key, metadata.clone(), &secret(), epoch, None)
        }).unwrap();
        metadata
    }
    fn connected_sub(&self, user: &str) -> Option<String> {
        vault::get(self.path(), user).unwrap().map(|record| record.metadata.google_sub)
    }
}
fn metadata(user: &str, sub: &str) -> Metadata {
    Metadata {
        owner_kind: "user".into(), owner_id: user.into(), provider: "google".into(),
        credential_id: uuid::Uuid::new_v4().to_string(), revision: 1,
        account_email: format!("{user}@c.test"), google_sub: sub.into(), hosted_domain: Some("c.test".into()),
        local_purposes: vec!["base".into()], granted_scopes: vec!["openid".into(), "email".into()],
        status: Status::Connected, status_before_suspension: None, connected_at: chrono::Utc::now().to_rfc3339(),
        last_refresh_at: None, oauth_client_id: "client-1".into(),
        google_data_consent: Some(crate::google_data_consent::Evidence::issue(user).unwrap()),
    }
}
fn secret() -> Secret {
    Secret { access_token: "fixture-access-secret".into(), refresh_token: Some("fixture-refresh-secret".into()),
        expires_at: Some(4_000_000_000) }
}
fn google(owner: &str) -> Target { Target::personal(owner, "google").unwrap() }
fn max_generation(f: &Fixture) -> u64 {
    f.catalog().list(None, 200).unwrap().iter().map(|route| route.generation).max().unwrap_or(0)
}

/// A real C1 selection pinned through the catalog; the binding double keeps
/// authorizing, so only the catalog route can fence it.
fn selection(home: &Path, account: &AccountId, owner: &str) -> Arc<SelectedConnector> {
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
    let catalog = Arc::new(ConnectorCatalog::open(home).unwrap());
    let principal = Principal::Human { user_id: text(owner), generation: 1 };
    let mut store = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    store.register_principal(&principal).unwrap();
    store.create_employee(EmployeeId([2; 16]), &personal(owner)).unwrap();
    store.register_connector(account.as_str(), &personal(owner)).unwrap();
    let bindings: Arc<dyn BindingAuthority> = Arc::new(Mutex::new(store));
    let context = RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal, employee_binding: personal(owner), lane: Lane::Standard,
        tools: List::new(vec![text("gmail_search")]).unwrap(),
        resources: List::new(vec![ResourceGrant::Connector { account: text(account.as_str()), binding: personal(owner) }]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Live));
    let policy = Arc::new(Policy(vec![ProductConnectorAccount::Personal { account: account.to_string(), owner: owner.into() }]));
    let selected = SelectedConnector::select_with_policy(&context, "gmail_search", &serde_json::json!({}),
        bindings, catalog, Some(policy)).unwrap();
    selected.recheck().unwrap();
    Arc::new(selected)
}

#[cfg(unix)]
#[test]
fn c4_personal_google_disconnect_revokes_route() {
    let f = fixture();
    let alice_google = f.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice"));
    let alice_db = f.register("alice db", "database", ConnectorKind::Personal, &personal("alice"));
    let bob_google = f.register("bob gmail", "google", ConnectorKind::Personal, &personal("bob"));
    let home_google = f.register("home google", "google", ConnectorKind::SharedHome, &company());
    f.connect("alice", "sub-alice");
    let selected = selection(f.path(), &alice_google, "alice");
    let before = f.route(&alice_google).generation;

    // The owner's own disconnect.
    let removed = disconnect_material(f.path(), "alice", "alice", false).unwrap();
    assert!(removed.is_some());
    assert_eq!(f.connected_sub("alice"), None);
    let after = f.route(&alice_google);
    assert!(!after.active && after.generation > before);
    assert_eq!(selected.recheck(), Err(VerificationError::BindingMismatch));
    // Only the owner's Personal Google routes: other providers, people and home stay.
    for other in [&alice_db, &bob_google, &home_google] { assert!(f.route(other).active); }
    assert!(f.active(&google("alice")).is_empty());
    assert_eq!(f.pending(&google("alice")), 1, "the owner's own disconnect is relinkable");

    // An admin-initiated disconnect is not relinkable by the owner.
    f.connect("alice", "sub-alice");
    assert_eq!(f.active(&google("alice")).len(), 1);
    disconnect_material(f.path(), "alice", "admin-1", false).unwrap();
    assert!(f.active(&google("alice")).is_empty());
    assert_eq!(f.pending(&google("alice")), 0);
    f.connect("alice", "sub-alice");
    assert!(f.active(&google("alice")).is_empty(), "needs a new admin provision");

    // Consent revoked at Google: fenced before the credential is marked.
    let alice2 = f.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice"));
    let record = vault::get(f.path(), "alice").unwrap().unwrap();
    crate::google_personal::lifecycle::consent_revoked(f.path(), &record.metadata);
    assert!(!f.route(&alice2).active);
    assert_eq!(f.pending(&google("alice")), 1);
    let audit = std::fs::read_to_string(f.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("isolation.connector_event_revoked"));
    for leak in ["sub-alice", "alice@c.test", "alice gmail", "company", "fixture-"] {
        assert!(!audit.contains(leak), "{leak}");
    }
}

#[cfg(unix)]
#[test]
fn c4_personal_google_reconnect_new_generation() {
    let f = fixture();
    let first = f.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice"));
    f.connect("alice", "sub-a");
    let pinned = selection(f.path(), &first, "alice");
    let g1 = f.route(&first).generation;

    // Re-consent of the SAME account while connected: same id, new generation.
    f.connect("alice", "sub-a");
    let route = f.route(&first);
    assert!(route.active && route.generation > g1);
    assert!(pinned.recheck().is_err(), "a selection pinned to the old generation refuses");

    // Disconnect, then reconnect the same account: a NEW id, never the old one.
    disconnect_material(f.path(), "alice", "alice", false).unwrap();
    let revoked_generation = f.route(&first).generation;
    f.connect("alice", "sub-a");
    let active = f.active(&google("alice"));
    assert_eq!(active.len(), 1);
    let second = active[0].clone();
    assert_ne!(second.account, first);
    assert!(second.generation > revoked_generation);
    assert_eq!((second.label.as_str(), second.provider, second.kind), ("alice gmail", "google", ConnectorKind::Personal));
    assert_eq!(second.binding, personal("alice"));
    assert!(!f.route(&first).active, "revocation is final");
    assert_eq!(f.pending(&google("alice")), 0);

    // Disconnect, then connect ANOTHER account: the old account's hint is
    // cancelled and nothing is recreated (setup required).
    disconnect_material(f.path(), "alice", "alice", false).unwrap();
    f.connect("alice", "sub-b");
    assert!(f.active(&google("alice")).is_empty());
    assert_eq!(f.pending(&google("alice")), 0);
    f.connect("alice", "sub-a");
    assert!(f.active(&google("alice")).is_empty(), "a cancelled hint never comes back");

    // Account switch while connected: the old route is revoked (final) and
    // the new account needs an admin provision.
    let third = f.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice"));
    let pinned = selection(f.path(), &third, "alice");
    f.connect("alice", "sub-c");
    assert_eq!(f.connected_sub("alice").as_deref(), Some("sub-c"));
    assert!(!f.route(&third).active);
    assert!(pinned.recheck().is_err());
    assert!(f.active(&google("alice")).is_empty());
    assert_eq!(f.pending(&google("alice")), 0);
}

#[cfg(unix)]
#[test]
fn c4_owner_change_revokes_personal_routes() {
    let f = fixture();
    let alice: Vec<_> = ["google", "database", "channel", "computer"].iter()
        .map(|provider| f.register("alice", provider, ConnectorKind::Personal, &personal("alice"))).collect();
    let bob = f.register("bob gmail", "google", ConnectorKind::Personal, &personal("bob"));
    let tenant = f.register("org github", "github", ConnectorKind::SharedTenant, &company());
    f.connect("alice", "sub-a");
    // A pending relink hint from an earlier own disconnect is cancelled too.
    let earlier = f.register("alice old", "google", ConnectorKind::Personal, &personal("alice"));
    f.catalog().revoke_scope(RouteScope { kind: ConnectorKind::Personal, binding: Some(&personal("alice")),
        provider: Some("google") }, Some(&RelinkSubject::of("google", "sub-a").unwrap())).unwrap();
    assert!(!f.route(&earlier).active);
    for account in &alice[..1] { assert!(!f.route(account).active); }
    let alice: Vec<_> = std::iter::once(f.register("alice", "google", ConnectorKind::Personal, &personal("alice")))
        .chain(alice[1..].iter().cloned()).collect();

    // Suspension/offboarding path (`users.remove` / `users.offboard`).
    let removed = disconnect_material(f.path(), "alice", "admin-1", true).unwrap();
    assert!(removed.is_some());
    assert_eq!(f.connected_sub("alice"), None);
    for account in &alice { assert!(!f.route(account).active, "every Personal route of the owner"); }
    assert!(Target::personal_all("alice").map(|t| f.active(&t)).unwrap().is_empty());
    assert_eq!(f.pending(&Target::personal_all("alice").unwrap()), 0);
    assert!(f.route(&bob).active && f.route(&tenant).active);

    // A restored owner reconnecting the same account gets nothing back.
    f.connect("alice", "sub-a");
    assert!(f.active(&Target::personal_all("alice").unwrap()).is_empty());

    // Without a catalog fence, the owner's material is kept.
    f.connect("alice", "sub-a");
    let path = f.path().join(crate::isolation_connectors::CATALOG_FILE);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(disconnect_material(f.path(), "alice", "admin-1", true).is_err());
    assert_eq!(f.connected_sub("alice").as_deref(), Some("sub-a"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(disconnect_material(f.path(), "alice", "admin-1", true).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn c4_home_oauth_disconnect_revokes_shared_home() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let catalog = ConnectorCatalog::open(home.path()).unwrap();
    let notion = catalog.register("home notion", "notion", ConnectorKind::SharedHome, &company()).unwrap();
    let github = catalog.register("home github", "github", ConnectorKind::SharedHome, &company()).unwrap();
    let tenant_notion = catalog.register("tenant notion", "notion", ConnectorKind::SharedTenant, &company()).unwrap();
    let alice = catalog.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice")).unwrap();
    let home_notion = Target::home("notion").unwrap();

    // Disconnect: SharedHome notion is revoked BEFORE the token is removed.
    let mut removed = false;
    crate::mcp_oauth::remove_home_credential(home.path(), "notion", || {
        let route = ConnectorCatalog::open(home.path()).unwrap().route(&notion).unwrap().unwrap();
        assert!(!route.active, "fenced before the token goes");
        removed = true;
        Ok(())
    }).unwrap();
    assert!(removed);
    assert!(!catalog.route(&notion).unwrap().unwrap().active);
    for other in [&github, &tenant_notion, &alice] { assert!(catalog.route(other).unwrap().unwrap().active); }
    assert_eq!(catalog.pending_relinks(home_notion.scope()).unwrap(), 1);

    // Reconnect: a new SharedHome route (new id, new generation).
    let before = catalog.list(None, 200).unwrap().iter().map(|r| r.generation).max().unwrap();
    crate::mcp_oauth::publish_home_credential(home.path(), "notion", || Ok::<_, String>(())).unwrap();
    let active = catalog.active_in_scope(home_notion.scope()).unwrap();
    assert_eq!(active.len(), 1);
    assert_ne!(active[0].account, notion);
    assert!(active[0].generation > before);
    assert_eq!((active[0].binding.clone(), active[0].kind), (company(), ConnectorKind::SharedHome));

    // Reconnect while connected: same id, new generation.
    let current = active[0].clone();
    crate::mcp_oauth::publish_home_credential(home.path(), "notion", || Ok::<_, String>(())).unwrap();
    let route = catalog.route(&current.account).unwrap().unwrap();
    assert!(route.active && route.generation > current.generation);

    // An admin revoke of the disconnected id (C3) cancels its relink hint:
    // a later home connect cannot undo the administrator's decision.
    crate::mcp_oauth::remove_home_credential(home.path(), "notion", || Ok(())).unwrap();
    assert_eq!(catalog.pending_relinks(home_notion.scope()).unwrap(), 1);
    catalog.revoke(&current.account).unwrap();
    assert_eq!(catalog.pending_relinks(home_notion.scope()).unwrap(), 0);
    crate::mcp_oauth::publish_home_credential(home.path(), "notion", || Ok::<_, String>(())).unwrap();
    assert!(catalog.active_in_scope(home_notion.scope()).unwrap().is_empty());

    // Providers without catalog routes are untouched; GitHub is independent.
    let mut slack = false;
    crate::mcp_oauth::remove_home_credential(home.path(), "slack", || { slack = true; Ok(()) }).unwrap();
    assert!(slack);
    assert!(catalog.route(&github).unwrap().unwrap().active);

    // A home without a catalog file is untouched (no file is created).
    let plain = crate::fs_safe::private_test_home().unwrap();
    crate::mcp_oauth::remove_home_credential(plain.path(), "github", || Ok(())).unwrap();
    assert!(!plain.path().join(crate::isolation_connectors::CATALOG_FILE).exists());
}

#[cfg(unix)]
#[test]
fn c4_revoke_before_material_drop_order() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let catalog = ConnectorCatalog::open(home.path()).unwrap();
    let account = catalog.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice")).unwrap();
    let target = google("alice");

    // Disconnect: when the material is dropped, the route is already
    // revoked and the pinned selection already refuses.
    let selected = selection(home.path(), &account, "alice");
    let mut observed = None;
    revoke_then(home.path(), &target, Hint::Leave, "test", || {
        let route = ConnectorCatalog::open(home.path()).unwrap().route(&account).unwrap().unwrap();
        observed = Some((route.active, selected.recheck()));
        Ok::<_, String>(())
    }).unwrap();
    assert_eq!(observed, Some((false, Err(VerificationError::BindingMismatch))));

    // Rotation: the old generation is fenced before the new material is
    // written, and selections made during publication refuse afterwards.
    let account = catalog.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice")).unwrap();
    let old = selection(home.path(), &account, "alice");
    let subject = RelinkSubject::of("google", "sub-a").unwrap();
    let mut during = None;
    let mut observed = None;
    rotate(home.path(), target.clone(), Reconnect::Same(subject.clone()), "test", || {
        observed = Some(old.recheck());
        during = Some(selection(home.path(), &account, "alice"));
        Ok::<_, String>(())
    }).unwrap();
    assert_eq!(observed, Some(Err(VerificationError::BindingMismatch)));
    assert!(during.unwrap().recheck().is_err());
    assert!(catalog.route(&account).unwrap().unwrap().active);

    // A fence failure refuses before any material is touched.
    let path = home.path().join(crate::isolation_connectors::CATALOG_FILE);
    use std::os::unix::fs::PermissionsExt;
    drop(catalog);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut called = false;
    assert_eq!(revoke_then(home.path(), &target, Hint::Cancel, "test", || { called = true; Ok::<_, String>(()) }),
        Err(EventFailure::Catalog(CatalogError::Unavailable)));
    assert_eq!(rotate(home.path(), target.clone(), Reconnect::Switch, "test", || { called = true; Ok::<_, String>(()) }),
        Err(EventFailure::Catalog(CatalogError::Unavailable)));
    let mut removed = false;
    assert!(crate::mcp_oauth::remove_home_credential(home.path(), "notion", || { removed = true; Ok(()) }).is_err());
    assert!(!called && !removed);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(ConnectorCatalog::open(home.path()).unwrap().route(&account).unwrap().unwrap().active);
}

#[cfg(unix)]
#[test]
fn c4_event_idempotent() {
    let f = fixture();
    let account = f.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice"));
    f.connect("alice", "sub-a");

    // An interrupted disconnect (material drop fails after the fence) leaves
    // the route revoked, never active, and the credential in place.
    let subject = RelinkSubject::of("google", "sub-a").unwrap();
    let failed = revoke_then(f.path(), &google("alice"), Hint::Record(&subject),
        "test", || Err::<(), _>("crash".to_string()));
    assert_eq!(failed, Err(EventFailure::Product("crash".into())));
    assert!(!f.route(&account).active);
    assert_eq!(f.connected_sub("alice").as_deref(), Some("sub-a"));
    let generation = f.route(&account).generation;
    // Retrying (any number of times) completes it without further catalog change.
    for _ in 0..3 {
        disconnect_material(f.path(), "alice", "alice", false).unwrap();
        assert_eq!(f.route(&account).generation, generation);
        assert!(f.active(&google("alice")).is_empty());
        assert_eq!(f.pending(&google("alice")), 1);
    }
    assert_eq!(f.connected_sub("alice"), None);

    // Repeated reconnects never create more than one route; the hint is
    // consumed once.
    f.connect("alice", "sub-a");
    let relinked = f.active(&google("alice"));
    assert_eq!(relinked.len(), 1);
    for _ in 0..3 {
        let before = max_generation(&f);
        f.connect("alice", "sub-a");
        let active = f.active(&google("alice"));
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].account, relinked[0].account);
        assert!(active[0].generation > before);
        assert_eq!(f.pending(&google("alice")), 0);
    }

    // Repeated consent-revoke / offboarding events are no-ops once applied.
    let metadata = vault::get(f.path(), "alice").unwrap().unwrap().metadata;
    for _ in 0..3 { crate::google_personal::lifecycle::consent_revoked(f.path(), &metadata); }
    assert!(f.active(&google("alice")).is_empty());
    assert_eq!(f.pending(&google("alice")), 1);
    for _ in 0..2 {
        disconnect_material(f.path(), "alice", "admin-1", true).unwrap();
        assert_eq!(f.pending(&google("alice")), 0);
    }
    let scope_all = Target::personal_all("alice").unwrap();
    assert!(f.active(&scope_all).is_empty());
    // Hints and routes are never deleted; a reopened catalog verifies its schema.
    assert!(f.catalog().list(None, 200).unwrap().len() >= 2);
    assert_eq!(f.catalog().revoke_scope(scope_all.scope(), None).unwrap(), Vec::new());
}
