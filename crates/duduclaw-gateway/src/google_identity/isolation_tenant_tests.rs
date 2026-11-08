//! v1.81 E4: delegate / service-account tenant sources at the Google bridge.
use super::*;
use duduclaw_core::google_identity::CredentialKind;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane},
    ids::{RunId, Sha256Digest}};
use crate::google_identity::isolation_routes::tests::{catalog, lifecycle, routes_with_source};
use crate::isolation_egress::{ResolvedRunTurn, RunMetadata, RunTurnIdentity};

const GMAIL_READ: &str = "https://www.googleapis.com/auth/gmail.readonly";

fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(Text::new(owner.into()).unwrap()) }
fn shared(tenant: &str) -> EmployeeBinding { EmployeeBinding::Shared(Text::new(tenant.into()).unwrap()) }
fn human(user: &str) -> Requester {
    Requester::Human { user_id: user.into(), identity: RequesterIdentity::DashboardUser { user_id: user.into() } }
}
fn system() -> Requester {
    Requester::System(RequesterIdentity::System { kind: "isolation_background".into(), on_behalf_of: None })
}
fn account(user: &str) -> SubjectAccount { SubjectAccount { user_id: user.into(), active: true } }
fn delegate(subject: &str, mailboxes: &[&str]) -> TenantSource {
    TenantSource::fixture(TenantSourceKind::Delegate, subject, mailboxes, &[GMAIL_READ])
}
fn service_account(subject: &str, mailboxes: &[&str]) -> TenantSource {
    TenantSource::fixture(TenantSourceKind::ServiceAccount, subject, mailboxes, &[GMAIL_READ])
}
/// A Shared("company") employee whose catalog holds one SharedTenant route.
fn company_routes(kind: TenantSourceKind) -> (Arc<ConnectorCatalog>, GoogleAccountRoutes, String) {
    let catalog = catalog();
    let account = catalog.register("tenant", "google", ConnectorKind::SharedTenant, &shared("company")).unwrap();
    let routes = routes_with_source(&catalog, &lifecycle(shared("company")), Some(kind)).unwrap();
    (catalog, routes, account.as_str().to_owned())
}

#[test]
fn e4_delegate_impersonates_requesting_user_only() {
    let (_, routes, account) = company_routes(TenantSourceKind::Delegate);
    let company = shared("company");
    // agent.toml: identity = "delegate:alice@customer.com"; users.db: alice-id.
    let source = delegate("alice@customer.com", &[]);
    let alice = [account_of("alice-id")];
    let route = GoogleIsolationBridge::tenant_route(&routes, &company, &human("alice-id"), &source, &alice, "gmail_read")
        .expect("the requesting human is the delegate subject");
    assert_eq!(route.product.account(), account);
    assert_eq!(route.kind(), ConnectorKind::SharedTenant);
    // Any other requester (including background work) is refused: the
    // delegate subject is a person and only that person may act as it.
    for requester in [human("bob-id"), human("ALICE-ID"), system()] {
        assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &company, &requester, &source, &alice, "gmail_read").err(),
            Some(VerificationError::BindingMismatch));
    }
    // The person must be exactly one Active users.db account.
    let inactive = [SubjectAccount { user_id: "alice-id".into(), active: false }];
    let ambiguous = [account_of("alice-id"), account_of("alice-2")];
    let nobody: [SubjectAccount; 0] = [];
    for accounts in [&inactive[..], &ambiguous[..], &nobody[..]] {
        assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &company, &human("alice-id"), &source, accounts,
            "gmail_read").err(), Some(VerificationError::BindingMismatch));
    }
    // Scopes stay limited to the configured set (gmail.readonly ⇒ no drafts).
    assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &company, &human("alice-id"), &source, &alice,
        "gmail_create_draft").err(), Some(VerificationError::BindingMismatch));
    // A designated service mailbox is a Shared action for any requester.
    let mailbox = delegate("support@customer.com", &["support@customer.com"]);
    for requester in [human("bob-id"), system()] {
        assert!(GoogleIsolationBridge::tenant_route(&routes, &company, &requester, &mailbox, &[], "gmail_read").is_ok());
    }
    // A person's mailbox listed as a service mailbox is refused outright.
    let disguised = delegate("alice@customer.com", &["alice@customer.com"]);
    assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &company, &human("bob-id"), &disguised, &alice,
        "gmail_read").err(), Some(VerificationError::BindingMismatch));
}
fn account_of(user: &str) -> SubjectAccount { account(user) }

#[test]
fn e4_delegate_subject_from_args_refused() {
    // Arguments naming a Google identity are refused before any source read.
    for key in IDENTITY_SELECTOR_ARGUMENTS {
        let mut arguments = serde_json::json!({"message_id": "m-1"});
        arguments[key] = serde_json::json!("ceo@customer.com");
        assert_eq!(GoogleIsolationBridge::refuse_identity_arguments(&arguments),
            Err(VerificationError::BindingMismatch), "{key}");
    }
    // A mail subject line is data, not an identity selector.
    assert_eq!(GoogleIsolationBridge::refuse_identity_arguments(
        &serde_json::json!({"to": "a@customer.com", "subject": "hi", "body": "x"})), Ok(()));
    assert_eq!(GoogleIsolationBridge::refuse_identity_arguments(&serde_json::json!("x")),
        Err(VerificationError::Invalid));
    // The policy entry point used by selection and every recheck refuses too,
    // ahead of liveness, requester and source checks.
    let home = tempfile::tempdir().unwrap();
    let bridge = bridge(home.path());
    let context = context(Principal::Human { user_id: Text::new("bob-id".into()).unwrap(), generation: 1 });
    assert_eq!(LiveConnectorSelectionPolicy::candidates_for_call(&bridge, &context, "gmail_read", "google",
        &serde_json::json!({"message_id": "m-1", "impersonate": "alice@customer.com"})).err(),
        Some(VerificationError::BindingMismatch));
    // The subject decision itself takes no employee input: naming the
    // configured subject in arguments does not make bob that person.
    let (_, routes, _) = company_routes(TenantSourceKind::Delegate);
    assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &shared("company"), &human("bob-id"),
        &delegate("alice@customer.com", &[]), &[account_of("alice-id")], "gmail_read").err(),
        Some(VerificationError::BindingMismatch));
}

