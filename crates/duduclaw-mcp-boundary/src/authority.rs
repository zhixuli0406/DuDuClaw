use std::{collections::BTreeMap, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};
use duduclaw_isolation::{bounded::{List, Text}, capability::*, execution::RunHandle,
    ids::{EmployeeId, Sha256Digest}, ipc::{ControlConnectionId, RunStatus}};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;
const MAX_RECORDS: usize = 4096;

/// Populated only by the authenticated spawnd control client (packages A/B).
#[derive(Clone)]
pub struct Reservation {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub control: ControlConnectionId,
    pub expires_at_unix_ms: u64,
}

/// Read afresh under the host's authority-store transaction. No cached allow.
pub struct LiveSnapshot {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub control: ControlConnectionId,
    pub status: RunStatus,
    pub principal: Principal,
    pub binding: EmployeeBinding,
    pub policy_revision: u64,
    /// Registry mapping, never a display name or caller argument.
    pub agent_slug: String,
}

/// Integration seam for A and the gateway's policy/binding stores. Implementations
/// must reject revoked principals, tombstoned employees and unreadable stores.
/// The returned snapshot must also validate Personal's durable 1:1 constraint.
pub trait LiveAuthority: Send + Sync {
    fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError>;
}

/// Trusted host policy, deliberately not deserializable. Grants only narrow the
/// existing dispatcher gates; a grant never bypasses scope or approvals.
pub struct GrantPolicy {
    pub task: Option<Text<128>>,
    pub principal: Principal,
    pub binding: EmployeeBinding,
    pub lane: Lane,
    pub tools: List<Text<128>, 256>,
    pub resources: List<ResourceGrant, 128>,
    pub policy_revision: u64,
    pub not_before: u64,
    pub expires_at: u64,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
#[derive(Clone, PartialEq, Eq)]
enum State { Pending, Active([u8; 16]), Revoked }
#[derive(Clone)]
struct Entry {
    record: CapabilityRecord,
    mac: [u8; 32],
    control: ControlConnectionId,
    agent_slug: String,
    state: State,
}

/// One instance per gateway epoch. Keys and plaintext random capability material
/// are never serialized, logged, passed to the runtime or kept in an employee home.
pub struct Boundary {
    key: Zeroizing<[u8; 32]>,
    epoch: [u8; 16],
    live: Arc<dyn LiveAuthority>,
    entries: Mutex<BTreeMap<Sha256Digest, Entry>>,
}

fn random<const N: usize>() -> Result<[u8; N], VerificationError> {
    let mut bytes = [0; N];
    OsRng.try_fill_bytes(&mut bytes).map_err(|_| VerificationError::Unavailable)?;
    Ok(bytes)
}

pub fn now_seconds() -> Result<u64, VerificationError> {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs())
        .map_err(|_| VerificationError::Unavailable)
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.len() <= 128 && slug != "." && slug != ".."
        && slug.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn binding_matches(binding: &EmployeeBinding, principal: &Principal) -> bool {
    match (binding, principal) {
        (EmployeeBinding::Personal(owner), Principal::Human { user_id, .. }) => owner == user_id,
        (EmployeeBinding::Shared(tenant), Principal::Background { tenant_id, .. }) => tenant == tenant_id,
        // Membership of a human in a shared tenant is checked by LiveAuthority.
        (EmployeeBinding::Shared(_), Principal::Human { .. }) => true,
        _ => false,
    }
}

impl Boundary {
    pub fn new(live: Arc<dyn LiveAuthority>) -> Result<Arc<Self>, VerificationError> {
        Ok(Arc::new(Self { key: Zeroizing::new(random()?), epoch: random()?, live,
            entries: Mutex::new(BTreeMap::new()) }))
    }

