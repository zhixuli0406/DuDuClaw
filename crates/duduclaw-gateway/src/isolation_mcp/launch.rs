//! Registers host authority between A's reservation and D's capability issuance.
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use duduclaw_execution::command::{NativeCommandSnapshot, ProtectedLaunch};
use duduclaw_isolation::{bounded::Text, execution::*, ids::EmployeeId};
use super::{ProtectedMcpLaunch, authority::{GatewayAuthority, PreparedAuthority}};

pub(super) struct AuthorizedLaunch {
    inner: Arc<ProtectedMcpLaunch>,
    authority: Arc<GatewayAuthority>,
    prepared: Mutex<Option<PreparedAuthority>>,
    run: Mutex<Option<RunHandle>>,
    revoked: Arc<AtomicBool>,
}
impl AuthorizedLaunch {
    pub(super) fn new(inner: Arc<ProtectedMcpLaunch>, authority: Arc<GatewayAuthority>,
        prepared: PreparedAuthority) -> Self {
        Self { inner, authority, prepared: Mutex::new(Some(prepared)), run: Mutex::new(None), revoked: Arc::new(AtomicBool::new(false)) }
    }
}
impl ProtectedLaunch for AuthorizedLaunch {
    fn employee(&self) -> EmployeeId { self.inner.employee() }
    fn task(&self) -> Option<Text<128>> { self.inner.task() }
    fn wait_origin(&self) -> WaitOrigin { self.inner.wait_origin() }
    fn caller_employee(&self) -> Option<EmployeeId> { self.inner.caller_employee() }
    fn prepare<'a>(&'a self, run: RunHandle, class: ExecutionClass, runtime: RuntimeKind,
        native: &'a NativeCommandSnapshot, pty: bool) -> BackendFuture<'a, SpawnPolicy> {
        Box::pin(async move {
            let prepared = self.prepared.lock().map_err(|_| BackendError::Unavailable)?
                .take().ok_or(BackendError::Denied)?;
            // Record ownership before registering: cancellation or unwinding must
            // still erase the run even if preparation never returns a plan.
            *self.run.lock().map_err(|_| BackendError::Unavailable)? = Some(run);
            let authority = self.authority.clone();
            let revoked = self.revoked.clone();
            let mut lease = duduclaw_mcp_boundary::authority::verify_blocking(move || {
                if revoked.load(Ordering::Acquire) { return Err(super::VerificationError::Revoked); }
                authority.bind_run(run, prepared)?;
                let lease = BindingLease { authority, run, armed: true };
                if revoked.load(Ordering::Acquire) { return Err(super::VerificationError::Revoked); }
                Ok(lease)
            }).await.map_err(|_| BackendError::Denied)?;
            if self.revoked.load(Ordering::Acquire) { return Err(BackendError::Denied); }
            let result = self.inner.prepare(run, class, runtime, native, pty).await;
            if result.is_ok() { lease.armed = false; }
            result
        })
    }
    fn activate(&self, run: RunHandle, mcp: Option<std::os::fd::OwnedFd>) -> BackendFuture<'_, ()> {
        self.inner.activate(run, mcp)
    }
    fn revoke(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            self.revoked.store(true, Ordering::Release);
            self.authority.revoke_run(run);
            self.inner.revoke(run).await
        })
    }
}
impl Drop for AuthorizedLaunch {
    fn drop(&mut self) {
        self.revoked.store(true, Ordering::Release);
        if let Some(run) = self.run.get_mut().unwrap_or_else(|error| error.into_inner()).take() {
            self.authority.revoke_run(run);
        }
    }
}

// The blocking job owns this lease until the async caller accepts it. A dropped
// join result or a cancelled prepare therefore cannot leave a late binding.
struct BindingLease { authority: Arc<GatewayAuthority>, run: RunHandle, armed: bool }
impl Drop for BindingLease {
    fn drop(&mut self) { if self.armed { self.authority.revoke_run(self.run); } }
}
