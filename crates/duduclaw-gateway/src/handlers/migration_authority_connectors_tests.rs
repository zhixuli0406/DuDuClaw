//! C5 tests: real confirmation store, catalog, users.db, employee lifecycle
//! stores and legacy files; only the root allocator ReadBack is a fixture.
#![cfg(unix)]
use super::*;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use duduclaw_auth::{UserDb, UserRole};
use duduclaw_isolation::bounded::Text;
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation::{self, Receipt};
use duduclaw_migration::confirmation::{ApprovalSession, ProposalApplier};
use duduclaw_migration::contract::ProposalBinding;
use duduclaw_migration::ids::Sha256Digest;
use rusqlite::Connection;
use super::super::GatewayAuthority;
use crate::isolation_connectors::Route;

const ACTOR: &str = "0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10";
const AGENT: &str = "sales";
const EMPLOYEE: EmployeeId = EmployeeId([5; 16]);
const CSRF: [u8; 32] = [2; 32];
const NOW: u64 = 100;

struct Accept;
impl ProposalValidator for Accept {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
}
impl ProposalApplier for Accept {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
}

struct Root(Receipt);
impl RootAllocation for Root {
    fn read_back(&self, item: &ConfirmationItem) -> std::result::Result<Receipt, EmployeeError> {
        if self.0.binding != item.binding { return Err(EmployeeError::Unavailable); }
        Ok(self.0.clone())
    }
}

struct Fixture { home: tempfile::TempDir, owner: String, tenant: String, env: ConnectorEnv }
impl Fixture {
    fn path(&self) -> &Path { self.home.path() }
}

fn session() -> ApprovalSession {
    ApprovalSession::from_verified_session(true, true, Some(NOW), CSRF, RequestId([3; 16]))
}
fn activate(home: &Path, user: &str) {
    Connection::open(home.join("users.db")).unwrap().execute(
        "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-08T00:00:00Z' WHERE id=?1",
        [user]).unwrap();
}
fn write_config(home: &Path, extra: &str) {
    let url = format!("{:?}", home.join("demo.sqlite").to_str().unwrap());
    std::fs::write(home.join("config.toml"),
        format!("[db_sources.demo]\ndriver='sqlite'\nurl={url}\nallowed_tables=['*']\n{extra}")).unwrap();
}
fn write_agent(home: &Path, body: &str) {
    std::fs::create_dir_all(home.join("agents").join(AGENT)).unwrap();
    std::fs::write(home.join("agents").join(AGENT).join("agent.toml"), body).unwrap();
}
/// A v1 personal credential vault; the sealed secret is a canary, never read.
fn write_person_credentials(home: &Path, owners: &[&str]) {
    let records: Vec<Value> = owners.iter().enumerate().map(|(index, owner)| json!({
        "owner_kind": "user", "owner_id": owner, "provider": "google",
        "credential_id": uuid::Uuid::new_v4().to_string(), "revision": 1,
        "account_email": format!("person{index}@example.com"), "google_sub": format!("10000{index}"),
        "hosted_domain": null, "granted_scopes": ["openid"], "status": "connected",
        "connected_at": "2026-11-01T00:00:00Z", "last_refresh_at": null, "oauth_client_id": "client-1",
        "secret": "pk:v1:SECRET-SEALED-CANARY",
    })).collect();
    std::fs::create_dir_all(home.join("person_credentials")).unwrap();
    std::fs::write(crate::person_credentials::path(home),
        serde_json::to_vec(&json!({"version": 1, "records": records, "epochs": {}})).unwrap()).unwrap();
}

