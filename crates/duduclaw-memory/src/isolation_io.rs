//! The same live authority used by the gateway adapters follows memory's
//! derived writes. Unscoped host/operator calls retain their existing policy.
pub(crate) fn verify() -> duduclaw_core::Result<()> {
    duduclaw_mcp_boundary::io_authority::verify_before_io()
        .map_err(|error| duduclaw_core::error::DuDuClawError::Memory(error.to_string()))
}

#[cfg(test)]
pub(crate) fn test_context(live: std::sync::Arc<std::sync::atomic::AtomicBool>) -> duduclaw_mcp_boundary::RequestContext {
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
