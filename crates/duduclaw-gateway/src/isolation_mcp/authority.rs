//! Production authority composed from existing product registries. The maps here
//! are bounded, epoch-local run associations, never an alternative durable ACL.
//!
//! The employee principal comes only from the A2 lifecycle read model
//! (`crate::isolation_lifecycle`). A Personal employee is authorized only for
//! a Human principal that is its lifecycle owner, while users.db mirrors that
//! owner and binds exactly one member (the owner); it is never authorized for
//! a Background principal. Shared employees keep the tenant rules.
//!
//! Connector accounts (v1.81 C2) are authorized per use by
//! `connector_with`: the same identity gate, then the production
//! `ConnectorCatalog` route (binding, generation pinned by selection,
//! product-home check, C6 detail pin), then a re-read.
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}};
use duduclaw_auth::{IsolationAuthoritySnapshot, UserDb};
use duduclaw_execution::SpawndBackend;
use duduclaw_isolation::{capability::{EmployeeBinding, Principal}, execution::RunHandle,
    ids::{EmployeeId, RunId}, ipc::RunStatus};
use duduclaw_mcp_boundary::{bindings::{BindingAuthority, BindingError}, GrantPolicy, LiveAuthority,
    LiveSnapshot, RequestContext, VerificationError};
use crate::isolation_egress::{RunMetadata, RunTurnIdentity, RunTurnResolver, ResolvedRunTurn};
use crate::isolation_lifecycle::{self as lifecycle, EmployeeLifecycle, LifecycleError};
use crate::isolation_connectors::{AccountId, ConnectorCatalog, ConnectorKind, Route};
use crate::isolation_connector_boot::ConnectorAccountDetails;
use crate::isolation_connector_details::{CatalogAccountDetails, needs_details};

const MAX_ASSOCIATIONS: usize = 4096;

/// Root allocator ReadBack used by the lifecycle read model (tests inject a fixture).
type RootSource = dyn lifecycle::RootAllocation + Send + Sync;

/// The stores one identity check reads, in this order: lifecycle (apply.db,
/// confirmations, root ReadBack, users.db), then users.db again.
struct Stores<'a> {
    home: &'a Path,
    users: &'a UserDb,
    root: &'a RootSource,
}

/// Facts established by one successful identity check.
struct Identity {
    credential_epoch: Option<u64>,
    policy_epoch: u64,
    tenant_id: String,
    lifecycle: EmployeeLifecycle,
}

/// Every lifecycle refusal fails closed; only an unreadable store is reported
/// as unavailable rather than revoked.
pub(super) fn lifecycle_refusal(error: LifecycleError) -> VerificationError {
    match error {
        LifecycleError::Unavailable => VerificationError::Unavailable,
        LifecycleError::Denied | LifecycleError::Stale | LifecycleError::Tombstoned => VerificationError::Revoked,
    }
}

fn snapshot_user(principal: &Principal) -> Option<&str> {
    match principal {
        Principal::Human { user_id, .. } => Some(user_id.as_str()),
        Principal::Background { .. } => None,
    }
}

/// Personal contract on one users.db snapshot (all exact comparisons): the
/// principal is the Human owner, the snapshot's human is that owner, the
/// mirror records that owner, and the owner is the agent's only member.
/// Also for the B2 issuer, which must apply exactly these checks.
pub(crate) fn personal_facts(agent: &str, owner: &str, principal: &Principal, current: &IsolationAuthoritySnapshot)
    -> Result<(), VerificationError> {
    // Never a Background principal, whatever its tenant or generation.
    let Principal::Human { user_id, .. } = principal else { return Err(VerificationError::Revoked); };
    let human = current.human.as_ref().ok_or(VerificationError::Revoked)?;
    if user_id.as_str() != owner || human.user_id != owner
        || current.personal_owner.as_deref() != Some(owner)
        || current.agent_member_count != 1
        || !human.agent_access.contains_key(agent) {
        return Err(VerificationError::Revoked);
    }
    Ok(())
}

