//! Production authority composed from existing product registries. The maps here
//! are bounded, epoch-local run associations, never an alternative durable ACL.
use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}};
use duduclaw_auth::UserDb;
use duduclaw_execution::SpawndBackend;
use duduclaw_isolation::{capability::{EmployeeBinding, Principal}, execution::RunHandle,
    ids::{EmployeeId, RunId}, ipc::RunStatus};
use duduclaw_mcp_boundary::{bindings::{BindingAuthority, BindingError}, GrantPolicy, LiveAuthority,
    LiveSnapshot, RequestContext, VerificationError};
use crate::isolation_egress::{RunMetadata, RunTurnIdentity, RunTurnResolver, ResolvedRunTurn};

const MAX_ASSOCIATIONS: usize = 4096;

/// Captured by the authenticated gateway turn/dispatch path. Missing values stay
/// missing; employee payloads and process environment never reconstruct them.
pub struct HostTurn {
    pub identity: RunTurnIdentity,
    pub metadata: Option<RunMetadata>,
    pub principal_context: Option<String>,
}
#[derive(Clone)]
struct CapturedTurn {
    turn: String,
    approval_session: Option<String>,
    metadata: Option<RunMetadata>,
    principal_context: Option<String>,
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
    expires_at: u64,
    turn: Option<CapturedTurn>,
}
impl PreparedAuthority {
    pub fn employee(&self) -> EmployeeId { self.employee }
    pub fn revision(&self) -> u64 { self.revision }
    pub fn with_turn(mut self, turn: HostTurn) -> Self {
        self.turn = Some(CapturedTurn { turn: turn.identity.turn,
            approval_session: turn.identity.approval_session, metadata: turn.metadata,
            principal_context: turn.principal_context });
        self
    }
}

