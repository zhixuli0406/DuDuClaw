//! R8z-3: the refusal site of every MCP-boundary launch step.
//!
//! One `site!` literal per step of `duduclaw_mcp_boundary::launch_steps`:
//! `launch.bind.inner_prepare.<label>` for a preparation step and
//! `launch.activate.inner_activate.<label>` for an activation step, where
//! `<label>` is the step's segments joined with `.` (checked for every step
//! by `r8z3_every_boundary_step_maps_to_its_listed_site`). There is no
//! string-to-site function: each arm names its listed label.
use duduclaw_isolation::execution::BackendError;
use duduclaw_mcp_boundary::launch_steps::{AcceptanceCheck as A, ActivateStep, BoundaryActivateStep as BA, BridgeStep,
    EncodeCheck as E, IssueStep as I, PrepareStep, PublishStep, ReceiptCheck as RC, RegistrationStep as R};
use super::super::refusal::{site, RefusalSite};

pub(super) fn prepare(step: PrepareStep) -> RefusalSite {
    use PrepareStep as P;
    match step {
        P::Revoked => site!("launch.bind.inner_prepare.revoked"),
        P::Reservation(error) => match error {
            BackendError::Unavailable => site!("launch.bind.inner_prepare.reservation.unavailable"),
            BackendError::Denied => site!("launch.bind.inner_prepare.reservation.denied"),
            BackendError::WouldDeadlock => site!("launch.bind.inner_prepare.reservation.would_deadlock"),
            BackendError::NotFound => site!("launch.bind.inner_prepare.reservation.not_found"),
            BackendError::Timeout => site!("launch.bind.inner_prepare.reservation.timeout"),
            BackendError::Io => site!("launch.bind.inner_prepare.reservation.io"),
            BackendError::Protocol => site!("launch.bind.inner_prepare.reservation.protocol"),
        },
        P::EmployeeMismatch => site!("launch.bind.inner_prepare.employee_mismatch"),
        P::StateLock => site!("launch.bind.inner_prepare.state_lock"),
        P::PreparedMissing => site!("launch.bind.inner_prepare.prepared_missing"),
        P::PlanClass => site!("launch.bind.inner_prepare.plan_class"),
        P::PlanRuntime => site!("launch.bind.inner_prepare.plan_runtime"),
        P::PlanPty => site!("launch.bind.inner_prepare.plan_pty"),
        P::Registry(step) => match step {
            R::PublicationRead => site!("launch.bind.inner_prepare.registry.publication_read"),
            R::Generation => site!("launch.bind.inner_prepare.registry.generation"),
            R::RuntimeMissing => site!("launch.bind.inner_prepare.registry.runtime_missing"),
            R::RuntimeDuplicate => site!("launch.bind.inner_prepare.registry.runtime_duplicate"),
            R::AcceptanceMissing => site!("launch.bind.inner_prepare.registry.acceptance_missing"),
            R::ScriptExecutable => site!("launch.bind.inner_prepare.registry.script_executable"),
            R::McpDisabled => site!("launch.bind.inner_prepare.registry.mcp_disabled"),
            R::ProjectionInvalid => site!("launch.bind.inner_prepare.registry.projection_invalid"),
            R::Unreported => site!("launch.bind.inner_prepare.registry.unreported"),
            R::Acceptance(check) => match check {
                A::Runtime => site!("launch.bind.inner_prepare.registry.acceptance.runtime"),
                A::Version => site!("launch.bind.inner_prepare.registry.acceptance.version"),
                A::RuntimeDigest => site!("launch.bind.inner_prepare.registry.acceptance.runtime_digest"),
                A::BridgeDigest => site!("launch.bind.inner_prepare.registry.acceptance.bridge_digest"),
                A::ProjectionDigest => site!("launch.bind.inner_prepare.registry.acceptance.projection_digest"),
                A::InheritedFd => site!("launch.bind.inner_prepare.registry.acceptance.inherited_fd"),
                A::InitializeExit => site!("launch.bind.inner_prepare.registry.acceptance.initialize_exit"),
                A::ParentExecExit => site!("launch.bind.inner_prepare.registry.acceptance.parent_exec_exit"),
                A::McpChildExecExit => site!("launch.bind.inner_prepare.registry.acceptance.mcp_child_exec_exit"),
                A::ExclusiveConfigExit => site!("launch.bind.inner_prepare.registry.acceptance.exclusive_config_exit"),
                A::TranscriptDigest => site!("launch.bind.inner_prepare.registry.acceptance.transcript_digest"),
            },
        },
        P::RegistrationRuntime => site!("launch.bind.inner_prepare.registration_runtime"),
        P::Projection => site!("launch.bind.inner_prepare.projection"),
        P::Argv => site!("launch.bind.inner_prepare.argv"),
        P::Clock => site!("launch.bind.inner_prepare.clock"),
        P::Issue(step) => match step {
            I::LiveSnapshot => site!("launch.bind.inner_prepare.issue.live_snapshot"),
            I::NotReserved => site!("launch.bind.inner_prepare.issue.not_reserved"),
            I::Reservation => site!("launch.bind.inner_prepare.issue.reservation"),
            I::Expiry => site!("launch.bind.inner_prepare.issue.expiry"),
            I::Time => site!("launch.bind.inner_prepare.issue.time"),
            I::Principal => site!("launch.bind.inner_prepare.issue.principal"),
            I::Binding => site!("launch.bind.inner_prepare.issue.binding"),
            I::PolicyRevision => site!("launch.bind.inner_prepare.issue.policy_revision"),
            I::AgentSlug => site!("launch.bind.inner_prepare.issue.agent_slug"),
            I::BindingPrincipal => site!("launch.bind.inner_prepare.issue.binding_principal"),
            I::Grants => site!("launch.bind.inner_prepare.issue.grants"),
            I::Encode(check) => match check {
                E::ResourceMode => site!("launch.bind.inner_prepare.issue.encode.resource_mode"),
                E::Interval => site!("launch.bind.inner_prepare.issue.encode.interval"),
                E::DuplicateGrant => site!("launch.bind.inner_prepare.issue.encode.duplicate_grant"),
                E::Other => site!("launch.bind.inner_prepare.issue.encode.other"),
            },
            I::Random => site!("launch.bind.inner_prepare.issue.random"),
            I::Signer => site!("launch.bind.inner_prepare.issue.signer"),
            I::StateLock => site!("launch.bind.inner_prepare.issue.state_lock"),
            I::Capacity => site!("launch.bind.inner_prepare.issue.capacity"),
            I::Duplicate => site!("launch.bind.inner_prepare.issue.duplicate"),
        },
        P::VerifyBlocking => site!("launch.bind.inner_prepare.verify_blocking"),
        P::PendingDuplicate => site!("launch.bind.inner_prepare.pending_duplicate"),
        P::RevokedAfter => site!("launch.bind.inner_prepare.revoked_after"),
    }
}

