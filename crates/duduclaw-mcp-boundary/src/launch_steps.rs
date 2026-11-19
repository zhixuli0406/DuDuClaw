//! R8z-3: which check of the MCP-boundary launch refused.
//!
//! `ProtectedMcpLaunch::prepare`/`activate`, the runtime registry and the
//! capability boundary each collapse their refusals into one `BackendError`
//! or `VerificationError`. The `*_checked` variants of those functions return
//! the same error (same value, same order, same side effects) together with
//! one of the closed step values below. This crate does not log: the gateway
//! logs the step once and records it as the turn's refusal site.
//!
//! Every step is a path of fixed `&'static str` segments ([`PrepareStep::segments`],
//! [`ActivateStep::segments`]); each segment is lowercase `[a-z_]`. The
//! gateway's site list holds the joined labels as literals and its tests check
//! every one against these segments.
use duduclaw_isolation::execution::BackendError;

/// Every `BackendError`, in declaration order.
pub const BACKEND_ERRORS: [BackendError; 7] = [BackendError::Unavailable, BackendError::Denied,
    BackendError::WouldDeadlock, BackendError::NotFound, BackendError::Timeout, BackendError::Io, BackendError::Protocol];

/// The fixed class segment of a backend error.
pub const fn backend_class(error: BackendError) -> &'static str {
    match error {
        BackendError::Unavailable => "unavailable",
        BackendError::Denied => "denied",
        BackendError::WouldDeadlock => "would_deadlock",
        BackendError::NotFound => "not_found",
        BackendError::Timeout => "timeout",
        BackendError::Io => "io",
        BackendError::Protocol => "protocol",
    }
}

/// One check of an installed MCP acceptance record against the projection
/// recomputed from the published runtime (`McpAcceptance::validate`, in its
/// order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptanceCheck {
    Runtime, Version, RuntimeDigest, BridgeDigest, ProjectionDigest, InheritedFd, InitializeExit,
    ParentExecExit, McpChildExecExit, ExclusiveConfigExit, TranscriptDigest,
}
impl AcceptanceCheck {
    pub const ALL: [Self; 11] = [Self::Runtime, Self::Version, Self::RuntimeDigest, Self::BridgeDigest,
        Self::ProjectionDigest, Self::InheritedFd, Self::InitializeExit, Self::ParentExecExit,
        Self::McpChildExecExit, Self::ExclusiveConfigExit, Self::TranscriptDigest];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Version => "version",
            Self::RuntimeDigest => "runtime_digest",
            Self::BridgeDigest => "bridge_digest",
            Self::ProjectionDigest => "projection_digest",
            Self::InheritedFd => "inherited_fd",
            Self::InitializeExit => "initialize_exit",
            Self::ParentExecExit => "parent_exec_exit",
            Self::McpChildExecExit => "mcp_child_exec_exit",
            Self::ExclusiveConfigExit => "exclusive_config_exit",
            Self::TranscriptDigest => "transcript_digest",
        }
    }
}

/// A check of `RuntimeBridgeRegistry::registration` (the installed
/// registry: root's gateway publication, the runtime record, its acceptance).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationStep {
    /// The gateway publication could not be read (missing, ownership/mode,
    /// other boot, older than its 1 s validity, strict parse).
    PublicationRead,
    /// The publication names another generation than the gateway's boot.
    Generation,
    /// No published runtime record of this kind.
    RuntimeMissing,
    /// More than one published runtime record of this kind.
    RuntimeDuplicate,
    /// The record carries no `mcp_acceptance`.
    AcceptanceMissing,
    /// The record's executable is a script (no single attestable digest).
    ScriptExecutable,
    /// The record has `mcp: false`.
    McpDisabled,
    /// `runtime_projection` refused the recomputed registration.
    ProjectionInvalid,
    /// The acceptance record does not match the recomputed projection.
    Acceptance(AcceptanceCheck),
    /// A registry that does not report its steps refused.
    Unreported,
}
impl RegistrationStep {
    pub const PLAIN: [Self; 9] = [Self::PublicationRead, Self::Generation, Self::RuntimeMissing,
        Self::RuntimeDuplicate, Self::AcceptanceMissing, Self::ScriptExecutable, Self::McpDisabled,
        Self::ProjectionInvalid, Self::Unreported];
    /// Every step (the plain ones, then each acceptance check).
    pub fn every() -> Vec<Self> {
        Self::PLAIN.into_iter().chain(AcceptanceCheck::ALL.into_iter().map(Self::Acceptance)).collect()
    }
    pub fn segments(self, out: &mut Vec<&'static str>) {
        let last = match self {
            Self::PublicationRead => "publication_read",
            Self::Generation => "generation",
            Self::RuntimeMissing => "runtime_missing",
            Self::RuntimeDuplicate => "runtime_duplicate",
            Self::AcceptanceMissing => "acceptance_missing",
            Self::ScriptExecutable => "script_executable",
            Self::McpDisabled => "mcp_disabled",
            Self::ProjectionInvalid => "projection_invalid",
            Self::Unreported => "unreported",
            Self::Acceptance(check) => { out.push("acceptance"); check.as_str() }
        };
        out.push(last);
    }
}