/// Lifecycle first (fresh read, or a recheck against the lifecycle pinned at
/// prepare), then users.db, so the users.db facts are no older than the root
/// readback. The capability binding must equal the lifecycle principal.
fn identity_with(stores: &Stores<'_>, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
    principal: &Principal, expected: Option<&EmployeeLifecycle>) -> Result<Identity, VerificationError> {
    // No store state can authorize this; refuse before any I/O.
    if matches!((binding, principal), (EmployeeBinding::Personal(_), Principal::Background { .. })) {
        return Err(VerificationError::Revoked);
    }
    let lifecycle = match expected {
        Some(expected) => {
            if expected.label != agent || expected.employee != employee { return Err(VerificationError::Revoked); }
            lifecycle::recheck_with(stores.home, expected, stores.root)
        }
        None => lifecycle::read_lifecycle_with(stores.home, agent, stores.root),
    }.map_err(lifecycle_refusal)?;
    if lifecycle.employee != employee || lifecycle.label != agent { return Err(VerificationError::Revoked); }
    // The binding is the immutable root principal, never a caller claim.
    if lifecycle.binding != *binding { return Err(VerificationError::BindingMismatch); }
    let current = stores.users.isolation_authority_snapshot(snapshot_user(principal), agent)
        .map_err(|_| VerificationError::Unavailable)?;
    match binding {
        EmployeeBinding::Shared(tenant) => {
            if current.tenant_id != tenant.as_str() { return Err(VerificationError::BindingMismatch); }
        }
        EmployeeBinding::Personal(owner) => personal_facts(agent, owner.as_str(), principal, &current)?,
    }
    let credential_epoch = match principal {
        Principal::Human { user_id, generation } => {
            let human = current.human.as_ref().ok_or(VerificationError::Revoked)?;
            if human.user_id != user_id.as_str() || *generation != current.acl_generation {
                return Err(VerificationError::Revoked);
            }
            Some(human.credential_epoch)
        }
        Principal::Background { tenant_id, generation } => {
            if tenant_id.as_str() != current.tenant_id || *generation != current.acl_generation {
                return Err(VerificationError::Revoked);
            }
            None
        }
    };
    Ok(Identity { credential_epoch, policy_epoch: current.policy_epoch, tenant_id: current.tenant_id, lifecycle })
}

/// Recheck of a prepared run: the lifecycle must be the very same employee
/// and principal, and the users.db facts must still be the prepared ones.
fn recheck_identity(stores: &Stores<'_>, prepared: &PreparedAuthority) -> Result<Identity, VerificationError> {
    let identity = identity_with(stores, &prepared.agent, prepared.employee, &prepared.binding,
        &prepared.principal, Some(&prepared.lifecycle))?;
    if identity.tenant_id != prepared.tenant_id || identity.credential_epoch != prepared.credential_epoch
        || identity.policy_epoch != prepared.policy_epoch {
        return Err(VerificationError::Revoked);
    }
    Ok(identity)
}

/// users.db-only re-read after file/root/backend I/O. Catches a member
/// removal, a second member, a mirror owner change or removal (tombstone),
/// a tenant replacement and any ACL/policy/credential epoch change.
fn check_current(users: &UserDb, prepared: &PreparedAuthority) -> Result<(), VerificationError> {
    if prepared.binding != prepared.lifecycle.binding { return Err(VerificationError::Revoked); }
    let current = users.isolation_authority_snapshot(snapshot_user(&prepared.principal), &prepared.agent)
        .map_err(|_| VerificationError::Revoked)?;
    current_facts(&current, &prepared.agent, &prepared.principal, &prepared.binding, &prepared.tenant_id,
        prepared.policy_epoch, prepared.credential_epoch)
}

/// One users.db snapshot still shows exactly the facts an identity check
/// established: same tenant, policy/ACL/credential epochs, and the binding's
/// member rules (Shared: no Personal mirror; Personal: [`personal_facts`]).
fn current_facts(current: &IsolationAuthoritySnapshot, agent: &str, principal: &Principal, binding: &EmployeeBinding,
    tenant_id: &str, policy_epoch: u64, credential_epoch: Option<u64>) -> Result<(), VerificationError> {
    let generation = match principal {
        Principal::Human { generation, .. } | Principal::Background { generation, .. } => *generation,
    };
    if current.policy_epoch != policy_epoch || current.acl_generation != generation || current.tenant_id != tenant_id
        || current.human.as_ref().map(|human| human.credential_epoch) != credential_epoch {
        return Err(VerificationError::Revoked);
    }
    match binding {
        EmployeeBinding::Shared(tenant) => {
            // A Personal mirror never appears under a live Shared employee.
            if current.tenant_id != tenant.as_str() || current.personal_owner.is_some() {
                return Err(VerificationError::Revoked);
            }
        }
        EmployeeBinding::Personal(owner) => personal_facts(agent, owner.as_str(), principal, current)?,
    }
    Ok(())
}

