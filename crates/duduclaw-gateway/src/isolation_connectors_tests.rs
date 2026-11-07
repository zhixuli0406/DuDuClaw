use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
use serde_json::json;
struct FixtureAuthority;
impl duduclaw_isolation::capability::LiveRequestVerifier for FixtureAuthority {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
fn text<const N: usize>(s: &str) -> Text<N> { Text::new(s.into()).unwrap() }
fn context(binding: EmployeeBinding, principal: Principal, accounts: &[&str]) -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal, employee_binding: binding.clone(), lane: Lane::Standard,
        tools: List::new(vec![text("gmail_search")]).unwrap(),
        resources: List::new(accounts.iter().map(|account| ResourceGrant::Connector {
            account: text(account), binding: binding.clone() }).collect()).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(FixtureAuthority))
}
fn select(context: &RequestContext, tool: &str, args: &Value,
    bindings: Arc<Mutex<BindingStore>>, catalog: Arc<ConnectorCatalog>)
    -> Result<SelectedConnector, VerificationError> {
    let candidates = context.record().io_resources().filter_map(|grant| match grant {
        ResourceGrant::Connector { account, binding: EmployeeBinding::Personal(owner) } =>
            Some(ProductConnectorAccount::Personal { account: account.as_str().into(), owner: owner.as_str().into() }),
        ResourceGrant::Connector { account, binding: EmployeeBinding::Shared(tenant) } =>
            Some(ProductConnectorAccount::SharedHome { account: account.as_str().into(), tenant: tenant.as_str().into() }),
        _ => None,
    }).collect();
    SelectedConnector::select_with_policy(context, tool, args, bindings, catalog,
        Some(Arc::new(ProductPolicy(Mutex::new(candidates)))))
}
// Fixture ids use the production id format (32 lowercase hex) so the catalog
// and the BindingStore double can share them. Production ids come only from
// `ConnectorCatalog::register`.
const ALICE_GOOGLE: &str = concat!("a11ce000", "00000000", "00000000", "00000001");
const ALICE_GOOGLE_2: &str = concat!("a11ce000", "00000000", "00000000", "00000002");
const ALICE_GITHUB: &str = concat!("a11ce000", "00000000", "00000000", "00000003");
const BOB_GOOGLE: &str = concat!("b0b00000", "00000000", "00000000", "00000001");
const COMPANY_HOME: &str = concat!("c0000000", "00000000", "00000000", "00000001");
fn alice() -> EmployeeBinding { EmployeeBinding::Personal(text("alice")) }
fn company() -> EmployeeBinding { EmployeeBinding::Shared(text("company")) }
fn id(value: &str) -> AccountId { AccountId::parse(value).unwrap() }
fn fixture() -> (Arc<Mutex<BindingStore>>, Arc<ConnectorCatalog>, RequestContext) {
    let binding = alice();
    let principal = Principal::Human { user_id: text("alice"), generation: 1 };
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(EmployeeId([2; 16]), &binding).unwrap();
    for account in [ALICE_GOOGLE, ALICE_GOOGLE_2, ALICE_GITHUB] { bindings.register_connector(account, &binding).unwrap(); }
    let catalog = Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap());
    catalog.register_fixture(ALICE_GOOGLE, "google", ConnectorKind::Personal, &binding).unwrap();
    catalog.register_fixture(ALICE_GOOGLE_2, "google", ConnectorKind::Personal, &binding).unwrap();
    catalog.register_fixture(ALICE_GITHUB, "github", ConnectorKind::Personal, &binding).unwrap();
    (Arc::new(Mutex::new(bindings)), catalog, context(binding, principal, &[ALICE_GOOGLE]))
}
#[test]
fn isolated_connector_rechecks_revocation_and_principal_generation() {
    let (bindings, catalog, ctx) = fixture();
    let selected = select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog.clone()).unwrap();
    assert_eq!(selected.account(), ALICE_GOOGLE);
    bindings.lock().unwrap().revoke_connector(ALICE_GOOGLE).unwrap();
    assert!(selected.recheck().is_err());
    let (bindings, catalog, ctx) = fixture();
    let selected = select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog).unwrap();
    bindings.lock().unwrap().advance_principal(&Principal::Human { user_id: text("alice"), generation: 2 }).unwrap();
    assert!(selected.recheck().is_err());
}
#[test]
fn isolated_connector_refuses_ambiguity_wrong_provider_and_credential_override() {
    let (bindings, catalog, ctx) = fixture();
    for (tool, args) in [("github_status", json!({})), ("gmail_search", json!({"connector_account":"home"})),
        ("gmail_search", json!({"api_key":"not-authority"}))] {
        assert!(select(&ctx, tool, &args, bindings.clone(), catalog.clone()).is_err());
    }
    let ctx = context(ctx.record().employee_binding.clone(), ctx.record().principal.clone(), &[ALICE_GOOGLE, ALICE_GOOGLE_2]);
    assert!(select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog.clone()).is_err());
    assert!(select(&ctx, "gmail_search", &json!({"connector_account":ALICE_GOOGLE_2}), bindings, catalog).is_ok());
}
#[test]
fn isolated_connector_shared_employee_cannot_select_personal_account() {
    let (bindings, catalog, _) = fixture();
    let principal = Principal::Background { tenant_id: text("company"), generation: 1 };
    let binding = company();
    let ctx = context(binding.clone(), principal.clone(), &[ALICE_GOOGLE]);
    // A forged Shared principal cannot replace the stored Personal employee.
    assert!(select(&ctx, "gmail_search", &json!({}), bindings, catalog.clone()).is_err());
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(EmployeeId([2; 16]), &binding).unwrap();
    bindings.register_connector(ALICE_GOOGLE, &alice()).unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    assert!(select(&ctx, "gmail_search", &json!({}), bindings, catalog).is_err());
}
#[test]
fn isolated_connector_route_is_immutable_and_live() {
    let (bindings, catalog, ctx) = fixture();
    let selected = select(&ctx, "gmail_search", &json!({}), bindings, catalog.clone()).unwrap();
    assert!(catalog.register_fixture(ALICE_GOOGLE, "github", ConnectorKind::Personal, &alice()).is_err());
    catalog.revoke(&id(ALICE_GOOGLE)).unwrap();
    assert!(selected.recheck().is_err());
    assert!(catalog.register_fixture(ALICE_GOOGLE, "google", ConnectorKind::Personal, &alice()).is_err());
}

