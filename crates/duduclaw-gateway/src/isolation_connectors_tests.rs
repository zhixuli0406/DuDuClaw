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
    let selected = select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog.clone()).unwrap();
    assert_eq!(selected.account(), "alice-google");
    bindings.lock().unwrap().revoke_connector("alice-google").unwrap();
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
    let ctx = context(ctx.record().employee_binding.clone(), ctx.record().principal.clone(), &["alice-google", "alice-google-2"]);
    assert!(select(&ctx, "gmail_search", &json!({}), bindings.clone(), catalog.clone()).is_err());
    assert!(select(&ctx, "gmail_search", &json!({"connector_account":"alice-google-2"}), bindings, catalog).is_ok());
}
#[test]
fn isolated_connector_shared_employee_cannot_select_personal_account() {
    let (bindings, catalog, _) = fixture();
    let principal = Principal::Background { tenant_id: text("company"), generation: 1 };
    let binding = EmployeeBinding::Shared(text("company"));
    let ctx = context(binding.clone(), principal.clone(), &["alice-google"]);
    // A forged Shared principal cannot replace the stored Personal employee.
    assert!(select(&ctx, "gmail_search", &json!({}), bindings, catalog.clone()).is_err());
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(EmployeeId([2; 16]), &binding).unwrap();
    bindings.register_connector("alice-google", &EmployeeBinding::Personal(text("alice"))).unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    assert!(select(&ctx, "gmail_search", &json!({}), bindings, catalog).is_err());
}
#[test]
fn isolated_connector_route_is_immutable_and_live() {
    let (bindings, catalog, ctx) = fixture();
    let selected = select(&ctx, "gmail_search", &json!({}), bindings, catalog.clone()).unwrap();
    assert!(catalog.register("alice-google", "github").is_err());
    catalog.revoke("alice-google").unwrap();
    assert!(selected.recheck().is_err());
    assert!(catalog.register("alice-google", "google").is_err());
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
        account: "alice-google".into(), owner: "alice".into() }])));
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
    bindings.lock().unwrap().register_connector("bob-google", &EmployeeBinding::Personal(text("bob"))).unwrap();
    catalog.register("bob-google", "google").unwrap();
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![ProductConnectorAccount::Personal {
        account: "bob-google".into(), owner: "bob".into() }])));
    assert!(SelectedConnector::select_with_policy(&employee_scope(&context), "gmail_search", &json!({}),
        bindings, catalog, Some(policy)).is_err());
}
#[test]
fn product_policy_explicit_shared_home_requires_live_opt_in_and_correct_binding() {
    let (bindings, catalog, context) = fixture();
    let shared = EmployeeBinding::Shared(text("company"));
    bindings.lock().unwrap().register_connector("company-home", &shared).unwrap();
    catalog.register("company-home", "google").unwrap();
    let policy = Arc::new(ProductPolicy(Mutex::new(vec![ProductConnectorAccount::SharedHome {
        account: "company-home".into(), tenant: "company".into() }])));
    let scope = employee_scope(&context);
    let selected = SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).unwrap();
    assert_eq!(selected.binding(), &shared);
    let mut explicit = context.record().clone();
    explicit.resources = List::new(vec![ResourceGrant::Connector { account: text("company-home"), binding: shared }]).unwrap();
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
        ProductConnectorAccount::Personal { account: "alice-google".into(), owner: "alice".into() },
        ProductConnectorAccount::Personal { account: "alice-google-2".into(), owner: "alice".into() },
    ])));
    let scope = employee_scope(&context);
    assert!(SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({}),
        bindings.clone(), catalog.clone(), Some(policy.clone())).is_err());
    assert!(SelectedConnector::select_with_policy(&scope, "gmail_search", &json!({"connector_account":"alice-google-2"}),
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
        catalog.register(policy.name, provider).unwrap();
        assert!(catalog.matches(policy.name, provider).unwrap());
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
            Ok(vec![ProductConnectorAccount::Personal { account: "alice-google".into(), owner: "alice".into() }])
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
        Ok(vec![ProductConnectorAccount::Personal { account: "alice-google".into(), owner: "alice".into() }])
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
        &CallAccount { account: "alice-google", binding: EmployeeBinding::Personal(text("alice")) }).unwrap()
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
    for resolver in [CallAccount { account: "alice-google-2", binding: EmployeeBinding::Personal(text("alice")) },
        CallAccount { account: "alice-google", binding: EmployeeBinding::Shared(text("company")) }] {
        let call = duduclaw_mcp_boundary::tool_policy::authorize(&context, "google_status", &json!({}), &resolver).unwrap();
        assert_eq!(selected.verify_call(&call), Err(VerificationError::BindingMismatch));
    }
    let mut other_record = context.record().clone();
    other_record.policy_revision += 1;
    let other_context = RequestContext::with_live_verifier(other_record, Arc::new(FixtureAuthority));
    assert_eq!(selected.verify_call(&operation_call(&other_context, "google_status", &json!({}))),
        Err(VerificationError::BindingMismatch));
    let call = operation_call(&context, "google_status", &json!({}));
    catalog.revoke("alice-google").unwrap();
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
    let alice=ctx.record().employee_binding.clone();
    for account in ["a-revoked","b-valid"] {
        bindings.lock().unwrap().register_connector(account,&alice).unwrap();
        catalog.register(account,"database").unwrap();
    }
    bindings.lock().unwrap().revoke_connector("a-revoked").unwrap();
    catalog.register("0-foreign","database").unwrap();
    let policy=Arc::new(Directory(vec![
        ProductConnectorAccount::Personal{account:"0-foreign".into(),owner:"bob".into()},
        ProductConnectorAccount::Personal{account:"a-revoked".into(),owner:"alice".into()},
        ProductConnectorAccount::Personal{account:"b-valid".into(),owner:"alice".into()},
    ]));
    let selected=SelectedConnector::select_with_policy(&ctx,"db_sources",&json!({}),bindings,catalog,Some(policy)).unwrap();
    assert_eq!(selected.account(),"b-valid");
    selected.recheck().unwrap();
    assert!(selected.collection_member("a-revoked").is_err());
    assert!(selected.collection_member("0-foreign").is_err());
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
            Ok(vec![ProductConnectorAccount::Personal { account:"alice-google".into(), owner:"alice".into() }])
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
        bindings.lock().unwrap().revoke_connector("alice-google").unwrap();
        assert!(duduclaw_mcp_boundary::io_authority::verify_before_io().is_err());
        assert_eq!(owner.reads.load(Ordering::SeqCst), reads);
    }).await;
    assert!(selected.recheck().is_err(), "entry/exit must still require the full product check");
}