/// A field group of root's per-run MCP receipt (`checked_receipt`, in order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptCheck {
    /// Generation.
    Generation,
    /// Run.
    Run,
    /// Employee.
    Employee,
    /// Runtime kind or runtime version.
    Runtime,
    /// Runtime or bridge digest.
    Digests,
    /// Projection digest.
    ProjectionDigest,
    /// Mount id, recursive read-only, root-owned ancestors.
    Mount,
    /// The receipt's acceptance against the projection.
    Acceptance,
}
impl ReceiptCheck {
    pub const ALL: [Self; 8] = [Self::Generation, Self::Run, Self::Employee, Self::Runtime, Self::Digests,
        Self::ProjectionDigest, Self::Mount, Self::Acceptance];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generation => "generation",
            Self::Run => "run",
            Self::Employee => "employee",
            Self::Runtime => "runtime",
            Self::Digests => "digests",
            Self::ProjectionDigest => "projection_digest",
            Self::Mount => "mount",
            Self::Acceptance => "acceptance",
        }
    }
}

/// A check of `RuntimeBridgeRegistry::publish_for_run` (after StartRun).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishStep {
    /// The publication read, or the registration of the derived record.
    Registration(RegistrationStep),
    /// The publication's root policy does not build.
    Policy,
    /// The root policy does not derive this employee's run of the runtime.
    Derive,
    /// No published record has the derived profile.
    Record,
    /// The derived record's registration differs from the one prepared.
    RegistrationChanged,
    /// Root's per-run MCP receipt could not be read (missing, stale, other boot).
    RunReceiptRead,
    /// Root's per-run MCP receipt does not match.
    Receipt(ReceiptCheck),
}
impl PublishStep {
    pub const PLAIN: [Self; 5] = [Self::Policy, Self::Derive, Self::Record, Self::RegistrationChanged, Self::RunReceiptRead];
    pub fn every() -> Vec<Self> {
        RegistrationStep::every().into_iter().map(Self::Registration).chain(Self::PLAIN)
            .chain(ReceiptCheck::ALL.into_iter().map(Self::Receipt)).collect()
    }
    pub fn segments(self, out: &mut Vec<&'static str>) {
        match self {
            Self::Registration(step) => step.segments(out),
            Self::Policy => out.push("policy"),
            Self::Derive => out.push("derive"),
            Self::Record => out.push("record"),
            Self::RegistrationChanged => out.push("registration_changed"),
            Self::RunReceiptRead => out.push("run_receipt_read"),
            Self::Receipt(check) => { out.push("receipt"); out.push(check.as_str()); }
        }
    }
}

/// A check of `config::prepare_runtime_bridge_checked`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeStep {
    /// Not Linux.
    Platform,
    /// The registry's registration.
    Registry(RegistrationStep),
    /// The registration names another runtime.
    RegistrationRuntime,
    /// `runtime_projection` refused the registration.
    Projection,
    /// The registry's per-run publication readback.
    Publish(PublishStep),
    /// The readback receipt does not bind this run/employee/projection/fd plan.
    ReceiptBinding,
}
impl BridgeStep {
    pub fn every() -> Vec<Self> {
        let mut steps = vec![Self::Platform];
        steps.extend(RegistrationStep::every().into_iter().map(Self::Registry));
        steps.extend([Self::RegistrationRuntime, Self::Projection]);
        steps.extend(PublishStep::every().into_iter().map(Self::Publish));
        steps.push(Self::ReceiptBinding);
        steps
    }
    pub fn segments(self, out: &mut Vec<&'static str>) {
        match self {
            Self::Platform => out.push("platform"),
            Self::Registry(step) => { out.push("registry"); step.segments(out); }
            Self::RegistrationRuntime => out.push("registration_runtime"),
            Self::Projection => out.push("projection"),
            Self::Publish(step) => { out.push("publish"); step.segments(out); }
            Self::ReceiptBinding => out.push("receipt_binding"),
        }
    }
}