#[test]
fn connector_recheck_uses_live_run_authority_before_each_effect() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct RunAuthority(AtomicBool);
    impl duduclaw_isolation::capability::LiveRequestVerifier for RunAuthority {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
            if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
        }
    }
    let (bindings, catalog, context) = fixture();
    let authority = Arc::new(RunAuthority(AtomicBool::new(true)));
    let context = RequestContext::with_live_verifier(context.record().clone(), authority.clone());
    let selected = select(&context, "gmail_search", &json!({}), bindings, catalog).unwrap();
    selected.recheck().unwrap();
    authority.0.store(false, Ordering::SeqCst);
    let mut effects = 0;
    let mut send = || -> Result<(), VerificationError> {
        selected.recheck()?;
        effects += 1;
        Ok(())
    };
    assert_eq!(send(), Err(VerificationError::Revoked));
    assert_eq!(effects, 0);
}

#[test]
fn connector_cannot_use_record_only_authority() {
    let (bindings, catalog, context) = fixture();
    let detached = RequestContext::from_verified_record(context.record().clone());
    assert!(matches!(select(&detached, "gmail_search", &json!({}), bindings, catalog),
        Err(VerificationError::Unavailable)));
}

struct ProductPolicy(Mutex<Vec<ProductConnectorAccount>>);
impl LiveConnectorSelectionPolicy for ProductPolicy {
    fn supports(&self, tool: &str) -> bool { tool == "gmail_search" }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Ok(self.0.lock().unwrap().clone())
    }
}
fn employee_scope(context: &RequestContext) -> RequestContext {
    let mut record = context.record().clone();
    record.resources = List::new(vec![ResourceGrant::EmployeeScope { id: record.employee }]).unwrap();
    RequestContext::with_live_verifier(record, Arc::new(FixtureAuthority))
}
#[test]
fn employee_scope_requires_product_selection_and_rechecks_live_revocation() {
    let (bindings, catalog, context) = fixture();
    let context = employee_scope(&context);
    assert!(matches!(SelectedConnector::select_with_policy(&context, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), None), Err(VerificationError::Unavailable)));
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![ProductConnectorAccount::Personal {
        account: ALICE_GOOGLE.into(), owner: "alice".into() }])));
    let selected = SelectedConnector::select_with_policy(&context, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).unwrap();
    selected.recheck().unwrap();
    policy.0.lock().unwrap().clear();
    assert_eq!(selected.recheck(), Err(VerificationError::BindingMismatch));
    assert!(SelectedConnector::select_with_policy(&context, "gmail_search", &json!({}),
        bindings, catalog, Some(policy)).is_err());
}
#[test]
fn employee_scope_personal_cannot_select_another_owners_account() {
    let (bindings, catalog, context) = fixture();
    let bob = EmployeeBinding::Personal(text("bob"));
    bindings.lock().unwrap().register_connector(BOB_GOOGLE, &bob).unwrap();
    catalog.register_fixture(BOB_GOOGLE, "google", ConnectorKind::Personal, &bob).unwrap();
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![ProductConnectorAccount::Personal {
        account: BOB_GOOGLE.into(), owner: "bob".into() }])));
    assert!(SelectedConnector::select_with_policy(&employee_scope(&context), "gmail_search", &json!({}),
        bindings, catalog, Some(policy)).is_err());
}
#[test]
fn product_policy_explicit_shared_home_requires_live_opt_in_and_correct_binding() {
    let (bindings, catalog, context) = fixture();
    let shared = company();
    bindings.lock().unwrap().register_connector(COMPANY_HOME, &shared).unwrap();
    catalog.register_fixture(COMPANY_HOME, "google", ConnectorKind::SharedHome, &shared).unwrap();
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![ProductConnectorAccount::SharedHome {
        account: COMPANY_HOME.into(), tenant: "company".into() }])));
    let scope = employee_scope(&context);
    let selected = SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).unwrap();
    assert_eq!(selected.binding(), &shared);
    assert_eq!(selected.kind(), ConnectorKind::SharedHome);
    let mut explicit = context.record().clone();
    explicit.resources = List::new(vec![ResourceGrant::Connector { account: text(COMPANY_HOME), binding: shared }]).unwrap();
    let explicit = RequestContext::with_live_verifier(explicit, Arc::new(FixtureAuthority));
    assert!(SelectedConnector::select_with_policy(&explicit, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).is_ok());
    // A narrow personal grant cannot silently switch to the allowed home account.
    assert!(SelectedConnector::select_with_policy(&context, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).is_err());
    policy.0.lock().unwrap().clear();
    assert!(selected.recheck().is_err());
    assert!(SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({}),
        bindings, catalog, Some(policy)).is_err());
}
#[test]
fn product_policy_refuses_multiple_accounts_and_explicit_account_cannot_expand_candidates() {
    let (bindings, catalog, context) = fixture();
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![
        ProductConnectorAccount::Personal { account: ALICE_GOOGLE.into(), owner: "alice".into() },
        ProductConnectorAccount::Personal { account: ALICE_GOOGLE_2.into(), owner: "alice".into() },
    ])));
    let scope = employee_scope(&context);
    assert!(SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).is_err());
    assert!(SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({"connector_account":ALICE_GOOGLE_2}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).is_ok());
    assert!(SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({"connector_account":"home"}),
        bindings, catalog, Some(policy)).is_err());
}