pub(super) fn activate(step: ActivateStep) -> RefusalSite {
    use ActivateStep as S;
    match step {
        S::McpFdMissing => site!("launch.activate.inner_activate.mcp_fd_missing"),
        S::McpReceipt(error) => match error {
            BackendError::Unavailable => site!("launch.activate.inner_activate.mcp_receipt.unavailable"),
            BackendError::Denied => site!("launch.activate.inner_activate.mcp_receipt.denied"),
            BackendError::WouldDeadlock => site!("launch.activate.inner_activate.mcp_receipt.would_deadlock"),
            BackendError::NotFound => site!("launch.activate.inner_activate.mcp_receipt.not_found"),
            BackendError::Timeout => site!("launch.activate.inner_activate.mcp_receipt.timeout"),
            BackendError::Io => site!("launch.activate.inner_activate.mcp_receipt.io"),
            BackendError::Protocol => site!("launch.activate.inner_activate.mcp_receipt.protocol"),
        },
        S::StateLock => site!("launch.activate.inner_activate.state_lock"),
        S::PendingMissing => site!("launch.activate.inner_activate.pending_missing"),
        S::ReceiptControl => site!("launch.activate.inner_activate.receipt_control"),
        S::Bridge(step) => bridge(step),
        S::ProjectionDigest => site!("launch.activate.inner_activate.projection_digest"),
        S::Delivery => site!("launch.activate.inner_activate.delivery"),
        S::Clock => site!("launch.activate.inner_activate.clock"),
        S::BoundaryActivate(step) => match step {
            BA::Delivery => site!("launch.activate.inner_activate.boundary_activate.delivery"),
            BA::Unknown => site!("launch.activate.inner_activate.boundary_activate.unknown"),
            BA::Binding => site!("launch.activate.inner_activate.boundary_activate.binding"),
            BA::Time => site!("launch.activate.inner_activate.boundary_activate.time"),
            BA::LiveSnapshot => site!("launch.activate.inner_activate.boundary_activate.live_snapshot"),
            BA::LiveMismatch => site!("launch.activate.inner_activate.boundary_activate.live_mismatch"),
            BA::Random => site!("launch.activate.inner_activate.boundary_activate.random"),
            BA::StateLock => site!("launch.activate.inner_activate.boundary_activate.state_lock"),
            BA::Revoked => site!("launch.activate.inner_activate.boundary_activate.revoked"),
        },
        S::VerifyBlocking => site!("launch.activate.inner_activate.verify_blocking"),
        S::ActiveDuplicate => site!("launch.activate.inner_activate.active_duplicate"),
        S::RevokedAfter => site!("launch.activate.inner_activate.revoked_after"),
    }
}