/// Private home with: Alice (active Admin), a confirmed Shared protected
/// employee `sales` (fixture root ReadBack), a `[db_sources.demo]` granted to
/// `sales`, and Alice's legacy Personal Google credential.
fn fixture() -> Fixture {
    let home = crate::fs_safe::private_test_home().unwrap();
    let path = home.path();
    std::fs::DirBuilder::new().mode(0o700).create(path.join("migration-authority")).unwrap();
    for name in ["users.db", "migration-authority/apply.db", "migration-confirmations.db"] {
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path.join(name)).unwrap();
    }
    let users = UserDb::new(&path.join("users.db")).unwrap();
    let owner = users.create_user("alice@example.invalid", "Alice", "password1", UserRole::Admin).unwrap().id;
    activate(path, &owner);
    let tenant = users.isolation_authority_snapshot(Some(&owner), AGENT).unwrap().tenant_id;
    let mut store = ConfirmationStore::from_connection(Connection::open(path.join("migration-confirmations.db")).unwrap()).unwrap();
    let fields = [("employee_id", AGENT), ("disk_bytes", "67108864"), ("binding_kind", "shared"), ("tenant_id", tenant.as_str())]
        .into_iter().map(|(name, value)| text_field(name, value)).collect();
    let staged = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
        ConfirmationCategory::Employee, fields, &Accept).unwrap();
    store.approve(&[staged.binding], &session(), CSRF, NOW, &Accept).unwrap();
    for _ in 0..3 { store.apply_next(&staged.binding, &mut Accept).unwrap(); }
    let item = store.list().unwrap().into_iter().find(|item| item.binding == staged.binding).unwrap();
    assert_eq!(item.status, ConfirmationStatus::Applied);
    let spec = allocation::specification(&item).unwrap();
    let receipt = Receipt { binding: item.binding, label: spec.label, employee: EMPLOYEE, uid: 70_002,
        bytes: spec.bytes, inodes: spec.inodes, disabled: false, principal_binding: spec.binding };
    crate::handlers::save_confirmed_receipt(path, &item, &receipt).unwrap();
    drop(store);
    write_agent(path, "[capabilities]\ndb_sources=['demo']\n");
    write_config(path, "");
    write_person_credentials(path, &[&owner]);
    let env = ConnectorEnv::fixture(Arc::new(Root(receipt)), ACTOR);
    Fixture { home, owner, tenant, env }
}

fn authority(fx: &Fixture) -> GatewayAuthority {
    GatewayAuthority::new(fx.path().to_path_buf()).unwrap().with_connector_env(fx.env.clone())
}
fn confirmations(fx: &Fixture) -> ConfirmationStore {
    ConfirmationStore::open_existing(&fx.path().join("migration-confirmations.db")).unwrap()
}
fn connector_items(fx: &Fixture) -> Vec<ConfirmationItem> {
    confirmations(fx).list().unwrap().into_iter().filter(|item| item.category == ConfirmationCategory::Connector).collect()
}
fn current(fx: &Fixture, binding: &ProposalBinding) -> ConfirmationItem {
    confirmations(fx).list().unwrap().into_iter().find(|item| item.binding == *binding).unwrap()
}
fn by_provider(items: &[ConfirmationItem], provider: &str) -> ConfirmationItem {
    items.iter().find(|item| card(item).unwrap()["provider"] == provider).unwrap().clone()
}
fn field<'a>(item: &'a ConfirmationItem, name: &str) -> Option<&'a str> {
    item.fields.iter().find(|field| field.name == name).and_then(|field| match &field.value {
        ProposalValue::Text { value } => Some(value.as_str()), _ => None })
}
fn catalog_routes(fx: &Fixture) -> Vec<Route> {
    if !fx.path().join(CATALOG_FILE).exists() { return Vec::new(); }
    ConnectorCatalog::open(fx.path()).unwrap().list(None, 200).unwrap()
}
/// Admin approval on the card, then the three durable apply steps.
fn apply(fx: &Fixture, binding: &ProposalBinding) -> Result<ConfirmationItem> {
    let mut store = confirmations(fx);
    store.approve(&[*binding], &session(), CSRF, NOW, &authority(fx))?;
    let mut applier = authority(fx);
    let mut last = None;
    for _ in 0..3 { last = Some(store.apply_next(binding, &mut applier)?); }
    last.ok_or(Error::Apply)
}
fn actor() -> AdminActor { AdminActor::verified(ACTOR).unwrap() }