#[test]
fn every_connector_policy_has_an_exact_catalog_provider_mapping() {
    use duduclaw_mcp_boundary::tool_policy::{POLICIES, Route};
    let catalog = ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap();
    let mut count = 0;
    for policy in POLICIES.iter().filter(|policy| policy.route == Route::Connector) {
        let provider = provider_for_tool(policy.name)
            .unwrap_or_else(|| panic!("missing connector provider for {}", policy.name));
        let account = catalog.register(policy.name, provider, ConnectorKind::Personal, &alice()).unwrap();
        let route = catalog.route(&account).unwrap().unwrap();
        assert!(route.active && route.provider == provider && route.label == policy.name);
        count += 1;
    }
    assert!(count > 50, "connector inventory unexpectedly empty");
    assert_eq!(provider_for_tool("computer_login"), Some("computer"));
    assert!(provider_for_tool("unknown_tool").is_none());
}

#[test]
fn explicit_account_grants_cannot_replace_live_product_selection_policy() {
    let (bindings, catalog, context) = fixture();
    assert!(matches!(SelectedConnector::select_with_policy(&context, "gmail_search", &json!({}),
        bindings, catalog, None), Err(VerificationError::Unavailable)));
}

#[test]
fn revocation_during_product_policy_read_is_fenced_before_io() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Authority(Arc<AtomicBool>);
    impl duduclaw_isolation::capability::LiveRequestVerifier for Authority {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
            if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
        }
    }
    struct RevokingPolicy { active: Arc<AtomicBool>, calls: AtomicUsize }
    impl LiveConnectorSelectionPolicy for RevokingPolicy {
        fn supports(&self, _: &str) -> bool { true }
        fn candidates(&self, _: &RequestContext, _: &str, _: &str)
            -> Result<Vec<ProductConnectorAccount>, VerificationError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) > 0 { self.active.store(false, Ordering::SeqCst); }
            Ok(vec![ProductConnectorAccount::Personal { account: ALICE_GOOGLE.into(), owner: "alice".into() }])
        }
    }
    let (bindings, catalog, context) = fixture();
    let active = Arc::new(AtomicBool::new(true));
    let context = RequestContext::with_live_verifier(context.record().clone(), Arc::new(Authority(active.clone())));
    let policy = Arc::new(RevokingPolicy { active, calls: AtomicUsize::new(0) });
    assert!(matches!(SelectedConnector::select_with_policy(&context, "gmail_search", &json!({}),
        bindings, catalog, Some(policy)), Err(VerificationError::Revoked)));
}

struct GooglePolicy;
impl LiveConnectorSelectionPolicy for GooglePolicy {
    fn supports(&self, tool: &str) -> bool { provider_for_tool(tool) == Some("google") }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Ok(vec![ProductConnectorAccount::Personal { account: ALICE_GOOGLE.into(), owner: "alice".into() }])
    }
}
struct CallAccount { account: &'static str, binding: EmployeeBinding }
impl duduclaw_mcp_boundary::tool_policy::ResourceResolver for CallAccount {
    fn resolve(&self, context: &RequestContext, _: &duduclaw_mcp_boundary::tool_policy::ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Connector { account: text(self.account), binding: self.binding.clone() }])
    }
}
fn operation_context(context: &RequestContext) -> RequestContext {
    let mut record = employee_scope(context).record().clone();
    record.tools = List::new(vec![text("google_status"), text("gmail_search"), text("gmail_create_draft")]).unwrap();
    RequestContext::with_live_verifier(record, Arc::new(FixtureAuthority))
}
fn operation_call(context: &RequestContext, tool: &str, arguments: &Value) -> AuthorizedToolCall {
    duduclaw_mcp_boundary::tool_policy::authorize(context, tool, arguments,
        &CallAccount { account: ALICE_GOOGLE, binding: alice() }).unwrap()
}

