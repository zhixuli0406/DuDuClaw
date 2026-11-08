//! v1.81 E3 test support: a real C1 `SelectedConnector` pinned through the
//! catalog for a Personal(owner) employee. The capability is always live and
//! the binding double keeps authorizing, so ONLY the catalog route (active +
//! generation + binding) can fence it — exactly the recheck a refresh must
//! repeat after the token exchange returns.
use std::{path::Path, sync::{Arc, Mutex}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    LiveRequestVerifier, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, bindings::{BindingAuthority, BindingStore}};
use rusqlite::Connection;
use crate::isolation_connectors::{AccountId, ConnectorCatalog, LiveConnectorSelectionPolicy, ProductConnectorAccount,
    SelectedConnector};

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
pub(crate) fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(text(owner)) }
pub(crate) fn company() -> EmployeeBinding { EmployeeBinding::Shared(text("company")) }

struct Live;
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
struct Fixed { tool: &'static str, candidates: Vec<ProductConnectorAccount> }
impl LiveConnectorSelectionPolicy for Fixed {
    fn supports(&self, tool: &str) -> bool { tool == self.tool }
    fn candidates(&self, _: &RequestContext, _: &str, _: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Ok(self.candidates.clone())
    }
}

fn record(owner: &str, tool: &'static str, grant: Option<(&AccountId, EmployeeBinding)>) -> CapabilityRecord {
    let resources: Vec<ResourceGrant> = grant
        .map(|(account, binding)| ResourceGrant::Connector { account: text(account.as_str()), binding })
        .into_iter().collect();
    CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]), task: None,
        principal: Principal::Human { user_id: text(owner), generation: 1 }, employee_binding: personal(owner),
        lane: Lane::Standard, tools: List::new(vec![text(tool)]).unwrap(), resources: List::new(resources).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }
}

/// An always-live isolated request context (no connector selection): marks a
/// call as "inside an isolation scope" for `io_authority::is_scoped()`.
pub(crate) fn live_context(owner: &str) -> RequestContext {
    RequestContext::with_live_verifier(record(owner, "gmail_search", None), Arc::new(Live))
}

/// Personal(owner) employee selecting `account` through `tool`. A
/// `SharedHome` product is the explicit home fallback (route bound to
/// `Shared("company")`); a `Personal` product is the owner's own route.
pub(crate) fn selection(home: &Path, account: &AccountId, owner: &str, tool: &'static str, shared_home: bool)
    -> Arc<SelectedConnector> {
    let (product, binding) = if shared_home {
        (ProductConnectorAccount::SharedHome { account: account.to_string(), tenant: "company".into() }, company())
    } else {
        (ProductConnectorAccount::Personal { account: account.to_string(), owner: owner.into() }, personal(owner))
    };
    let principal = Principal::Human { user_id: text(owner), generation: 1 };
    let mut store = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    store.register_principal(&principal).unwrap();
    store.create_employee(EmployeeId([2; 16]), &personal(owner)).unwrap();
    store.register_connector(account.as_str(), &binding).unwrap();
    let bindings: Arc<dyn BindingAuthority> = Arc::new(Mutex::new(store));
    let context = RequestContext::with_live_verifier(record(owner, tool, Some((account, binding))), Arc::new(Live));
    let policy = Arc::new(Fixed { tool, candidates: vec![product] });
    let catalog = Arc::new(ConnectorCatalog::open(home).unwrap());
    let selected = SelectedConnector::select_with_policy(&context, tool, &serde_json::json!({}), bindings, catalog,
        Some(policy)).unwrap();
    selected.recheck().unwrap();
    Arc::new(selected)
}
