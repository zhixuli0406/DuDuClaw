use std::{collections::BTreeMap, sync::{Arc, Mutex, Weak}, time::{SystemTime, UNIX_EPOCH}};
use duduclaw_isolation::{bounded::{List, Text}, capability::*, execution::RunHandle,
    ids::{EmployeeId, Sha256Digest}, ipc::{ControlConnectionId, RunStatus}};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;
pub use crate::launch_steps::{BoundaryActivateStep, ContextStep, EncodeCheck, IssueStep};

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
    revoked: tokio::sync::watch::Sender<bool>,
    /// R8z-5: the live check of a request-context read that revoked this
    /// entry (diagnosis only; first cause wins). `None` for every other
    /// revocation (release, sweep, control loss, connection drop).
    cause: Option<ContextStep>,
    /// R8z-7: when the latest full live check of this entry that succeeded
    /// STARTED, among the checks of an activated connection's own reads (its
    /// request contexts and live watch, `verify_live`); activation does not
    /// count, so an active entry that is not being served is checked by every
    /// sweep. Read only by [`Boundary::sweep`], never as authority for a request.
    live_ok: Option<std::time::Instant>,
}
impl Entry {
    /// R8z-7: record a successful full live check that started at `started`.
    fn checked_at(&mut self, started: std::time::Instant) {
        if self.live_ok.is_none_or(|at| at < started) { self.live_ok = Some(started); }
    }
}

/// R8z-7: the gateway's lifecycle sweep runs every second
/// (`duduclaw-gateway` `isolation_mcp.rs`). An active entry whose own full
/// live check started less than this long ago and succeeded was already
/// checked within the sweep's period, so the sweep does not repeat it.
const SWEEP_REUSE: std::time::Duration = std::time::Duration::from_secs(1);

/// R8z-7: the live watch of a served connection: a full verification starts
/// no later than this long after the connection's previous one completed
/// (any of them: admission, a request's, or the watch's own).
#[cfg(target_os = "linux")]
pub(crate) const WATCH_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);
impl Entry {
    fn revoke(&mut self) {
        self.state = State::Revoked;
        // send_replace preserves revocation even before a receiver subscribes.
        self.revoked.send_replace(true);
    }
}

