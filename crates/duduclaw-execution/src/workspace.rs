//! Trusted hookup for B/H's root-derived workspace receipt. This module neither
//! opens a host path nor turns a gateway-selected directory into root authority.
use std::{fs::File, future::Future, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use duduclaw_isolation::{bounded::Text, execution::{BackendError, BackendFuture,
    ExecutionAuthority, RunHandle, RuntimeKind, SpawnPolicy}, ids::EmployeeId};

/// Complete selection that the root receipt producer must bind atomically to
/// its reservation and the actual work mount. It is never serialized as IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootWorkspaceRequest {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub runtime: RuntimeKind,
    pub handle: Text<128>,
    pub relative_cwd: Text<4096>,
}
impl RootWorkspaceRequest {
    pub(crate) fn from_policy(policy: &SpawnPolicy) -> Result<Self, BackendError> {
        let cwd = policy.workspace.relative_cwd.as_str();
        if policy.workspace.workspace.as_str().is_empty() || cwd.is_empty()
            || (cwd != "." && (cwd.starts_with('/') || cwd.contains('\\') || cwd.contains('\0')
                || cwd.split('/').any(|part| part.is_empty() || matches!(part, "." | "..")))) {
            return Err(BackendError::Denied);
        }
        Ok(Self { run: policy.run, employee: policy.employee, runtime: policy.runtime.clone(),
            handle: policy.workspace.workspace.clone(), relative_cwd: policy.workspace.relative_cwd.clone() })
    }
}

/// Root integration owns the generation/lease and must verify that this exact
/// reservation still uses the retained directory as its work mount and cwd.
/// It must fence StartRun against policy/mount replacement, not merely compare
/// names or reopen a pathname. Drop may release a pin only after root cleanup.
pub trait RootWorkspaceLease: Send + Sync {
    fn verify(&self, request: &RootWorkspaceRequest, directory: &File) -> Result<(), BackendError>;
}

/// Immutable root-derived receipt plus its retained directory and generation
/// pin. Construct only inside B/H's trusted receipt adapter; a capability grant,
/// ordinary File, or gateway path lookup is not an attestation.
pub struct RootWorkspaceReceipt {
    request: RootWorkspaceRequest,
    directory: File,
    lease: Arc<dyn RootWorkspaceLease>,
}
impl RootWorkspaceReceipt {
    pub fn from_root_attestation(request: RootWorkspaceRequest, directory: File,
        lease: Arc<dyn RootWorkspaceLease>) -> Result<Self, BackendError> {
        if !directory.metadata().map_err(|_| BackendError::Denied)?.is_dir() {
            return Err(BackendError::Denied);
        }
        lease.verify(&request, &directory)?;
        Ok(Self { request, directory, lease })
    }
    fn verify(&self, request: &RootWorkspaceRequest, authority: &dyn ExecutionAuthority) -> Result<(), BackendError> {
        authority.verify_live()?;
        if self.request != *request { return Err(BackendError::Denied); }
        self.lease.verify(request, &self.directory)?;
        authority.verify_workspace(request.handle.as_str(), &self.directory)?;
        authority.verify_live()
    }
}

/// Explicit installation seam. A real implementation must obtain B/H's root
/// attestation for the same authenticated reservation/connection and retain its
/// mount generation until root teardown. No production path-based implementation
/// or optimistic default exists in this crate.
pub trait RootWorkspaceBridge: Send + Sync {
    fn prepare(&self, request: RootWorkspaceRequest) -> BackendFuture<'_, RootWorkspaceReceipt>;
}

pub(crate) struct WorkspaceGuard {
    request: RootWorkspaceRequest,
    receipt: RootWorkspaceReceipt,
    authority: Arc<dyn ExecutionAuthority>,
    revoked: AtomicBool,
}
impl WorkspaceGuard {
    pub(crate) fn new(request: RootWorkspaceRequest, receipt: RootWorkspaceReceipt,
        authority: Arc<dyn ExecutionAuthority>) -> Result<Self, BackendError> {
        let guard = Self { request, receipt, authority, revoked: AtomicBool::new(false) };
        guard.verify()?;
        Ok(guard)
    }
    pub(crate) fn verify(&self) -> Result<(), BackendError> {
        if self.revoked.load(Ordering::SeqCst) { return Err(BackendError::Denied); }
        let result = self.receipt.verify(&self.request, self.authority.as_ref());
        if result.is_err() { self.revoked.store(true, Ordering::SeqCst); }
        if self.revoked.load(Ordering::SeqCst) { return Err(BackendError::Denied); }
        result
    }
    /// Abort a blocked input/IPC wait after revocation. The transport closes its
    /// owned connection when this drops an outstanding transaction.
    pub(crate) async fn checked<T>(&self, future: impl Future<Output = Result<T, BackendError>>) -> Result<T, BackendError> {
        checked_wait(|| self.verify(), future).await
    }
}

pub(crate) async fn checked_wait<T>(check: impl Fn() -> Result<(), BackendError>,
    future: impl Future<Output = Result<T, BackendError>>) -> Result<T, BackendError> {
    tokio::pin!(future);
    loop {
        check()?;
        tokio::select! {
            biased;
            result = &mut future => { check()?; return result; }
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
        }
    }
}

#[cfg(all(test, unix))]
#[path = "workspace_tests.rs"]
mod tests;
