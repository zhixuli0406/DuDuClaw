use super::*;
use duduclaw_isolation::bounded::Text;
use crate::isolation_connector_lifecycle::{provision, revoke_with, AdminActor, LifecycleError};
use crate::isolation_connectors::CATALOG_FILE;

const AGENT: &str = "employee-a";
const OTHER_AGENT: &str = "employee-b";
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
const TOKEN: &str = "123456:AAHfixtureSecretTokenValue_x";

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn alice() -> EmployeeBinding { EmployeeBinding::Personal(text("alice")) }
fn bob() -> EmployeeBinding { EmployeeBinding::Personal(text("bob")) }
fn company() -> EmployeeBinding { EmployeeBinding::Shared(text("company")) }
fn actor() -> AdminActor { AdminActor::verified("0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10").unwrap() }
fn employee(binding: EmployeeBinding) -> DetailEmployee {
    DetailEmployee { employee: EMPLOYEE, agent: AGENT.into(), binding }
}
fn database(source: &str) -> DetailReference { DetailReference::Database { source: source.into() } }
fn telegram(token: ChannelTokenRef) -> DetailReference {
    DetailReference::Channel { channel: "telegram".into(), scope: "123456789".into(), status_label: "telegram".into(),
        token, extra_id: None }
}

/// Private home: AGENT may use source `demo` and has its own Telegram bot;
/// OTHER_AGENT may use `demo2` only.
fn fixture() -> tempfile::TempDir {
    let home = crate::fs_safe::private_test_home().unwrap();
    let agents = home.path().join("agents");
    std::fs::create_dir_all(agents.join(AGENT)).unwrap();
    std::fs::create_dir_all(agents.join(OTHER_AGENT)).unwrap();
    std::fs::write(agents.join(AGENT).join("agent.toml"),
        format!("[capabilities]\ndb_sources=['demo']\n\n[channels.telegram]\nbot_token = \"{TOKEN}\"\n")).unwrap();
    std::fs::write(agents.join(OTHER_AGENT).join("agent.toml"), "[capabilities]\ndb_sources=['demo2']\n").unwrap();
    let url = |name: &str| format!("{:?}", home.path().join(name).to_str().unwrap());
    std::fs::write(home.path().join("config.toml"), format!(
        "[db_sources.demo]\ndriver='sqlite'\nurl={}\nallowed_tables=['*']\n\n[db_sources.demo2]\ndriver='sqlite'\nurl={}\nallowed_tables=['*']\n",
        url("demo.sqlite"), url("demo2.sqlite"))).unwrap();
    home
}
fn catalog(home: &Path) -> Arc<ConnectorCatalog> { Arc::new(ConnectorCatalog::open(home).unwrap()) }
fn reader(home: &Path) -> CatalogAccountDetails { CatalogAccountDetails::new(home.to_path_buf(), catalog(home)).unwrap() }
/// Resolve as EMPLOYEE/AGENT, the employee the fixtures provision for.
fn resolve(details: &CatalogAccountDetails, route: &Route) -> Result<Option<ProvisionedDetail>, VerificationError> {
    details.detail(route, EMPLOYEE, AGENT).map(DetailLookup::ready)
}
fn current(home: &Path, account: &AccountId) -> (Route, Option<AccountDetailRecord>) {
    catalog(home).route_details(account).unwrap().unwrap()
}
fn raw(home: &Path) -> Connection { Connection::open(home.join(CATALOG_FILE)).unwrap() }
fn detail_rows(home: &Path, account: &AccountId) -> i64 {
    raw(home).query_row("SELECT count(*) FROM isolation_connector_details WHERE account=?1", [account.as_str()],
        |row| row.get(0)).unwrap()
}
fn route_count(home: &Path) -> usize {
    match ConnectorCatalog::open(home) { Ok(catalog) => catalog.list(None, 200).unwrap().len(), Err(_) => 0 }
}
fn audit(home: &Path) -> String { std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default() }
fn refused(result: Result<AccountId, LifecycleError>) -> DetailError {
    match result { Err(LifecycleError::Detail(error)) => error, other => panic!("expected a detail refusal, got {other:?}") }
}

