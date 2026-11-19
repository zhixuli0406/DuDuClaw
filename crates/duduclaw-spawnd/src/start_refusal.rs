//! R8y: the fixed step at which spawnd refused to start a run.
//!
//! A refused `ReserveRun`/`StartRun` (or a queued/starting run that never got
//! a running unit) used to leave nothing in spawnd's journal. Every refusal
//! exit on that path now carries a [`StartStep`] from this closed set and
//! spawnd prints ONE fixed line per refused start:
//! `duduclaw-spawnd: start refused: step=<label>` ([`StartStep::line`]).
//! No employee id, run id, path or request content ever enters a label; the
//! only parameters are spawnd's own fixed identifiers (a readiness.json check
//! name, an [`crate::accounts::Step`] name, another label of this set).
//!
//! The mechanism that keeps every exit labelled: the labelled functions
//! return [`Labelled`], whose error [`Refusal`] has NO `From<ErrorCode>`, so a
//! bare `?` on an `ErrorCode` result does not compile there; each exit must
//! name its step ([`at`], [`gate`]). The wire is unchanged: the code sent to
//! the gateway is [`Refusal::code`], exactly the code the exit returned before.
use duduclaw_isolation::ipc::ErrorCode;

/// The host admission gate (`Host::ready`): why this employee may not start
/// a run now. Shared by `ReserveRun`, `StartRun`, queued admission and the
/// lifecycle worker's own re-check inside the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// An adapter without its own labels refused (fixtures only).
    Host,
    /// spawnd's core is poisoned (an earlier failed write); restart needed.
    Poisoned,
    /// The UID monitor's state was locked at that instant.
    ScanBusy,
    /// The UID monitor's last scan started more than 2 × its period ago.
    ScanStale,
    /// The UID monitor's last scan failed.
    ScanFailed,
    /// The UID monitor saw a process of this employee outside a run.
    Intruder,
    /// The published view was locked at that instant.
    ViewBusy,
    /// The employee is not a Ready registry row in spawnd's admission view.
    EmployeeNotAdmitted,
    /// No verified root publication (policy) is loaded.
    PolicyMissing,
    /// The boot id or `CLOCK_BOOTTIME` could not be read.
    BootClock,
    /// The evidence report is of another boot or version.
    ReportInvalid,
    /// The evidence report is older than its 1 s validity.
    ReportStale,
    /// The host PTY budget is zero.
    PtyBudget,
    /// The trampoline is verified without valid Landlock evidence.
    LandlockEvidence,
    /// The readiness protocol differs.
    Protocol,
    /// The first false readiness check, by its readiness.json name
    /// (`Readiness::named_checks`; never any other string).
    NotReady(&'static str),
    /// The employee has no registry row.
    AccountUnknown,
    /// The employee's SSH denial probe has no result for its current key yet.
    SshPending,
    /// The employee's account verification (lockdown, SSH) failed at this step.
    Account(crate::accounts::Step),
}
impl std::fmt::Display for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Gate::Host => "host_gate", Gate::Poisoned => "poisoned", Gate::ScanBusy => "scan_busy",
            Gate::ScanStale => "scan_stale", Gate::ScanFailed => "scan_failed", Gate::Intruder => "intruder",
            Gate::ViewBusy => "view_busy", Gate::EmployeeNotAdmitted => "employee_not_admitted",
            Gate::PolicyMissing => "policy_missing", Gate::BootClock => "boot_clock",
            Gate::ReportInvalid => "report_invalid", Gate::ReportStale => "report_stale",
            Gate::PtyBudget => "pty_budget", Gate::LandlockEvidence => "landlock_evidence",
            Gate::Protocol => "protocol", Gate::AccountUnknown => "account_unknown", Gate::SshPending => "ssh_pending",
            Gate::NotReady(flag) => return write!(f, "not_ready({flag})"),
            Gate::Account(step) => return write!(f, "account({})", step.name()),
        };
        f.write_str(name)
    }
}