/// One instance per gateway epoch. Keys and plaintext random capability material
/// are never serialized, logged, passed to the runtime or kept in an employee home.
pub struct Boundary {
    weak: Weak<Self>,
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
        let key = Zeroizing::new(random()?);
        let epoch = random()?;
        Ok(Arc::new_cyclic(|weak| Self { weak: weak.clone(), key, epoch, live,
            entries: Mutex::new(BTreeMap::new()) }))
    }

    pub fn issue(&self, reservation: &Reservation, policy: GrantPolicy, now: u64)
        -> Result<CapabilityId, VerificationError> {
        self.issue_checked(reservation, policy, now).map_err(|(_, error)| error)
    }

    /// R8z-3: [`Self::issue`] with the fixed check that refused. The checks,
    /// their order and every returned error are exactly `issue`'s (the former
    /// single condition chain is split in its own order).
    pub fn issue_checked(&self, reservation: &Reservation, policy: GrantPolicy, now: u64)
        -> Result<CapabilityId, (IssueStep, VerificationError)> {
        let mismatch = |step: IssueStep| (step, VerificationError::BindingMismatch);
        let snapshot = self.live.snapshot(reservation.employee, reservation.run)
            .map_err(|error| (IssueStep::LiveSnapshot, error))?;
        let RunStatus::Reserved { expires_at_unix_ms } = snapshot.status else {
            return Err((IssueStep::NotReserved, VerificationError::Invalid));
        };
        if snapshot.employee != reservation.employee || snapshot.run != reservation.run
            || snapshot.control != reservation.control {
            return Err(mismatch(IssueStep::Reservation));
        }
        if expires_at_unix_ms != reservation.expires_at_unix_ms { return Err(mismatch(IssueStep::Expiry)); }
        if now.checked_mul(1000).ok_or((IssueStep::Time, VerificationError::InvalidTime))? >= expires_at_unix_ms {
            return Err(mismatch(IssueStep::Time));
        }
        if snapshot.principal != policy.principal { return Err(mismatch(IssueStep::Principal)); }
        if snapshot.binding != policy.binding { return Err(mismatch(IssueStep::Binding)); }
        if snapshot.policy_revision != policy.policy_revision { return Err(mismatch(IssueStep::PolicyRevision)); }
        if !valid_slug(&snapshot.agent_slug) { return Err(mismatch(IssueStep::AgentSlug)); }
        if !binding_matches(&policy.binding, &policy.principal) { return Err(mismatch(IssueStep::BindingPrincipal)); }
        let random_step = |error: VerificationError| (IssueStep::Random, error);
        let bearer = Zeroizing::new(random::<32>().map_err(random_step)?);
        let id = CapabilityId(Sha256Digest(Sha256::digest(bearer.as_ref()).into()));
        // R8z-4: the grant's `not_before` is stamped when the per-run
        // authorization is issued, before the run is reserved; `now` is this
        // (later) issue. The capability is never valid before it is issued,
        // so its window starts at the later of the two. This only narrows the
        // grant's window (`expires_at` is the grant's); a grant whose window
        // has passed (`now >= expires_at`) still fails the interval rule.
        let record = CapabilityRecord {
            id, employee: reservation.employee, run: reservation.run.0, task: policy.task,
            principal: policy.principal, employee_binding: policy.binding, lane: policy.lane,
            tools: policy.tools, resources: policy.resources, policy_revision: policy.policy_revision,
            gateway_epoch: self.epoch, issued_at: now, not_before: policy.not_before.max(now),
            expires_at: policy.expires_at, nonce: random().map_err(random_step)?, jti: random().map_err(random_step)?,
        };
        crate::resources::validate_grants(&record).map_err(|error| (IssueStep::Grants, error))?;
        let bytes = record.canonical_bytes()
            .map_err(|_| (IssueStep::Encode(EncodeCheck::of(&record)), VerificationError::Invalid))?;
        let mut signer = HmacSha256::new_from_slice(self.key.as_ref())
            .map_err(|_| (IssueStep::Signer, VerificationError::Unavailable))?;
        signer.update(&bytes);
        let mac = signer.finalize().into_bytes().into();
        let mut entries = self.entries.lock().map_err(|_| (IssueStep::StateLock, VerificationError::Unavailable))?;
        entries.retain(|_, e| {
            if e.record.expires_at > now { true } else { e.revoke(); false }
        });
        if entries.len() >= MAX_RECORDS { return Err((IssueStep::Capacity, VerificationError::Invalid)); }
        if entries.contains_key(&id.0) || entries.values().any(|entry| entry.record.run == record.run) {
            return Err((IssueStep::Duplicate, VerificationError::Invalid));
        }
        entries.insert(id.0, Entry { record, mac, control: reservation.control,
            agent_slug: snapshot.agent_slug, state: State::Pending, revoked: tokio::sync::watch::channel(false).0,
            cause: None, live_ok: None });
        Ok(id)
    }

    fn check_live(&self, entry: &Entry, now: u64) -> Result<(), VerificationError> {
        self.check_live_checked(entry, now).map_err(|(_, error)| error)
    }

    /// R8z-3: [`Self::check_live`] with the fixed check that refused.
    fn check_live_checked(&self, entry: &Entry, now: u64) -> Result<(), (BoundaryActivateStep, VerificationError)> {
        let r = &entry.record;
        if now < r.not_before || now >= r.expires_at {
            return Err((BoundaryActivateStep::Time, VerificationError::InvalidTime));
        }
        let live = self.live.snapshot(r.employee, RunHandle(r.run))
            .map_err(|error| (BoundaryActivateStep::LiveSnapshot, error))?;
        if live.employee != r.employee || live.run.0 != r.run || live.control != entry.control
            || live.principal != r.principal || live.binding != r.employee_binding
            || live.policy_revision != r.policy_revision || live.agent_slug != entry.agent_slug
            || !matches!(live.status, RunStatus::Running) {
            return Err((BoundaryActivateStep::LiveMismatch, VerificationError::Revoked));
        }
        Ok(())
    }

    /// Consume exactly the endpoint returned by a validated TakeIo reply. Neither
    /// the capability id nor reservation comes from the stream. There is no
    /// reconnect operation: another socket requires a fresh reserved run.
    #[cfg(target_os = "linux")]
    pub fn activate(self: &Arc<Self>, id: CapabilityId, reservation: &Reservation,
        delivery: crate::socket::McpDelivery, now: u64) -> Result<Connection, VerificationError> {
        self.activate_checked(id, reservation, delivery, now).map_err(|(_, error)| error)
    }

    /// R8z-3: [`Self::activate`] with the fixed check that refused (same
    /// checks, order, errors and revocation as `activate`).
    #[cfg(target_os = "linux")]
    pub fn activate_checked(self: &Arc<Self>, id: CapabilityId, reservation: &Reservation,
        delivery: crate::socket::McpDelivery, now: u64) -> Result<Connection, (BoundaryActivateStep, VerificationError)> {
        if delivery.run != reservation.run || delivery.control != reservation.control {
            return Err((BoundaryActivateStep::Delivery, VerificationError::BindingMismatch));
        }
        let mut connection = self.activate_inner_checked(id, reservation, now)?;
        connection.socket = Some(delivery.socket);
        Ok(connection)
    }

    #[cfg(test)]
    fn activate_inner(self: &Arc<Self>, id: CapabilityId, reservation: &Reservation, now: u64)
        -> Result<Connection, VerificationError> {
        self.activate_inner_checked(id, reservation, now).map_err(|(_, error)| error)
    }

    #[cfg(any(target_os = "linux", test))]
    fn activate_inner_checked(self: &Arc<Self>, id: CapabilityId, reservation: &Reservation, now: u64)
        -> Result<Connection, (BoundaryActivateStep, VerificationError)> {
        use BoundaryActivateStep as S;
        let snapshot = self.entries.lock().map_err(|_| (S::StateLock, VerificationError::Unavailable))?
            .get(&id.0).cloned().ok_or((S::Unknown, VerificationError::Invalid))?;
        if snapshot.state != State::Pending || snapshot.record.run != reservation.run.0
            || snapshot.record.employee != reservation.employee || snapshot.control != reservation.control {
            return Err((S::Binding, VerificationError::BindingMismatch));
        }
        // Never call the external live store while holding the capability lock.
        let live = self.check_live_checked(&snapshot, now);
        let nonce = random().map_err(|error| (S::Random, error))?;
        let mut entries = self.entries.lock().map_err(|_| (S::StateLock, VerificationError::Unavailable))?;
        let entry = entries.get_mut(&id.0).ok_or((S::Revoked, VerificationError::Revoked))?;
        if entry.state != State::Pending { return Err((S::Revoked, VerificationError::Revoked)); }
        if let Err(refusal) = live { entry.revoke(); return Err(refusal); }
        entry.state = State::Active(nonce);
        Ok(Connection { boundary: self.clone(), id, nonce,
            #[cfg(target_os = "linux")] socket: None,
            #[cfg(target_os = "linux")] watch_period: WATCH_PERIOD })
    }

    pub fn revoke_run(&self, run: RunHandle) -> Result<(), VerificationError> {
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        for entry in entries.values_mut().filter(|e| e.record.run == run.0) { entry.revoke(); }
        Ok(())
    }

    pub fn revoke_control(&self, control: ControlConnectionId) -> Result<(), VerificationError> {
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        for entry in entries.values_mut().filter(|e| e.control == control) { entry.revoke(); }
        Ok(())
    }

    pub fn revoke_all(&self) -> Result<(), VerificationError> {
        let mut entries = self.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        for entry in entries.values_mut() { entry.revoke(); }
        Ok(())
    }

    /// Run from the gateway's lifecycle sweep even when no socket has arrived.
    /// A queued/starting run may outlive its consumed reservation, but an unused
    /// reservation, dead control connection or unreadable authority never may.
    ///
    /// R8z-7: an active entry whose own full live check (a served
    /// connection's watch or request read) started less than [`SWEEP_REUSE`]
    /// ago and succeeded is not checked again here: it has been fully checked
    /// within the sweep's period, and that check revokes on failure exactly
    /// as this one would. Expiry is still checked on every pass.
    pub fn sweep(&self, now: u64) -> Result<(), VerificationError> {
        let entries: Vec<_> = self.entries.lock().map_err(|_| VerificationError::Unavailable)?
            .iter().map(|(id, entry)| (*id, entry.clone())).collect();
        for (id, entry) in entries {
            if entry.state == State::Revoked { continue; }
            let valid = if now >= entry.record.expires_at { false }
            else if matches!(entry.state, State::Active(_)) {
                entry.live_ok.is_some_and(|at| at.elapsed() < SWEEP_REUSE) || self.check_live(&entry, now).is_ok()
            }
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
                    if current.state == entry.state { current.revoke(); }
                }
            }
        }
        Ok(())
    }
}

