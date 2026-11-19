//! Concrete A/D run lifecycle. Construction is restricted to host-owned plans;
//! no argv/environment from the native command builder is elevated into policy.
use std::{collections::BTreeMap, os::{fd::OwnedFd, unix::net::UnixStream}, sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}}};
use duduclaw_execution::{command::{NativeCommandSnapshot, ProtectedLaunch}, SpawndBackend};
use duduclaw_isolation::{bounded::Text, capability::CapabilityId, execution::*, ids::{EmployeeId, RunId}};
use crate::{Boundary, GrantPolicy, Reservation, VerificationError, config::{prepare_runtime_bridge_checked, runtime_projection, RuntimeBridgeRegistry},
    server::Dispatcher, socket::McpDelivery};
pub use crate::launch_steps::{ActivateStep, PrepareStep, SessionStep};

/// R8z-5: called once, with its fixed step, when a run's MCP session ends
/// abnormally before the run is released (see [`ProtectedMcpLaunch::observe_sessions`]).
pub type SessionObserver = Arc<dyn Fn(SessionStep) + Send + Sync>;

struct Pending {
    reservation: Reservation,
    capability: CapabilityId,
    runtime: RuntimeKind,
    projection_digest: duduclaw_isolation::ids::Sha256Digest,
    boundary: Arc<Boundary>,
    armed: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.armed { let _ = self.boundary.revoke_run(self.reservation.run); }
    }
}
struct Active {
    task: tokio::task::JoinHandle<Result<(), crate::VerificationError>>,
    /// R8z-5: set before the run's release revokes it; the session's end is
    /// then the release, not an abnormal end.
    released: Arc<AtomicBool>,
}

/// R8z-5: the serve task's body. `serve` is awaited unchanged and its
/// result returned as before; when it ended abnormally (an `Err`, or an
/// abnormal close) and the run was not released meanwhile, the observer is
/// called exactly once with the step. A normal end, or a release (which also
/// aborts this task), calls nothing.
async fn observed_session(session: impl std::future::Future<Output = crate::server::SessionOutcome>,
    released: Arc<AtomicBool>, observer: Option<SessionObserver>) -> Result<(), VerificationError> {
    let outcome = session.await;
    let step = match &outcome { Ok(step) => *step, Err((step, _)) => Some(*step) };
    if let (Some(step), Some(observer)) = (step, observer) {
        if !released.load(Ordering::Acquire) { observer(step); }
    }
    outcome.map(|_| ()).map_err(|(_, error)| error)
}

/// One host-authorized invocation, including its bounded input streams. The
/// `plan.run` placeholder is replaced only with A's authenticated reservation.
/// A second launch cannot reuse inputs or capabilities. Native runtime configs
/// are accepted only after D verifies B's exact-version publication receipt.
/// The owner must retain the gateway Boundary sweep and live authority service.
pub struct ProtectedMcpLaunch {
    backend: Arc<SpawndBackend>,
    boundary: Arc<Boundary>,
    registry: Arc<dyn RuntimeBridgeRegistry>,
    dispatcher: Arc<dyn Dispatcher>,
    employee: EmployeeId,
    task: Option<Text<128>>,
    origin: WaitOrigin,
    caller: Option<EmployeeId>,
    prepared: Mutex<Option<(SpawnPolicy, GrantPolicy)>>,
    revoked: AtomicBool,
    pending: Mutex<BTreeMap<RunId, Pending>>,
    active: Mutex<BTreeMap<RunId, Active>>,
    sessions: OnceLock<SessionObserver>,
}
impl ProtectedMcpLaunch {
    /// R8z-5: install the observer of this launch's MCP session ends (once;
    /// `false` if one is already installed). Sessions activated before it is
    /// installed are not observed. Nothing in this crate logs.
    pub fn observe_sessions(&self, observer: SessionObserver) -> bool { self.sessions.set(observer).is_ok() }

    /// R8z-5: the run is being released. A session end from here on (the
    /// release's own revocation, a live check failing because the owner
    /// dropped the run's authority first) is not reported. Idempotent; does
    /// not revoke anything itself.
    pub fn mark_released(&self, run: RunHandle) {
        let active = self.active.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(session) = active.get(&run.0) { session.released.store(true, Ordering::Release); }
    }