#[test]
fn c5_import_stages_connector_proposals() {
    let fx = fixture();
    let summary = import_legacy_with(fx.path(), &fx.env).unwrap();
    assert_eq!(summary, ImportSummary { staged: 2, unchanged: 0, closed: 0, unproven_personal: 0 });
    let items = connector_items(&fx);
    assert_eq!(items.len(), 2, "one proposal per legacy account");
    assert!(items.iter().all(|item| item.status == ConfirmationStatus::Pending && item.high_risk));

    let google = by_provider(&items, "google");
    assert_eq!(field(&google, "binding_kind"), Some("personal"));
    assert_eq!(field(&google, "owner_id"), Some(fx.owner.as_str()));
    assert_eq!(google.binding.operation_id.proposal_id, proposal_id(&format!("google_personal:{}", fx.owner)));
    let google_card = card(&google).unwrap();
    assert_eq!(google_card["kind"], "personal");
    assert_eq!(google_card["details"], Value::Null);

    let database = by_provider(&items, "database");
    assert_eq!(field(&database, "binding_kind"), Some("shared"));
    assert_eq!(field(&database, "owner_id"), None);
    let database_card = card(&database).unwrap();
    assert_eq!(database_card["kind"], "shared_home");
    assert_eq!(database_card["label"], "資料庫 demo");
    assert_eq!(database_card["details"], json!({"employee": AGENT, "source": "demo"}));
    assert!(GatewayAuthority::built_in_item_supported(&database), "the card can offer apply");

    // Re-import: same accounts ⇒ same proposals; staging never writes a route.
    assert_eq!(import_legacy_with(fx.path(), &fx.env).unwrap(),
        ImportSummary { staged: 0, unchanged: 2, closed: 0, unproven_personal: 0 });
    assert_eq!(connector_items(&fx).len(), 2);
    assert!(!fx.path().join(CATALOG_FILE).exists());

    // A newly granted source is one more proposal; the others are untouched.
    write_config(fx.path(), "\n[db_sources.demo2]\ndriver='sqlite'\nurl='/nonexistent/demo2.sqlite'\nallowed_tables=['*']\n");
    write_agent(fx.path(), "[capabilities]\ndb_sources=['demo','demo2','*']\n");
    assert_eq!(import_legacy_with(fx.path(), &fx.env).unwrap(),
        ImportSummary { staged: 1, unchanged: 2, closed: 0, unproven_personal: 0 });
    assert_eq!(connector_items(&fx).len(), 3, "a wildcard grant names no account");
}

#[test]
fn c5_apply_provisions_route_and_details_via_c3() {
    let fx = fixture();
    import_legacy_with(fx.path(), &fx.env).unwrap();
    let items = connector_items(&fx);
    for item in &items {
        assert_eq!(apply(&fx, &item.binding).unwrap().status, ConfirmationStatus::Applied);
    }
    assert_eq!(catalog_routes(&fx).len(), 2);
    let catalog = ConnectorCatalog::open(fx.path()).unwrap();
    let shared = EmployeeBinding::Shared(Text::new(fx.tenant.clone()).unwrap());
    let personal = EmployeeBinding::Personal(Text::new(fx.owner.clone()).unwrap());

    let database = by_provider(&items, "database");
    let account = catalog.migrated_account(&origin(&database).unwrap()).unwrap().unwrap();
    let (route, stored) = catalog.route_details(&account).unwrap().unwrap();
    assert!(route.active);
    assert_eq!((route.provider, route.kind, &route.binding, route.label.as_str()),
        ("database", ConnectorKind::SharedHome, &shared, "資料庫 demo"));
    // C6 details written with the route, pinned to the confirmed employee.
    assert_eq!(stored, Some(AccountDetailRecord { employee: EMPLOYEE, agent: AGENT.into(),
        reference: DetailReference::Database { source: "demo".into() } }));

    let google = by_provider(&items, "google");
    let account = catalog.migrated_account(&origin(&google).unwrap()).unwrap().unwrap();
    let (route, stored) = catalog.route_details(&account).unwrap().unwrap();
    assert!(route.active && stored.is_none());
    assert_eq!((route.provider, route.kind, &route.binding), ("google", ConnectorKind::Personal, &personal));

    // Provisioned through C3: its audit event, marked as a migration.
    let audit = std::fs::read_to_string(fx.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("isolation.connector_provisioned") && audit.contains("migration"));
    assert!(!audit.contains(&fx.tenant), "C3 audit never names the tenant");
}

