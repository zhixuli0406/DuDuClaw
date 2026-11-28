//! Test-only live authority fixture for Service storage boundaries.
/// Inject isolation facts for each poll without latching process-global state.
pub(crate) async fn with_isolation<F: std::future::Future>(future: F) -> F::Output {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("var/lib/duduclaw-spawnd")).unwrap();
    let sources = duduclaw_core::protected_mode::Sources {
        env: Some(duduclaw_core::protected_mode::MODE_VALUE.into()),
        marker: root.path().join("unused-mode"),
        service_home: root.path().join("unused-home"),
    };
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        duduclaw_core::protected_mode::with_thread_sources(sources.clone(), || {
            duduclaw_core::isolation_install::with_thread_root(root.path().to_path_buf(), || {
                assert!(duduclaw_core::execution_gate::isolation_required());
                future.as_mut().poll(cx)
            })
        })
    }).await
}

pub(crate) fn context(live: std::sync::Arc<std::sync::atomic::AtomicBool>) -> duduclaw_mcp_boundary::RequestContext {
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, LiveRequestVerifier, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
    struct Live(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl LiveRequestVerifier for Live {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> std::result::Result<(), duduclaw_mcp_boundary::VerificationError> {
            if self.0.load(std::sync::atomic::Ordering::SeqCst) { Ok(()) }
            else { Err(duduclaw_mcp_boundary::VerificationError::Revoked) }
        }
    }
    duduclaw_mcp_boundary::RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
        gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, std::sync::Arc::new(Live(live)))
}
