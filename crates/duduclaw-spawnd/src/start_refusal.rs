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

/// [`StartStep`] variants without a parameter (all but `Gate`, `QueueTimeout`,
/// `QueueCancelled`). Pinned against the exhaustive test list.
const UNIT_STEPS: usize = 58;
/// [`Gate`] variants without a parameter (all but `NotReady`, `Account`).
const UNIT_GATES: usize = 17;
/// [`Blocker`] variants without a parameter (all but `Gate`).
const UNIT_BLOCKERS: usize = 6;
/// R8y review round 1 (F2): the size of the closed set of rendered start
/// refusal labels. The parameterised labels have bounded inner sets:
/// `not_ready(<flag>)` over the readiness.json check names (14),
/// `account(<step>)` over [`crate::accounts::Step::ALL`] (37), so
/// G = 17 + 14 + 37 = 68 gate labels, and `queue_timeout(<blocker>)` /
/// `queue_cancelled(<blocker>)` over 6 + G = 74 blockers each:
/// 58 + 68 + 2 × 74 = 274 labels.
pub fn label_bound() -> usize {
    let gates = UNIT_GATES + crate::readiness::Readiness::default().named_checks().len() + crate::accounts::Step::ALL.len();
    UNIT_STEPS + gates + 2 * (UNIT_BLOCKERS + gates)
}