#[test]
fn c5_reject_creates_nothing() {
    let fx = fixture();
    import_legacy_with(fx.path(), &fx.env).unwrap();
    let items = connector_items(&fx);
    let (google, database) = (by_provider(&items, "google"), by_provider(&items, "database"));
    let mut store = confirmations(&fx);
    let mut applier = authority(&fx);
    // Rejected while pending.
    store.reject(&google.binding, &session(), CSRF, NOW, &mut applier).unwrap();
    // Approved, then rejected before any apply step: nothing to compensate.
    store.approve(&[database.binding], &session(), CSRF, NOW, &authority(&fx)).unwrap();
    store.reject(&database.binding, &session(), CSRF, NOW, &mut applier).unwrap();
    assert!(connector_items(&fx).iter().all(|item| item.status == ConfirmationStatus::Rejected));
    assert!(catalog_routes(&fx).is_empty());
    // A rejected proposal can no longer be applied, and a re-import never re-stages it.
    assert!(store.apply_next(&database.binding, &mut applier).is_err());
    assert_eq!(import_legacy_with(fx.path(), &fx.env).unwrap(),
        ImportSummary { staged: 0, unchanged: 0, closed: 2, unproven_personal: 0 });
    assert!(catalog_routes(&fx).is_empty());
}

#[test]
fn c5_personal_requires_proven_owner() {
    let fx = fixture();
    write_person_credentials(fx.path(), &[&fx.owner, "ghost-user"]);
    let summary = import_legacy_with(fx.path(), &fx.env).unwrap();
    assert_eq!((summary.staged, summary.unproven_personal), (2, 1));
    let items = connector_items(&fx);
    let personal: Vec<&ConfirmationItem> = items.iter().filter(|item| field(item, "binding_kind") == Some("personal")).collect();
    assert_eq!(personal.len(), 1);
    assert_eq!(field(personal[0], "owner_id"), Some(fx.owner.as_str()));
    // The unproven credential is not downgraded to a shared route either.
    assert!(!serde_json::to_string(&items).unwrap().contains("ghost-user"));
    assert_eq!(items.iter().filter(|item| card(item).unwrap()["provider"] == "google").count(), 1);

    // A Personal proposal for a real user without legacy evidence is refused.
    let users = UserDb::new(&fx.path().join("users.db")).unwrap();
    let bob = users.create_user("bob@example.invalid", "Bob", "password1", UserRole::Employee).unwrap().id;
    activate(fx.path(), &bob);
    let forged = ConnectorPlan { source: format!("google_personal:{bob}"), provider: "google".into(),
        route_kind: "personal".into(), label: PERSONAL_GOOGLE_LABEL.into(), owner: Some(bob.clone()), details: None };
    let mut store = confirmations(&fx);
    let staged = store.stage_proposal(ProposalOperationId { proposal_id: proposal_id(&forged.source), revision: 1 },
        ConfirmationCategory::Connector, forged.fields().unwrap(), &Structural).unwrap();
    assert!(store.approve(&[staged.binding], &session(), CSRF, NOW, &authority(&fx)).is_err());
    // Nor does the legacy data prove a shared or re-owned version of Alice's account.
    let synthetic = |plan: &ConnectorPlan| ConfirmationItem {
        binding: ProposalBinding { operation_id: ProposalOperationId { proposal_id: proposal_id(&plan.source), revision: 9 },
            proposal_digest: Sha256Digest([8; 32]) },
        category: ConfirmationCategory::Connector, status: ConfirmationStatus::Pending, high_risk: true,
        fields: plan.fields().unwrap() };
    let alice = parse(personal[0]).unwrap();
    let shared = ConnectorPlan { route_kind: "shared_home".into(), owner: None, ..alice.clone() };
    let reowned = ConnectorPlan { owner: Some(bob.clone()), ..alice.clone() };
    for plan in [&shared, &reowned] {
        assert!(matches!(validate(fx.path(), &synthetic(plan), &fx.env), Err(Error::Stale)));
    }
    assert!(validate(fx.path(), personal[0], &fx.env).is_ok());
    // The proven owner must also still be eligible when the Admin approves.
    Connection::open(fx.path().join("users.db")).unwrap()
        .execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&fx.owner]).unwrap();
    assert!(store.approve(&[personal[0].binding], &session(), CSRF, NOW, &authority(&fx)).is_err());
    assert_eq!(current(&fx, &personal[0].binding).status, ConfirmationStatus::Pending);
    assert!(catalog_routes(&fx).is_empty());
}