// ── Connector accounts (v1.81 C2) ───────────────────────────────────────────

/// Product half of the product-home check: is the home-wide integration that
/// owns `provider`'s installed home account still connected in the product?
/// `Ok(false)` refuses (Revoked); `Err` is an unreadable product state.
///
/// Called from `SelectedConnector::recheck_binding`, which may run while a
/// sealed source owner (Google) holds its source/token locks. Implementations
/// must therefore never open a credential store, token file or source lock;
/// they read only lock-free product configuration.
pub(crate) trait ProductHome: Send + Sync {
    fn connected(&self, provider: &str) -> Result<bool, VerificationError>;
}

/// Production [`ProductHome`]: the current `config.toml [integrations]` gate
/// of the OAuth home integrations, with the same rules as their product
/// tools and the D2 home policies. Unreadable or malformed config (including
/// a non-boolean flag) is `Unavailable`, never "enabled".
/// - google: `google_workspace = true`; github: `github = true`;
/// - notion: no enable flag exists (connecting is the opt-in), so absent is
///   connected and an explicit `notion = false` is the kill switch.
/// - Every other provider's home account is home configuration that its own
///   provider re-reads at each use (database/computer/channel additionally
///   through the C6 detail pin checked by the authority); there is no
///   separate product connection to read, so the active SharedHome catalog
///   route of this installation (moved by C3/C4 on revoke/disconnect) is the
///   connection.
/// - An unknown provider is never connected.
///
/// The OAuth token itself is read only by the credential providers at
/// acquisition, before and after their await (D2, E2).
pub(crate) struct ConfiguredProductHome { home: PathBuf }
impl ConfiguredProductHome {
    pub(crate) fn new(home: PathBuf) -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        Ok(Self { home })
    }
    fn integration_flag(&self, key: &str) -> Result<Option<bool>, VerificationError> {
        let raw = std::fs::read_to_string(self.home.join("config.toml")).map_err(|_| VerificationError::Unavailable)?;
        let table: toml::Table = toml::from_str(&raw).map_err(|_| VerificationError::Unavailable)?;
        let Some(section) = table.get("integrations") else { return Ok(None) };
        match section.as_table().ok_or(VerificationError::Unavailable)?.get(key) {
            None => Ok(None),
            Some(value) => value.as_bool().map(Some).ok_or(VerificationError::Unavailable),
        }
    }
}
impl ProductHome for ConfiguredProductHome {
    fn connected(&self, provider: &str) -> Result<bool, VerificationError> {
        Ok(match provider {
            "google" => self.integration_flag("google_workspace")? == Some(true),
            "github" => self.integration_flag("github")? == Some(true),
            "notion" => self.integration_flag("notion")? != Some(false),
            "odoo" | "database" | "channel" | "search" | "model" | "computer" => true,
            _ => false,
        })
    }
}

/// The stores a connector authorization reads besides the identity stores.
/// `details` must be built over this same `catalog`.
pub(crate) struct ConnectorStores {
    catalog: Arc<ConnectorCatalog>,
    details: Arc<dyn ConnectorAccountDetails>,
    product: Arc<dyn ProductHome>,
}
impl ConnectorStores {
    fn production(home: &Path, catalog: Arc<ConnectorCatalog>) -> Result<Self, VerificationError> {
        Ok(Self { details: Arc::new(CatalogAccountDetails::new(home.to_owned(), catalog.clone())?),
            product: Arc::new(ConfiguredProductHome::new(home.to_owned())?), catalog })
    }
}

/// Is `binding` exactly this installation's home binding, `Shared(tenant)`
/// with the users.db installation tenant (which the lifecycle read has just
/// required to equal the tenant pinned in apply.db)?
fn installation_home(binding: &EmployeeBinding, tenant_id: &str) -> bool {
    matches!(binding, EmployeeBinding::Shared(tenant) if tenant.as_str() == tenant_id)
}