impl Boundary {
    /// R8z-5: [`CapabilityVerifier::verify`] with the fixed check that
    /// refused (same checks, order, errors and revocation as `verify`).
    ///
    /// R8z-7: `dispatch` arms the returned context's single-use dispatch
    /// proof ([`RequestContext::take_dispatch_proof`]); only the MCP server's
    /// pre-dispatch read of a request sets it.
    fn verify_checked(&self, record: &CapabilityRecord, proof: CapabilityProof<'_>, binding: &ConnectionBinding,
        now: u64, dispatch: bool) -> Result<RequestContext, (ContextStep, VerificationError)> {
        use ContextStep as S;
        let revoked = (S::Revoked, VerificationError::Revoked);
        let bytes = record.canonical_bytes().map_err(|_| (S::Proof, VerificationError::Invalid))?;
        let mut verifier = HmacSha256::new_from_slice(self.key.as_ref())
            .map_err(|_| (S::Proof, VerificationError::Unavailable))?;
        verifier.update(&bytes);
        verifier.verify_slice(proof.0).map_err(|_| (S::Proof, VerificationError::Invalid))?;
        if record.gateway_epoch != self.epoch || binding.gateway_epoch != self.epoch
            || binding.capability != record.id || binding.employee != record.employee || binding.run != record.run {
            return Err((S::Proof, VerificationError::BindingMismatch));
        }
        let snapshot = self.entries.lock().map_err(|_| (S::StateLock, VerificationError::Unavailable))?
            .get(&record.id.0).cloned().ok_or(revoked)?;
        if !matches!(snapshot.state, State::Active(_)) || &snapshot.record != record {
            return Err(revoked);
        }
        let started = std::time::Instant::now();
        let live = self.check_live_checked(&snapshot, now);
        let mut entries = self.entries.lock().map_err(|_| (S::StateLock, VerificationError::Unavailable))?;
        let entry = entries.get_mut(&record.id.0).ok_or(revoked)?;
        if entry.state != snapshot.state { return Err(revoked); }
        if let Err((step, error)) = live {
            // `check_live_checked` refuses only at these three checks.
            let step = match step {
                BoundaryActivateStep::Time => S::Time,
                BoundaryActivateStep::LiveSnapshot => S::LiveSnapshot,
                _ => S::LiveMismatch,
            };
            // Recorded under the lock before the revocation is signalled, so
            // a session woken by that signal reads it (diagnosis only).
            if entry.cause.is_none() { entry.cause = Some(step); }
            entry.revoke();
            return Err((step, error));
        }
        let State::Active(nonce) = entry.state else { return Err(revoked); };
        entry.checked_at(started);
        // HX5 [F7]: this context came from the run's own activated MCP
        // connection; the binding's run (== record.run, checked above) is the
        // transport run that in-run helper calls must target.
        Ok(RequestContext::with_transport(record.clone(), Arc::new(RequestAuthority {
            boundary: self.weak.clone(), id: record.id, nonce,
            dispatch: std::sync::atomic::AtomicBool::new(dispatch),
        }), binding.run))
    }
}