/// R8y review round 1 (F2): at most this many identical journal lines are
/// printed per [`LINE_WINDOW`]; further ones are only counted.
pub const LINES_PER_WINDOW: u64 = 5;
/// R8y review round 1 (F2): the aggregation window.
pub const LINE_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// R8y review round 1 (F2): bounded, aggregated journal lines a peer can
/// trigger repeatedly (`start refused: step=<label>`, `allocation failed:
/// step=<label>`). Keyed by the full fixed line (its label determines it).
/// In one window the first [`LINES_PER_WINDOW`] occurrences of a line are
/// printed as before; further ones are counted, and when the window ends ONE
/// `<line> suppressed=<count>` is printed per line that was suppressed. No
/// occurrence is lost: printed + suppressed = noted. Memory: at most `cap`
/// distinct lines (the closed label set's size, [`label_bound`]) per window,
/// cleared at every roll-over; a line beyond `cap` (unreachable when `cap` is
/// the closed set's size) is still counted, as `<kind>: step=other`.
/// No thread of its own: callers pass their monotonic `Instant` and call
/// [`Self::flush`] on their existing cadence (server loop, lifecycle refresh).
#[derive(Debug)]
pub struct LineLimiter {
    kind: &'static str,
    cap: usize,
    window: Option<std::time::Instant>,
    seen: std::collections::BTreeMap<String, Seen>,
    other: u64,
}
#[derive(Debug, Default)]
struct Seen { printed: u64, suppressed: u64 }
impl LineLimiter {
    /// `kind` is the line's fixed prefix before `: step=` (for the overflow line).
    pub fn new(kind: &'static str, cap: usize) -> Self {
        Self { kind, cap, window: None, seen: std::collections::BTreeMap::new(), other: 0 }
    }
    /// `count` occurrences of `line` at `now`: the lines to print now (a
    /// finished window's summaries first, then up to the remaining budget of
    /// copies of `line`).
    pub fn note(&mut self, line: String, count: u64, now: std::time::Instant) -> Vec<String> {
        let mut out = self.flush(now);
        if count == 0 { return out; }
        if !self.seen.contains_key(&line) && self.seen.len() >= self.cap {
            self.other = self.other.saturating_add(count);
            return out;
        }
        let seen = self.seen.entry(line.clone()).or_default();
        let print = LINES_PER_WINDOW.saturating_sub(seen.printed).min(count);
        seen.printed += print;
        seen.suppressed = seen.suppressed.saturating_add(count - print);
        out.extend((0..print).map(|_| line.clone()));
        out
    }
    /// Ends the window when [`LINE_WINDOW`] has passed since it began: one
    /// `<line> suppressed=<count>` per suppressed line (fixed order), and a
    /// new window begins at `now`. Otherwise nothing.
    pub fn flush(&mut self, now: std::time::Instant) -> Vec<String> {
        let start = *self.window.get_or_insert(now);
        if now.saturating_duration_since(start) < LINE_WINDOW { return Vec::new(); }
        self.window = Some(now);
        self.summaries()
    }
    /// R8y review round 2: ends the current window NOW, whatever its age, for
    /// a normal shutdown: the same `<line> suppressed=<count>` lines
    /// [`Self::flush`] would print when the window ends (same format, same
    /// order), so a tally pending at shutdown is not lost. Empty when nothing
    /// is suppressed; idempotent (a second call returns nothing). A later
    /// [`Self::note`] starts a fresh window.
    pub fn drain(&mut self) -> Vec<String> {
        self.window = None;
        self.summaries()
    }
    /// The window's summaries; clears its counts.
    fn summaries(&mut self) -> Vec<String> {
        let mut out: Vec<String> = self.seen.iter().filter(|(_, seen)| seen.suppressed > 0)
            .map(|(line, seen)| format!("{line} suppressed={}", seen.suppressed)).collect();
        if self.other > 0 { out.push(format!("{}: step=other suppressed={}", self.kind, self.other)); }
        self.seen.clear();
        self.other = 0;
        out
    }
}

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
        // R8y review round 1: the server's hand-labelled exits (no fake can
        // reach the peer and random-id exits) are pinned by their labels.
        assert_eq!(StartStep::Peer.line(), "start refused: step=peer");
        assert_eq!(StartStep::Fds.line(), "start refused: step=fds");
        assert_eq!(StartStep::RunId.line(), "start refused: step=run_id");
        assert_eq!(StartStep::Frame.line(), "start refused: step=frame");
        // Every readiness flag parameter is a readiness.json name.
        for (flag, _) in crate::readiness::Readiness::default().named_checks() {
            assert_eq!(StartStep::Gate(Gate::NotReady(flag)).to_string(), format!("not_ready({flag})"));
        }
    }

    fn blocker_listed(blocker: Blocker) {
        match blocker {
            Blocker::Unexamined | Blocker::HostRuns | Blocker::EmployeeBusy | Blocker::HostPtys | Blocker::Cleanup
            | Blocker::CleanupIncomplete | Blocker::Gate(_) => {}
        }
    }
    /// Every rendered label (every parameter of every variant).
    fn every_label() -> std::collections::BTreeSet<String> {
        let listed = all();
        let units: Vec<StartStep> = listed.iter().copied()
            .filter(|step| !matches!(step, StartStep::Gate(_) | StartStep::QueueTimeout(_) | StartStep::QueueCancelled(_))).collect();
        let mut gates: Vec<Gate> = listed.iter().filter_map(|step| match step {
            StartStep::Gate(gate) if !matches!(gate, Gate::NotReady(_) | Gate::Account(_)) => Some(*gate), _ => None }).collect();
        assert_eq!((units.len(), gates.len()), (UNIT_STEPS, UNIT_GATES));
        gates.extend(crate::readiness::Readiness::default().named_checks().into_iter().map(|(flag, _)| Gate::NotReady(flag)));
        gates.extend(Step::ALL.into_iter().map(Gate::Account));
        let mut blockers = vec![Blocker::Unexamined, Blocker::HostRuns, Blocker::EmployeeBusy, Blocker::HostPtys,
            Blocker::Cleanup, Blocker::CleanupIncomplete];
        for blocker in &blockers { blocker_listed(*blocker); }
        assert_eq!(blockers.len(), UNIT_BLOCKERS);
        blockers.extend(gates.iter().copied().map(Blocker::Gate));
        let mut steps = units;
        steps.extend(gates.iter().copied().map(StartStep::Gate));
        steps.extend(blockers.iter().copied().map(StartStep::QueueTimeout));
        steps.extend(blockers.iter().copied().map(StartStep::QueueCancelled));
        let labels: std::collections::BTreeSet<String> = steps.iter().map(ToString::to_string).collect();
        assert_eq!(labels.len(), steps.len(), "rendered labels are distinct");
        labels
    }
    /// R8y review round 1 (F2): the limiter's cap is the closed set's size.
    #[test]
    fn label_bound_is_the_closed_label_set_size() {
        assert_eq!(every_label().len(), label_bound());
        assert_eq!(label_bound(), 274);
    }

    fn summed(lines: &[String], line: &str) -> u64 {
        let printed = lines.iter().filter(|l| *l == line).count() as u64;
        let prefix = format!("{line} suppressed=");
        printed + lines.iter().filter_map(|l| l.strip_prefix(prefix.as_str())).map(|n| n.parse::<u64>().unwrap()).sum::<u64>()
    }
    /// R8y review round 1 (F2): 1,000 refusals of one label in one window
    /// print at most N + 1 lines, and the counts add up.
    #[test]
    fn limiter_prints_at_most_n_plus_one_lines_per_label_and_window() {
        let t0 = std::time::Instant::now();
        let line = StartStep::Gate(Gate::EmployeeNotAdmitted).line();
        let mut limiter = LineLimiter::new("start refused", label_bound());
        let mut out = Vec::new();
        for i in 0..1_000u64 { out.extend(limiter.note(line.clone(), 1, t0 + std::time::Duration::from_millis(i))); }
        assert_eq!(out, vec![line.clone(); LINES_PER_WINDOW as usize], "the first N, as before");
        assert!(limiter.flush(t0 + LINE_WINDOW - std::time::Duration::from_millis(1)).is_empty(), "the window has not ended");
        out.extend(limiter.flush(t0 + LINE_WINDOW));
        assert!(out.len() as u64 <= LINES_PER_WINDOW + 1, "{out:?}");
        assert_eq!(out.last().unwrap(), "start refused: step=employee_not_admitted suppressed=995");
        assert_eq!(summed(&out, &line), 1_000);
        // Nothing left over: the next window starts empty.
        assert!(limiter.flush(t0 + LINE_WINDOW * 3).is_empty());
    }
    #[test]
    fn limiter_counts_two_interleaved_labels_separately() {
        let t0 = std::time::Instant::now();
        let (a, b) = (StartStep::Fds.line(), StartStep::QueueTimeout(Blocker::HostRuns).line());
        let mut limiter = LineLimiter::new("start refused", label_bound());
        let mut out = Vec::new();
        for _ in 0..20 {
            out.extend(limiter.note(a.clone(), 1, t0));
            out.extend(limiter.note(b.clone(), 1, t0));
        }
        assert_eq!(out.iter().filter(|l| **l == a).count(), 5);
        assert_eq!(out.iter().filter(|l| **l == b).count(), 5);
        let summaries = limiter.flush(t0 + LINE_WINDOW);
        assert_eq!(summaries, vec![format!("{a} suppressed=15"), format!("{b} suppressed=15")]);
        out.extend(summaries);
        assert_eq!((summed(&out, &a), summed(&out, &b)), (20, 20));
    }
    #[test]
    fn limiter_window_rolls_over_and_reports_before_the_next_line() {
        let t0 = std::time::Instant::now();
        let line = StartStep::Peer.line();
        let mut limiter = LineLimiter::new("start refused", label_bound());
        let mut out = Vec::new();
        for _ in 0..7 { out.extend(limiter.note(line.clone(), 1, t0)); }
        assert_eq!(out.len(), 5);
        // The first note after the window: the summary, then a fresh budget.
        let next = limiter.note(line.clone(), 1, t0 + LINE_WINDOW);
        assert_eq!(next, vec![format!("{line} suppressed=2"), line.clone()]);
        // A window with no suppression prints no summary.
        assert!(limiter.flush(t0 + LINE_WINDOW * 2).is_empty());
        // Idle for an hour: the next line prints at once.
        assert_eq!(limiter.note(line.clone(), 1, t0 + std::time::Duration::from_secs(3_600)), vec![line]);
    }
    /// R8y review round 1 (F2): a burst far beyond the old 256-entry buffer
    /// (one aggregated count, as the coordinator hands it over) loses nothing.
    #[test]
    fn limiter_burst_beyond_the_old_buffer_loses_nothing_from_the_count() {
        let t0 = std::time::Instant::now();
        let line = StartStep::QueueTimeout(Blocker::HostRuns).line();
        let mut limiter = LineLimiter::new("start refused", label_bound());
        let mut out = limiter.note(line.clone(), 10_000, t0);
        assert_eq!(out.len(), LINES_PER_WINDOW as usize);
        out.extend(limiter.flush(t0 + LINE_WINDOW));
        assert_eq!(summed(&out, &line), 10_000);
        // Beyond the cap (unreachable with `label_bound`): still counted.
        let mut tiny = LineLimiter::new("start refused", 1);
        let mut out = tiny.note(StartStep::Peer.line(), 1, t0);
        out.extend(tiny.note(StartStep::Fds.line(), 3, t0));
        out.extend(tiny.flush(t0 + LINE_WINDOW));
        assert_eq!(out, vec![StartStep::Peer.line(), "start refused: step=other suppressed=3".to_string()]);
    }
    /// R8y review round 2: a shutdown before the window ends prints each
    /// label's pending `suppressed=<count>` exactly once (the sink is the
    /// returned lines, which every caller writes to the journal verbatim).
    #[test]
    fn limiter_drain_prints_pending_summaries_once_before_the_window_ends() {
        let t0 = std::time::Instant::now();
        let (a, b) = (StartStep::Fds.line(), StartStep::Peer.line());
        let mut limiter = LineLimiter::new("start refused", label_bound());
        let mut sink: Vec<String> = Vec::new();
        for _ in 0..8 { sink.extend(limiter.note(a.clone(), 1, t0)); }
        sink.extend(limiter.note(b.clone(), 12, t0));
        // A label within its budget has nothing pending.
        let c = StartStep::Request.line();
        sink.extend(limiter.note(c.clone(), 2, t0));
        assert!(limiter.flush(t0 + std::time::Duration::from_secs(1)).is_empty(), "the window has not ended");
        let drained = limiter.drain();
        assert_eq!(drained, vec![format!("{a} suppressed=3"), format!("{b} suppressed=7")]);
        sink.extend(drained);
        assert_eq!((summed(&sink, &a), summed(&sink, &b), summed(&sink, &c)), (8, 12, 2), "nothing lost");
        for line in [&a, &b] {
            assert_eq!(sink.iter().filter(|l| l.starts_with(&format!("{line} suppressed="))).count(), 1, "{line}: exactly once");
        }
        // Idempotent: a second drain, and the window's own end, print nothing more.
        assert!(limiter.drain().is_empty());
        assert!(limiter.flush(t0 + LINE_WINDOW).is_empty());
        // The overflow tally is drained in the same format.
        let mut tiny = LineLimiter::new("allocation failed", 1);
        let _ = tiny.note("allocation failed: step=one".into(), 1, t0);
        let _ = tiny.note("allocation failed: step=two".into(), 4, t0);
        assert_eq!(tiny.drain(), vec!["allocation failed: step=other suppressed=4".to_string()]);
        assert!(tiny.drain().is_empty());
    }
    #[test]
    fn limiter_drain_of_an_empty_limiter_prints_nothing() {
        let mut limiter = LineLimiter::new("start refused", label_bound());
        assert!(limiter.drain().is_empty());
        assert!(limiter.drain().is_empty());
        // Nothing suppressed (within budget): nothing to drain either.
        let t0 = std::time::Instant::now();
        assert_eq!(limiter.note(StartStep::Peer.line(), 5, t0).len(), 5);
        assert!(limiter.drain().is_empty());
        // After a drain the next line starts a fresh window and prints at once.
        assert_eq!(limiter.note(StartStep::Peer.line(), 1, t0), vec![StartStep::Peer.line()]);
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