/// Catalog half of a connector use, on one route row. State first: an
/// inactive route or one whose generation moved since selection is Revoked
/// whatever its binding. Then the binding (BindingMismatch):
/// - own binding (`home` = None): Personal employee ⇒ Personal route of the
///   same owner; Shared employee ⇒ SharedTenant route of the same tenant, or
///   SharedHome route of this installation;
/// - explicit home fallback (`home` = Some): only a SharedHome route whose
///   binding is the claimed home binding, which must be this installation's;
///   a Shared employee may only name its own tenant there.
fn route_use(route: &Route, employee: &EmployeeBinding, tenant_id: &str, home: Option<&EmployeeBinding>,
    generation: u64) -> Result<(), VerificationError> {
    if !route.active || route.generation != generation { return Err(VerificationError::Revoked); }
    let admitted = match home {
        None => &route.binding == employee && match (employee, route.kind) {
            (EmployeeBinding::Personal(_), ConnectorKind::Personal) => true,
            (EmployeeBinding::Shared(_), ConnectorKind::SharedTenant) => true,
            (EmployeeBinding::Shared(_), ConnectorKind::SharedHome) => installation_home(&route.binding, tenant_id),
            _ => false,
        },
        Some(home) => route.kind == ConnectorKind::SharedHome && &route.binding == home
            && installation_home(home, tenant_id)
            && match employee {
                EmployeeBinding::Personal(_) => true,
                EmployeeBinding::Shared(_) => employee == home,
            },
    };
    if admitted { Ok(()) } else { Err(VerificationError::BindingMismatch) }
}

/// The C2 connector contract, for one use of `account` at the route
/// `generation` pinned by selection:
/// (a) the employee identity/lifecycle check exactly as for any tool (fresh
///     lifecycle read; B1/B2 Personal and Shared rules);
/// (b)+(c) the account is an active catalog route at exactly `generation`
///     whose binding is the lifecycle binding ([`route_use`]);
/// (d) a SharedHome route is this installation's home account and its
///     integration is still connected in the product ([`ProductHome`]);
/// (e) a provider needing C6 details has current details pinned to exactly
///     this generation, employee and agent (`current_pin`).
/// Then the route and users.db are re-read after that I/O. Unreadable stores
/// are Unavailable; a malformed account id is Invalid; every mismatch is
/// Revoked or BindingMismatch.
#[allow(clippy::too_many_arguments)]
fn connector_with(stores: &Stores<'_>, connectors: &ConnectorStores, agent: &str, employee: EmployeeId,
    binding: &EmployeeBinding, principal: &Principal, account: &str, generation: u64,
    home: Option<&EmployeeBinding>) -> Result<(), VerificationError> {
    let account = AccountId::parse(account).map_err(|_| VerificationError::Invalid)?;
    let identity = identity_with(stores, agent, employee, binding, principal, None)?;
    let route = connectors.catalog.route(&account)?.ok_or(VerificationError::Revoked)?;
    if route.account != account { return Err(VerificationError::Unavailable); }
    route_use(&route, &identity.lifecycle.binding, &identity.tenant_id, home, generation)?;
    // F6k (§3.7): after a Shared restore only the tenant routes its approval ticked (detail routes: own pins).
    if !needs_details(route.provider) {
        crate::isolation_restore_shared::tenant_route_gate(stores.home, identity.lifecycle.employee, identity.lifecycle.incarnation,
            &identity.lifecycle.binding, route.account.as_str(), route.kind)?;
    }
    if route.kind == ConnectorKind::SharedHome && !connectors.product.connected(route.provider)? {
        return Err(VerificationError::Revoked);
    }
    if needs_details(route.provider) {
        let pin = connectors.details.current_pin(&account)?.ok_or(VerificationError::Revoked)?;
        if pin.account() != &account || pin.generation() != generation || pin.employee() != employee
            || pin.agent() != agent {
            return Err(VerificationError::Revoked);
        }
    }
    // Re-read after the catalog/details/config I/O: the route must be the
    // very same row state, and users.db must still show the identity's facts.
    if connectors.catalog.route(&account)?.as_ref() != Some(&route) { return Err(VerificationError::Revoked); }
    let current = stores.users.isolation_authority_snapshot(snapshot_user(principal), agent)
        .map_err(|_| VerificationError::Unavailable)?;
    current_facts(&current, agent, principal, &identity.lifecycle.binding, &identity.tenant_id,
        identity.policy_epoch, identity.credential_epoch)
}

/// Trait-level refusal kinds: only an unreadable store stays distinguishable.
fn binding_refusal(error: VerificationError) -> BindingError {
    match error {
        VerificationError::Unavailable => BindingError::Unavailable,
        VerificationError::Invalid => BindingError::Invalid,
        _ => BindingError::Denied,
    }
}