/// What last held a queued run back (admission tick), for
/// [`StartStep::QueueTimeout`]/[`StartStep::QueueCancelled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    /// Nothing was recorded (the run was never examined).
    Unexamined,
    /// The host-wide run limit was reached.
    HostRuns,
    /// Another run of the same employee was active (normal serialization).
    EmployeeBusy,
    /// The host PTY budget had no room.
    HostPtys,
    /// The cleanup readback failed or was still pending.
    Cleanup,
    /// The cleanup readback was incomplete (unit, cgroup or uid processes).
    CleanupIncomplete,
    /// The host admission gate refused.
    Gate(Gate),
}
impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Blocker::Unexamined => f.write_str("unexamined"), Blocker::HostRuns => f.write_str("host_runs"),
            Blocker::EmployeeBusy => f.write_str("employee_busy"), Blocker::HostPtys => f.write_str("host_ptys"),
            Blocker::Cleanup => f.write_str("cleanup"), Blocker::CleanupIncomplete => f.write_str("cleanup_incomplete"),
            Blocker::Gate(gate) => gate.fmt(f),
        }
    }
}

/// Every refusal exit of the start path. Grouped by where it is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartStep {
    // Control socket (`server.rs`).
    /// An undecodable frame (the op is unknown; the connection is closed).
    Frame,
    /// A connection whose peer credentials are not the gateway's.
    Peer,
    /// The request failed its own validation (incl. the step-limit bound).
    Request,
    /// The passed descriptors do not match the request's fd plan.
    Fds,
    /// No random run id could be drawn.
    RunId,
    // Coordinator (`admission.rs`).
    /// spawnd's coordination clock went backwards.
    Clock,
    /// The update drain barrier is closed.
    UpdateDrain,
    /// A StopEmployeeRuns of this employee is in progress.
    Stopping,
    /// No reservation of this run on this connection.
    NotReserved,
    /// The reservation is of another employee, used, expired, or could not be made.
    Reservation,
    /// The PTY request differs from the reservation or the derived policy.
    Pty,
    /// The host request limit or the employee's queue is full.
    QueueFull,
    /// The derived launch policy is invalid (timeouts, PTY idle, deadline).
    LaunchPolicy,
    /// The pending input bytes budget has no room.
    InputBytes,
    /// The input descriptor budget has no room.
    InputFds,
    /// A coordinator invariant failed.
    Internal,
    // Host gate (`Host::ready`).
    Gate(Gate),
    // Entitlement (`RootPolicy::derive`).
    /// The employee has no entitlement in the loaded host publication.
    EntitlementMissing,
    /// No entitled profile of the requested runtime kind.
    RuntimeNotEntitled,
    /// Two entitled profiles of the requested runtime kind.
    RuntimeAmbiguous,
    /// A PTY was requested from a profile without PTY.
    PtyNotAllowed,
    /// An adapter without its own policy labels refused (fixtures only).
    HostPolicy,
    // Admission tick (queued run).
    /// The queued run expired; the last blocker recorded.
    QueueTimeout(Blocker),
    /// The gateway cancelled a queued run spawnd was holding back.
    QueueCancelled(Blocker),
    // Host start (`WorkerHost::start`, then `Core::start` on the lifecycle thread).
    /// An adapter without its own start labels refused (fixtures only).
    HostStart,
    /// The UID monitor has no row for the employee, or a run is registered.
    UidMonitorRegister,
    /// The lifecycle worker's mailbox is full or stalled.
    WorkerMailbox,
    /// The run's fence was raised (cancelled/fenced) before its unit existed.
    RunFence,
    /// The cleanup readback before start failed.
    Cleanup,
    /// The cleanup readback before start was incomplete.
    CleanupIncomplete,
    /// The requested step limit is outside the signed range.
    TurnLimit,
    /// The employee's storage usage could not be read.
    StorageUsage,
    /// The employee's storage usage exceeds the profile's disk limits.
    DiskQuota,
    /// The run journal (WAL) could not be written.
    RunJournal,
    /// The employee image could not be attached.
    StorageAttach,
    /// The attach receipt names another employee or run.
    StorageReceipt,
    /// The selected workspace could not be pinned or mounted.
    Workspace,
    /// The MCP channel pair could not be created.
    McpChannel,
    /// The egress channel pair could not be created.
    EgressChannel,
    /// The egress service refused or did not answer the registration.
    EgressRegister,
    /// The output relay pipes could not be created.
    OutputRelay,
    /// The stdin pipe could not be created.
    StdinPipe,
    /// The supervisor identity could not be resolved.
    SupervisorIdentity,
    /// No random bootstrap nonce could be drawn.
    Nonce,
    /// The bootstrap plan could not be built or is invalid.
    BootstrapPlan,
    /// The UID monitor did not accept the run.
    UidMonitor,
    /// The MCP projection could not be prepared.
    McpProjection,
    /// The root publication anchors could not be opened.
    Anchors,
    /// The bootstrap mounts could not be prepared.
    Mounts,
    /// The MCP projection readback could not be recorded.
    McpRecord,
    /// The runtime's native profile could not be loaded.
    NativeProfile,
    /// The helper launcher mounts could not be prepared.
    HelperMounts,
    /// The bootstrap plan could not be sealed.
    Seal,
    /// The trampoline could not be prepared.
    TrampolinePrepare,
    /// The prepared bootstrap descriptor is invalid.
    BootstrapInvalid,
    /// The transient unit policy is invalid.
    UnitPolicy,
    /// systemd refused or did not start the transient unit.
    UnitStart,
    /// The started unit's cgroup is not usable.
    UnitCgroup,
    /// The trampoline could not be bound to the started unit.
    BindUnit,
    // After the unit started.
    /// The run did not reach Running within its startup budget.
    StartupTimeout,
    /// The trampoline reported a failure before the run was Running.
    LaunchPoll,
}
impl std::fmt::Display for StartStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            StartStep::Frame => "frame", StartStep::Peer => "peer", StartStep::Request => "request",
            StartStep::Fds => "fds", StartStep::RunId => "run_id", StartStep::Clock => "clock",
            StartStep::UpdateDrain => "update_drain", StartStep::Stopping => "stopping",
            StartStep::NotReserved => "not_reserved", StartStep::Reservation => "reservation", StartStep::Pty => "pty",
            StartStep::QueueFull => "queue_full", StartStep::LaunchPolicy => "launch_policy",
            StartStep::InputBytes => "input_bytes", StartStep::InputFds => "input_fds", StartStep::Internal => "internal",
            StartStep::Gate(gate) => return gate.fmt(f),
            StartStep::EntitlementMissing => "entitlement_missing", StartStep::RuntimeNotEntitled => "runtime_not_entitled",
            StartStep::RuntimeAmbiguous => "runtime_ambiguous", StartStep::PtyNotAllowed => "pty_not_allowed",
            StartStep::HostPolicy => "host_policy",
            StartStep::QueueTimeout(Blocker::Unexamined) => "queue_timeout",
            StartStep::QueueTimeout(blocker) => return write!(f, "queue_timeout({blocker})"),
            StartStep::QueueCancelled(blocker) => return write!(f, "queue_cancelled({blocker})"),
            StartStep::HostStart => "host_start", StartStep::UidMonitorRegister => "uid_monitor_register",
            StartStep::WorkerMailbox => "worker_mailbox", StartStep::RunFence => "run_fence",
            StartStep::Cleanup => "cleanup", StartStep::CleanupIncomplete => "cleanup_incomplete",
            StartStep::TurnLimit => "turn_limit", StartStep::StorageUsage => "storage_usage",
            StartStep::DiskQuota => "disk_quota", StartStep::RunJournal => "run_journal",
            StartStep::StorageAttach => "storage_attach", StartStep::StorageReceipt => "storage_receipt",
            StartStep::Workspace => "workspace", StartStep::McpChannel => "mcp_channel",
            StartStep::EgressChannel => "egress_channel", StartStep::EgressRegister => "egress_register",
            StartStep::OutputRelay => "output_relay", StartStep::StdinPipe => "stdin_pipe",
            StartStep::SupervisorIdentity => "supervisor_identity", StartStep::Nonce => "nonce",
            StartStep::BootstrapPlan => "bootstrap_plan", StartStep::UidMonitor => "uid_monitor",
            StartStep::McpProjection => "mcp_projection", StartStep::Anchors => "anchors", StartStep::Mounts => "mounts",
            StartStep::McpRecord => "mcp_record", StartStep::NativeProfile => "native_profile",
            StartStep::HelperMounts => "helper_mounts", StartStep::Seal => "seal",
            StartStep::TrampolinePrepare => "trampoline_prepare", StartStep::BootstrapInvalid => "bootstrap_invalid",
            StartStep::UnitPolicy => "unit_policy", StartStep::UnitStart => "unit_start",
            StartStep::UnitCgroup => "unit_cgroup", StartStep::BindUnit => "bind_unit",
            StartStep::StartupTimeout => "startup_timeout", StartStep::LaunchPoll => "launch_poll",
        };
        f.write_str(name)
    }
}
impl StartStep {
    /// The journal line after the `duduclaw-spawnd: ` prefix.
    pub fn line(self) -> String { format!("start refused: step={self}") }
}
impl From<Gate> for StartStep { fn from(gate: Gate) -> Self { StartStep::Gate(gate) } }