fn bridge(step: BridgeStep) -> RefusalSite {
    match step {
        BridgeStep::Platform => site!("launch.activate.inner_activate.bridge.platform"),
        BridgeStep::Registry(step) => bridge_registry(step),
        BridgeStep::RegistrationRuntime => site!("launch.activate.inner_activate.bridge.registration_runtime"),
        BridgeStep::Projection => site!("launch.activate.inner_activate.bridge.projection"),
        BridgeStep::Publish(step) => match step {
            PublishStep::Registration(step) => bridge_publish_registration(step),
            PublishStep::Policy => site!("launch.activate.inner_activate.bridge.publish.policy"),
            PublishStep::Derive => site!("launch.activate.inner_activate.bridge.publish.derive"),
            PublishStep::Record => site!("launch.activate.inner_activate.bridge.publish.record"),
            PublishStep::RegistrationChanged => site!("launch.activate.inner_activate.bridge.publish.registration_changed"),
            PublishStep::RunReceiptRead => site!("launch.activate.inner_activate.bridge.publish.run_receipt_read"),
            PublishStep::Receipt(check) => match check {
                RC::Generation => site!("launch.activate.inner_activate.bridge.publish.receipt.generation"),
                RC::Run => site!("launch.activate.inner_activate.bridge.publish.receipt.run"),
                RC::Employee => site!("launch.activate.inner_activate.bridge.publish.receipt.employee"),
                RC::Runtime => site!("launch.activate.inner_activate.bridge.publish.receipt.runtime"),
                RC::Digests => site!("launch.activate.inner_activate.bridge.publish.receipt.digests"),
                RC::ProjectionDigest => site!("launch.activate.inner_activate.bridge.publish.receipt.projection_digest"),
                RC::Mount => site!("launch.activate.inner_activate.bridge.publish.receipt.mount"),
                RC::Acceptance => site!("launch.activate.inner_activate.bridge.publish.receipt.acceptance"),
            },
        },
        BridgeStep::ReceiptBinding => site!("launch.activate.inner_activate.bridge.receipt_binding"),
    }
}