/// Captured by the authenticated gateway turn/dispatch path. Missing values stay
/// missing; employee payloads and process environment never reconstruct them.
pub struct HostTurn {
    pub identity: RunTurnIdentity,
    pub egress_turn: Option<String>,
    pub metadata: Option<RunMetadata>,
    pub principal_context: Option<String>,
    pub background: Option<crate::isolation_background::RecordAuthority>,
}
#[derive(Clone)]
struct CapturedTurn {
    turn: String,
    egress_turn: Option<String>,
    approval_session: Option<String>,
    metadata: Option<RunMetadata>,
    principal_context: Option<String>,
    background: Option<crate::isolation_background::RecordAuthority>,
}
#[derive(Clone)]
pub struct PreparedAuthority {
    agent: String,
    employee: EmployeeId,
    principal: Principal,
    binding: EmployeeBinding,
    revision: u64,
    policy: String,
    credential_epoch: Option<u64>,
    policy_epoch: u64,
    /// users.db installation tenant observed at prepare.
    tenant_id: String,
    /// Lifecycle at prepare; every recheck requires the very same one.
    lifecycle: EmployeeLifecycle,
    expires_at: u64,
    turn: Option<CapturedTurn>,
}
impl PreparedAuthority {
    pub fn employee(&self) -> EmployeeId { self.employee }
    /// F6b (A2): the incarnation pinned at prepare; every recheck of this
    /// run (`recheck_with`) refuses another one.
    pub fn incarnation(&self) -> u32 { self.lifecycle.incarnation }
    pub fn revision(&self) -> u64 { self.revision }
    pub fn with_turn(mut self, turn: HostTurn) -> Self {
        self.turn = Some(CapturedTurn { turn: turn.identity.turn,
            egress_turn: turn.egress_turn,
            approval_session: turn.identity.approval_session, metadata: turn.metadata,
            principal_context: turn.principal_context, background: turn.background });
        self
    }
}