#[cfg(unix)]
#[test]
fn c6_provision_records_details_pinned_to_generation() {
    let home = fixture();
    let owner = employee(alice());
    let account = provision(home.path(), "database", ConnectorKind::Personal, &alice(), "alice demo",
        Some((&owner, &database("demo"))), &actor()).unwrap();

    let (route, stored) = current(home.path(), &account);
    assert!(route.active);
    assert_eq!(stored, Some(AccountDetailRecord { employee: EMPLOYEE, agent: AGENT.into(), reference: database("demo") }));
    let (generation, binding_id): (i64, String) = raw(home.path()).query_row(
        "SELECT generation, binding_id FROM isolation_connector_details WHERE account=?1", [account.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!((generation as u64, binding_id.as_str()), (route.generation, "personal:alice"));

    // The production reader resolves it and pins generation, binding and employee.
    let details = reader(home.path());
    let provisioned = resolve(&details, &route).unwrap().unwrap();
    assert!(matches!(&provisioned.detail, ConnectorAccountDetail::Database { source } if source == "demo"));
    assert_eq!((provisioned.pin.generation(), provisioned.pin.employee(), provisioned.pin.agent()),
        (route.generation, EMPLOYEE, AGENT));
    assert_eq!(details.current_pin(&account).unwrap(), Some(provisioned.pin.clone()));

    // Rows are immutable while their generation is current.
    let a = account.as_str();
    let other = catalog(home.path()).register("alice bare", "database", ConnectorKind::Personal, &alice()).unwrap();
    let other_generation = current(home.path(), &other).0.generation;
    let insert = |account: &str, generation: u64, binding: &str| format!(
        "INSERT INTO isolation_connector_details (account,generation,binding_id,provider,employee,agent,source,created_at)
         VALUES ('{account}',{generation},'{binding}','database','{}','{AGENT}','demo',1)", "02".repeat(16));
    for sql in [
        format!("UPDATE isolation_connector_details SET source='demo2' WHERE account='{a}'"),
        format!("UPDATE isolation_connector_details SET generation=generation+1 WHERE account='{a}'"),
        format!("DELETE FROM isolation_connector_details WHERE account='{a}'"),
        insert(a, route.generation, "personal:alice"),
        format!("INSERT OR REPLACE INTO isolation_connector_details (account,generation,binding_id,provider,employee,agent,source,created_at)
            VALUES ('{a}',{},'personal:alice','database','{}','{AGENT}','demo2',1)", route.generation, "02".repeat(16)),
        // A row for another route must match its exact generation and binding.
        insert(other.as_str(), other_generation + 1, "personal:alice"),
        insert(other.as_str(), other_generation, "personal:bob"),
    ] {
        assert!(raw(home.path()).execute_batch(&sql).is_err(), "{sql}");
    }
    assert_eq!(current(home.path(), &account).1, stored);

    // A generation move (credential re-consent, owner change) ends them.
    let next = catalog(home.path()).advance(&account).unwrap();
    let (moved, stored) = current(home.path(), &account);
    assert!(moved.active && moved.generation == next && stored.is_none());
    assert_eq!(detail_rows(home.path(), &account), 0);
    assert!(resolve(&details, &moved).unwrap().is_none(), "old-generation details never resolve");
    assert_eq!(details.current_pin(&account).unwrap(), None);

    // The employee must be one that can use a route of this binding/kind.
    let routes = route_count(home.path());
    assert_eq!(refused(provision(home.path(), "database", ConnectorKind::Personal, &alice(), "x",
        Some((&employee(bob()), &database("demo"))), &actor())), DetailError::Employee);
    assert_eq!(refused(provision(home.path(), "database", ConnectorKind::SharedTenant, &company(), "x",
        Some((&employee(alice()), &database("demo"))), &actor())), DetailError::Employee);
    assert_eq!(route_count(home.path()), routes, "a refused provision registers nothing");
    let audit = audit(home.path());
    assert!(audit.contains("isolation.connector_provisioned") && audit.contains("employee_mismatch"));
    assert!(!audit.contains("alice demo"));
}

#[cfg(unix)]
#[test]
fn c6_relink_invalidates_old_details() {
    let home = fixture();
    let owner = employee(alice());
    let old = provision(home.path(), "database", ConnectorKind::Personal, &alice(), "alice demo",
        Some((&owner, &database("demo"))), &actor()).unwrap();
    let details = reader(home.path());
    let (old_route, _) = current(home.path(), &old);
    let old_pin = resolve(&details, &old_route).unwrap().unwrap().pin;

    // Relink: retire the old account, provision the same account again.
    revoke_with(home.path(), &old, &actor(), None).unwrap();
    let new = provision(home.path(), "database", ConnectorKind::Personal, &alice(), "alice demo",
        Some((&owner, &database("demo"))), &actor()).unwrap();
    assert_ne!(old, new, "a relink always mints a new id");

    // The old account's details are gone for good; a fence pinned to them refuses.
    assert!(resolve(&details, &old_route).unwrap().is_none());
    assert_eq!(details.current_pin(&old).unwrap(), None);
    assert_eq!(detail_rows(home.path(), &old), 0);
    // The new account resolves under its own, newer pin only.
    let (new_route, _) = current(home.path(), &new);
    let new_pin = resolve(&details, &new_route).unwrap().unwrap().pin;
    assert!(new_route.generation > old_route.generation);
    assert_ne!(new_pin, old_pin);
    assert_eq!(details.current_pin(&new).unwrap(), Some(new_pin));
    // Asking with the stale route snapshot for the new id never mixes them.
    let mut stale = new_route.clone();
    stale.generation = old_route.generation;
    assert_eq!(resolve(&details, &stale).err(), Some(VerificationError::Unavailable));
}

#[cfg(unix)]
#[test]
fn c6_revoke_removes_details() {
    let home = fixture();
    let owner = employee(alice());
    let db = provision(home.path(), "database", ConnectorKind::Personal, &alice(), "alice demo",
        Some((&owner, &database("demo"))), &actor()).unwrap();
    let chat = provision(home.path(), "channel", ConnectorKind::Personal, &alice(), "alice bot",
        Some((&owner, &telegram(ChannelTokenRef::Agent))), &actor()).unwrap();
    assert_eq!((detail_rows(home.path(), &db), detail_rows(home.path(), &chat)), (1, 1));

    let route = revoke_with(home.path(), &db, &actor(), None).unwrap();
    assert!(!route.active);
    assert_eq!(detail_rows(home.path(), &db), 0, "removed in the revoke transaction");
    assert_eq!(current(home.path(), &db).1, None);
    assert_eq!(detail_rows(home.path(), &chat), 1, "other accounts keep theirs");
    // Final and idempotent; nothing can re-attach details to a revoked route.
    revoke_with(home.path(), &db, &actor(), None).unwrap();
    assert!(raw(home.path()).execute_batch(&format!(
        "INSERT INTO isolation_connector_details (account,generation,binding_id,provider,employee,agent,source,created_at)
         VALUES ('{}',{},'personal:alice','database','{}','{AGENT}','demo',1)", db.as_str(), route.generation, "02".repeat(16)))
        .is_err());
    assert!(reader(home.path()).current_pin(&db).unwrap().is_none());
    assert!(audit(home.path()).contains("isolation.connector_revoked"));
}

#[cfg(unix)]
#[test]
fn c6_foreign_website_credential_refused() {
    let home = fixture();
    let upsert = |label: &str, agents: &[&str]| crate::website_credentials::upsert(home.path(),
        crate::website_credentials::Upsert { id: None, label: label.into(), login_url: "https://erp.example.com/login".into(),
            allowed_paths: vec![], username: "robot".into(), password: "fixture-password".into(),
            allowed_agents: agents.iter().map(|agent| (*agent).to_owned()).collect() }, "admin-user").unwrap().id;
    let foreign = upsert("other erp", &[OTHER_AGENT]);
    let own = upsert("own erp", &[AGENT]);
    let owner = employee(alice());
    let computer = |credential: &str| provision(home.path(), "computer", ConnectorKind::Personal, &alice(), "alice erp",
        Some((&owner, &DetailReference::Computer { credential: credential.into() })), &actor());

    assert_eq!(refused(computer(&foreign)), DetailError::Foreign, "allowed_agents does not include the employee");
    assert_eq!(refused(computer(&uuid::Uuid::new_v4().to_string())), DetailError::Unknown);
    assert_eq!(refused(computer("not-a-credential-id")), DetailError::Invalid);
    assert_eq!(route_count(home.path()), 0);

    let account = computer(&own).unwrap();
    let (route, _) = current(home.path(), &account);
    let provisioned = resolve(&reader(home.path()), &route).unwrap().unwrap();
    assert!(matches!(&provisioned.detail, ConnectorAccountDetail::Computer { credential } if *credential == own));
    let audit = audit(home.path());
    assert!(!audit.contains("fixture-password") && !audit.contains("robot"));
}

#[cfg(unix)]
#[test]
fn c6_unknown_data_source_refused() {
    let home = fixture();
    let owner = employee(alice());
    let attempt = |provider: &str, reference: &DetailReference| provision(home.path(), provider,
        ConnectorKind::Personal, &alice(), "alice db", Some((&owner, reference)), &actor());

    assert_eq!(refused(attempt("database", &database("missing"))), DetailError::Unknown);
    assert_eq!(refused(attempt("database", &database("dem"))), DetailError::Unknown, "exact registered name only");
    assert_eq!(refused(attempt("database", &database("Demo"))), DetailError::Invalid, "never case-folded into `demo`");
    assert_eq!(refused(attempt("database", &database("demo2"))), DetailError::Foreign, "registered but not granted");
    assert_eq!(refused(attempt("database", &database("bad name!"))), DetailError::Invalid);
    assert_eq!(refused(attempt("database", &DetailReference::Computer { credential: uuid::Uuid::new_v4().to_string() })),
        DetailError::Invalid, "details of another provider");
    assert_eq!(refused(attempt("google", &database("demo"))), DetailError::NotApplicable);
    assert_eq!(route_count(home.path()), 0, "nothing registered for a refused reference");
    // The same source for the employee that is granted it.
    assert!(attempt("database", &database("demo")).is_ok());
}

#[cfg(unix)]
#[test]
fn c6_channel_token_is_reference_only() {
    let home = fixture();
    let owner = employee(alice());
    let attempt = |reference: &DetailReference| provision(home.path(), "channel", ConnectorKind::Personal, &alice(),
        "alice bot", Some((&owner, reference)), &actor());
    // Personal routes can use neither the home bot nor "no token".
    assert_eq!(refused(attempt(&telegram(ChannelTokenRef::Home))), DetailError::Foreign);
    assert_eq!(refused(attempt(&telegram(ChannelTokenRef::None))), DetailError::Foreign);
    // A raw token is not a reference form.
    assert_eq!(ChannelTokenRef::parse(TOKEN), None);
    assert_eq!(ChannelTokenRef::parse("Agent"), None);

    let account = attempt(&telegram(ChannelTokenRef::Agent)).unwrap();
    let token_ref: String = raw(home.path()).query_row(
        "SELECT token_ref FROM isolation_connector_details WHERE account=?1", [account.as_str()], |row| row.get(0)).unwrap();
    assert_eq!(token_ref, "agent");
    // The token value is in no store file and no audit row.
    for suffix in ["", "-wal", "-journal"] {
        let bytes = std::fs::read(home.path().join(format!("{CATALOG_FILE}{suffix}"))).unwrap_or_default();
        assert!(!bytes.windows(TOKEN.len()).any(|window| window == TOKEN.as_bytes()), "{suffix}");
    }
    assert!(!audit(home.path()).contains(TOKEN));

    // The reader resolves it in memory for the provider build only.
    let details = reader(home.path());
    let (route, _) = current(home.path(), &account);
    let provisioned = resolve(&details, &route).unwrap().unwrap();
    assert!(matches!(&provisioned.detail, ConnectorAccountDetail::Channel { token: Some(token), scope, .. }
        if token == TOKEN && scope == "123456789"));
    assert!(!format!("{:?}", provisioned.pin).contains(TOKEN), "the pin is secret-free");

    // Token removed from the agent: setup required (never another token).
    std::fs::write(home.path().join("agents").join(AGENT).join("agent.toml"), "[capabilities]\ndb_sources=['demo']\n").unwrap();
    assert!(resolve(&details, &route).unwrap().is_none());
    assert_eq!(refused(attempt(&telegram(ChannelTokenRef::Agent))), DetailError::Unknown);
}

#[cfg(unix)]
#[test]
fn c6_shared_tenant_channel_refuses_home_token() {
    // The home bot is a home-wide credential; a SharedTenant route may not
    // borrow it until tenant token sources exist (task E4).
    let home = fixture();
    let owner = employee(company());
    assert_eq!(refused(provision(home.path(), "channel", ConnectorKind::SharedTenant, &company(), "company bot",
        Some((&owner, &telegram(ChannelTokenRef::Home))), &actor())), DetailError::Foreign);
    assert_eq!(route_count(home.path()), 0, "nothing registered for a refused reference");
}

#[cfg(unix)]
#[test]
fn c6_foreign_pin_is_decided_before_token_resolution() {
    let home = fixture();
    let other_employee = EmployeeId([9; 16]);
    let catalog = catalog(home.path());
    // Provisioned for another employee whose agent has no Telegram token.
    let account = catalog.register_with_details("other bot", "channel", ConnectorKind::Personal, &alice(),
        &AccountDetailRecord { employee: other_employee, agent: OTHER_AGENT.into(), reference: telegram(ChannelTokenRef::Agent) })
        .unwrap();
    let details = CatalogAccountDetails::new(home.path().to_path_buf(), catalog.clone()).unwrap();
    let (route, _) = current(home.path(), &account);
    assert!(matches!(details.detail(&route, EMPLOYEE, AGENT), Ok(DetailLookup::Foreign)),
        "another employee's details are foreign, not a missing token of this employee");
    assert!(matches!(details.detail(&route, EMPLOYEE, OTHER_AGENT), Ok(DetailLookup::Foreign)));
    assert!(matches!(details.detail(&route, other_employee, AGENT), Ok(DetailLookup::Foreign)),
        "a recreated employee under another id is not the pinned one");
    // Only the pinned employee resolves its (missing) token.
    assert!(matches!(details.detail(&route, other_employee, OTHER_AGENT), Ok(DetailLookup::Missing)));

    // Pinned to another employee whose agent HAS a token: still foreign, the
    // token is never resolved into this employee's lookup.
    let theirs = catalog.register_with_details("their bot", "channel", ConnectorKind::Personal, &alice(),
        &AccountDetailRecord { employee: other_employee, agent: AGENT.into(), reference: telegram(ChannelTokenRef::Agent) })
        .unwrap();
    let (route, _) = current(home.path(), &theirs);
    assert!(matches!(details.detail(&route, EMPLOYEE, AGENT), Ok(DetailLookup::Foreign)));
}
