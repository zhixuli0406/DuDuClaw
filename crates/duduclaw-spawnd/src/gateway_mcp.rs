//! Gateway reader of the root's exact-runtime and per-run MCP publications.
//! This module never publishes files or turns installation flags into receipts.
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind}, ids::{EmployeeId, Sha256Digest}};
use duduclaw_mcp_boundary::{config::{FdInheritanceReceipt, ProjectionReceipt, RuntimeBridgeRegistry,
    RuntimeProjection, RuntimeRegistration, runtime_projection}, VerificationError};
use crate::installed::{ExecutableRecord, GatewayPublication, McpRunReceipt, RuntimeRecord};

pub struct InstalledRuntimeRegistry { generation: Sha256Digest, gateway_gid: u32 }
impl InstalledRuntimeRegistry {
    /// The gateway compares this generation with F's authenticated live probe.
    /// A generation change requires boot composition again, never silent reuse.
    pub fn new(generation: Sha256Digest, gateway_gid: u32) -> Result<Self, VerificationError> {
        if generation.0 == [0; 32] { return Err(VerificationError::Invalid); }
        let value = Self { generation, gateway_gid };
        value.publication()?;
        Ok(value)
    }
    fn publication(&self) -> Result<GatewayPublication, VerificationError> {
        let publication = GatewayPublication::read(self.gateway_gid).map_err(|_| VerificationError::Unavailable)?;
        if publication.generation != self.generation { return Err(VerificationError::BindingMismatch); }
        Ok(publication)
    }
}
fn registration(record: &RuntimeRecord) -> Result<RuntimeRegistration, VerificationError> {
    let acceptance = record.mcp_acceptance.as_ref().ok_or(VerificationError::Unavailable)?;
    let digest = match record.executable {
        ExecutableRecord::Elf { digest, .. } => digest,
        // A script identity must cover both interpreter and script. The current
        // receipt has one binary digest, so it cannot attest that combination.
        ExecutableRecord::Script { .. } => return Err(VerificationError::Unavailable),
    };
    if !record.mcp { return Err(VerificationError::Unavailable); }
    let value = RuntimeRegistration { runtime: record.runtime.clone(), version: record.pinned_version.as_str().into(),
        runtime_digest: digest, bridge_digest: acceptance.bridge_digest,
        bridge_executable: "/runtime/duduclaw-mcp-bridge".into() };
    let projection = runtime_projection(value.clone())?;
    acceptance.validate(&projection).map_err(|_| VerificationError::BindingMismatch)?;
    Ok(value)
}
impl RuntimeBridgeRegistry for InstalledRuntimeRegistry {
    fn registration(&self, runtime: &RuntimeKind) -> Result<RuntimeRegistration, VerificationError> {
        let publication = self.publication()?;
        let mut matches = publication.runtimes.iter().filter(|record| record.runtime == *runtime);
        let record = matches.next().ok_or(VerificationError::Unavailable)?;
        if matches.next().is_some() { return Err(VerificationError::BindingMismatch); }
        registration(record)
    }
    fn publish_for_run(&self, run: RunHandle, employee: EmployeeId, projection: &RuntimeProjection)
        -> Result<ProjectionReceipt, VerificationError> {
        let publication = self.publication()?;
        let policy = publication.policy().map_err(|_| VerificationError::Unavailable)?;
        let (runtime, _) = policy.derive(employee, &projection.registration.runtime, projection.registration.runtime == RuntimeKind::Grok)
            .map_err(|_| VerificationError::BindingMismatch)?;
        let record = publication.runtimes.iter().find(|record| record.profile == runtime.profile)
            .ok_or(VerificationError::Unavailable)?;
        if registration(record)? != projection.registration { return Err(VerificationError::BindingMismatch); }
        let receipt = McpRunReceipt::read(run, self.gateway_gid).map_err(|_| VerificationError::Unavailable)?;
        checked_receipt(self.generation, run, employee, projection, receipt)
    }
}
fn checked_receipt(generation: Sha256Digest, run: RunHandle, employee: EmployeeId,
    projection: &RuntimeProjection, receipt: McpRunReceipt) -> Result<ProjectionReceipt, VerificationError> {
    let expected = &projection.registration;
    if receipt.generation != generation || receipt.run != run || receipt.employee != employee
        || receipt.runtime != expected.runtime || receipt.runtime_version != expected.version
        || receipt.runtime_digest != expected.runtime_digest || receipt.bridge_digest != expected.bridge_digest
        || receipt.projection_digest != projection.digest() || receipt.projection_mount_id == 0
        || !receipt.recursive_readonly || !receipt.root_owned_ancestors {
        return Err(VerificationError::BindingMismatch);
    }
    receipt.acceptance.validate(projection).map_err(|_| VerificationError::BindingMismatch)?;
    Ok(ProjectionReceipt { run, employee, projection_digest: receipt.projection_digest,
        recursive_readonly: receipt.recursive_readonly, root_owned_ancestors: receipt.root_owned_ancestors,
        inheritance: FdInheritanceReceipt { registration: expected.clone(), projection_digest: receipt.projection_digest,
            inherited_fd: receipt.acceptance.inherited_fd, initializes_mcp: receipt.acceptance.initialize_exit == 0,
            survives_parent_exec: receipt.acceptance.parent_exec_exit == 0,
            survives_mcp_child_exec: receipt.acceptance.mcp_child_exec_exit == 0,
            config_is_exclusive: receipt.acceptance.exclusive_config_exit == 0 } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installed::McpAcceptance;
    use duduclaw_isolation::ids::RunId;
    fn fixture() -> (RuntimeProjection, McpRunReceipt) {
        let registration = RuntimeRegistration { runtime: RuntimeKind::Claude, version: "pinned-test-version".into(),
            runtime_digest: Sha256Digest([1; 32]), bridge_digest: Sha256Digest([2; 32]),
            bridge_executable: "/runtime/duduclaw-mcp-bridge".into() };
        let projection = runtime_projection(registration.clone()).unwrap();
        let acceptance = McpAcceptance { runtime: registration.runtime.clone(), version: registration.version.clone(),
            runtime_digest: registration.runtime_digest, bridge_digest: registration.bridge_digest,
            projection_digest: projection.digest(), inherited_fd: 3, initialize_exit: 0, parent_exec_exit: 0,
            mcp_child_exec_exit: 0, exclusive_config_exit: 0, transcript_digest: Sha256Digest([3; 32]), launch_policy_digest: Sha256Digest([4; 32]) };
        let receipt = McpRunReceipt { version: 1, generation: Sha256Digest([4; 32]), boot_id: "fixture".into(),
            checked_boottime_ms: 1, run: RunHandle(RunId([5; 16])), employee: EmployeeId([6; 16]),
            runtime: registration.runtime, runtime_version: registration.version, runtime_digest: registration.runtime_digest,
            bridge_digest: registration.bridge_digest, projection_digest: projection.digest(), projection_mount_id: 7,
            recursive_readonly: true, root_owned_ancestors: true, acceptance };
        (projection, receipt)
    }
    #[test]
    fn root_receipt_binds_generation_employee_run_bytes_and_real_mount() {
        let (projection, receipt) = fixture();
        let check = |r| checked_receipt(receipt.generation, receipt.run, receipt.employee, &projection, r);
        assert!(check(receipt.clone()).is_ok());
        let mut bad = receipt.clone(); bad.generation.0[0] ^= 1; assert!(check(bad).is_err());
        let mut bad = receipt.clone(); bad.employee.0[0] ^= 1; assert!(check(bad).is_err());
        let mut bad = receipt.clone(); bad.run.0.0[0] ^= 1; assert!(check(bad).is_err());
        let mut bad = receipt.clone(); bad.projection_digest.0[0] ^= 1; assert!(check(bad).is_err());
        let mut bad = receipt.clone(); bad.projection_mount_id = 0; assert!(check(bad).is_err());
        let mut bad = receipt.clone(); bad.acceptance.mcp_child_exec_exit = 1; assert!(check(bad).is_err());
        let mut bad = receipt.clone(); bad.acceptance.transcript_digest = Sha256Digest([0; 32]); assert!(check(bad).is_err());
    }
}