pub struct GatewayAuthority {
    home: PathBuf,
    users: Arc<UserDb>,
    backend: Arc<SpawndBackend>,
    agents: Arc<super::AgentAdmission>,
    root: Arc<RootSource>,
    /// `None` when boot has no production catalog: every connector use is
    /// then Unavailable (never a fallback store).
    connectors: Option<ConnectorStores>,
    next_revision: AtomicU64,
    employees: Mutex<BTreeMap<EmployeeId, String>>,
    runs: Mutex<BTreeMap<RunId, PreparedAuthority>>,
}
impl GatewayAuthority {
    pub(super) fn ingress_snapshot(&self, user: &str, agent: &str)
        -> Result<duduclaw_auth::IsolationAuthoritySnapshot, VerificationError> {
        (self.agents)(agent)?;
        self.users.isolation_authority_snapshot(Some(user), agent).map_err(|_| VerificationError::Revoked)
    }
    /// `catalog` must be the same production `ConnectorCatalog` instance boot
    /// gives selection and the connector factory (F1).
    pub fn new(home: PathBuf, users: Arc<UserDb>, backend: Arc<SpawndBackend>, agents: Arc<super::AgentAdmission>,
        catalog: Option<Arc<ConnectorCatalog>>) -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        let connectors = catalog.map(|catalog| ConnectorStores::production(&home, catalog)).transpose()?;
        Ok(Self { home, users, backend, agents, root: Arc::new(lifecycle::RootAllocator), connectors,
            next_revision: AtomicU64::new(1),
            employees: Mutex::new(BTreeMap::new()), runs: Mutex::new(BTreeMap::new()) })
    }

    /// Shared by both `BindingAuthority` connector methods. The agent comes
    /// only from this epoch's prepared association for the employee id.
    fn connector(&self, employee: EmployeeId, binding: &EmployeeBinding, principal: &Principal, account: &str,
        generation: u64, home: Option<&EmployeeBinding>) -> Result<(), BindingError> {
        let agent = self.employees.lock().map_err(|_| BindingError::Unavailable)?.get(&employee)
            .cloned().ok_or(BindingError::Denied)?;
        let connectors = self.connectors.as_ref().ok_or(BindingError::Unavailable)?;
        (self.agents)(&agent).map_err(binding_refusal)?;
        connector_with(&self.stores(), connectors, &agent, employee, binding, principal, account, generation, home)
            .map_err(binding_refusal)
    }

    fn stores(&self) -> Stores<'_> {
        Stores { home: &self.home, users: self.users.as_ref(), root: self.root.as_ref() }
    }

    fn identity(&self, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, expected: Option<&EmployeeLifecycle>) -> Result<Identity, VerificationError> {
        (self.agents)(agent)?;
        identity_with(&self.stores(), agent, employee, binding, principal, expected)
    }

    pub fn prepare(&self, agent: &str, grant: &mut GrantPolicy, authenticated_epoch: Option<u64>)
        -> Result<PreparedAuthority, VerificationError> {
        if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        let now = duduclaw_mcp_boundary::authority::now_seconds()?;
        if grant.not_before > now || grant.expires_at <= now { return Err(VerificationError::InvalidTime); }
        let current = lifecycle::read_lifecycle_with(&self.home, agent, self.root.as_ref())
            .map_err(lifecycle_refusal)?;
        let Identity { credential_epoch, policy_epoch, tenant_id, lifecycle } =
            self.identity(agent, current.employee, &grant.binding, &grant.principal, Some(&current))?;
        // Preserve the epoch authenticated at ingress; taking the current one
        // here would revive a session revoked before execution admission.
        if credential_epoch != authenticated_epoch { return Err(VerificationError::Revoked); }
        // Ingress grants were derived under this policy epoch. A policy change
        // between capture and reservation must not revive the old grant set.
        if grant.policy_revision != policy_epoch { return Err(VerificationError::Revoked); }
        let policy = crate::approval::strict_policy_revision(&self.home, agent)
            .map_err(|_| VerificationError::Unavailable)?;
        // This is an epoch-local handle, not a shortened policy hash. Every
        // verification compares the complete original digest before returning it.
        let revision = self.next_revision.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |value| value.checked_add(1)).map_err(|_| VerificationError::Unavailable)?;
        let mut employees = self.employees.lock().map_err(|_| VerificationError::Unavailable)?;
        if let Some(prior) = employees.get(&lifecycle.employee) {
            if prior != agent { return Err(VerificationError::BindingMismatch); }
        } else {
            if employees.len() >= MAX_ASSOCIATIONS { return Err(VerificationError::Unavailable); }
            employees.insert(lifecycle.employee, agent.to_owned());
        }
        grant.policy_revision = revision;
        Ok(PreparedAuthority { agent: agent.to_owned(), employee: lifecycle.employee,
            principal: grant.principal.clone(), binding: grant.binding.clone(), revision,
            policy, credential_epoch, policy_epoch, tenant_id, lifecycle, expires_at: grant.expires_at, turn: None })
    }

    fn recheck(&self, prepared: &PreparedAuthority) -> Result<(), VerificationError> {
        if let Some(receipt) = prepared.turn.as_ref().and_then(|turn| turn.background.as_ref()) {
            crate::isolation_background::recheck(receipt)?;
        }
        if duduclaw_mcp_boundary::authority::now_seconds()? >= prepared.expires_at {
            return Err(VerificationError::InvalidTime);
        }
        (self.agents)(&prepared.agent)?;
        let identity = recheck_identity(&self.stores(), prepared)?;
        let policy = crate::approval::strict_policy_revision(&self.home, &prepared.agent)
            .map_err(|_| VerificationError::Unavailable)?;
        compare_revision(prepared, identity.credential_epoch, identity.policy_epoch, &policy)?;
        self.check_epoch(prepared)
    }

    // Re-read after file/root I/O: a policy writer may have fenced and restored
    // identical bytes while the preceding digest or Status RPC was in flight,
    // and a Personal owner may have lost membership or the mirror meanwhile.
    fn check_epoch(&self, prepared: &PreparedAuthority) -> Result<(), VerificationError> {
        (self.agents)(&prepared.agent)?;
        check_current(&self.users, prepared)
    }

    /// Called after A reserves the run and before D issues its capability.
    /// Duplicate runs cannot replace the original host-authorized association.
    pub fn bind_run(&self, run: RunHandle, prepared: PreparedAuthority) -> Result<(), VerificationError> {
        self.recheck(&prepared)?;
        let (reservation, status) = self.backend.live_snapshot(run).map_err(|_| VerificationError::Unavailable)?;
        if reservation.employee != prepared.employee || reservation.run != run
            || !matches!(status, RunStatus::Reserved { .. }) {
            return Err(VerificationError::BindingMismatch);
        }
        self.check_epoch(&prepared)?;
        let mut runs = self.runs.lock().map_err(|_| VerificationError::Unavailable)?;
        if runs.len() >= MAX_ASSOCIATIONS || runs.contains_key(&run.0) { return Err(VerificationError::Unavailable); }
        // H13b: the ephemeral-job start reads this as "the parent holds its capability".
        crate::isolation_ephemeral_runs::record_bound(run, prepared.employee);
        // H13d: an ephemeral child's run ends its job (`revoke_run`).
        if let Some(receipt) = prepared.turn.as_ref().and_then(|turn| turn.background.as_ref()) {
            if let Some(job) = crate::isolation_background::ephemeral_child::ephemeral_job_of(receipt) {
                crate::isolation_ephemeral_runs::record_child_bound(run, receipt.home().to_path_buf(), job.to_owned());
            }
        }
        runs.insert(run.0, prepared);
        Ok(())
    }

    pub fn revoke_run(&self, run: RunHandle) {
        // Recover ownership on poison so cleanup cannot retain an association.
        self.runs.lock().unwrap_or_else(|error| error.into_inner()).remove(&run.0);
        crate::isolation_ephemeral_runs::record_exited(run);
    }

    fn prepared(&self, run: RunHandle) -> Result<PreparedAuthority, VerificationError> {
        self.runs.lock().map_err(|_| VerificationError::Unavailable)?.get(&run.0)
            .cloned().ok_or(VerificationError::Revoked)
    }
    fn turn(&self, context: &RequestContext) -> Result<CapturedTurn, VerificationError> {
        let record = context.record();
        let snapshot = self.snapshot(record.employee, RunHandle(record.run))?;
        if snapshot.principal != record.principal || snapshot.binding != record.employee_binding
            || snapshot.policy_revision != record.policy_revision || !matches!(snapshot.status, RunStatus::Running) {
            return Err(VerificationError::Revoked);
        }
        self.prepared(RunHandle(record.run))?.turn.ok_or(VerificationError::Unavailable)
    }

    pub(super) fn delegation_source(&self, context: &RequestContext)
        -> Result<crate::isolation_background::DelegationSource, VerificationError> {
        let turn = self.turn(context)?;
        let prepared = self.prepared(RunHandle(context.record().run))?;
        let Principal::Human { user_id, generation } = prepared.principal else {
            return Err(VerificationError::Invalid);
        };
        Ok(crate::isolation_background::DelegationSource {
            owner: duduclaw_core::HumanPrincipal::user(user_id.as_str()).ok_or(VerificationError::Invalid)?.to_string(),
            epoch: prepared.credential_epoch.ok_or(VerificationError::Invalid)?,
            acl_generation: generation, policy_epoch: prepared.policy_epoch,
            policy: prepared.policy, agent: prepared.agent, employee: prepared.employee,
            run: RunHandle(context.record().run), expires_at: prepared.expires_at,
            lane: context.record().lane, parent: turn.background,
            // The lifecycle principal pinned at prepare (equal to the grant
            // binding, B1). A Personal source needs its owner's approval
            // card before the delegation is queued (SR2, owner decision).
            binding: prepared.lifecycle.binding.clone(),
            // F6b (A3/A4): the source incarnation pinned at prepare.
            incarnation: prepared.lifecycle.incarnation,
        })
    }
}