#[test]
fn selected_connector_is_bound_to_exact_tool_and_arguments_not_just_provider() {
    let (bindings, catalog, context) = fixture();
    let context = operation_context(&context);
    let selected = SelectedConnector::select_with_policy(&context, "google_status", &json!({}),
        bindings.clone(), catalog.clone(), Some(Arc::new(GooglePolicy))).unwrap();
    assert_eq!(selected.tool(), "google_status");
    selected.verify_call(&operation_call(&context, "google_status", &json!({}))).unwrap();
    // Identical record/provider/account/arguments do not bind a different tool.
    assert_eq!(selected.verify_call(&operation_call(&context, "gmail_search", &json!({}))),
        Err(VerificationError::BindingMismatch));
    let draft = json!({"to":"alice@example.com", "subject":"subject", "body":"original"});
    assert_eq!(selected.verify_call(&operation_call(&context, "gmail_create_draft", &draft)),
        Err(VerificationError::BindingMismatch));
    let selected = SelectedConnector::select_with_policy(&context, "gmail_create_draft", &draft,
        bindings, catalog, Some(Arc::new(GooglePolicy))).unwrap();
    selected.verify_call(&operation_call(&context, "gmail_create_draft", &draft)).unwrap();
    for (key, replacement) in [("to", "other@example.com"), ("subject", "changed"), ("body", "changed")] {
        let mut changed = draft.clone();
        changed[key] = json!(replacement);
        assert_eq!(selected.verify_call(&operation_call(&context, "gmail_create_draft", &changed)),
            Err(VerificationError::BindingMismatch), "{key}");
    }
}

#[test]
fn selected_connector_call_rechecks_account_binding_record_and_revocation() {
    let (bindings, catalog, context) = fixture();
    let context = operation_context(&context);
    let selected = SelectedConnector::select_with_policy(&context, "google_status", &json!({}),
        bindings, catalog.clone(), Some(Arc::new(GooglePolicy))).unwrap();
    for resolver in [CallAccount { account: ALICE_GOOGLE_2, binding: alice() },
        CallAccount { account: ALICE_GOOGLE, binding: company() }] {
        let call = duduclaw_mcp_boundary::tool_policy::authorize(&context, "google_status", &json!({}), &resolver).unwrap();
        assert_eq!(selected.verify_call(&call), Err(VerificationError::BindingMismatch));
    }
    let mut other_record = context.record().clone();
    other_record.policy_revision += 1;
    let other_context = RequestContext::with_live_verifier(other_record, Arc::new(FixtureAuthority));
    assert_eq!(selected.verify_call(&operation_call(&other_context, "google_status", &json!({}))),
        Err(VerificationError::BindingMismatch));
    let call = operation_call(&context, "google_status", &json!({}));
    catalog.revoke(&id(ALICE_GOOGLE)).unwrap();
    assert!(selected.verify_call(&call).is_err());
}

#[test]
fn database_directory_anchor_skips_foreign_and_revoked_accounts_after_full_checks() {
    struct Directory(Vec<ProductConnectorAccount>);
    impl LiveConnectorSelectionPolicy for Directory {
        fn supports(&self,tool:&str)->bool{tool=="db_sources"}
        fn candidates(&self,_:&RequestContext,_:&str,_:&str)->Result<Vec<ProductConnectorAccount>,VerificationError>{Ok(self.0.clone())}
    }
    let (bindings,catalog,base)=fixture();
    let mut record=base.record().clone();
    record.tools=List::new(vec![text("db_sources")]).unwrap();
    record.resources=List::new(vec![ResourceGrant::EmployeeScope{id:record.employee}]).unwrap();
    let ctx=RequestContext::with_live_verifier(record,Arc::new(FixtureAuthority));
    // Id order matters: foreign < revoked < valid, so the anchor must skip two.
    const FOREIGN: &str = concat!("0f000000", "00000000", "00000000", "00000001");
    const REVOKED: &str = concat!("a0000000", "00000000", "00000000", "00000001");
    const VALID: &str = concat!("b0000000", "00000000", "00000000", "00000001");
    let alice=ctx.record().employee_binding.clone();
    for account in [REVOKED,VALID] {
        bindings.lock().unwrap().register_connector(account,&alice).unwrap();
        catalog.register_fixture(account,"database",ConnectorKind::Personal,&alice).unwrap();
    }
    bindings.lock().unwrap().revoke_connector(REVOKED).unwrap();
    catalog.register_fixture(FOREIGN,"database",ConnectorKind::Personal,&EmployeeBinding::Personal(text("bob"))).unwrap();
    let policy=Arc::new(Directory(vec![
        ProductConnectorAccount::Personal{account:FOREIGN.into(),owner:"bob".into()},
        ProductConnectorAccount::Personal{account:REVOKED.into(),owner:"alice".into()},
        ProductConnectorAccount::Personal{account:VALID.into(),owner:"alice".into()},
    ]));
    let selected=SelectedConnector::select_with_policy(&ctx,"db_sources",&json!({}),bindings,catalog,Some(policy)).unwrap();
    assert_eq!(selected.account(),VALID);
    selected.recheck().unwrap();
    assert!(selected.collection_member(REVOKED).is_err());
    assert!(selected.collection_member(FOREIGN).is_err());
}