/// R8z-4: the rule of `CapabilityRecord::canonical_bytes` that refused (in its
/// order). Derived from the record's own public checks, never from the
/// message text of the `ContractError`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeCheck {
    /// `CapabilityRecord::validate_resource_mode`.
    ResourceMode,
    /// `CapabilityRecord::validate_interval` (`issued_at <= not_before < expires_at`).
    Interval,
    /// `CapabilityRecord::validate_distinct_grants` (a repeated tool or resource).
    DuplicateGrant,
    /// The encoding refused, but none of the checks above does.
    Other,
}
impl EncodeCheck {
    pub const ALL: [Self; 4] = [Self::ResourceMode, Self::Interval, Self::DuplicateGrant, Self::Other];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResourceMode => "resource_mode",
            Self::Interval => "interval",
            Self::DuplicateGrant => "duplicate_grant",
            Self::Other => "other",
        }
    }
    /// Which rule refused a record whose `canonical_bytes` returned `Err`
    /// (the same checks in the same order as `canonical_bytes`).
    pub fn of(record: &duduclaw_isolation::capability::CapabilityRecord) -> Self {
        if record.validate_resource_mode().is_err() { Self::ResourceMode }
        else if record.validate_interval().is_err() { Self::Interval }
        else if record.validate_distinct_grants().is_err() { Self::DuplicateGrant }
        else { Self::Other }
    }
}

/// A check of `Boundary::issue` (in its order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueStep {
    /// The live authority snapshot refused.
    LiveSnapshot,
    /// Root's run is not `Reserved`.
    NotReserved,
    /// The snapshot's employee, run or control connection differ.
    Reservation,
    /// The snapshot's reservation expiry differs.
    Expiry,
    /// The clock overflows, or the reservation has expired.
    Time,
    /// The snapshot's principal differs from the grant's.
    Principal,
    /// The snapshot's employee binding differs from the grant's.
    Binding,
    /// The snapshot's policy revision differs from the grant's.
    PolicyRevision,
    /// The snapshot's agent slug is not a valid slug.
    AgentSlug,
    /// The grant's binding does not admit the grant's principal.
    BindingPrincipal,
    /// The grant's resource set is refused (`resources::validate_grants`).
    Grants,
    /// The capability record does not encode (this rule refused).
    Encode(EncodeCheck),
    /// The OS random source failed.
    Random,
    /// The signing key is unusable.
    Signer,
    /// The capability table lock is poisoned.
    StateLock,
    /// The capability table is full.
    Capacity,
    /// The capability id or the run already has a capability.
    Duplicate,
}
impl IssueStep {
    /// Every step, in declaration order with each encode rule expanded.
    pub const ALL: [Self; 20] = [Self::LiveSnapshot, Self::NotReserved, Self::Reservation, Self::Expiry, Self::Time,
        Self::Principal, Self::Binding, Self::PolicyRevision, Self::AgentSlug, Self::BindingPrincipal, Self::Grants,
        Self::Encode(EncodeCheck::ResourceMode), Self::Encode(EncodeCheck::Interval),
        Self::Encode(EncodeCheck::DuplicateGrant), Self::Encode(EncodeCheck::Other),
        Self::Random, Self::Signer, Self::StateLock, Self::Capacity, Self::Duplicate];
    pub fn segments(self, out: &mut Vec<&'static str>) {
        if let Self::Encode(check) = self { out.push("encode"); out.push(check.as_str()); return; }
        out.push(self.plain_str());
    }
    const fn plain_str(self) -> &'static str {
        match self {
            Self::Encode(_) => "encode",
            Self::LiveSnapshot => "live_snapshot",
            Self::NotReserved => "not_reserved",
            Self::Reservation => "reservation",
            Self::Expiry => "expiry",
            Self::Time => "time",
            Self::Principal => "principal",
            Self::Binding => "binding",
            Self::PolicyRevision => "policy_revision",
            Self::AgentSlug => "agent_slug",
            Self::BindingPrincipal => "binding_principal",
            Self::Grants => "grants",
            Self::Random => "random",
            Self::Signer => "signer",
            Self::StateLock => "state_lock",
            Self::Capacity => "capacity",
            Self::Duplicate => "duplicate",
        }
    }
}