    pub fn issue(&self, reservation: &Reservation, policy: GrantPolicy, now: u64)
        -> Result<CapabilityId, VerificationError> {
        let snapshot = self.live.snapshot(reservation.employee, reservation.run)?;
        let RunStatus::Reserved { expires_at_unix_ms } = snapshot.status else {
            return Err(VerificationError::Invalid);
        };
        if snapshot.employee != reservation.employee || snapshot.run != reservation.run
            || snapshot.control != reservation.control
            || expires_at_unix_ms != reservation.expires_at_unix_ms
            || now.checked_mul(1000).ok_or(VerificationError::InvalidTime)? >= expires_at_unix_ms
            || snapshot.principal != policy.principal || snapshot.binding != policy.binding
            || snapshot.policy_revision != policy.policy_revision || !valid_slug(&snapshot.agent_slug)
            || !binding_matches(&policy.binding, &policy.principal) {
            return Err(VerificationError::BindingMismatch);
        }
        let bearer = Zeroizing::new(random::<32>()?);
        let id = CapabilityId(Sha256Digest(Sha256::digest(bearer.as_ref()).into()));
        let record = CapabilityRecord {
            id, employee: reservation.employee, run: reservation.run.0, task: policy.task,
            principal: policy.principal, employee_binding: policy.binding, lane: policy.lane,
            tools: policy.tools, resources: policy.resources, policy_revision: policy.policy_revision,
            gateway_epoch: self.epoch, issued_at: now, not_before: policy.not_before,
            expires_at: policy.expires_at, nonce: random()?, jti: random()?,
        };
        crate::resources::validate_grants(&record)?;
        let bytes = record.canonical_bytes().map_err(|_| VerificationError::Invalid)?;
        let mut signer = HmacSha256::new_from_slice(self.key.as_ref()).map_err(|_| VerificationError::Unavailable)?;
        signer.update(&bytes);
        let mac = signer.finalize().into_bytes().into();
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        entries.retain(|_, e| e.record.expires_at > now);
        if entries.len() >= MAX_RECORDS || entries.contains_key(&id.0)
            || entries.values().any(|entry| entry.record.run == record.run) {
            return Err(VerificationError::Invalid);
        }
        entries.insert(id.0, Entry { record, mac, control: reservation.control,
            agent_slug: snapshot.agent_slug, state: State::Pending });
        Ok(id)
    }

    fn check_live(&self, entry: &Entry, now: u64) -> Result<(), VerificationError> {
        let r = &entry.record;
        if now < r.not_before || now >= r.expires_at { return Err(VerificationError::InvalidTime); }
        let live = self.live.snapshot(r.employee, RunHandle(r.run))?;
        if live.employee != r.employee || live.run.0 != r.run || live.control != entry.control
            || live.principal != r.principal || live.binding != r.employee_binding
            || live.policy_revision != r.policy_revision || live.agent_slug != entry.agent_slug
            || !matches!(live.status, RunStatus::Running) {
            return Err(VerificationError::Revoked);
        }
        Ok(())
    }

    /// Consume exactly the endpoint returned by a validated TakeIo reply. Neither
    /// the capability id nor reservation comes from the stream. There is no
    /// reconnect operation: another socket requires a fresh reserved run.
    #[cfg(target_os = "linux")]
    pub fn activate(self: &Arc<Self>, id: CapabilityId, reservation: &Reservation,
        delivery: crate::socket::McpDelivery, now: u64) -> Result<Connection, VerificationError> {
        if delivery.run != reservation.run || delivery.control != reservation.control {
            return Err(VerificationError::BindingMismatch);
        }
        let mut connection = self.activate_inner(id, reservation, now)?;
        connection.socket = Some(delivery.socket);
        Ok(connection)
    }