    /// The gateway supplies policy and grants from its live authenticated
    /// principal/binding/approval stores. Neither value may come from MCP data,
    /// process environment, or runtime-generated command arguments.
    pub fn new(backend: Arc<SpawndBackend>, boundary: Arc<Boundary>,
        registry: Arc<dyn RuntimeBridgeRegistry>, dispatcher: Arc<dyn Dispatcher>,
        plan: SpawnPolicy, grant: GrantPolicy, origin: WaitOrigin, caller: Option<EmployeeId>)
        -> Result<Arc<Self>, BackendError> {
        if !plan.capabilities.mcp_bridge || matches!(plan.runtime, RuntimeKind::Registered(_))
            && crate::config::registered_cli_name(&plan.runtime).is_none()
            || matches!(origin, WaitOrigin::Run(_)) && caller.is_none()
            || matches!(origin, WaitOrigin::Gateway) && caller.is_some() {
            return Err(BackendError::Denied);
        }
        Ok(Arc::new(Self { employee: plan.employee, task: grant.task.clone(), backend,
            boundary, registry, dispatcher, origin, caller, prepared: Mutex::new(Some((plan, grant))), revoked: AtomicBool::new(false),
            pending: Mutex::new(BTreeMap::new()), active: Mutex::new(BTreeMap::new()), sessions: OnceLock::new() }))
    }

    /// R8z-3: [`ProtectedLaunch::prepare`] with the fixed check that refused.
    /// Every check, its order, its side effects (the one-shot plan is taken,
    /// a dropped pending guard revokes) and the returned `BackendError` are
    /// exactly `prepare`'s; `prepare` is this with the step dropped. The
    /// blocking job returns its step as a value (`Ok(Err(step))`); an `Err`
    /// is only ever the blocking-pool wrapper's. Nothing here logs.
    pub async fn prepare_checked(&self, run: RunHandle, class: ExecutionClass, runtime: RuntimeKind, pty: bool)
        -> Result<SpawnPolicy, (PrepareStep, BackendError)> {
        use PrepareStep as S;
        let denied = |step: PrepareStep| (step, BackendError::Denied);
        if self.revoked.load(Ordering::Acquire) { return Err(denied(S::Revoked)); }
        let metadata = self.backend.reservation(run).await.map_err(|error| (S::Reservation(error), error))?;
        if metadata.employee != self.employee { return Err(denied(S::EmployeeMismatch)); }
        let (mut plan, grant) = self.prepared.lock().map_err(|_| (S::StateLock, BackendError::Unavailable))?
            .take().ok_or(denied(S::PreparedMissing))?;
        if plan.class != class { return Err(denied(S::PlanClass)); }
        if plan.runtime != runtime { return Err(denied(S::PlanRuntime)); }
        if plan.pty != pty { return Err(denied(S::PlanPty)); }
        let registry = self.registry.clone();
        let boundary = self.boundary.clone();
        let bound = crate::authority::verify_blocking(move || Ok((|| -> Result<(SpawnPolicy, Pending), PrepareStep> {
            // Root publication and current gateway authority may perform
            // blocking I/O. Never hold an async launch/map lock here.
            let registration = registry.registration_checked(&runtime).map_err(|(step, _)| S::Registry(step))?;
            if registration.runtime != runtime { return Err(S::RegistrationRuntime); }
            let bridge = runtime_projection(registration).map_err(|_| S::Projection)?;
            // Only fixed D-owned arguments from the verified projection may
            // be added. StartRun carries no native argv or environment.
            for argument in &bridge.arguments {
                plan.argv_allowlist.push(Text::new(argument.clone()).map_err(|_| S::Argv)?);
            }
            plan.run = run;
            let reservation = Reservation { run, employee: metadata.employee,
                control: metadata.control, expires_at_unix_ms: metadata.expires_at_unix_ms };
            let now = crate::authority::now_seconds().map_err(|_| S::Clock)?;
            let capability = boundary.issue_checked(&reservation, grant, now).map_err(|(step, _)| S::Issue(step))?;
            // Retain ownership across detached blocking completion and the
            // async handoff. Dropping either future/result revokes the run.
            let guard = Pending { reservation, capability, runtime, projection_digest: bridge.digest(),
                boundary, armed: true };
            Ok((plan, guard))
        })())).await;
        let (plan, guard) = match bound {
            Ok(Ok(value)) => value,
            Ok(Err(step)) => return Err(denied(step)),
            Err(_) => return Err(denied(S::VerifyBlocking)),
        };
        let mut pending = self.pending.lock().map_err(|_| (S::StateLock, BackendError::Unavailable))?;
        if self.revoked.load(Ordering::Acquire) { return Err(denied(S::RevokedAfter)); }
        if pending.contains_key(&run.0) { return Err(denied(S::PendingDuplicate)); }
        pending.insert(run.0, guard);
        Ok(plan)
    }