#[tokio::test]
async fn sealed_owner_binding_checkpoint_does_not_reenter_product_selection_but_revokes_live() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct SourceOwner { allowed: AtomicBool, reads: AtomicUsize }
    impl LiveConnectorSelectionPolicy for SourceOwner {
        fn supports(&self, tool: &str) -> bool { tool == "gmail_search" }
        fn candidates(&self, _: &RequestContext, _: &str, _: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if !self.allowed.load(Ordering::SeqCst) { return Err(VerificationError::Unavailable); }
            Ok(vec![ProductConnectorAccount::Personal { account:ALICE_GOOGLE.into(), owner:"alice".into() }])
        }
    }
    let (bindings, catalog, context) = fixture();
    let owner = Arc::new(SourceOwner { allowed: AtomicBool::new(true), reads: AtomicUsize::new(0) });
    let selected = Arc::new(SelectedConnector::select_with_policy(&context, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(owner.clone())).unwrap());
    let reads = owner.reads.load(Ordering::SeqCst);
    // A real sealed owner holds its source lock here. The product selector is
    // intentionally unavailable to model a non-reentrant source mutex.
    owner.allowed.store(false, Ordering::SeqCst);
    duduclaw_mcp_boundary::io_authority::scope(selected.binding_io_context(), async {
        duduclaw_mcp_boundary::io_authority::verify_before_io().unwrap();
        assert_eq!(owner.reads.load(Ordering::SeqCst), reads);
        bindings.lock().unwrap().revoke_connector(ALICE_GOOGLE).unwrap();
        assert!(duduclaw_mcp_boundary::io_authority::verify_before_io().is_err());
        assert_eq!(owner.reads.load(Ordering::SeqCst), reads);
    }).await;
    assert!(selected.recheck().is_err(), "entry/exit must still require the full product check");
}

// ---- C1: production catalog -------------------------------------------------

fn memory_catalog() -> ConnectorCatalog { ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap() }
/// Select `account` for alice's Personal employee through the live product path.
fn select_account(account: &str, catalog: Arc<ConnectorCatalog>) -> Result<SelectedConnector, VerificationError> {
    let principal = Principal::Human { user_id: text("alice"), generation: 1 };
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(EmployeeId([2; 16]), &alice()).unwrap();
    bindings.register_connector(account, &alice()).unwrap();
    let scope = employee_scope(&context(alice(), principal, &[]));
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![ProductConnectorAccount::Personal {
        account: account.into(), owner: "alice".into() }])));
    SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({}),
        Arc::new(Mutex::new(bindings)), catalog, Some(policy))
}

#[test]
fn catalog_same_name_replacement_new_id_old_revoked() {
    let catalog = Arc::new(memory_catalog());
    let first = catalog.register("alice@example.com", "google", ConnectorKind::Personal, &alice()).unwrap();
    assert!(AccountId::parse(first.as_str()).is_ok());
    assert_ne!(first.as_str(), "alice@example.com", "the hint is never the id");
    let selected = select_account(first.as_str(), catalog.clone()).unwrap();
    catalog.revoke(&first).unwrap();
    assert!(selected.recheck().is_err(), "revoked route refuses pinned selection");
    // Relinking the same-named account mints a new id; the old one stays revoked.
    let second = catalog.register("alice@example.com", "google", ConnectorKind::Personal, &alice()).unwrap();
    assert_ne!(first, second);
    let old = catalog.route(&first).unwrap().unwrap();
    let new = catalog.route(&second).unwrap().unwrap();
    assert!(!old.active && new.active);
    assert_eq!((old.label.as_str(), new.label.as_str()), ("alice@example.com", "alice@example.com"));
    assert!(new.generation > old.generation);
    assert_eq!(catalog.active_routes_for(&alice()).unwrap(), vec![new]);
    assert!(catalog.active_routes_for(&company()).unwrap().is_empty());
    // The old id can never come back or be reused, and revoke stays final.
    catalog.revoke(&first).unwrap();
    assert_eq!(catalog.advance(&first), Err(CatalogError::Revoked));
    assert!(catalog.register_fixture(first.as_str(), "google", ConnectorKind::Personal, &alice()).is_err());
    assert!(select_account(first.as_str(), catalog.clone()).is_err());
    assert!(select_account(second.as_str(), catalog.clone()).is_ok());
    // Unknown ids and non-id strings are refused, not guessed.
    assert_eq!(catalog.revoke(&id(BOB_GOOGLE)), Err(CatalogError::NotFound));
    assert!(catalog.route(&id(BOB_GOOGLE)).unwrap().is_none());
    let upper = ALICE_GOOGLE.to_uppercase();
    let long = format!("{ALICE_GOOGLE}0");
    for bad in ["", "alice@example.com", upper.as_str(), &ALICE_GOOGLE[..31], long.as_str()] {
        assert_eq!(AccountId::parse(bad), Err(CatalogError::Invalid), "{bad:?}");
    }
    assert!(select_account("alice@example.com", catalog).is_err());
}