/// A check of `Boundary::activate` (in its order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryActivateStep {
    /// The delivered endpoint names another run or control connection.
    Delivery,
    /// No capability with this id.
    Unknown,
    /// The capability is not pending, or names another run/employee/control.
    Binding,
    /// Outside the capability's validity window.
    Time,
    /// The live authority snapshot refused.
    LiveSnapshot,
    /// The live snapshot differs from the capability, or the run is not `Running`.
    LiveMismatch,
    /// The OS random source failed.
    Random,
    /// The capability table lock is poisoned.
    StateLock,
    /// The capability was revoked meanwhile.
    Revoked,
}
impl BoundaryActivateStep {
    pub const ALL: [Self; 9] = [Self::Delivery, Self::Unknown, Self::Binding, Self::Time, Self::LiveSnapshot,
        Self::LiveMismatch, Self::Random, Self::StateLock, Self::Revoked];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delivery => "delivery",
            Self::Unknown => "unknown",
            Self::Binding => "binding",
            Self::Time => "time",
            Self::LiveSnapshot => "live_snapshot",
            Self::LiveMismatch => "live_mismatch",
            Self::Random => "random",
            Self::StateLock => "state_lock",
            Self::Revoked => "revoked",
        }
    }
}

/// The check of `ProtectedMcpLaunch::prepare` that refused (in its order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareStep {
    /// The launch was revoked before preparation.
    Revoked,
    /// The backend's reservation metadata (this error class).
    Reservation(BackendError),
    /// The reservation belongs to another employee.
    EmployeeMismatch,
    /// A launch state mutex is poisoned.
    StateLock,
    /// The one-shot plan and grant were already taken.
    PreparedMissing,
    /// The plan's execution class differs from the spawn's.
    PlanClass,
    /// The plan's runtime differs from the spawn's.
    PlanRuntime,
    /// The plan's PTY flag differs from the spawn's.
    PlanPty,
    /// The runtime registry's registration.
    Registry(RegistrationStep),
    /// The registration names another runtime.
    RegistrationRuntime,
    /// `runtime_projection` refused the registration.
    Projection,
    /// A projected fixed argument does not fit the plan's argument type.
    Argv,
    /// The wall clock is unreadable.
    Clock,
    /// The capability issuance.
    Issue(IssueStep),
    /// The blocking-pool wrapper itself failed (2 s slot wait, 10 s job
    /// limit, or the job panicked).
    VerifyBlocking,
    /// This run already has a pending capability.
    PendingDuplicate,
    /// The launch was revoked while the blocking job ran.
    RevokedAfter,
}
impl PrepareStep {
    /// Every step, in declaration order with nested steps expanded.
    pub fn every() -> Vec<Self> {
        let mut steps = vec![Self::Revoked];
        steps.extend(BACKEND_ERRORS.map(Self::Reservation));
        steps.extend([Self::EmployeeMismatch, Self::StateLock, Self::PreparedMissing, Self::PlanClass,
            Self::PlanRuntime, Self::PlanPty]);
        steps.extend(RegistrationStep::every().into_iter().map(Self::Registry));
        steps.extend([Self::RegistrationRuntime, Self::Projection, Self::Argv, Self::Clock]);
        steps.extend(IssueStep::ALL.map(Self::Issue));
        steps.extend([Self::VerifyBlocking, Self::PendingDuplicate, Self::RevokedAfter]);
        steps
    }
    pub fn segments(self, out: &mut Vec<&'static str>) {
        match self {
            Self::Revoked => out.push("revoked"),
            Self::Reservation(error) => { out.push("reservation"); out.push(backend_class(error)); }
            Self::EmployeeMismatch => out.push("employee_mismatch"),
            Self::StateLock => out.push("state_lock"),
            Self::PreparedMissing => out.push("prepared_missing"),
            Self::PlanClass => out.push("plan_class"),
            Self::PlanRuntime => out.push("plan_runtime"),
            Self::PlanPty => out.push("plan_pty"),
            Self::Registry(step) => { out.push("registry"); step.segments(out); }
            Self::RegistrationRuntime => out.push("registration_runtime"),
            Self::Projection => out.push("projection"),
            Self::Argv => out.push("argv"),
            Self::Clock => out.push("clock"),
            Self::Issue(step) => { out.push("issue"); step.segments(out); }
            Self::VerifyBlocking => out.push("verify_blocking"),
            Self::PendingDuplicate => out.push("pending_duplicate"),
            Self::RevokedAfter => out.push("revoked_after"),
        }
    }
    /// The segments joined with `.` (tests and documentation).
    pub fn label(self) -> String { let mut out = Vec::new(); self.segments(&mut out); out.join(".") }
}