pub struct GatewayAuthority {
    home: PathBuf,
    users: Arc<UserDb>,
    backend: Arc<SpawndBackend>,
    agents: Arc<super::AgentAdmission>,
    next_revision: AtomicU64,
    employees: Mutex<BTreeMap<EmployeeId, String>>,
    runs: Mutex<BTreeMap<RunId, PreparedAuthority>>,
}
impl GatewayAuthority {
    pub fn new(home: PathBuf, users: Arc<UserDb>, backend: Arc<SpawndBackend>, agents: Arc<super::AgentAdmission>)
        -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        Ok(Self { home, users, backend, agents, next_revision: AtomicU64::new(1),
            employees: Mutex::new(BTreeMap::new()), runs: Mutex::new(BTreeMap::new()) })
    }

    fn identity(&self, agent: &str, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal) -> Result<(Option<u64>, u64), VerificationError> {
        (self.agents)(agent)?;
        let EmployeeBinding::Shared(tenant) = binding else { return Err(VerificationError::BindingMismatch); };
        let user = match principal { Principal::Human { user_id, .. } => Some(user_id.as_str()),
            Principal::Background { .. } => None };
        let current = self.users.isolation_authority_snapshot(user, agent)
            .map_err(|_| VerificationError::Unavailable)?;
        if current.tenant_id != tenant.as_str() { return Err(VerificationError::BindingMismatch); }
        let credential_epoch = match principal {
            Principal::Human { user_id, generation } => {
                let human = current.human.ok_or(VerificationError::Revoked)?;
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
        let receipt = crate::handlers::resolve_confirmed_employee(&self.home, agent)
            .map_err(|_| VerificationError::Unavailable)?;
        if receipt.disabled || receipt.employee != employee || receipt.label != agent {
            return Err(VerificationError::Revoked);
        }
        Ok((credential_epoch, current.policy_epoch))
    }

    pub fn prepare(&self, agent: &str, grant: &mut GrantPolicy, authenticated_epoch: Option<u64>)
        -> Result<PreparedAuthority, VerificationError> {
        if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        let now = duduclaw_mcp_boundary::authority::now_seconds()?;
        if grant.not_before > now || grant.expires_at <= now { return Err(VerificationError::InvalidTime); }
        let receipt = crate::handlers::resolve_confirmed_employee(&self.home, agent)
            .map_err(|_| VerificationError::Unavailable)?;
        let (credential_epoch, policy_epoch) = self.identity(agent, receipt.employee, &grant.binding, &grant.principal)?;
        // Preserve the epoch authenticated at ingress; taking the current one
        // here would revive a session revoked before execution admission.
        if credential_epoch != authenticated_epoch { return Err(VerificationError::Revoked); }
        let policy = crate::approval::strict_policy_revision(&self.home, agent)
            .map_err(|_| VerificationError::Unavailable)?;
        // This is an epoch-local handle, not a shortened policy hash. Every
        // verification compares the complete original digest before returning it.
        let revision = self.next_revision.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |value| value.checked_add(1)).map_err(|_| VerificationError::Unavailable)?;
        let mut employees = self.employees.lock().map_err(|_| VerificationError::Unavailable)?;
        if let Some(prior) = employees.get(&receipt.employee) {
            if prior != agent { return Err(VerificationError::BindingMismatch); }
        } else {
            if employees.len() >= MAX_ASSOCIATIONS { return Err(VerificationError::Unavailable); }
            employees.insert(receipt.employee, agent.to_owned());
        }
        grant.policy_revision = revision;
        Ok(PreparedAuthority { agent: agent.to_owned(), employee: receipt.employee,
            principal: grant.principal.clone(), binding: grant.binding.clone(), revision,
            policy, credential_epoch, policy_epoch, expires_at: grant.expires_at, turn: None })
    }

    fn recheck(&self, prepared: &PreparedAuthority) -> Result<(), VerificationError> {
        if duduclaw_mcp_boundary::authority::now_seconds()? >= prepared.expires_at {
            return Err(VerificationError::InvalidTime);
        }
        let (epoch, policy_epoch) = self.identity(&prepared.agent, prepared.employee, &prepared.binding, &prepared.principal)?;
        let policy = crate::approval::strict_policy_revision(&self.home, &prepared.agent)
            .map_err(|_| VerificationError::Unavailable)?;
        compare_revision(prepared, epoch, policy_epoch, &policy)?;
        self.check_epoch(prepared)
    }

    // Re-read after file/root I/O: a policy writer may have fenced and restored
    // identical bytes while the preceding digest or Status RPC was in flight.
    fn check_epoch(&self, prepared: &PreparedAuthority) -> Result<(), VerificationError> {
        (self.agents)(&prepared.agent)?;
        let user = match &prepared.principal { Principal::Human { user_id, .. } => Some(user_id.as_str()),
            Principal::Background { .. } => None };
        let current = self.users.isolation_authority_snapshot(user, &prepared.agent)
            .map_err(|_| VerificationError::Revoked)?;
        let generation = match &prepared.principal {
            Principal::Human { generation, .. } | Principal::Background { generation, .. } => *generation,
        };
        let EmployeeBinding::Shared(tenant) = &prepared.binding else { return Err(VerificationError::Revoked); };
        if current.policy_epoch != prepared.policy_epoch || current.acl_generation != generation
            || current.tenant_id != tenant.as_str()
            || current.human.map(|human| human.credential_epoch) != prepared.credential_epoch {
            return Err(VerificationError::Revoked);
        }
        Ok(())
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
        self.identity(&agent, employee, binding, principal).map(|_|()).map_err(|_| BindingError::Denied)
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
            metadata: turn.metadata, principal_context: turn.principal_context, utility_runner: None })
    }
    fn resolve(&self, context: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
        let turn = self.turn(context)?;
        if turn.turn.is_empty() { return Err(VerificationError::Unavailable); }
        Ok(RunTurnIdentity { turn: turn.turn, approval_session: turn.approval_session })
    }
    fn metadata(&self, context: &RequestContext) -> Result<Option<RunMetadata>, VerificationError> {
        Ok(self.turn(context)?.metadata)
    }
    fn principal_context(&self, context: &RequestContext) -> Result<Option<String>, VerificationError> {
        Ok(self.turn(context)?.principal_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::bounded::Text;
    #[test]
    fn isolation_authority_compares_full_policy_and_credential_epoch() {
        let prepared = PreparedAuthority { agent: "employee".into(), employee: EmployeeId([1; 16]),
            principal: Principal::Human { user_id: Text::new("user".into()).unwrap(), generation: 5 },
            binding: EmployeeBinding::Shared(Text::new("tenant".into()).unwrap()), revision: 1,
            policy: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123".into(),
            credential_epoch: Some(9), policy_epoch: 12, expires_at: u64::MAX, turn: None };
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