fn compare_revision(prepared: &PreparedAuthority, epoch: Option<u64>, policy_epoch: u64, policy: &str)
    -> Result<(), VerificationError> {
    if epoch != prepared.credential_epoch || policy_epoch != prepared.policy_epoch || policy != prepared.policy {
        return Err(VerificationError::Revoked);
    }
    Ok(())
}

impl BindingAuthority for GatewayAuthority {
    fn validate(&self, employee: EmployeeId, binding: &EmployeeBinding, principal: &Principal)
        -> Result<(), BindingError> {
        let agent = self.employees.lock().map_err(|_| BindingError::Unavailable)?.get(&employee)
            .cloned().ok_or(BindingError::Denied)?;
        // A fresh lifecycle read: a recreated employee (new id), another
        // principal or a lost Personal owner/membership is refused.
        self.identity(&agent, employee, binding, principal, None).map(|_|()).map_err(|_| BindingError::Denied)
    }
    /// Own-binding use: Personal route of the owner, SharedTenant route of the
    /// tenant, or SharedHome route of this installation for a Shared employee.
    fn authorize_connector(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str, generation: u64) -> Result<(), BindingError> {
        self.connector(employee, binding, principal, account, generation, None)
    }
    /// Explicit home fallback: only this installation's SharedHome route, and
    /// only while its integration is connected. Shared employees can never
    /// reach a Personal credential through it.
    fn authorize_product_home_connector(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str, generation: u64, home_binding: &EmployeeBinding)
        -> Result<(), BindingError> {
        self.connector(employee, binding, principal, account, generation, Some(home_binding))
    }
}
impl LiveAuthority for GatewayAuthority {
    fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError> {
        let prepared = self.prepared(run)?;
        if prepared.employee != employee { return Err(VerificationError::BindingMismatch); }
        self.recheck(&prepared)?;
        let (reservation, status) = self.backend.live_snapshot(run).map_err(|_| VerificationError::Unavailable)?;
        if reservation.run != run || reservation.employee != employee { return Err(VerificationError::BindingMismatch); }
        self.check_epoch(&prepared)?;
        // A concurrent explicit revoke cannot be undone by this snapshot.
        if self.prepared(run)?.revision != prepared.revision { return Err(VerificationError::Revoked); }
        Ok(LiveSnapshot { run, employee, control: reservation.control, status,
            principal: prepared.principal, binding: prepared.binding,
            policy_revision: prepared.revision, agent_slug: prepared.agent })
    }
}
impl RunTurnResolver for GatewayAuthority {
    fn resolve_run(&self, context: &RequestContext) -> Result<ResolvedRunTurn, VerificationError> {
        let turn = self.turn(context)?;
        if turn.turn.is_empty() { return Err(VerificationError::Unavailable); }
        Ok(ResolvedRunTurn { identity: RunTurnIdentity { turn: turn.turn, approval_session: turn.approval_session },
            egress_turn: turn.egress_turn,
            metadata: turn.metadata, principal_context: turn.principal_context, utility_runner: None })
    }
    fn resolve(&self, context: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
        let turn = self.turn(context)?;
        if turn.turn.is_empty() { return Err(VerificationError::Unavailable); }
        Ok(RunTurnIdentity { turn: turn.turn, approval_session: turn.approval_session })
    }
    fn egress_turn(&self, context: &RequestContext) -> Result<Option<String>, VerificationError> {
        Ok(self.turn(context)?.egress_turn)
    }
    fn metadata(&self, context: &RequestContext) -> Result<Option<RunMetadata>, VerificationError> {
        Ok(self.turn(context)?.metadata)
    }
    fn principal_context(&self, context: &RequestContext) -> Result<Option<String>, VerificationError> {
        Ok(self.turn(context)?.principal_context)
    }
}

