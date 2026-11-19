//! R8z: fixed step names of the spawnd client's local refusals.
//!
//! A refusal produced inside the gateway process (this crate) before or
//! instead of a root reply carries the same field-less `BackendError` as a
//! root refusal, so the gateway's turn line can only say
//! `site="spawn.backend.denied"`. Each local refusal point therefore writes
//! one warning, at that point, naming a fixed step. Fields are fixed strings
//! only: never a run/employee id, path, argument, prompt or error text.

use duduclaw_isolation::execution::BackendError;

/// The message of every local refusal warning (the literal in [`refuse`]).
#[cfg(test)]
pub(crate) const MESSAGE: &str = "isolated spawn refused locally";

/// One local refusal point. Closed set; [`Step::as_str`] is the `step` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    // SpawndBackend::reserve, after or around the ReserveRun exchange.
    /// Readiness (`check_admission`) refused a reserve/start.
    Admission,
    /// The local run table is unavailable or full (`MAX_TRACKED_RUNS`).
    TrackedRuns,
    /// The ReserveRun reply is not `Reserved` or its expiry is outside
    /// `(now, now + RESERVATION_TIMEOUT_MS]`.
    ReservationReply,
    /// The reserved run id is already tracked, or the employee already has
    /// `DEFAULT_QUEUE_LENGTH` live runs in this gateway.
    ReservationQueue,
    // command::protected, between ReserveRun and StartRun.
    /// The protected launch context (`ProtectedLaunch::prepare`) refused.
    LaunchPrepare,
    /// The prepared policy does not bind this run/employee/class/runtime/pty
    /// or the caller's memory ceiling.
    PolicyBinding,
    /// Task binding after the reservation was started or expired.
    TaskBinding,
    /// An initial PTY size that does not validate.
    PtySize,
    // SpawndBackend::spawn_inner (StartRun preparation).
    /// A workspace delivery that does not match the policy/run/employee, or
    /// whose directory cannot be selected or duplicated.
    WorkspaceDelivery,
    /// No delivery and no root workspace guard, and the policy is not the
    /// plain `employee-work` root (`validate_workspace`).
    Workspace,
    /// The run is not tracked by this backend (`entry`).
    Entry,
    /// Other employee, already started, or the reservation has expired.
    ReservationState,
    /// A zero output total, output buffer or memfd budget.
    ResourceLimits,
    /// Sealing the prompt/attachments failed (count, size, budget, deadline).
    Inputs,
    /// More attachment descriptors than StartRun can carry.
    Attachments,
    /// The assembled StartRun does not validate (e.g. step limit 0 or > 1000).
    RequestInvalid,
    /// Readiness changed after StartRun was sent (the control is closed).
    AdmissionAfterSend,
    /// A start reply other than `Accepted`.
    StartReply,
    // transport (local checks before a frame is sent).
    /// `/run` or the service directory is not root-owned/unwritable.
    EndpointDirectory,
    /// The control socket is missing, not a socket or world-accessible.
    EndpointSocket,
    /// Creating or connecting the control socket failed.
    EndpointConnect,
    /// The connected peer is not root or not a local seqpacket socket.
    PeerIdentity,
    /// The run's control connection is already closed when StartRun would be
    /// sent (checked in `spawn_inner`; the transport returns the same
    /// `Unavailable` without logging, since its callers retry on it).
    ConnectionState,
    /// The caller's live authority/workspace check refused before sending.
    LiveCheck,
    /// The request has no valid descriptor plan, or the descriptors differ.
    FdPlan,
    /// A cancel was abandoned or already rejected before sending.
    CancelState,
    /// A live Status reports a reservation other than the authenticated one.
    LiveStatusReservation,
    /// A work op that is not one of the four Package H work ops.
    WorkOp,
    /// A helper op that is not one of the four HX helper ops.
    HelperOp,
}

impl Step {
    /// Every step, in declaration order (for tests and documentation).
    pub(crate) const ALL: &'static [Step] = &[Step::Admission, Step::TrackedRuns, Step::ReservationReply,
        Step::ReservationQueue, Step::LaunchPrepare, Step::PolicyBinding, Step::TaskBinding, Step::PtySize,
        Step::WorkspaceDelivery, Step::Workspace, Step::Entry, Step::ReservationState, Step::ResourceLimits,
        Step::Inputs, Step::Attachments, Step::RequestInvalid, Step::AdmissionAfterSend, Step::StartReply,
        Step::EndpointDirectory, Step::EndpointSocket, Step::EndpointConnect, Step::PeerIdentity,
        Step::ConnectionState, Step::LiveCheck, Step::FdPlan, Step::CancelState,
        Step::LiveStatusReservation, Step::WorkOp, Step::HelperOp];

    /// The fixed `step` log value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Step::Admission => "admission",
            Step::TrackedRuns => "tracked_runs",
            Step::ReservationReply => "reservation_reply",
            Step::ReservationQueue => "reservation_queue",
            Step::LaunchPrepare => "launch_prepare",
            Step::PolicyBinding => "policy_binding",
            Step::TaskBinding => "task_binding",
            Step::PtySize => "pty_size",
            Step::WorkspaceDelivery => "workspace_delivery",
            Step::Workspace => "workspace",
            Step::Entry => "entry",
            Step::ReservationState => "reservation_state",
            Step::ResourceLimits => "resource_limits",
            Step::Inputs => "inputs",
            Step::Attachments => "attachments",
            Step::RequestInvalid => "request_invalid",
            Step::AdmissionAfterSend => "admission_after_send",
            Step::StartReply => "start_reply",
            Step::EndpointDirectory => "endpoint_directory",
            Step::EndpointSocket => "endpoint_socket",
            Step::EndpointConnect => "endpoint_connect",
            Step::PeerIdentity => "peer_identity",
            Step::ConnectionState => "connection_state",
            Step::LiveCheck => "live_check",
            Step::FdPlan => "fd_plan",
            Step::CancelState => "cancel_state",
            Step::LiveStatusReservation => "live_status_reservation",
            Step::WorkOp => "work_op",
            Step::HelperOp => "helper_op",
        }
    }
}

/// Write the one warning of a local refusal at its point and return the
/// error unchanged (same variant as before R8z).
pub(crate) fn refuse(step: Step, error: BackendError) -> BackendError {
    // The literal equals MESSAGE (asserted in tests); `tracing` needs a literal.
    tracing::warn!(step = step.as_str(), "isolated spawn refused locally");
    error
}

/// `map_err` adapter: `.map_err(refused(Step::Entry))`.
pub(crate) fn refused(step: Step) -> impl FnOnce(BackendError) -> BackendError {
    move |error| refuse(step, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_names_are_fixed_distinct_lowercase_labels() {
        let mut seen = std::collections::BTreeSet::new();
        for step in Step::ALL {
            let name = step.as_str();
            assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{name}");
            assert!(seen.insert(name), "duplicate step {name}");
        }
        assert_eq!(seen.len(), Step::ALL.len());
        assert_eq!(MESSAGE, "isolated spawn refused locally");
    }

    #[test]
    fn refuse_returns_the_same_error_variant() {
        for error in [BackendError::Denied, BackendError::Unavailable, BackendError::NotFound,
            BackendError::Timeout, BackendError::Io, BackendError::Protocol] {
            assert_eq!(refuse(Step::Workspace, error), error);
            assert_eq!(refused(Step::Entry)(error), error);
        }
    }
}