    /// R8z-3: [`ProtectedLaunch::activate`] with the fixed check that refused
    /// (same contract as [`Self::prepare_checked`]). A refusal after the
    /// pending capability was taken drops its armed guard, which revokes it.
    pub async fn activate_checked(&self, run: RunHandle, mcp: Option<OwnedFd>) -> Result<(), (ActivateStep, BackendError)> {
        use ActivateStep as S;
        let denied = |step: ActivateStep| (step, BackendError::Denied);
        let fd = mcp.ok_or(denied(S::McpFdMissing))?;
        let receipt = self.backend.mcp_receipt(run).await.map_err(|error| (S::McpReceipt(error), error))?;
        let pending = self.pending.lock().map_err(|_| (S::StateLock, BackendError::Unavailable))?
            .remove(&run.0).ok_or(denied(S::PendingMissing))?;
        let registry = self.registry.clone();
        let boundary = self.boundary.clone();
        let employee = self.employee;
        let bound = crate::authority::verify_blocking(move || Ok((|| -> Result<(crate::Connection, Pending), ActivateStep> {
            if receipt.control != pending.reservation.control { return Err(S::ReceiptControl); }
            let bridge = prepare_runtime_bridge_checked(registry.as_ref(), &pending.runtime, run, employee)
                .map_err(|(step, _)| S::Bridge(step))?;
            if bridge.projection().digest() != pending.projection_digest { return Err(S::ProjectionDigest); }
            let delivery = McpDelivery::from_take_io(&receipt.response, receipt.control, UnixStream::from(fd))
                .map_err(|_| S::Delivery)?;
            let now = crate::authority::now_seconds().map_err(|_| S::Clock)?;
            let connection = boundary.activate_checked(pending.capability, &pending.reservation, delivery, now)
                .map_err(|(step, _)| S::BoundaryActivate(step))?;
            Ok((connection, pending))
        })())).await;
        let (connection, mut guard) = match bound {
            Ok(Ok(value)) => value,
            Ok(Err(step)) => return Err(denied(step)),
            Err(_) => return Err(denied(S::VerifyBlocking)),
        };
        let mut active = self.active.lock().map_err(|_| (S::StateLock, BackendError::Unavailable))?;
        if self.revoked.load(Ordering::Acquire) { return Err(denied(S::RevokedAfter)); }
        if active.contains_key(&run.0) { return Err(denied(S::ActiveDuplicate)); }
        let dispatcher = self.dispatcher.clone();
        let released = Arc::new(AtomicBool::new(false));
        let (ended, observer) = (released.clone(), self.sessions.get().cloned());
        // R8z-5: `serve` itself is unchanged (`serve` = `serve_checked`
        // without the step); the wrapper only reports how it ended.
        let task = tokio::spawn(async move {
            observed_session(crate::server::serve_checked(connection, dispatcher.as_ref()), ended, observer).await
        });
        active.insert(run.0, Active { task, released });
        // The active connection/task now owns revocation. Until this exact
        // point every failure/cancellation drops the still-armed guard.
        guard.armed = false;
        Ok(())
    }
}
impl ProtectedLaunch for ProtectedMcpLaunch {
    fn employee(&self) -> EmployeeId { self.employee }
    fn task(&self) -> Option<Text<128>> { self.task.clone() }
    fn wait_origin(&self) -> WaitOrigin { self.origin }
    fn caller_employee(&self) -> Option<EmployeeId> { self.caller }
    fn prepare<'a>(&'a self, run: RunHandle, class: ExecutionClass, runtime: RuntimeKind,
        _native: &'a NativeCommandSnapshot, pty: bool) -> BackendFuture<'a, SpawnPolicy> {
        Box::pin(async move { self.prepare_checked(run, class, runtime, pty).await.map_err(|(_, error)| error) })
    }
    fn activate(&self, run: RunHandle, mcp: Option<OwnedFd>) -> BackendFuture<'_, ()> {
        Box::pin(async move { self.activate_checked(run, mcp).await.map_err(|(_, error)| error) })
    }
    fn revoke(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            self.revoked.store(true, Ordering::Release);
            // R8z-5: before the revocation the session would observe.
            self.mark_released(run);
            let mut revoked = self.boundary.revoke_run(run).map_err(|_| BackendError::Unavailable);
            let mut pending = self.pending.lock().unwrap_or_else(|error| {
                revoked = Err(BackendError::Unavailable);
                error.into_inner()
            });
            pending.remove(&run.0);
            drop(pending);
            let mut active = self.active.lock().unwrap_or_else(|error| {
                revoked = Err(BackendError::Unavailable);
                error.into_inner()
            });
            if let Some(active) = active.remove(&run.0) { active.task.abort(); }
            revoked
        })
    }
}
impl Drop for ProtectedMcpLaunch {
    fn drop(&mut self) {
        // Recover poisoned ownership maps on teardown: revocation and task
        // abortion must not be skipped because an earlier handler panicked.
        let pending = self.pending.get_mut().unwrap_or_else(|error| error.into_inner());
        for run in pending.keys() { let _ = self.boundary.revoke_run(RunHandle(*run)); }
        let active = self.active.get_mut().unwrap_or_else(|error| error.into_inner());
        for session in active.values() { session.released.store(true, Ordering::Release); }
        for (run, session) in active.iter() {
            let _ = self.boundary.revoke_run(RunHandle(*run));
            session.task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LiveAuthority, LiveSnapshot, VerificationError};
    use duduclaw_isolation::{bounded::List, capability::{EmployeeBinding, Lane, Principal},
        ids::Sha256Digest, ipc::{ControlConnectionId, RunStatus}};

    struct Live { running: AtomicBool, expiry: u64 }
    impl LiveAuthority for Live {
        fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError> {
            Ok(LiveSnapshot { employee, run, control: ControlConnectionId([3; 16]),
                status: if self.running.load(Ordering::Acquire) { RunStatus::Running }
                    else { RunStatus::Reserved { expires_at_unix_ms: self.expiry } },
                principal: principal(), binding: binding(), policy_revision: 1, agent_slug: "employee".into() })
        }
    }
    fn principal() -> Principal {
        Principal::Human { user_id: Text::new("user".into()).unwrap(), generation: 1 }
    }
    fn binding() -> EmployeeBinding { EmployeeBinding::Personal(Text::new("user".into()).unwrap()) }
    fn pending() -> (Arc<Boundary>, Arc<Live>, Pending) {
        let now = crate::authority::now_seconds().unwrap();
        let expiry = (now + 60) * 1000;
        let live = Arc::new(Live { running: AtomicBool::new(false), expiry });
        let boundary = Boundary::new(live.clone()).unwrap();
        let reservation = Reservation { run: RunHandle(RunId([1; 16])), employee: EmployeeId([2; 16]),
            control: ControlConnectionId([3; 16]), expires_at_unix_ms: expiry };
        let grant = GrantPolicy { task: None, principal: principal(), binding: binding(), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
            not_before: now, expires_at: now + 60 };
        let capability = boundary.issue(&reservation, grant, now).unwrap();
        let guard = Pending { boundary: boundary.clone(), armed: true, reservation, capability,
            runtime: RuntimeKind::Claude, projection_digest: Sha256Digest([0; 32]) };
        (boundary, live, guard)
    }
    fn cannot_activate(boundary: &Arc<Boundary>, reservation: &Reservation, capability: CapabilityId) {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let delivery = McpDelivery { run: reservation.run, control: reservation.control, socket };
        assert!(boundary.activate(capability, reservation, delivery, crate::authority::now_seconds().unwrap()).is_err());
    }
    /// R8z-5: the session wrapper returns `serve`'s result unchanged and calls
    /// the observer exactly once on an abnormal end, never on the normal end
    /// or after the run was released.
    #[tokio::test(flavor = "current_thread")]
    async fn r8z5_the_session_observer_is_called_once_on_an_abnormal_end_only() {
        let seen = Arc::new(Mutex::new(Vec::<SessionStep>::new()));
        let observer = || -> Option<SessionObserver> {
            let seen = seen.clone();
            let observer: SessionObserver = Arc::new(move |step: SessionStep| seen.lock().unwrap().push(step));
            Some(observer)
        };
        let end = |outcome: crate::server::SessionOutcome| std::future::ready(outcome);
        let live = || Arc::new(AtomicBool::new(false));
        // `serve` returned an error.
        let refused = Err((SessionStep::SecondRequest, VerificationError::Invalid));
        assert_eq!(observed_session(end(refused), live(), observer()).await, Err(VerificationError::Invalid));
        assert_eq!(*seen.lock().unwrap(), vec![SessionStep::SecondRequest]);
        // `serve` returned `Ok(())`, but the peer closed in a request.
        let closed = Ok(Some(SessionStep::PeerClosedInRequest));
        assert_eq!(observed_session(end(closed), live(), observer()).await, Ok(()));
        assert_eq!(*seen.lock().unwrap(), vec![SessionStep::SecondRequest, SessionStep::PeerClosedInRequest]);
        // The normal end: nothing.
        assert_eq!(observed_session(end(Ok(None)), live(), observer()).await, Ok(()));
        // Released before the end (the release's own revocation): nothing.
        let released = Arc::new(AtomicBool::new(true));
        let revoked = Err((SessionStep::Revoked, VerificationError::Revoked));
        assert_eq!(observed_session(end(revoked), released, observer()).await, Err(VerificationError::Revoked));
        // No observer installed: the result alone.
        let refused = Err((SessionStep::FrameInvalid, VerificationError::Invalid));
        assert_eq!(observed_session(end(refused), live(), None).await, Err(VerificationError::Invalid));
        assert_eq!(seen.lock().unwrap().len(), 2, "exactly one call per abnormal end");
    }

    #[test]
    fn pending_guard_drop_revokes_issued_capability() {
        let (boundary, live, guard) = pending();
        let reservation = guard.reservation.clone();
        let capability = guard.capability;
        drop(guard);
        live.running.store(true, Ordering::Release);
        cannot_activate(&boundary, &reservation, capability);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_blocking_handoff_drops_pending_capability() {
        struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Dropped {
            fn drop(&mut self) { if let Some(sender) = self.0.take() { let _ = sender.send(()); } }
        }
        let (boundary, live, guard) = pending();
        let reservation = guard.reservation.clone();
        let capability = guard.capability;
        let (release, wait) = std::sync::mpsc::channel();
        let (dropped, observed) = tokio::sync::oneshot::channel();
        let (entered, started) = tokio::sync::oneshot::channel();
        let mut work = Box::pin(crate::authority::verify_blocking(move || {
            entered.send(()).map_err(|_| VerificationError::Unavailable)?;
            wait.recv_timeout(std::time::Duration::from_secs(5)).map_err(|_| VerificationError::Unavailable)?;
            // Tuple drop order revokes before reporting discarded completion.
            Ok((guard, Dropped(Some(dropped))))
        }));
        tokio::select! {
            start = tokio::time::timeout(std::time::Duration::from_secs(3), started) => start.unwrap().unwrap(),
            result = &mut work => { let _ = result; panic!("blocking work completed before release"); },
        }
        drop(work);
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), observed).await.unwrap().unwrap();
        live.running.store(true, Ordering::Release);
        cannot_activate(&boundary, &reservation, capability);
    }
}