    #[cfg(any(target_os = "linux", test))]
    fn activate_inner(self: &Arc<Self>, id: CapabilityId, reservation: &Reservation, now: u64)
        -> Result<Connection, VerificationError> {
        let snapshot = self.entries.lock().map_err(|_| VerificationError::Unavailable)?
            .get(&id.0).cloned().ok_or(VerificationError::Invalid)?;
        if snapshot.state != State::Pending || snapshot.record.run != reservation.run.0
            || snapshot.record.employee != reservation.employee || snapshot.control != reservation.control {
            return Err(VerificationError::BindingMismatch);
        }
        // Never call the external live store while holding the capability lock.
        let live = self.check_live(&snapshot, now);
        let nonce = random()?;
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        let entry = entries.get_mut(&id.0).ok_or(VerificationError::Revoked)?;
        if entry.state != State::Pending { return Err(VerificationError::Revoked); }
        if let Err(error) = live { entry.state = State::Revoked; return Err(error); }
        entry.state = State::Active(nonce);
        Ok(Connection { boundary: self.clone(), id, nonce,
            #[cfg(target_os = "linux")] socket: None })
    }

    pub fn revoke_run(&self, run: RunHandle) -> Result<(), VerificationError> {
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        for entry in entries.values_mut().filter(|e| e.record.run == run.0) { entry.state = State::Revoked; }
        Ok(())
    }

    pub fn revoke_control(&self, control: ControlConnectionId) -> Result<(), VerificationError> {
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        for entry in entries.values_mut().filter(|e| e.control == control) { entry.state = State::Revoked; }
        Ok(())
    }

    pub fn revoke_all(&self) -> Result<(), VerificationError> {
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        for entry in entries.values_mut() { entry.state = State::Revoked; }
        Ok(())
    }

    /// Run from the gateway's lifecycle sweep even when no socket has arrived.
    /// A queued/starting run may outlive its consumed reservation, but an unused
    /// reservation, dead control connection or unreadable authority never may.
    pub fn sweep(&self, now: u64) -> Result<(), VerificationError> {
        let entries: Vec<_> = self.entries.lock().map_err(|_| VerificationError::Unavailable)?
            .iter().map(|(id, entry)| (*id, entry.clone())).collect();
        for (id, entry) in entries {
            if entry.state == State::Revoked { continue; }
            let valid = if now >= entry.record.expires_at { false }
            else if matches!(entry.state, State::Active(_)) { self.check_live(&entry, now).is_ok() }
            else { self.live.snapshot(entry.record.employee, RunHandle(entry.record.run)).is_ok_and(|s| {
                s.run.0 == entry.record.run && s.employee == entry.record.employee && s.control == entry.control
                    && s.principal == entry.record.principal && s.binding == entry.record.employee_binding
                    && s.policy_revision == entry.record.policy_revision && s.agent_slug == entry.agent_slug
                    && match s.status {
                        RunStatus::Reserved { expires_at_unix_ms } => now.checked_mul(1000).is_some_and(|n| n < expires_at_unix_ms),
                        RunStatus::Queued(_) | RunStatus::Starting | RunStatus::Running => true,
                        RunStatus::Ended(_) => false,
                    }
            }) };
            if !valid {
                let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
                if let Some(current) = entries.get_mut(&id) {
                    if current.state == entry.state { current.state = State::Revoked; }
                }
            }
        }
        Ok(())
    }
}

impl CapabilityVerifier for Boundary {
    fn verify(&self, record: &CapabilityRecord, proof: CapabilityProof<'_>, binding: &ConnectionBinding,
        now: u64) -> Result<RequestContext, VerificationError> {
        let bytes = record.canonical_bytes().map_err(|_| VerificationError::Invalid)?;
        let mut verifier = HmacSha256::new_from_slice(self.key.as_ref()).map_err(|_| VerificationError::Unavailable)?;
        verifier.update(&bytes);
        verifier.verify_slice(proof.0).map_err(|_| VerificationError::Invalid)?;
        if record.gateway_epoch != self.epoch || binding.gateway_epoch != self.epoch
            || binding.capability != record.id || binding.employee != record.employee || binding.run != record.run {
            return Err(VerificationError::BindingMismatch);
        }
        let snapshot = self.entries.lock().map_err(|_| VerificationError::Unavailable)?
            .get(&record.id.0).cloned().ok_or(VerificationError::Revoked)?;
        if !matches!(snapshot.state, State::Active(_)) || &snapshot.record != record {
            return Err(VerificationError::Revoked);
        }
        let live = self.check_live(&snapshot, now);
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        let entry = entries.get_mut(&record.id.0).ok_or(VerificationError::Revoked)?;
        if entry.state != snapshot.state { return Err(VerificationError::Revoked); }
        if let Err(error) = live { entry.state = State::Revoked; return Err(error); }
        Ok(RequestContext::from_verified_record(record.clone()))
    }
}

/// Non-cloneable, non-deserializable connection authority. Dropping it revokes
/// the run's capability even after an I/O error, cancellation or task panic.
pub struct Connection {
    boundary: Arc<Boundary>,
    id: CapabilityId,
    nonce: [u8; 16],
    #[cfg(target_os = "linux")]
    pub(crate) socket: Option<std::os::unix::net::UnixStream>,
}
impl Connection {
    pub fn context(&self, now: u64) -> Result<RequestContext, VerificationError> {
        verified_context(&self.boundary, self.id, self.nonce, now)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn verifier(&self) -> ConnectionVerifier {
        ConnectionVerifier { boundary: self.boundary.clone(), id: self.id, nonce: self.nonce,
            serial: Arc::new(tokio::sync::Semaphore::new(1)) }
    }
    pub fn agent_slug(&self) -> Result<String, VerificationError> {
        let entries = self.boundary.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        Ok(entries.get(&self.id.0).ok_or(VerificationError::Revoked)?.agent_slug.clone())
    }
}

fn verified_context(boundary: &Arc<Boundary>, id: CapabilityId, nonce: [u8; 16], now: u64)
    -> Result<RequestContext, VerificationError> {
        let (record, mac) = {
            let entries = boundary.entries.lock().map_err(|_| VerificationError::Unavailable)?;
            let entry = entries.get(&id.0).ok_or(VerificationError::Revoked)?;
            if !matches!(entry.state, State::Active(current) if current == nonce) {
                return Err(VerificationError::Revoked);
            }
            (entry.record.clone(), entry.mac)
        };
        let binding = ConnectionBinding { capability: record.id, employee: record.employee,
            run: record.run, gateway_epoch: record.gateway_epoch };
        boundary.verify(&record, CapabilityProof(&mac), &binding, now)
}

/// Blocking authority I/O shares one bounded pool. Cancellation or timeout
/// detaches a running job, but the job retains its permit until it really ends.
pub async fn verify_blocking<T, F>(work: F) -> Result<T, VerificationError>
where T: Send + 'static, F: FnOnce() -> Result<T, VerificationError> + Send + 'static {
    use std::{sync::OnceLock, time::Duration};
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let slots = SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(16))).clone();
    let permit = tokio::time::timeout(Duration::from_secs(2), slots.acquire_owned()).await
        .map_err(|_| VerificationError::Unavailable)?.map_err(|_| VerificationError::Unavailable)?;
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    });
    tokio::time::timeout(Duration::from_secs(10), job).await
        .map_err(|_| VerificationError::Unavailable)?.map_err(|_| VerificationError::Unavailable)?
}

