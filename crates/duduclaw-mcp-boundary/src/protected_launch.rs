//! Concrete A/D run lifecycle. Construction is restricted to host-owned plans;
//! no argv/environment from the native command builder is elevated into policy.
use std::{collections::BTreeMap, os::{fd::OwnedFd, unix::net::UnixStream}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};
use duduclaw_execution::{command::{NativeCommandSnapshot, ProtectedLaunch}, SpawndBackend};
use duduclaw_isolation::{bounded::Text, capability::CapabilityId, execution::*, ids::{EmployeeId, RunId}};
use crate::{Boundary, GrantPolicy, Reservation, config::{prepare_runtime_bridge, runtime_projection, RuntimeBridgeRegistry},
    server::Dispatcher, socket::McpDelivery};

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
struct Active { task: tokio::task::JoinHandle<Result<(), crate::VerificationError>> }

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
}
impl ProtectedMcpLaunch {
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
            pending: Mutex::new(BTreeMap::new()), active: Mutex::new(BTreeMap::new()) }))
    }
}
impl ProtectedLaunch for ProtectedMcpLaunch {
    fn employee(&self) -> EmployeeId { self.employee }
    fn task(&self) -> Option<Text<128>> { self.task.clone() }
    fn wait_origin(&self) -> WaitOrigin { self.origin }
    fn caller_employee(&self) -> Option<EmployeeId> { self.caller }
    fn prepare<'a>(&'a self, run: RunHandle, class: ExecutionClass, runtime: RuntimeKind,
        _native: &'a NativeCommandSnapshot, pty: bool) -> BackendFuture<'a, SpawnPolicy> {
        Box::pin(async move {
            if self.revoked.load(Ordering::Acquire) { return Err(BackendError::Denied); }
            let metadata = self.backend.reservation(run).await?;
            if metadata.employee != self.employee { return Err(BackendError::Denied); }
            let (mut plan, grant) = self.prepared.lock().map_err(|_| BackendError::Unavailable)?
                .take().ok_or(BackendError::Denied)?;
            if plan.class != class || plan.runtime != runtime || plan.pty != pty {
                return Err(BackendError::Denied);
            }
            let registry = self.registry.clone();
            let boundary = self.boundary.clone();
            let (plan, guard) = crate::authority::verify_blocking(move || {
                // Root publication and current gateway authority may perform
                // blocking I/O. Never hold an async launch/map lock here.
                let registration = registry.registration(&runtime)?;
                if registration.runtime != runtime { return Err(crate::VerificationError::BindingMismatch); }
                let bridge = runtime_projection(registration)?;
                // Only fixed D-owned arguments from the verified projection may
                // be added. StartRun carries no native argv or environment.
                for argument in &bridge.arguments {
                    plan.argv_allowlist.push(Text::new(argument.clone()).map_err(|_| crate::VerificationError::Invalid)?);
                }
                plan.run = run;
                let reservation = Reservation { run, employee: metadata.employee,
                    control: metadata.control, expires_at_unix_ms: metadata.expires_at_unix_ms };
                let capability = boundary.issue(&reservation, grant, crate::authority::now_seconds()?)?;
                // Retain ownership across detached blocking completion and the
                // async handoff. Dropping either future/result revokes the run.
                let guard = Pending { reservation, capability, runtime, projection_digest: bridge.digest(),
                    boundary, armed: true };
                Ok((plan, guard))
            }).await.map_err(|_| BackendError::Denied)?;
            let mut pending = self.pending.lock().map_err(|_| BackendError::Unavailable)?;
            if self.revoked.load(Ordering::Acquire) || pending.contains_key(&run.0) { return Err(BackendError::Denied); }
            pending.insert(run.0, guard);
            Ok(plan)
        })
    }
    fn activate(&self, run: RunHandle, mcp: Option<OwnedFd>) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let fd = mcp.ok_or(BackendError::Denied)?;
            let receipt = self.backend.mcp_receipt(run).await?;
            let pending = self.pending.lock().map_err(|_| BackendError::Unavailable)?
                .remove(&run.0).ok_or(BackendError::Denied)?;
            let registry = self.registry.clone();
            let boundary = self.boundary.clone();
            let employee = self.employee;
            let (connection, mut guard) = crate::authority::verify_blocking(move || {
                if receipt.control != pending.reservation.control { return Err(crate::VerificationError::BindingMismatch); }
                let bridge = prepare_runtime_bridge(registry.as_ref(), &pending.runtime, run, employee)?;
                if bridge.projection().digest() != pending.projection_digest { return Err(crate::VerificationError::BindingMismatch); }
                let delivery = McpDelivery::from_take_io(&receipt.response, receipt.control, UnixStream::from(fd))?;
                let connection = boundary.activate(pending.capability, &pending.reservation, delivery,
                    crate::authority::now_seconds()?)?;
                Ok((connection, pending))
            }).await.map_err(|_| BackendError::Denied)?;
            let mut active = self.active.lock().map_err(|_| BackendError::Unavailable)?;
            if self.revoked.load(Ordering::Acquire) || active.contains_key(&run.0) { return Err(BackendError::Denied); }
            let dispatcher = self.dispatcher.clone();
            let task = tokio::spawn(async move { crate::server::serve(connection, dispatcher.as_ref()).await });
            active.insert(run.0, Active { task });
            // The active connection/task now owns revocation. Until this exact
            // point every failure/cancellation drops the still-armed guard.
            guard.armed = false;
            Ok(())
        })
    }
    fn revoke(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            self.revoked.store(true, Ordering::Release);
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