#[test]
fn c5_reapply_idempotent() {
    let fx = fixture();
    import_legacy_with(fx.path(), &fx.env).unwrap();
    let items = connector_items(&fx);
    let applied = apply(&fx, &by_provider(&items, "database").binding).unwrap();
    let provenance = origin(&applied).unwrap();
    let account = ConnectorCatalog::open(fx.path()).unwrap().migrated_account(&provenance).unwrap().unwrap();

    // The same applier, and one after a restart, find the route this proposal created.
    let mut same = authority(&fx);
    for _ in 0..2 { same.publish(&applied).unwrap(); same.read_back(&applied).unwrap(); }
    let mut restarted = authority(&fx);
    restarted.stage(&applied).unwrap();
    restarted.publish(&applied).unwrap();
    restarted.read_back(&applied).unwrap();
    assert_eq!(catalog_routes(&fx).len(), 1);
    assert_eq!(ConnectorCatalog::open(fx.path()).unwrap().migrated_account(&provenance).unwrap(), Some(account.clone()));

    // A revoked migrated route is never reactivated nor replaced by a new id.
    route_lifecycle::revoke(fx.path(), &account, &actor()).unwrap();
    assert!(same.publish(&applied).is_err());
    assert!(same.read_back(&applied).is_err());
    let routes = catalog_routes(&fx);
    assert_eq!(routes.len(), 1);
    assert!(!routes[0].active && routes[0].account == account);
    same.disable_and_read_back(&applied).unwrap();
    same.disable_and_read_back(&applied).unwrap();

    // An account another route already covers is not duplicated by migration.
    let owner = EmployeeBinding::Personal(Text::new(fx.owner.clone()).unwrap());
    route_lifecycle::provision(fx.path(), "google", ConnectorKind::Personal, &owner, "admin google", None, &actor()).unwrap();
    let google = by_provider(&items, "google");
    assert!(apply(&fx, &google.binding).is_err());
    assert_eq!(current(&fx, &google.binding).status, ConfirmationStatus::Failed);
    assert_eq!(catalog_routes(&fx).iter().filter(|route| route.provider == "google").count(), 1);
}