impl CapabilityVerifier for Boundary {
    fn verify(&self, record: &CapabilityRecord, proof: CapabilityProof<'_>, binding: &ConnectionBinding,
        now: u64) -> Result<RequestContext, VerificationError> {
        self.verify_checked(record, proof, binding, now, false).map_err(|(_, error)| error)
    }
}

/// A request never owns the connection. Its live verifier becomes unusable
/// when the connection is dropped or the gateway authority disappears.
struct RequestAuthority {
    boundary: Weak<Boundary>,
    id: CapabilityId,
    nonce: [u8; 16],
    /// R8z-7: the single-use dispatch proof (armed only by the server's
    /// pre-dispatch read; consumed by the turn resolution of that dispatch,
    /// or by the server when the request's handler ends).
    dispatch: std::sync::atomic::AtomicBool,
}
impl LiveRequestVerifier for RequestAuthority {
    fn verify_live(&self, record: &CapabilityRecord, now: u64) -> Result<(), VerificationError> {
        let boundary = self.boundary.upgrade().ok_or(VerificationError::Revoked)?;
        let snapshot = boundary.entries.lock().map_err(|_| VerificationError::Unavailable)?
            .get(&self.id.0).cloned().ok_or(VerificationError::Revoked)?;
        if snapshot.state != State::Active(self.nonce) || &snapshot.record != record {
            return Err(VerificationError::Revoked);
        }
        let started = std::time::Instant::now();
        let live = boundary.check_live(&snapshot, now);
        let mut entries = boundary.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        let entry = entries.get_mut(&self.id.0).ok_or(VerificationError::Revoked)?;
        if entry.state != snapshot.state { return Err(VerificationError::Revoked); }
        if let Err(error) = live { entry.revoke(); return Err(error); }
        entry.checked_at(started);
        Ok(())
    }
    fn take_dispatch_proof(&self) -> bool {
        self.dispatch.swap(false, std::sync::atomic::Ordering::AcqRel)
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
    /// R8z-7: the live watch period of this connection's session
    /// ([`WATCH_PERIOD`]; tests lengthen it to count reads exactly).
    #[cfg(target_os = "linux")]
    pub(crate) watch_period: std::time::Duration,
}
impl Connection {
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn revocation(&self) -> Result<tokio::sync::watch::Receiver<bool>, VerificationError> {
        let entries = self.boundary.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        let entry = entries.get(&self.id.0).ok_or(VerificationError::Revoked)?;
        if entry.state != State::Active(self.nonce) { return Err(VerificationError::Revoked); }
        Ok(entry.revoked.subscribe())
    }
    pub fn context(&self, now: u64) -> Result<RequestContext, VerificationError> {
        verified_context(&self.boundary, self.id, self.nonce, now)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn verifier(&self) -> ConnectionVerifier {
        ConnectionVerifier { boundary: self.boundary.clone(), id: self.id, nonce: self.nonce,
            serial: Arc::new(tokio::sync::Semaphore::new(1)),
            // As before R8z-7, the watch's first read is one period after the
            // session starts (or after admission's read, if that is later).
            completed: Mutex::new(tokio::time::Instant::now()), period: self.watch_period }
    }
    pub fn agent_slug(&self) -> Result<String, VerificationError> {
        let entries = self.boundary.entries.lock().map_err(|_| VerificationError::Unavailable)?;
        Ok(entries.get(&self.id.0).ok_or(VerificationError::Revoked)?.agent_slug.clone())
    }
    /// R8z-5: the live check that revoked this connection's capability during
    /// a request-context read, if one did (diagnosis only; never authority).
    #[cfg(target_os = "linux")]
    pub(crate) fn revocation_cause(&self) -> Option<ContextStep> {
        self.boundary.entries.lock().ok()?.get(&self.id.0)?.cause
    }
}

fn verified_context(boundary: &Arc<Boundary>, id: CapabilityId, nonce: [u8; 16], now: u64)
    -> Result<RequestContext, VerificationError> {
    verified_context_checked(boundary, id, nonce, now, false).map_err(|(_, error)| error)
}

/// R8z-5: [`verified_context`] with the fixed check that refused.
/// R8z-7: `dispatch` as for [`Boundary::verify_checked`].
fn verified_context_checked(boundary: &Arc<Boundary>, id: CapabilityId, nonce: [u8; 16], now: u64, dispatch: bool)
    -> Result<RequestContext, (ContextStep, VerificationError)> {
        let (record, mac) = {
            let entries = boundary.entries.lock().map_err(|_| (ContextStep::StateLock, VerificationError::Unavailable))?;
            let entry = entries.get(&id.0).ok_or((ContextStep::Revoked, VerificationError::Revoked))?;
            if !matches!(entry.state, State::Active(current) if current == nonce) {
                return Err((ContextStep::Revoked, VerificationError::Revoked));
            }
            (entry.record.clone(), entry.mac)
        };
        let binding = ConnectionBinding { capability: record.id, employee: record.employee,
            run: record.run, gateway_epoch: record.gateway_epoch };
        boundary.verify_checked(&record, CapabilityProof(&mac), &binding, now, dispatch)
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

/// One full live verification of a served connection at a time (`serial`).
///
/// R8z-7: every read of the session — admission, a request's pre-dispatch
/// read, the read after its handler, and the live watch — is a FULL
/// verification that starts after the caller asked for it; none reuses a
/// result. The only sharing is in the watch's direction: the watch needs "a
/// full verification at most [`WATCH_PERIOD`] after the previous one
/// completed", and a request's read is one, so the watch waits a period from
/// the latest completed read of ANY kind instead of running its own right
/// before or after a request's (before R8z-7 it ran back to back with them,
/// holding `serial` while the request's read queued behind it).
#[cfg(target_os = "linux")]
pub(crate) struct ConnectionVerifier {
    boundary: Arc<Boundary>, id: CapabilityId, nonce: [u8; 16],
    serial: Arc<tokio::sync::Semaphore>,
    /// R8z-7: when this connection's latest SUCCESSFUL read completed.
    completed: Mutex<tokio::time::Instant>,
    period: std::time::Duration,
}
#[cfg(target_os = "linux")]
type Permit = tokio::sync::OwnedSemaphorePermit;
#[cfg(target_os = "linux")]
impl ConnectionVerifier {
    #[cfg(test)]
    pub(crate) async fn context(&self) -> Result<RequestContext, VerificationError> {
        self.context_checked().await.map_err(|(_, error)| error)
    }

    /// R8z-5: the request context with the fixed check that refused. The
    /// checks, their order and every error are the former `context`'s; the
    /// blocking job returns its step as a value (`Ok(Err(..))`), an `Err` is
    /// only ever the blocking-pool wrapper's.
    pub(crate) async fn context_checked(&self) -> Result<RequestContext, (ContextStep, VerificationError)> {
        let permit = self.acquire().await?;
        self.verify_with(permit, false).await
    }

    /// R8z-7: [`Self::context_checked`] for the request about to be
    /// dispatched (call it only after that request's frame was read). Same
    /// checks and errors; the returned context also carries the single-use
    /// dispatch proof, so the dispatch's own turn resolution does not repeat
    /// this full verification (`RequestContext::take_dispatch_proof`).
    pub(crate) async fn dispatch_context_checked(&self) -> Result<RequestContext, (ContextStep, VerificationError)> {
        let permit = self.acquire().await?;
        self.verify_with(permit, true).await
    }

    /// R8z-7: one step of the live watch. Resolves after the next full
    /// verification the watch itself ran (`Err` exactly as
    /// [`Self::context_checked`]); never runs one sooner than a period after
    /// the latest completed read. The due time is checked again once the
    /// connection's slot is held: a read that completed while the watch
    /// waited for the slot moves the next watch read a period later.
    pub(crate) async fn watch_checked(&self) -> Result<(), (ContextStep, VerificationError)> {
        loop {
            tokio::time::sleep_until(self.due()?).await;
            let permit = self.acquire().await?;
            if tokio::time::Instant::now() < self.due()? {
                drop(permit);
                continue;
            }
            return self.verify_with(permit, false).await.map(|_| ());
        }
    }

    fn due(&self) -> Result<tokio::time::Instant, (ContextStep, VerificationError)> {
        let completed = self.completed.lock().map_err(|_| (ContextStep::StateLock, VerificationError::Unavailable))?;
        Ok(*completed + self.period)
    }

    async fn acquire(&self) -> Result<Permit, (ContextStep, VerificationError)> {
        use ContextStep as S;
        tokio::time::timeout(std::time::Duration::from_secs(2), self.serial.clone().acquire_owned())
            .await.map_err(|_| (S::SerialWait, VerificationError::Unavailable))?
            .map_err(|_| (S::SerialWait, VerificationError::Unavailable))
    }

    async fn verify_with(&self, permit: Permit, dispatch: bool) -> Result<RequestContext, (ContextStep, VerificationError)> {
        use ContextStep as S;
        let (boundary, id, nonce) = (self.boundary.clone(), self.id, self.nonce);
        let checked = verify_blocking(move || {
            let _permit = permit;
            Ok((|| -> Result<RequestContext, (ContextStep, VerificationError)> {
                let now = now_seconds().map_err(|error| (S::Clock, error))?;
                let context = verified_context_checked(&boundary, id, nonce, now, dispatch)?;
                // Root readback may have consumed the remaining capability TTL.
                let now = now_seconds().map_err(|error| (S::Clock, error))?;
                if now < context.record().not_before || now >= context.record().expires_at {
                    return Err((S::ExpiredAfter, VerificationError::InvalidTime));
                }
                Ok(context)
            })())
        }).await;
        let result = match checked {
            Ok(result) => result,
            Err(error) => Err((S::Blocking, error)),
        };
        if result.is_ok() {
            // Only paces the watch; a poisoned lock fails its next step.
            if let Ok(mut completed) = self.completed.lock() {
                *completed = (*completed).max(tokio::time::Instant::now());
            }
        }
        result
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.boundary.entries.lock() {
            if let Some(entry) = entries.get_mut(&self.id.0) { entry.revoke(); }
        }
    }
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "authority_async_tests.rs"]
mod async_tests;

/// R8z-7: how many full live verifications each request and the watch run.
#[cfg(test)]
#[path = "authority_pacing_tests.rs"]
mod pacing_tests;
