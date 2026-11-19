//! Gateway reader of the root's exact-runtime and per-run MCP publications.
//! This module never publishes files or turns installation flags into receipts.
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind}, ids::{EmployeeId, Sha256Digest}};
use duduclaw_mcp_boundary::{config::{FdInheritanceReceipt, ProjectionReceipt, PublishStep, ReceiptCheck,
    RegistrationStep, RuntimeBridgeRegistry, RuntimeProjection, RuntimeRegistration, runtime_projection}, VerificationError};
use crate::installed::{ExecutableRecord, GatewayPublication, McpRunReceipt, RuntimeRecord};

/// R8z-3: a refusal of the registry with the check that refused; the error
/// is exactly the one returned before the steps existed.
type Refusal<S> = (S, VerificationError);

pub struct InstalledRuntimeRegistry { generation: Sha256Digest, gateway_gid: u32 }
impl InstalledRuntimeRegistry {
    /// The gateway compares this generation with F's authenticated live probe.
    /// A generation change requires boot composition again, never silent reuse.
    pub fn new(generation: Sha256Digest, gateway_gid: u32) -> Result<Self, VerificationError> {
        if generation.0 == [0; 32] { return Err(VerificationError::Invalid); }
        let value = Self { generation, gateway_gid };
        value.publication().map_err(|(_, error)| error)?;
        Ok(value)
    }
    fn publication(&self) -> Result<GatewayPublication, Refusal<RegistrationStep>> {
        let publication = GatewayPublication::read(self.gateway_gid)
            .map_err(|_| (RegistrationStep::PublicationRead, VerificationError::Unavailable))?;
        if publication.generation != self.generation {
            return Err((RegistrationStep::Generation, VerificationError::BindingMismatch));
        }
        Ok(publication)
    }
}
fn registration(record: &RuntimeRecord) -> Result<RuntimeRegistration, Refusal<RegistrationStep>> {
    let acceptance = record.mcp_acceptance.as_ref()
        .ok_or((RegistrationStep::AcceptanceMissing, VerificationError::Unavailable))?;
    let digest = match record.executable {
        ExecutableRecord::Elf { digest, .. } => digest,
        // A script identity must cover both interpreter and script. The current
        // receipt has one binary digest, so it cannot attest that combination.
        ExecutableRecord::Script { .. } => return Err((RegistrationStep::ScriptExecutable, VerificationError::Unavailable)),
    };
    if !record.mcp { return Err((RegistrationStep::McpDisabled, VerificationError::Unavailable)); }
    let value = RuntimeRegistration { runtime: record.runtime.clone(), version: record.pinned_version.as_str().into(),
        runtime_digest: digest, bridge_digest: acceptance.bridge_digest,
        bridge_executable: "/runtime/duduclaw-mcp-bridge".into() };
    let projection = runtime_projection(value.clone()).map_err(|error| (RegistrationStep::ProjectionInvalid, error))?;
    acceptance.validate_check(&projection)
        .map_err(|check| (RegistrationStep::Acceptance(check), VerificationError::BindingMismatch))?;
    Ok(value)
}
fn in_publish((step, error): Refusal<RegistrationStep>) -> Refusal<PublishStep> { (PublishStep::Registration(step), error) }
impl RuntimeBridgeRegistry for InstalledRuntimeRegistry {
    fn registration(&self, runtime: &RuntimeKind) -> Result<RuntimeRegistration, VerificationError> {
        self.registration_checked(runtime).map_err(|(_, error)| error)
    }
    fn publish_for_run(&self, run: RunHandle, employee: EmployeeId, projection: &RuntimeProjection)
        -> Result<ProjectionReceipt, VerificationError> {
        self.publish_for_run_checked(run, employee, projection).map_err(|(_, error)| error)
    }
    fn registration_checked(&self, runtime: &RuntimeKind) -> Result<RuntimeRegistration, Refusal<RegistrationStep>> {
        let publication = self.publication()?;
        let mut matches = publication.runtimes.iter().filter(|record| record.runtime == *runtime);
        let record = matches.next().ok_or((RegistrationStep::RuntimeMissing, VerificationError::Unavailable))?;
        if matches.next().is_some() { return Err((RegistrationStep::RuntimeDuplicate, VerificationError::BindingMismatch)); }
        registration(record)
    }
    fn publish_for_run_checked(&self, run: RunHandle, employee: EmployeeId, projection: &RuntimeProjection)
        -> Result<ProjectionReceipt, Refusal<PublishStep>> {
        let publication = self.publication().map_err(in_publish)?;
        let policy = publication.policy().map_err(|_| (PublishStep::Policy, VerificationError::Unavailable))?;
        let (runtime, _) = policy.derive(employee, &projection.registration.runtime, projection.registration.runtime == RuntimeKind::Grok)
            .map_err(|_| (PublishStep::Derive, VerificationError::BindingMismatch))?;
        let record = publication.runtimes.iter().find(|record| record.profile == runtime.profile)
            .ok_or((PublishStep::Record, VerificationError::Unavailable))?;
        if registration(record).map_err(in_publish)? != projection.registration {
            return Err((PublishStep::RegistrationChanged, VerificationError::BindingMismatch));
        }
        let receipt = McpRunReceipt::read(run, self.gateway_gid)
            .map_err(|_| (PublishStep::RunReceiptRead, VerificationError::Unavailable))?;
        checked_receipt(self.generation, run, employee, projection, receipt)
            .map_err(|check| (PublishStep::Receipt(check), VerificationError::BindingMismatch))
    }
}
/// Every refusal is `BindingMismatch`; the check names the field group (the
/// former single condition chain, in its order).
fn checked_receipt(generation: Sha256Digest, run: RunHandle, employee: EmployeeId,
    projection: &RuntimeProjection, receipt: McpRunReceipt) -> Result<ProjectionReceipt, ReceiptCheck> {
    let expected = &projection.registration;
    if receipt.generation != generation { return Err(ReceiptCheck::Generation); }
    if receipt.run != run { return Err(ReceiptCheck::Run); }
    if receipt.employee != employee { return Err(ReceiptCheck::Employee); }
    if receipt.runtime != expected.runtime || receipt.runtime_version != expected.version { return Err(ReceiptCheck::Runtime); }
    if receipt.runtime_digest != expected.runtime_digest || receipt.bridge_digest != expected.bridge_digest {
        return Err(ReceiptCheck::Digests);
    }
    if receipt.projection_digest != projection.digest() { return Err(ReceiptCheck::ProjectionDigest); }
    if receipt.projection_mount_id == 0 || !receipt.recursive_readonly || !receipt.root_owned_ancestors {
        return Err(ReceiptCheck::Mount);
    }
    receipt.acceptance.validate(projection).map_err(|_| ReceiptCheck::Acceptance)?;
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

    /// R8z-3: each receipt refusal names its field group, in order.
    #[test]
    fn r8z3_receipt_refusals_name_their_field_group() {
        let (projection, receipt) = fixture();
        let check = |r| checked_receipt(receipt.generation, receipt.run, receipt.employee, &projection, r).err();
        let edits: [(ReceiptCheck, fn(&mut McpRunReceipt)); 11] = [
            (ReceiptCheck::Generation, |r| r.generation.0[0] ^= 1),
            (ReceiptCheck::Run, |r| r.run.0.0[0] ^= 1),
            (ReceiptCheck::Employee, |r| r.employee.0[0] ^= 1),
            (ReceiptCheck::Runtime, |r| r.runtime = RuntimeKind::Codex),
            (ReceiptCheck::Runtime, |r| r.runtime_version = "other".into()),
            (ReceiptCheck::Digests, |r| r.bridge_digest.0[0] ^= 1),
            (ReceiptCheck::ProjectionDigest, |r| r.projection_digest.0[0] ^= 1),
            (ReceiptCheck::Mount, |r| r.projection_mount_id = 0),
            (ReceiptCheck::Mount, |r| r.recursive_readonly = false),
            (ReceiptCheck::Mount, |r| r.root_owned_ancestors = false),
            (ReceiptCheck::Acceptance, |r| r.acceptance.initialize_exit = 1),
        ];
        assert_eq!(check(receipt.clone()), None);
        for (expected, edit) in edits {
            let mut bad = receipt.clone();
            edit(&mut bad);
            assert_eq!(check(bad), Some(expected));
        }
    }

    /// R8z-3: each refusal of a published runtime record names its check;
    /// the error is the one returned before the steps existed.
    #[test]
    fn r8z3_runtime_record_refusals_name_their_check() {
        use duduclaw_mcp_boundary::config::AcceptanceCheck;
        use duduclaw_isolation::bounded::Text;
        let mut record = crate::entitlement::fixture::runtime(1, RuntimeKind::OpenAiCompat);
        record.mcp = true;
        record.pinned_version = Text::new("api-worker-v1".into()).unwrap();
        let ExecutableRecord::Elf { digest, .. } = record.executable else { unreachable!() };
        let accepted = RuntimeRegistration { runtime: RuntimeKind::OpenAiCompat, version: "api-worker-v1".into(),
            runtime_digest: digest, bridge_digest: Sha256Digest([2; 32]), bridge_executable: "/runtime/duduclaw-mcp-bridge".into() };
        let projection = runtime_projection(accepted.clone()).unwrap();
        record.mcp_acceptance = Some(McpAcceptance { runtime: RuntimeKind::OpenAiCompat, version: "api-worker-v1".into(),
            runtime_digest: digest, bridge_digest: Sha256Digest([2; 32]), projection_digest: projection.digest(),
            inherited_fd: 3, initialize_exit: 0, parent_exec_exit: 0, mcp_child_exec_exit: 0, exclusive_config_exit: 0,
            transcript_digest: Sha256Digest([3; 32]), launch_policy_digest: Sha256Digest([4; 32]) });
        assert_eq!(registration(&record).ok(), Some(accepted));
        let step = |record: &RuntimeRecord| registration(record).err();
        let mut bad = record.clone(); bad.mcp_acceptance = None;
        assert_eq!(step(&bad), Some((RegistrationStep::AcceptanceMissing, VerificationError::Unavailable)));
        let mut bad = record.clone();
        bad.executable = ExecutableRecord::Script { interpreter: Text::new("sh".into()).unwrap(), interpreter_digest: digest,
            script: Text::new("run".into()).unwrap(), script_digest: digest };
        assert_eq!(step(&bad), Some((RegistrationStep::ScriptExecutable, VerificationError::Unavailable)));
        let mut bad = record.clone(); bad.mcp = false;
        assert_eq!(step(&bad), Some((RegistrationStep::McpDisabled, VerificationError::Unavailable)));
        let mut bad = record.clone(); bad.mcp_acceptance.as_mut().unwrap().bridge_digest = Sha256Digest([0; 32]);
        assert_eq!(step(&bad).map(|(step, _)| step), Some(RegistrationStep::ProjectionInvalid));
        // A version the harness wrote differently than the published record.
        let mut bad = record.clone(); bad.pinned_version = Text::new("v-api-worker-v1".into()).unwrap();
        assert_eq!(step(&bad), Some((RegistrationStep::Acceptance(AcceptanceCheck::Version), VerificationError::BindingMismatch)));
        // A published runtime binary other than the accepted one.
        let mut bad = record.clone();
        bad.executable = ExecutableRecord::Elf { entry: Text::new("runtime-api-worker-v1".into()).unwrap(), digest: Sha256Digest([9; 32]) };
        assert_eq!(step(&bad), Some((RegistrationStep::Acceptance(AcceptanceCheck::RuntimeDigest), VerificationError::BindingMismatch)));
        let mut bad = record; bad.mcp_acceptance.as_mut().unwrap().projection_digest = Sha256Digest([9; 32]);
        assert_eq!(step(&bad), Some((RegistrationStep::Acceptance(AcceptanceCheck::ProjectionDigest), VerificationError::BindingMismatch)));
    }
}
