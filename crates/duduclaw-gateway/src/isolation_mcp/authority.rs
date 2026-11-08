//! Production authority composed from existing product registries. The maps here
//! are bounded, epoch-local run associations, never an alternative durable ACL.
//!
//! The employee principal comes only from the A2 lifecycle read model
//! (`crate::isolation_lifecycle`). A Personal employee is authorized only for
//! a Human principal that is its lifecycle owner, while users.db mirrors that
//! owner and binds exactly one member (the owner); it is never authorized for
//! a Background principal. Shared employees keep the tenant rules.
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}};
use duduclaw_auth::{IsolationAuthoritySnapshot, UserDb};
use duduclaw_execution::SpawndBackend;
use duduclaw_isolation::{capability::{EmployeeBinding, Principal}, execution::RunHandle,
    ids::{EmployeeId, RunId}, ipc::RunStatus};
use duduclaw_mcp_boundary::{bindings::{BindingAuthority, BindingError}, GrantPolicy, LiveAuthority,
    LiveSnapshot, RequestContext, VerificationError};
use crate::isolation_egress::{RunMetadata, RunTurnIdentity, RunTurnResolver, ResolvedRunTurn};
use crate::isolation_lifecycle::{self as lifecycle, EmployeeLifecycle, LifecycleError};

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
pub(super) fn personal_facts(agent: &str, owner: &str, principal: &Principal, current: &IsolationAuthoritySnapshot)
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
    let current = users.isolation_authority_snapshot(snapshot_user(&prepared.principal), &prepared.agent)
        .map_err(|_| VerificationError::Revoked)?;
    let generation = match &prepared.principal {
        Principal::Human { generation, .. } | Principal::Background { generation, .. } => *generation,
    };
    if prepared.binding != prepared.lifecycle.binding || current.policy_epoch != prepared.policy_epoch
        || current.acl_generation != generation || current.tenant_id != prepared.tenant_id
        || current.human.as_ref().map(|human| human.credential_epoch) != prepared.credential_epoch {
        return Err(VerificationError::Revoked);
    }
    match &prepared.binding {
        EmployeeBinding::Shared(tenant) => {
            // A Personal mirror never appears under a live Shared employee.
            if current.tenant_id != tenant.as_str() || current.personal_owner.is_some() {
                return Err(VerificationError::Revoked);
            }
        }
        EmployeeBinding::Personal(owner) => personal_facts(&prepared.agent, owner.as_str(), &prepared.principal, &current)?,
    }
    Ok(())
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
    pub fn new(home: PathBuf, users: Arc<UserDb>, backend: Arc<SpawndBackend>, agents: Arc<super::AgentAdmission>)
        -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        Ok(Self { home, users, backend, agents, root: Arc::new(lifecycle::RootAllocator),
            next_revision: AtomicU64::new(1),
            employees: Mutex::new(BTreeMap::new()), runs: Mutex::new(BTreeMap::new()) })
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
        runs.insert(run.0, prepared);
        Ok(())
    }

    pub fn revoke_run(&self, run: RunHandle) {
        // Recover ownership on poison so cleanup cannot retain an association.
        self.runs.lock().unwrap_or_else(|error| error.into_inner()).remove(&run.0);
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
    fn authorize_connector(&self, _employee: EmployeeId, _binding: &EmployeeBinding,
        _principal: &Principal, _account: &str) -> Result<(), BindingError> {
        // No product connector-to-tenant adapter has been installed. In
        // particular, Shared employees cannot inherit personal credentials.
        Err(BindingError::Denied)
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
            uid: 70_001, binding: binding.clone() };
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