#[cfg(test)]
#[path = "authority_personal_tests.rs"]
mod personal_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::bounded::Text;
    #[test]
    fn isolation_authority_compares_full_policy_and_credential_epoch() {
        let binding = EmployeeBinding::Shared(Text::new("tenant".into()).unwrap());
        let lifecycle = EmployeeLifecycle { employee: EmployeeId([1; 16]), label: "employee".into(),
            allocation: duduclaw_migration::contract::ProposalBinding {
                operation_id: duduclaw_migration::contract::ProposalOperationId {
                    proposal_id: duduclaw_migration::ids::RequestId([1; 16]), revision: 1 },
                proposal_digest: duduclaw_isolation::ids::Sha256Digest([2; 32]) },
            uid: 70_001, binding: binding.clone(), incarnation: 0 };
        let prepared = PreparedAuthority { agent: "employee".into(), employee: EmployeeId([1; 16]),
            principal: Principal::Human { user_id: Text::new("user".into()).unwrap(), generation: 5 },
            binding, revision: 1,
            policy: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123".into(),
            credential_epoch: Some(9), policy_epoch: 12, tenant_id: "tenant".into(), lifecycle,
            expires_at: u64::MAX, turn: None };
        assert!(compare_revision(&prepared, Some(9), 12, &prepared.policy).is_ok());
        // Restoring identical policy bytes cannot revive a pre-change run.
        assert!(compare_revision(&prepared, Some(9), 13, &prepared.policy).is_err());
        // Identical first 64 bits must not make distinct policy digests equal.
        assert!(compare_revision(&prepared, Some(9), 12,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0124").is_err());
        assert!(compare_revision(&prepared, Some(10), 12, &prepared.policy).is_err());
        assert!(compare_revision(&prepared, None, 12, &prepared.policy).is_err());
    }
}