fn bridge_registry(step: R) -> RefusalSite {
    match step {
        R::PublicationRead => site!("launch.activate.inner_activate.bridge.registry.publication_read"),
        R::Generation => site!("launch.activate.inner_activate.bridge.registry.generation"),
        R::RuntimeMissing => site!("launch.activate.inner_activate.bridge.registry.runtime_missing"),
        R::RuntimeDuplicate => site!("launch.activate.inner_activate.bridge.registry.runtime_duplicate"),
        R::AcceptanceMissing => site!("launch.activate.inner_activate.bridge.registry.acceptance_missing"),
        R::ScriptExecutable => site!("launch.activate.inner_activate.bridge.registry.script_executable"),
        R::McpDisabled => site!("launch.activate.inner_activate.bridge.registry.mcp_disabled"),
        R::ProjectionInvalid => site!("launch.activate.inner_activate.bridge.registry.projection_invalid"),
        R::Unreported => site!("launch.activate.inner_activate.bridge.registry.unreported"),
        R::Acceptance(check) => match check {
            A::Runtime => site!("launch.activate.inner_activate.bridge.registry.acceptance.runtime"),
            A::Version => site!("launch.activate.inner_activate.bridge.registry.acceptance.version"),
            A::RuntimeDigest => site!("launch.activate.inner_activate.bridge.registry.acceptance.runtime_digest"),
            A::BridgeDigest => site!("launch.activate.inner_activate.bridge.registry.acceptance.bridge_digest"),
            A::ProjectionDigest => site!("launch.activate.inner_activate.bridge.registry.acceptance.projection_digest"),
            A::InheritedFd => site!("launch.activate.inner_activate.bridge.registry.acceptance.inherited_fd"),
            A::InitializeExit => site!("launch.activate.inner_activate.bridge.registry.acceptance.initialize_exit"),
            A::ParentExecExit => site!("launch.activate.inner_activate.bridge.registry.acceptance.parent_exec_exit"),
            A::McpChildExecExit => site!("launch.activate.inner_activate.bridge.registry.acceptance.mcp_child_exec_exit"),
            A::ExclusiveConfigExit => site!("launch.activate.inner_activate.bridge.registry.acceptance.exclusive_config_exit"),
            A::TranscriptDigest => site!("launch.activate.inner_activate.bridge.registry.acceptance.transcript_digest"),
        },
    }
}

fn bridge_publish_registration(step: R) -> RefusalSite {
    match step {
        R::PublicationRead => site!("launch.activate.inner_activate.bridge.publish.publication_read"),
        R::Generation => site!("launch.activate.inner_activate.bridge.publish.generation"),
        R::RuntimeMissing => site!("launch.activate.inner_activate.bridge.publish.runtime_missing"),
        R::RuntimeDuplicate => site!("launch.activate.inner_activate.bridge.publish.runtime_duplicate"),
        R::AcceptanceMissing => site!("launch.activate.inner_activate.bridge.publish.acceptance_missing"),
        R::ScriptExecutable => site!("launch.activate.inner_activate.bridge.publish.script_executable"),
        R::McpDisabled => site!("launch.activate.inner_activate.bridge.publish.mcp_disabled"),
        R::ProjectionInvalid => site!("launch.activate.inner_activate.bridge.publish.projection_invalid"),
        R::Unreported => site!("launch.activate.inner_activate.bridge.publish.unreported"),
        R::Acceptance(check) => match check {
            A::Runtime => site!("launch.activate.inner_activate.bridge.publish.acceptance.runtime"),
            A::Version => site!("launch.activate.inner_activate.bridge.publish.acceptance.version"),
            A::RuntimeDigest => site!("launch.activate.inner_activate.bridge.publish.acceptance.runtime_digest"),
            A::BridgeDigest => site!("launch.activate.inner_activate.bridge.publish.acceptance.bridge_digest"),
            A::ProjectionDigest => site!("launch.activate.inner_activate.bridge.publish.acceptance.projection_digest"),
            A::InheritedFd => site!("launch.activate.inner_activate.bridge.publish.acceptance.inherited_fd"),
            A::InitializeExit => site!("launch.activate.inner_activate.bridge.publish.acceptance.initialize_exit"),
            A::ParentExecExit => site!("launch.activate.inner_activate.bridge.publish.acceptance.parent_exec_exit"),
            A::McpChildExecExit => site!("launch.activate.inner_activate.bridge.publish.acceptance.mcp_child_exec_exit"),
            A::ExclusiveConfigExit => site!("launch.activate.inner_activate.bridge.publish.acceptance.exclusive_config_exit"),
            A::TranscriptDigest => site!("launch.activate.inner_activate.bridge.publish.acceptance.transcript_digest"),
        },
    }
}