/// A labelled refusal: the step and the unchanged wire code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal { pub step: StartStep, pub code: ErrorCode }
impl Refusal {
    pub const fn new(step: StartStep, code: ErrorCode) -> Self { Self { step, code } }
    /// The wire code (what the exit returned before R8y).
    pub fn code(self) -> ErrorCode { self.code }
    /// The gate, when the step is one (`Gate::Host` otherwise).
    pub fn gate(self) -> Gate { match self.step { StartStep::Gate(gate) => gate, _ => Gate::Host } }
}
/// Deliberately no `From<ErrorCode> for Refusal`: see the module docs.
pub type Labelled<T> = std::result::Result<T, Refusal>;
/// Labels an `ErrorCode` with `step`, code unchanged.
pub fn at(step: StartStep) -> impl Fn(ErrorCode) -> Refusal { move |code| Refusal { step, code } }
/// Labels an `ErrorCode` with a gate, code unchanged.
pub fn gate(gate: Gate) -> impl Fn(ErrorCode) -> Refusal { at(StartStep::Gate(gate)) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Step;

    /// Every variant once (parameters: one representative). The match below
    /// has no wildcard, so a new variant fails to compile until it is listed.
    fn all() -> Vec<StartStep> {
        let gates = [Gate::Host, Gate::Poisoned, Gate::ScanBusy, Gate::ScanStale, Gate::ScanFailed, Gate::Intruder,
            Gate::ViewBusy, Gate::EmployeeNotAdmitted, Gate::PolicyMissing, Gate::BootClock, Gate::ReportInvalid,
            Gate::ReportStale, Gate::PtyBudget, Gate::LandlockEvidence, Gate::Protocol, Gate::NotReady("registry_verified"),
            Gate::AccountUnknown, Gate::SshPending, Gate::Account(Step::SshProbe)];
        let mut steps = vec![StartStep::Frame, StartStep::Peer, StartStep::Request, StartStep::Fds, StartStep::RunId,
            StartStep::Clock, StartStep::UpdateDrain, StartStep::Stopping, StartStep::NotReserved, StartStep::Reservation,
            StartStep::Pty, StartStep::QueueFull, StartStep::LaunchPolicy, StartStep::InputBytes, StartStep::InputFds,
            StartStep::Internal, StartStep::EntitlementMissing, StartStep::RuntimeNotEntitled, StartStep::RuntimeAmbiguous,
            StartStep::PtyNotAllowed, StartStep::HostPolicy, StartStep::QueueTimeout(Blocker::Unexamined),
            StartStep::QueueCancelled(Blocker::EmployeeBusy), StartStep::HostStart, StartStep::UidMonitorRegister,
            StartStep::WorkerMailbox, StartStep::RunFence, StartStep::Cleanup, StartStep::CleanupIncomplete,
            StartStep::TurnLimit, StartStep::StorageUsage, StartStep::DiskQuota, StartStep::RunJournal,
            StartStep::StorageAttach, StartStep::StorageReceipt, StartStep::Workspace, StartStep::McpChannel,
            StartStep::EgressChannel, StartStep::EgressRegister, StartStep::OutputRelay, StartStep::StdinPipe,
            StartStep::SupervisorIdentity, StartStep::Nonce, StartStep::BootstrapPlan, StartStep::UidMonitor,
            StartStep::McpProjection, StartStep::Anchors, StartStep::Mounts, StartStep::McpRecord,
            StartStep::NativeProfile, StartStep::HelperMounts, StartStep::Seal, StartStep::TrampolinePrepare,
            StartStep::BootstrapInvalid, StartStep::UnitPolicy, StartStep::UnitStart, StartStep::UnitCgroup,
            StartStep::BindUnit, StartStep::StartupTimeout, StartStep::LaunchPoll];
        steps.extend(gates.into_iter().map(StartStep::Gate));
        for step in &steps { listed(*step); }
        steps
    }
    /// Compile-time exhaustiveness of [`all`] (no wildcard arm).
    fn listed(step: StartStep) {
        match step {
            StartStep::Frame | StartStep::Peer | StartStep::Request | StartStep::Fds | StartStep::RunId
            | StartStep::Clock | StartStep::UpdateDrain | StartStep::Stopping | StartStep::NotReserved
            | StartStep::Reservation | StartStep::Pty | StartStep::QueueFull | StartStep::LaunchPolicy
            | StartStep::InputBytes | StartStep::InputFds | StartStep::Internal | StartStep::EntitlementMissing
            | StartStep::RuntimeNotEntitled | StartStep::RuntimeAmbiguous | StartStep::PtyNotAllowed
            | StartStep::HostPolicy | StartStep::QueueTimeout(_) | StartStep::QueueCancelled(_) | StartStep::HostStart
            | StartStep::UidMonitorRegister | StartStep::WorkerMailbox | StartStep::RunFence | StartStep::Cleanup
            | StartStep::CleanupIncomplete | StartStep::TurnLimit | StartStep::StorageUsage | StartStep::DiskQuota
            | StartStep::RunJournal | StartStep::StorageAttach | StartStep::StorageReceipt | StartStep::Workspace
            | StartStep::McpChannel | StartStep::EgressChannel | StartStep::EgressRegister | StartStep::OutputRelay
            | StartStep::StdinPipe | StartStep::SupervisorIdentity | StartStep::Nonce | StartStep::BootstrapPlan
            | StartStep::UidMonitor | StartStep::McpProjection | StartStep::Anchors | StartStep::Mounts
            | StartStep::McpRecord | StartStep::NativeProfile | StartStep::HelperMounts | StartStep::Seal
            | StartStep::TrampolinePrepare | StartStep::BootstrapInvalid | StartStep::UnitPolicy
            | StartStep::UnitStart | StartStep::UnitCgroup | StartStep::BindUnit | StartStep::StartupTimeout
            | StartStep::LaunchPoll => {}
            StartStep::Gate(gate) => match gate {
                Gate::Host | Gate::Poisoned | Gate::ScanBusy | Gate::ScanStale | Gate::ScanFailed | Gate::Intruder
                | Gate::ViewBusy | Gate::EmployeeNotAdmitted | Gate::PolicyMissing | Gate::BootClock
                | Gate::ReportInvalid | Gate::ReportStale | Gate::PtyBudget | Gate::LandlockEvidence | Gate::Protocol
                | Gate::NotReady(_) | Gate::AccountUnknown | Gate::SshPending | Gate::Account(_) => {}
            },
        }
    }

    /// R8y: the labels are distinct fixed identifiers (`[a-z_]` with at most
    /// fixed-identifier parameters in parentheses) and the line has one shape.
    #[test]
    fn start_refusal_labels_are_a_closed_set_of_fixed_identifiers() {
        let names: Vec<String> = all().iter().map(ToString::to_string).collect();
        let unique: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        for name in &names {
            assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_()".contains(&b)), "{name}");
        }
        assert_eq!(StartStep::Gate(Gate::EmployeeNotAdmitted).line(), "start refused: step=employee_not_admitted");
        assert_eq!(StartStep::Gate(Gate::NotReady("dns_route_verified")).line(), "start refused: step=not_ready(dns_route_verified)");
        assert_eq!(StartStep::Gate(Gate::Account(Step::CronAcl)).to_string(), "account(cron_acl)");
        assert_eq!(StartStep::QueueTimeout(Blocker::Unexamined).to_string(), "queue_timeout");
        assert_eq!(StartStep::QueueTimeout(Blocker::Gate(Gate::SshPending)).to_string(), "queue_timeout(ssh_pending)");
        assert_eq!(StartStep::QueueCancelled(Blocker::HostRuns).to_string(), "queue_cancelled(host_runs)");
        // Every readiness flag parameter is a readiness.json name.
        for (flag, _) in crate::readiness::Readiness::default().named_checks() {
            assert_eq!(StartStep::Gate(Gate::NotReady(flag)).to_string(), format!("not_ready({flag})"));
        }
    }
    #[test]
    fn labelling_keeps_the_wire_code() {
        let refusal = at(StartStep::EgressRegister)(ErrorCode::IsolationUnavailable);
        assert_eq!((refusal.step, refusal.code()), (StartStep::EgressRegister, ErrorCode::IsolationUnavailable));
        let refusal = gate(Gate::SshPending)(ErrorCode::NotReady);
        assert_eq!((refusal.gate(), refusal.code()), (Gate::SshPending, ErrorCode::NotReady));
        assert_eq!(at(StartStep::Frame)(ErrorCode::Internal).gate(), Gate::Host);
    }
}