#[test]
fn catalog_binding_kind_immutable() {
    let catalog = memory_catalog();
    // Kind must agree with the binding; provider and label are validated.
    for (kind, binding) in [(ConnectorKind::Personal, company()), (ConnectorKind::SharedTenant, alice()),
        (ConnectorKind::SharedHome, alice())] {
        assert_eq!(catalog.register("label", "google", kind, &binding), Err(CatalogError::Invalid));
    }
    for (label, provider) in [("label", "dropbox"), ("", "google"), ("  ", "google"), ("a\nb", "google")] {
        assert_eq!(catalog.register(label, provider, ConnectorKind::Personal, &alice()), Err(CatalogError::Invalid));
    }
    assert_eq!(catalog.register("x", "google", ConnectorKind::Personal, &EmployeeBinding::Personal(text(""))),
        Err(CatalogError::Invalid));
    let account = catalog.register("label", "google", ConnectorKind::Personal, &alice()).unwrap();
    let home = catalog.register("home", "google", ConnectorKind::SharedHome, &company()).unwrap();
    let before = catalog.route(&account).unwrap().unwrap();
    assert_eq!((before.kind, &before.binding, before.provider), (ConnectorKind::Personal, &alice(), "google"));
    assert_eq!(binding_id(&alice()).unwrap(), "personal:alice");
    assert_eq!(binding_id(&company()).unwrap(), "shared:company");
    {
        let connection = catalog.connection.lock().unwrap();
        let a = account.as_str();
        for sql in [
            format!("UPDATE isolation_connector_routes SET provider='github', generation=generation+100 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET kind='shared_tenant', binding_id='shared:company', generation=generation+100 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET binding_id='personal:bob', generation=generation+100 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET label='other', generation=generation+100 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET account='{BOB_GOOGLE}', generation=generation+100 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET created_at=created_at+1, generation=generation+100 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET generation=1 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET generation=generation WHERE account='{a}'"),
            format!("DELETE FROM isolation_connector_routes WHERE account='{a}'"),
            format!("INSERT OR REPLACE INTO isolation_connector_routes VALUES ('{a}','label','google','personal','personal:alice',999,1,1,NULL)"),
            format!("INSERT INTO isolation_connector_routes VALUES ('{BOB_GOOGLE}','x','google','personal','personal:bob',999,0,1,1)"),
            format!("INSERT INTO isolation_connector_routes VALUES ('{BOB_GOOGLE}','x','google','personal','personal:bob',1,1,1,NULL)"),
            format!("INSERT INTO isolation_connector_routes VALUES ('{BOB_GOOGLE}','x','google','shared_home','personal:bob',999,1,1,NULL)"),
            format!("INSERT INTO isolation_connector_routes VALUES ('alice-google','x','google','personal','personal:bob',999,1,1,NULL)"),
        ] {
            assert!(connection.execute_batch(&sql).is_err(), "{sql}");
        }
    }
    assert_eq!(catalog.route(&account).unwrap().unwrap(), before);
    catalog.revoke(&account).unwrap();
    {
        let connection = catalog.connection.lock().unwrap();
        let a = account.as_str();
        // active only 1→0, and a revoked row is final.
        for sql in [
            format!("UPDATE isolation_connector_routes SET active=1, revoked_at=NULL, generation=generation+100 WHERE account='{a}'"),
            format!("UPDATE isolation_connector_routes SET generation=generation+100 WHERE account='{a}'"),
        ] {
            assert!(connection.execute_batch(&sql).is_err(), "{sql}");
        }
    }
    let revoked = catalog.route(&account).unwrap().unwrap();
    assert!(!revoked.active && revoked.kind == ConnectorKind::Personal && revoked.binding == alice());
    assert_eq!(catalog.route(&home).unwrap().unwrap().kind, ConnectorKind::SharedHome);
}