/// The check of `ProtectedMcpLaunch::activate` that refused (in its order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivateStep {
    /// The spawn delivered no MCP endpoint.
    McpFdMissing,
    /// The backend's TakeIo receipt (this error class).
    McpReceipt(BackendError),
    /// A launch state mutex is poisoned.
    StateLock,
    /// No pending capability for this run (never prepared, or revoked).
    PendingMissing,
    /// The TakeIo receipt came over another control connection.
    ReceiptControl,
    /// Root's per-run projection readback (`prepare_runtime_bridge`).
    Bridge(BridgeStep),
    /// The published projection differs from the one prepared.
    ProjectionDigest,
    /// The TakeIo response does not deliver this run's MCP endpoint.
    Delivery,
    /// The wall clock is unreadable.
    Clock,
    /// The capability activation.
    BoundaryActivate(BoundaryActivateStep),
    /// The blocking-pool wrapper itself failed.
    VerifyBlocking,
    /// This run already has an active MCP connection.
    ActiveDuplicate,
    /// The launch was revoked while the blocking job ran.
    RevokedAfter,
}
impl ActivateStep {
    /// Every step, in declaration order with nested steps expanded.
    pub fn every() -> Vec<Self> {
        let mut steps = vec![Self::McpFdMissing];
        steps.extend(BACKEND_ERRORS.map(Self::McpReceipt));
        steps.extend([Self::StateLock, Self::PendingMissing, Self::ReceiptControl]);
        steps.extend(BridgeStep::every().into_iter().map(Self::Bridge));
        steps.extend([Self::ProjectionDigest, Self::Delivery, Self::Clock]);
        steps.extend(BoundaryActivateStep::ALL.map(Self::BoundaryActivate));
        steps.extend([Self::VerifyBlocking, Self::ActiveDuplicate, Self::RevokedAfter]);
        steps
    }
    pub fn segments(self, out: &mut Vec<&'static str>) {
        match self {
            Self::McpFdMissing => out.push("mcp_fd_missing"),
            Self::McpReceipt(error) => { out.push("mcp_receipt"); out.push(backend_class(error)); }
            Self::StateLock => out.push("state_lock"),
            Self::PendingMissing => out.push("pending_missing"),
            Self::ReceiptControl => out.push("receipt_control"),
            Self::Bridge(step) => { out.push("bridge"); step.segments(out); }
            Self::ProjectionDigest => out.push("projection_digest"),
            Self::Delivery => out.push("delivery"),
            Self::Clock => out.push("clock"),
            Self::BoundaryActivate(step) => { out.push("boundary_activate"); out.push(step.as_str()); }
            Self::VerifyBlocking => out.push("verify_blocking"),
            Self::ActiveDuplicate => out.push("active_duplicate"),
            Self::RevokedAfter => out.push("revoked_after"),
        }
    }
    /// The segments joined with `.` (tests and documentation).
    pub fn label(self) -> String { let mut out = Vec::new(); self.segments(&mut out); out.join(".") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn well_formed(label: &str) -> bool {
        !label.is_empty() && label.split('.').all(|segment| !segment.is_empty()
            && segment.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    }

    /// R8z-3: every prepare/activate step has a distinct, well-formed label.
    #[test]
    fn r8z3_launch_step_labels_are_fixed_distinct_and_well_formed() {
        let prepare: Vec<String> = PrepareStep::every().into_iter().map(PrepareStep::label).collect();
        let activate: Vec<String> = ActivateStep::every().into_iter().map(ActivateStep::label).collect();
        for labels in [&prepare, &activate] {
            let distinct: std::collections::BTreeSet<_> = labels.iter().collect();
            assert_eq!(distinct.len(), labels.len(), "duplicate label");
            for label in labels.iter() { assert!(well_formed(label), "{label}"); }
        }
        assert!(prepare.contains(&"registry.acceptance.projection_digest".to_owned()));
        assert!(prepare.contains(&"reservation.denied".to_owned()));
        assert!(prepare.contains(&"issue.binding_principal".to_owned()));
        for label in ["issue.encode.resource_mode", "issue.encode.interval", "issue.encode.duplicate_grant",
            "issue.encode.other"] {
            assert!(prepare.contains(&label.to_owned()), "{label}");
        }
        assert!(!prepare.contains(&"issue.encode".to_owned()));
        assert!(activate.contains(&"bridge.publish.receipt.mount".to_owned()));
        assert!(activate.contains(&"bridge.registry.acceptance_missing".to_owned()));
        assert!(activate.contains(&"boundary_activate.live_mismatch".to_owned()));
        // Expanded sizes (a new variant must be listed in `every`).
        assert_eq!(prepare.len(), 1 + 7 + 6 + 20 + 4 + 20 + 3);
        assert_eq!(activate.len(), 1 + 7 + 3 + (1 + 20 + 2 + 33 + 1) + 3 + 9 + 3);
    }
}