#[test]
fn e4_service_account_shared_only() {
    let (_, routes, account) = company_routes(TenantSourceKind::ServiceAccount);
    let company = shared("company");
    let source = service_account("support@customer.com", &["support@customer.com"]);
    for requester in [human("bob-id"), system()] {
        let route = GoogleIsolationBridge::tenant_route(&routes, &company, &requester, &source, &[], "gmail_read").unwrap();
        assert_eq!(route.product.account(), account);
    }
    // Metadata status is served; tools outside the configured scopes are not.
    assert!(GoogleIsolationBridge::tenant_route(&routes, &company, &system(), &source, &[], "google_status").is_ok());
    assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &company, &system(), &source, &[], "drive_read").err(),
        Some(VerificationError::BindingMismatch));
    // No impersonation: a service account never acts as a person, not even
    // for that person, and never as an undesignated mailbox.
    let person = service_account("alice@customer.com", &[]);
    assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &company, &human("alice-id"), &person,
        &[account_of("alice-id")], "gmail_read").err(), Some(VerificationError::BindingMismatch));
    let undesignated = service_account("support@customer.com", &[]);
    assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &company, &system(), &undesignated, &[], "gmail_read").err(),
        Some(VerificationError::BindingMismatch));
    // Personal employees never reach a tenant source.
    let catalog = catalog();
    catalog.register("alice", "google", ConnectorKind::Personal, &personal("alice-id")).unwrap();
    let alice = routes_with_source(&catalog, &lifecycle(personal("alice-id")), Some(TenantSourceKind::ServiceAccount)).unwrap();
    assert!(alice.tenant.is_none());
    for routes in [&alice, &routes] {
        assert_eq!(GoogleIsolationBridge::tenant_route(routes, &personal("alice-id"), &human("alice-id"), &source, &[],
            "gmail_read").err(), Some(VerificationError::BindingMismatch));
    }
}

#[test]
fn e4_tenant_mismatch_refused() {
    let (catalog, routes, _) = company_routes(TenantSourceKind::ServiceAccount);
    let source = service_account("support@customer.com", &["support@customer.com"]);
    // The capability names another tenant than the route's pinned tenant.
    assert_eq!(GoogleIsolationBridge::tenant_route(&routes, &shared("other"), &system(), &source, &[], "gmail_read").err(),
        Some(VerificationError::BindingMismatch));
    // Another tenant's route never becomes this employee's tenant route.
    catalog.register("other", "google", ConnectorKind::SharedTenant, &shared("other")).unwrap();
    let other = routes_with_source(&catalog, &lifecycle(shared("other")), Some(TenantSourceKind::ServiceAccount)).unwrap();
    assert_eq!(GoogleIsolationBridge::tenant_route(&other, &shared("company"), &system(), &source, &[], "gmail_read").err(),
        Some(VerificationError::BindingMismatch));
    // No tenant route at all is setup-required (Unavailable at this boundary).
    let none = GoogleAccountRoutes { personal: Default::default(), home: routes.home.clone(), tenant: None };
    assert_eq!(GoogleIsolationBridge::tenant_route(&none, &shared("company"), &system(), &source, &[], "gmail_read").err(),
        Some(VerificationError::Unavailable));
}

#[test]
fn e4_unreadable_key_unavailable_at_bridge() {
    // A configured delegate whose sealed key is damaged refuses before any
    // catalog, lifecycle or credential read, as Unavailable.
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"), "[google]\nidentity = \"delegate:alice@customer.com\"\n").unwrap();
    std::fs::write(home.path().join("config.toml"),
        "[integrations.google_identity]\nallowed_subject_domains = [\"customer.com\"]\n").unwrap();
    std::fs::create_dir_all(home.path().join("protected")).unwrap();
    std::fs::write(super::super::delegation_key::path(home.path()), "damaged").unwrap();
    let bridge = bridge(home.path());
    let context = context(Principal::Human { user_id: Text::new("alice-id".into()).unwrap(), generation: 1 });
    assert_eq!(bridge.source(&context, "gmail_read").err(), Some(VerificationError::Unavailable));
}

struct Turns;
impl RunTurnResolver for Turns {
    fn resolve_run(&self, _: &RequestContext) -> Result<ResolvedRunTurn, VerificationError> {
        Ok(ResolvedRunTurn { identity: RunTurnIdentity { turn: "host-turn".into(), approval_session: Some("session".into()) },
            egress_turn: None, metadata: Some(RunMetadata { ingress: Some(IngressRequester::Dashboard), ..RunMetadata::root() }),
            principal_context: None, utility_runner: None })
    }
    fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
        panic!("the bridge must use the combined host snapshot")
    }
}
fn bridge(home: &std::path::Path) -> GoogleIsolationBridge {
    GoogleIsolationBridge::new(home.into(), "bot".into(), EmployeeId([1; 16]), None,
        CredentialRef { kind: CredentialKind::Legacy, sha256: "a".repeat(64) }, catalog(), Arc::new(Turns)).unwrap()
}
fn context(principal: Principal) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([1; 16]), run: RunId([3; 16]),
        task: None, principal, employee_binding: shared("company"),
        lane: Lane::Standard, tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [5; 32], jti: [6; 16],
    })
}