#[cfg(target_os = "linux")]
pub(crate) struct ConnectionVerifier {
    boundary: Arc<Boundary>, id: CapabilityId, nonce: [u8; 16],
    serial: Arc<tokio::sync::Semaphore>,
}
#[cfg(target_os = "linux")]
impl ConnectionVerifier {
    pub(crate) async fn context(&self) -> Result<RequestContext, VerificationError> {
        let permit = tokio::time::timeout(std::time::Duration::from_secs(2), self.serial.clone().acquire_owned())
            .await.map_err(|_| VerificationError::Unavailable)?.map_err(|_| VerificationError::Unavailable)?;
        let (boundary, id, nonce) = (self.boundary.clone(), self.id, self.nonce);
        verify_blocking(move || {
            let _permit = permit;
            let context = verified_context(&boundary, id, nonce, now_seconds()?)?;
            // Root readback may have consumed the remaining capability TTL.
            let now = now_seconds()?;
            if now < context.record().not_before || now >= context.record().expires_at {
                return Err(VerificationError::InvalidTime);
            }
            Ok(context)
        }).await
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.boundary.entries.lock() {
            if let Some(entry) = entries.get_mut(&self.id.0) { entry.state = State::Revoked; }
        }
    }
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "authority_async_tests.rs"]
mod async_tests;