#[test]
fn c5_no_secrets_in_proposal() {
    let fx = fixture();
    write_agent(fx.path(), "[capabilities]\ndb_sources=['demo']\n\n[channels.telegram]\nbot_token = \"SECRET-AGENT-BOT\"\n");
    write_config(fx.path(), "\n[channels]\ntelegram_bot_token = \"SECRET-HOME-BOT\"\n\n[odoo]\nurl = \"https://odoo.example.invalid\"\napi_key = \"SECRET-ODOO-KEY\"\n");
    std::fs::write(fx.path().join("agent_bindings.json"), serde_json::to_vec(&json!({
        "bindings": {"telegram": {"424242": AGENT}}, "tokens": {"SECRET-BIND-DIGEST": {"agent_id": AGENT}}})).unwrap()).unwrap();
    assert_eq!(import_legacy_with(fx.path(), &fx.env).unwrap().staged, 4);
    let items = connector_items(&fx);
    let providers: BTreeSet<String> = items.iter()
        .map(|item| card(item).unwrap()["provider"].as_str().unwrap().to_owned()).collect();
    assert_eq!(providers, ["channel", "database", "google", "odoo"].into_iter().map(String::from).collect());

    // The employee's own bot: a token *reference* with the employee's binding.
    let channel = card(&by_provider(&items, "channel")).unwrap();
    assert_eq!(channel["kind"], "shared_tenant");
    assert_eq!(channel["details"], json!({"employee": AGENT, "channel": "telegram", "scope": "424242",
        "status_label": "telegram", "token_ref": "agent"}));
    assert_eq!(card(&by_provider(&items, "odoo")).unwrap()["kind"], "shared_home");

    let mut exported = serde_json::to_string(&items).unwrap();
    for item in &items {
        // References only: no secret field, no fingerprint.
        assert!(item.fields.iter().all(|field| field.name != "credential"
            && matches!(field.value, ProposalValue::Text { .. })));
        let summary = card(item).unwrap();
        let mut keys: Vec<String> = summary.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["binding", "details", "kind", "label", "legacy_source", "owner_user_id", "provider"]);
        exported.push_str(&summary.to_string());
    }
    for forbidden in ["SECRET", "pk:v1", "example.com", "odoo.example.invalid", "token\":", fx.tenant.as_str()] {
        assert!(!exported.contains(forbidden), "{forbidden}");
    }
    let bytes = std::fs::read(fx.path().join("migration-confirmations.db")).unwrap();
    assert!(!bytes.windows(6).any(|window| window == b"SECRET"));
}

#[test]
fn c5_unreadable_legacy_config_refuses() {
    let fx = fixture();
    // Any unreadable legacy store refuses the whole import before staging.
    std::fs::write(fx.path().join("config.toml"), "[db_sources\nbroken = ").unwrap();
    assert!(matches!(import_legacy_with(fx.path(), &fx.env), Err(Error::Storage)));
    assert!(connector_items(&fx).is_empty());
    write_config(fx.path(), "");
    std::fs::write(crate::person_credentials::path(fx.path()), b"{not json").unwrap();
    assert!(matches!(import_legacy_with(fx.path(), &fx.env), Err(Error::Storage)));
    std::fs::write(fx.path().join("agents").join(AGENT).join("agent.toml"), "[capabilities\n").unwrap();
    write_person_credentials(fx.path(), &[&fx.owner]);
    assert!(matches!(import_legacy_with(fx.path(), &fx.env), Err(Error::Storage)));
    assert!(connector_items(&fx).is_empty());

    // Staged proposals re-read the legacy stores at approval and at apply.
    write_agent(fx.path(), "[capabilities]\ndb_sources=['demo']\n");
    assert_eq!(import_legacy_with(fx.path(), &fx.env).unwrap().staged, 2);
    let items = connector_items(&fx);
    let (google, database) = (by_provider(&items, "google"), by_provider(&items, "database"));
    std::fs::write(crate::person_credentials::path(fx.path()), b"{not json").unwrap();
    let mut store = confirmations(&fx);
    assert!(store.approve(&[google.binding], &session(), CSRF, NOW, &authority(&fx)).is_err());
    store.approve(&[database.binding], &session(), CSRF, NOW, &authority(&fx)).unwrap();
    std::fs::write(fx.path().join("config.toml"), "[db_sources\n").unwrap();
    let mut applier = authority(&fx);
    assert!((0..3).try_for_each(|_| store.apply_next(&database.binding, &mut applier).map(|_| ())).is_err());
    assert_eq!(current(&fx, &google.binding).status, ConfirmationStatus::Pending);
    assert_eq!(current(&fx, &database.binding).status, ConfirmationStatus::Failed);
    assert!(catalog_routes(&fx).is_empty());
}