#[test]
fn catalog_home_fallback_requires_shared_home_kind() {
    // A tenant account is not a home-wide product account: a Personal
    // employee may not fall back to it even if the product adapter offers it.
    let (bindings, catalog, context) = fixture();
    let tenant = concat!("c0000000", "00000000", "00000000", "00000002");
    bindings.lock().unwrap().register_connector(tenant, &company()).unwrap();
    catalog.register_fixture(tenant, "google", ConnectorKind::SharedTenant, &company()).unwrap();
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![ProductConnectorAccount::SharedHome {
        account: tenant.into(), tenant: "company".into() }])));
    assert!(SelectedConnector::select_with_policy(&employee_scope(&context), "gmail_search", &json!({}),
        bindings, catalog, Some(policy)).is_err());
}

#[test]
fn catalog_unavailable_refuses() {
    let catalog = Arc::new(memory_catalog());
    let account = catalog.register("alice", "google", ConnectorKind::Personal, &alice()).unwrap();
    let selected = select_account(account.as_str(), catalog.clone()).unwrap();
    selected.recheck().unwrap();
    catalog.connection.lock().unwrap().execute_batch("DROP TABLE isolation_connector_routes").unwrap();
    assert_eq!(selected.recheck(), Err(VerificationError::Unavailable));
    assert!(matches!(select_account(account.as_str(), catalog.clone()), Err(VerificationError::Unavailable)));
    assert_eq!(catalog.route(&account), Err(CatalogError::Unavailable));
    assert_eq!(catalog.active_routes_for(&alice()), Err(CatalogError::Unavailable));
    assert_eq!(catalog.revoke(&account), Err(CatalogError::Unavailable));
    assert_eq!(catalog.register("alice", "google", ConnectorKind::Personal, &alice()), Err(CatalogError::Unavailable));

    // A poisoned store lock refuses as well.
    let catalog = Arc::new(memory_catalog());
    let account = catalog.register("alice", "google", ConnectorKind::Personal, &alice()).unwrap();
    let selected = select_account(account.as_str(), catalog.clone()).unwrap();
    let poison = catalog.clone();
    let _ = std::thread::spawn(move || { let _guard = poison.connection.lock().unwrap(); panic!("poison"); }).join();
    assert_eq!(selected.recheck(), Err(VerificationError::Unavailable));
    assert_eq!(catalog.route(&account), Err(CatalogError::Unavailable));

    // An existing store with a different (older or weakened) schema refuses.
    let schemas: [String; 4] = [
        "CREATE TABLE isolation_connector_routes (account TEXT PRIMARY KEY NOT NULL, provider TEXT NOT NULL,
            active INTEGER NOT NULL CHECK(active IN (0,1))) STRICT".to_owned(),
        format!("{ROUTES_TABLE}; {ROUTES_INDEX}; {INSERT_TRIGGER}; {UPDATE_TRIGGER}"),
        format!("{ROUTES_TABLE}; {ROUTES_INDEX}; {INSERT_TRIGGER}; {UPDATE_TRIGGER}; {DELETE_TRIGGER};
            CREATE TRIGGER extra AFTER INSERT ON isolation_connector_routes BEGIN SELECT 1; END"),
        format!("{ROUTES_TABLE}; {ROUTES_INDEX}; {INSERT_TRIGGER}; {DELETE_TRIGGER};
            CREATE TRIGGER isolation_connector_update BEFORE UPDATE ON isolation_connector_routes
            WHEN 0 BEGIN SELECT RAISE(ABORT,'immutable account route'); END"),
    ];
    for sql in &schemas {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(sql).unwrap();
        assert!(matches!(ConnectorCatalog::new(connection), Err(CatalogError::Unavailable)), "{sql}");
    }
    // A store with exactly our schema reopens.
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&format!("{ROUTES_TABLE}; {ROUTES_INDEX}; {INSERT_TRIGGER}; {UPDATE_TRIGGER}; {DELETE_TRIGGER}")).unwrap();
    ConnectorCatalog::new(connection).unwrap();

    // Damaged rows (writable only by bypassing the catalog) refuse on read.
    let catalog = memory_catalog();
    let account = catalog.register("alice", "google", ConnectorKind::Personal, &alice()).unwrap();
    catalog.connection.lock().unwrap().execute_batch("PRAGMA ignore_check_constraints=ON;
        DROP TRIGGER isolation_connector_update;
        UPDATE isolation_connector_routes SET binding_id='personal:al\nice'").unwrap();
    assert_eq!(catalog.route(&account), Err(CatalogError::Unavailable));
}

#[test]
fn selection_refused_after_generation_change() {
    let catalog = Arc::new(memory_catalog());
    let account = catalog.register("alice", "google", ConnectorKind::Personal, &alice()).unwrap();
    let selected = select_account(account.as_str(), catalog.clone()).unwrap();
    let pinned = catalog.route(&account).unwrap().unwrap().generation;
    assert_eq!(selected.generation(), pinned);
    assert_eq!(selected.kind(), ConnectorKind::Personal);
    selected.recheck().unwrap();
    // A state change on an unrelated route does not disturb this selection.
    let other = catalog.register("other", "google", ConnectorKind::Personal, &alice()).unwrap();
    catalog.advance(&other).unwrap();
    selected.recheck().unwrap();
    let advanced = catalog.advance(&account).unwrap();
    assert!(advanced > pinned);
    assert_eq!(selected.recheck(), Err(VerificationError::BindingMismatch));
    // A fresh selection pins the new generation and then works.
    let fresh = select_account(account.as_str(), catalog.clone()).unwrap();
    assert_eq!(fresh.generation(), advanced);
    fresh.recheck().unwrap();
    catalog.revoke(&account).unwrap();
    assert_eq!(fresh.recheck(), Err(VerificationError::BindingMismatch));
}

#[cfg(unix)]
mod open_file {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    fn mode(path: &std::path::Path) -> u32 { std::fs::metadata(path).unwrap().permissions().mode() & 0o777 }

    #[test]
    fn catalog_open_creates_private_file_and_persists() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let catalog = ConnectorCatalog::open(home.path()).unwrap();
        let account = catalog.register("alice", "google", ConnectorKind::Personal, &alice()).unwrap();
        drop(catalog);
        for suffix in ["", "-wal", "-shm"] {
            let path = home.path().join(format!("{CATALOG_FILE}{suffix}"));
            if path.exists() { assert_eq!(mode(&path), 0o600, "{suffix}"); }
        }
        let reopened = ConnectorCatalog::open(home.path()).unwrap();
        assert!(reopened.route(&account).unwrap().unwrap().active);
    }

    #[test]
    fn catalog_open_refuses_symlink() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let outside = home.path().join("outside.db");
        std::fs::write(&outside, b"untouched").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&outside, home.path().join(CATALOG_FILE)).unwrap();
        assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
        // A linked sidecar refuses too.
        let home = crate::fs_safe::private_test_home().unwrap();
        drop(ConnectorCatalog::open(home.path()).unwrap());
        let _ = std::fs::remove_file(home.path().join(format!("{CATALOG_FILE}-wal")));
        symlink(&outside, home.path().join(format!("{CATALOG_FILE}-wal"))).unwrap();
        assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
        // A linked home refuses.
        let root = crate::fs_safe::private_test_home().unwrap();
        let real = crate::fs_safe::private_test_home().unwrap();
        let alias = root.path().join("home");
        symlink(real.path(), &alias).unwrap();
        assert!(matches!(ConnectorCatalog::open(&alias), Err(CatalogError::Unavailable)));
        assert!(!real.path().join(CATALOG_FILE).exists());
    }

    #[test]
    fn catalog_open_refuses_wrong_permissions() {
        for file_mode in [0o640, 0o644, 0o604, 0o660] {
            let home = crate::fs_safe::private_test_home().unwrap();
            let path = home.path().join(CATALOG_FILE);
            std::fs::write(&path, b"").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(file_mode)).unwrap();
            assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)), "{file_mode:o}");
        }
        // An existing catalog that later became readable by others refuses.
        let home = crate::fs_safe::private_test_home().unwrap();
        drop(ConnectorCatalog::open(home.path()).unwrap());
        std::fs::set_permissions(home.path().join(CATALOG_FILE), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
        // A group/other-writable home refuses before creating anything.
        for home_mode in [0o775, 0o777] {
            let home = crate::fs_safe::private_test_home().unwrap();
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(home_mode)).unwrap();
            assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
            assert!(!home.path().join(CATALOG_FILE).exists());
        }
        // A directory, hard link or non-database file in place of the store refuses.
        let home = crate::fs_safe::private_test_home().unwrap();
        std::fs::create_dir(home.path().join(CATALOG_FILE)).unwrap();
        assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
        let home = crate::fs_safe::private_test_home().unwrap();
        drop(ConnectorCatalog::open(home.path()).unwrap());
        std::fs::hard_link(home.path().join(CATALOG_FILE), home.path().join("alias")).unwrap();
        assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
        let home = crate::fs_safe::private_test_home().unwrap();
        let path = home.path().join(CATALOG_FILE);
        std::fs::write(&path, b"not a sqlite database, definitely not").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
        // Missing home refuses without creating it.
        let root = crate::fs_safe::private_test_home().unwrap();
        assert!(matches!(ConnectorCatalog::open(&root.path().join("missing")), Err(CatalogError::Unavailable)));
        assert!(!root.path().join("missing").exists());
    }

    #[test]
    fn catalog_open_refuses_foreign_owner_when_privileged() {
        // Changing ownership needs root; unprivileged runs rely on the uid
        // comparison exercised by fs_safe and verify_private_files.
        if !nix::unistd::geteuid().is_root() { return; }
        let home = crate::fs_safe::private_test_home().unwrap();
        drop(ConnectorCatalog::open(home.path()).unwrap());
        for suffix in ["-wal", "-shm"] { let _ = std::fs::remove_file(home.path().join(format!("{CATALOG_FILE}{suffix}"))); }
        nix::unistd::chown(&home.path().join(CATALOG_FILE), Some(nix::unistd::Uid::from_raw(65534)), None).unwrap();
        assert!(matches!(ConnectorCatalog::open(home.path()), Err(CatalogError::Unavailable)));
    }
}
