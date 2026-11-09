use super::*;
use std::{os::unix::fs::MetadataExt, sync::atomic::{AtomicBool, Ordering}};
use duduclaw_isolation::ids::RunId;

struct Authority { live: Arc<AtomicBool>, identity: (u64, u64), handle: String }
impl ExecutionAuthority for Authority {
    fn verify_live(&self) -> Result<(), BackendError> {
        if self.live.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) }
    }
    fn verify_workspace(&self, handle: &str, file: &File) -> Result<(), BackendError> {
        self.verify_live()?;
        let metadata = file.metadata().map_err(|_| BackendError::Io)?;
        if handle == self.handle && (metadata.dev(), metadata.ino()) == self.identity { Ok(()) }
        else { Err(BackendError::Denied) }
    }
}
struct Lease(Arc<AtomicBool>);
impl RootWorkspaceLease for Lease {
    fn verify(&self, _: &RootWorkspaceRequest, _: &File) -> Result<(), BackendError> {
        if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) }
    }
}
fn request() -> RootWorkspaceRequest {
    RootWorkspaceRequest { run: RunHandle(RunId([1; 16])), employee: EmployeeId([2; 16]),
        runtime: RuntimeKind::Registered(Text::new("fixture".into()).unwrap()),
        handle: Text::new("workspace-1".into()).unwrap(), relative_cwd: Text::new(".".into()).unwrap() }
}
fn authority(directory: &File, live: Arc<AtomicBool>) -> Arc<dyn ExecutionAuthority> {
    let metadata = directory.metadata().unwrap();
    Arc::new(Authority { live, identity: (metadata.dev(), metadata.ino()), handle: "workspace-1".into() })
}
#[test]
fn receipt_requires_exact_root_binding_and_actual_selected_directory() {
    let first = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let directory = File::open(first.path()).unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let lease_live = Arc::new(AtomicBool::new(true));
    let auth = authority(&directory, live.clone());
    let receipt = RootWorkspaceReceipt::from_root_attestation(request(), directory,
        Arc::new(Lease(lease_live.clone()))).unwrap();
    assert!(receipt.verify(&request(), auth.as_ref()).is_ok());
    let mut changed = request(); changed.run = RunHandle(RunId([3; 16]));
    assert_eq!(receipt.verify(&changed, auth.as_ref()), Err(BackendError::Denied));
    changed = request(); changed.employee = EmployeeId([3; 16]);
    assert_eq!(receipt.verify(&changed, auth.as_ref()), Err(BackendError::Denied));
    changed = request(); changed.runtime = RuntimeKind::Codex;
    assert_eq!(receipt.verify(&changed, auth.as_ref()), Err(BackendError::Denied));
    changed = request(); changed.relative_cwd = Text::new("other".into()).unwrap();
    assert_eq!(receipt.verify(&changed, auth.as_ref()), Err(BackendError::Denied));
    changed = request(); changed.handle = Text::new("other".into()).unwrap();
    assert_eq!(receipt.verify(&changed, auth.as_ref()), Err(BackendError::Denied));
    let wrong_auth = authority(&File::open(other.path()).unwrap(), live.clone());
    assert_eq!(receipt.verify(&request(), wrong_auth.as_ref()), Err(BackendError::Denied));
    lease_live.store(false, Ordering::SeqCst);
    assert_eq!(receipt.verify(&request(), auth.as_ref()), Err(BackendError::Denied));
    lease_live.store(true, Ordering::SeqCst);
    live.store(false, Ordering::SeqCst);
    assert_eq!(receipt.verify(&request(), auth.as_ref()), Err(BackendError::Denied));
}
#[tokio::test]
async fn revocation_interrupts_blocked_input_and_checks_ready_future_before_return() {
    let directory = tempfile::tempdir().unwrap();
    let file = File::open(directory.path()).unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let auth = authority(&file, live.clone());
    let receipt = RootWorkspaceReceipt::from_root_attestation(request(), file,
        Arc::new(Lease(Arc::new(AtomicBool::new(true))))).unwrap();
    let guard = WorkspaceGuard::new(request(), receipt, auth).unwrap();
    let revoke = live.clone();
    tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(5)).await; revoke.store(false, Ordering::SeqCst); });
    let result = tokio::time::timeout(Duration::from_secs(1), guard.checked(std::future::pending::<Result<(), BackendError>>())).await.unwrap();
    assert_eq!(result, Err(BackendError::Denied));
    live.store(true, Ordering::SeqCst);
    assert_eq!(guard.verify(), Err(BackendError::Denied), "revocation must remain sticky");
    // A future that becomes ready in one poll must still be checked afterwards.
    let ready_live = Arc::new(AtomicBool::new(true));
    let result = checked_wait(|| if ready_live.load(Ordering::SeqCst) { Ok(()) }
        else { Err(BackendError::Denied) }, async { ready_live.store(false, Ordering::SeqCst); Ok(()) }).await;
    assert_eq!(result, Err(BackendError::Denied));
}
