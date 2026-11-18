//! R7c: per-account SSH denial evidence from the probe unit
//! `duduclaw-isolation-probe@<account>.service`, started through PID 1
//! (`Systemd::run_fixed_unit`, [`FixedUnit::Probe`]) instead of read from an
//! installer receipt.
//!
//! The probe (`/usr/lib/duduclaw/duduclaw-isolation-probe`) attempts a real
//! public-key login and a real `-N -R` forwarding session against the live
//! sshd and requires sshd's own DenyUsers journal record for each; its exit
//! code (`duduclaw_isolation::probe_exit`) is the only thing spawnd reads, and
//! only once it is bound to THIS invocation
//! ([`FixedUnitOutcome::exit_status_of_this_start`]): 0 ⇒ [`ProbeOutcome::Denied`],
//! "no sshd" ⇒ `NotApplicable`, "login/forwarding NOT denied" ⇒ `Reachable`,
//! anything else (other codes, timeout, a refused start, PID 1 errors) ⇒
//! `Inconclusive`.
//!
//! A result is kept in memory under a [`ProbeKey`]: the host part (boot id,
//! committed signed generation, the installed probe binary verified against
//! that generation's `IsolationProbe` artifact (digest, size, mode), the
//! installed template unit's digest) and the account part (name, uid, SHA-256
//! of `sshd -T -C user=<account>,host=localhost,addr=127.0.0.1`, which
//! `verify_login` computes anyway). Any component change makes the result not
//! current: the account is probed again. Nothing is persisted, so after a
//! spawnd restart every account is probed again.
//!
//! Only the single worker thread ([`worker_loop`], its own D-Bus connection)
//! measures the host key (every [`WORKER_PERIOD_MS`]) and starts probe units,
//! one at a time, each bounded by [`UNIT_DEADLINE_MS`]. The refresh path
//! ([`ProbeEvidence::lookup`]) only reads the shared state and queues a
//! request; a host key older than [`HOST_FRESH_MS`] makes every result not
//! current. The allocation path alone waits ([`ProbeEvidence::wait`]). An
//! inconclusive result is retried after [`INCONCLUSIVE_RETRY_MS`] (at once
//! for an allocation). The worker logs only when an account's outcome (or the
//! host key's availability) changes, never sshd output.
use std::{collections::{BTreeMap, VecDeque}, io, sync::{Arc, Condvar, Mutex}, time::{Duration, Instant}};
use duduclaw_isolation::{ids::Sha256Digest, probe_exit};
use crate::{accounts::SshRefusal, selftest::ArtifactKey, systemd::{FixedUnit, FixedUnitOutcome}};

/// The unit's own `TimeoutStartSec=30` plus margin for PID 1 to record it.
pub const UNIT_DEADLINE_MS: u64 = 40_000;
/// How long an allocation waits for its account's result: at most one probe
/// already running for another account plus its own, plus margin.
pub const ALLOCATION_WAIT_MS: u64 = 2 * UNIT_DEADLINE_MS + 5_000;
/// The worker re-measures the host key at least this often.
pub const WORKER_PERIOD_MS: u64 = 2_000;
/// Results are current only under a host key measured less than this long ago.
pub const HOST_FRESH_MS: u64 = 8_000;
/// An inconclusive (or contradictory "no sshd") result is probed again after this long.
pub const INCONCLUSIVE_RETRY_MS: u64 = 60_000;
/// Bound on queued requests (the registry holds at most 30 000 accounts).
pub const MAX_QUEUED: usize = 30_000;

/// Host-wide inputs of every probe, measured by the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey { pub boot_id: String, pub generation: Sha256Digest, pub probe: ArtifactKey, pub unit: Sha256Digest }
/// The account's inputs, computed by `verify_login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountKey { pub account: String, pub uid: u32, pub sshd_config: Sha256Digest }
/// Everything one result is valid for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeKey { pub host: HostKey, pub account: AccountKey }

/// One probe's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Exit 0 of this invocation: login and forwarding denied.
    Denied,
    /// The probe found no `/usr/sbin/sshd`.
    NotApplicable,
    /// The login or the forwarding session was NOT denied (stage, code).
    Reachable(String),
    /// Anything else (stage/code, or why the outcome is not this start's).
    Inconclusive(String),
}
impl ProbeOutcome {
    fn describe(&self) -> String {
        match self {
            Self::Denied => "denied".into(),
            Self::NotApplicable => "not applicable (the probe found no sshd)".into(),
            Self::Reachable(stage) => format!("NOT denied: {stage}"),
            Self::Inconclusive(reason) => format!("inconclusive: {reason}"),
        }
    }
}
/// `verify_login`'s answer for a lookup: `None` only for Denied; no result is
/// [`SshRefusal::Pending`]. On a host with `/usr/sbin/sshd`, "no sshd" from the
/// probe is a contradiction and refuses as well.
pub fn refusal(outcome: Option<&ProbeOutcome>) -> Option<SshRefusal> {
    match outcome {
        Some(ProbeOutcome::Denied) => None,
        None => Some(SshRefusal::Pending),
        Some(ProbeOutcome::NotApplicable) => Some(SshRefusal::NotApplicable),
        Some(ProbeOutcome::Reachable(stage)) => Some(SshRefusal::Reachable(stage.clone())),
        Some(ProbeOutcome::Inconclusive(reason)) => Some(SshRefusal::Inconclusive(reason.clone())),
    }
}
/// The probe's exit code mapped with the shared table.
pub fn exit_outcome(code: i32) -> ProbeOutcome {
    let stage = || match probe_exit::stage_name(code) {
        Some(name) => format!("{name} (exit {code})"),
        None => format!("exit {code}"),
    };
    match code {
        probe_exit::EXIT_DENIED => ProbeOutcome::Denied,
        probe_exit::EXIT_NO_SSHD => ProbeOutcome::NotApplicable,
        probe_exit::EXIT_LOGIN_NOT_DENIED | probe_exit::EXIT_FORWARD_NOT_DENIED => ProbeOutcome::Reachable(stage()),
        _ => ProbeOutcome::Inconclusive(stage()),
    }
}
/// The outcome PID 1 recorded for one start of `unit`: the exit code only
/// when bound to this invocation, otherwise inconclusive naming why.
pub fn unit_outcome(unit: &FixedUnit, outcome: &FixedUnitOutcome) -> ProbeOutcome {
    match outcome.exit_status_of_this_start(unit) {
        Ok(code) => exit_outcome(code),
        Err(reason) => ProbeOutcome::Inconclusive(format!("outcome not bound to this start: {reason}")),
    }
}

/// The account side's view of the evidence (`LinuxAccounts`). `lookup` never
/// blocks; `wait` blocks at most `timeout` (allocation only).
pub trait SshEvidence: Send + Sync {
    /// The current result for `account`, or `None`; a missing, stale or
    /// retry-due result is requested from the worker (non-blocking).
    fn lookup(&self, account: &AccountKey) -> Option<ProbeOutcome>;
    /// Request a probe of `account` ahead of background requests (also after
    /// an inconclusive result) and wait for its result; `None` at the deadline.
    fn wait(&self, account: &AccountKey, timeout: Duration) -> Option<ProbeOutcome>;
}
/// No evidence at all (the default component): every account is pending.
pub struct MissingSshEvidence;
impl SshEvidence for MissingSshEvidence {
    fn lookup(&self, _: &AccountKey) -> Option<ProbeOutcome> { None }
    fn wait(&self, _: &AccountKey, _: Duration) -> Option<ProbeOutcome> { None }
}

#[derive(Debug, Clone)]
struct Stored { key: ProbeKey, outcome: ProbeOutcome, at_ms: u64, sequence: u64 }
#[derive(Debug, Clone)]
struct Request { account: AccountKey, force: bool }
/// State shared by the refresh side and the worker. The lock is held only to
/// read or update it, never across a host-key measurement or a probe run.
#[derive(Default)]
pub struct State {
    host: Option<(HostKey, u64)>,
    results: BTreeMap<String, Stored>,
    queue: VecDeque<Request>,
    /// Results stored so far (orders them for `wait`).
    completed: u64,
    /// The host key's availability last logged (`Some(None)`: available).
    logged_host: Option<Option<String>>,
    stopped: bool,
}
impl State {
    fn host_at(&self, now: u64) -> Option<&HostKey> {
        self.host.as_ref().filter(|(_, at)| now >= *at && now - *at < HOST_FRESH_MS).map(|(key, _)| key)
    }
    /// The stored result for exactly `account` under the fresh host key.
    fn current(&self, account: &AccountKey, now: u64) -> Option<&Stored> {
        let host = self.host_at(now)?;
        self.results.get(&account.account).filter(|stored| stored.key.host == *host && stored.key.account == *account)
    }
    /// No current result, or an unresolved one due again (`force`: at once).
    fn needs_probe(&self, account: &AccountKey, now: u64, force: bool) -> bool {
        match self.current(account, now) {
            None => true,
            Some(stored) => matches!(stored.outcome, ProbeOutcome::Inconclusive(_) | ProbeOutcome::NotApplicable)
                && (force || now.saturating_sub(stored.at_ms) >= INCONCLUSIVE_RETRY_MS),
        }
    }
    /// One queued request per account (the newest key); a forced one goes first.
    fn enqueue(&mut self, account: AccountKey, force: bool) {
        let force = match self.queue.iter().position(|request| request.account.account == account.account) {
            Some(index) => self.queue.remove(index).is_some_and(|old| old.force) || force,
            None if self.queue.len() >= MAX_QUEUED => return,
            None => force,
        };
        let request = Request { account, force };
        if force { self.queue.push_front(request) } else { self.queue.push_back(request) }
    }
}
#[derive(Default)]
pub struct Shared { state: Mutex<State>, changed: Condvar }

/// Starts the worker for the shared state (once, on the first request).
pub type StartWorker = Box<dyn FnOnce(Arc<Shared>) + Send>;
/// Monotonic milliseconds for the refresh side (CLOCK_BOOTTIME in production).
pub type Clock = Box<dyn Fn() -> io::Result<u64> + Send + Sync>;

/// The installed component (`Components::ssh`).
pub struct ProbeEvidence { shared: Arc<Shared>, start: Mutex<Option<StartWorker>>, clock: Clock }
impl ProbeEvidence {
    pub fn new(start: StartWorker, clock: Clock) -> Self {
        Self { shared: Arc::new(Shared::default()), start: Mutex::new(Some(start)), clock }
    }
    fn start_worker(&self) {
        let start = self.start.lock().ok().and_then(|mut slot| slot.take());
        if let Some(start) = start { start(self.shared.clone()); }
    }
}
impl SshEvidence for ProbeEvidence {
    fn lookup(&self, account: &AccountKey) -> Option<ProbeOutcome> {
        let now = (self.clock)().ok();
        let (current, requested) = {
            let mut state = self.shared.state.lock().ok()?;
            let current = now.and_then(|now| state.current(account, now)).map(|stored| stored.outcome.clone());
            let requested = now.is_none_or(|now| state.needs_probe(account, now, false));
            if requested { state.enqueue(account.clone(), false); }
            (current, requested)
        };
        if requested { self.shared.changed.notify_all(); }
        self.start_worker();
        current
    }
    fn wait(&self, account: &AccountKey, timeout: Duration) -> Option<ProbeOutcome> {
        let deadline = Instant::now() + timeout;
        let since = {
            let mut state = self.shared.state.lock().ok()?;
            let now = (self.clock)().ok()?;
            if let Some(stored) = state.current(account, now) {
                if matches!(stored.outcome, ProbeOutcome::Denied | ProbeOutcome::Reachable(_)) { return Some(stored.outcome.clone()); }
            }
            state.enqueue(account.clone(), true);
            state.completed
        };
        self.shared.changed.notify_all();
        self.start_worker();
        let mut state = self.shared.state.lock().ok()?;
        loop {
            let now = (self.clock)().ok()?;
            if let Some(stored) = state.current(account, now).filter(|stored| stored.sequence > since) {
                return Some(stored.outcome.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || state.stopped { return None; }
            state = self.shared.changed.wait_timeout(state, remaining.min(Duration::from_millis(500))).ok()?.0;
        }
    }
}
impl Drop for ProbeEvidence {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() { state.stopped = true; }
        self.shared.changed.notify_all();
    }
}

/// The worker's side effects (fakes in tests). Called only on the worker.
pub trait Host {
    /// The live host key; `Err` when the generation, boot id, probe binary or
    /// template unit cannot be verified (no probe runs then).
    fn host_key(&mut self) -> io::Result<HostKey>;
    /// One start of the probe instance for `account` through PID 1, mapped.
    fn probe(&mut self, account: &str) -> ProbeOutcome;
    /// Monotonic milliseconds (CLOCK_BOOTTIME in production).
    fn now_ms(&mut self) -> io::Result<u64>;
    fn log(&mut self, line: &str);
}

/// Measure the host key and publish it (or its absence), logging a change.
fn measure_host(host: &mut dyn Host, shared: &Shared, now: u64) -> Option<HostKey> {
    let measured = host.host_key();
    let mut state = shared.state.lock().ok()?;
    let reason = measured.as_ref().err().map(io::Error::to_string);
    if state.logged_host.as_ref() != Some(&reason) {
        match &reason {
            None => host.log("SSH denial probe inputs verified (generation, probe binary, template unit)"),
            Some(reason) => host.log(&format!("SSH denial probe inputs unavailable: {reason}; no account is proven")),
        }
        state.logged_host = Some(reason);
    }
    state.host = measured.ok().map(|key| (key, now));
    state.host.as_ref().map(|(key, _)| key.clone())
}

/// One worker pass: re-measure the host key when due, then run at most one
/// queued probe that is still needed. The result is kept only when the host
/// key re-measured after the run is the one it ran under; otherwise it is
/// discarded and the request queued again. Returns whether a probe ran.
pub fn step(host: &mut dyn Host, shared: &Shared) -> bool {
    let Ok(now) = host.now_ms() else {
        if let Ok(mut state) = shared.state.lock() { state.host = None; }
        return false;
    };
    let due = shared.state.lock().map(|state| state.host.as_ref()
        .is_none_or(|(_, at)| now < *at || now - *at >= WORKER_PERIOD_MS)).unwrap_or(true);
    let host_key = if due {
        measure_host(host, shared, now)
    } else {
        shared.state.lock().ok().and_then(|state| state.host.as_ref().map(|(key, _)| key.clone()))
    };
    let Some(before) = host_key else { return false };
    let request = {
        let Ok(mut state) = shared.state.lock() else { return false };
        loop {
            match state.queue.pop_front() {
                None => break None,
                Some(request) if state.needs_probe(&request.account, now, request.force) => break Some(request),
                Some(_) => {}
            }
        }
    };
    let Some(request) = request else { return false };
    let outcome = host.probe(&request.account.account);
    let after = host.host_key();
    let at_ms = host.now_ms();
    if let Ok(mut state) = shared.state.lock() {
        match (after, at_ms) {
            (Ok(after), Ok(at_ms)) if after == before => {
                state.host = Some((after, at_ms));
                state.completed += 1;
                let previous = state.results.get(&request.account.account).map(|stored| stored.outcome.clone());
                if previous.as_ref() != Some(&outcome) {
                    host.log(&format!("SSH denial probe {}: {}", request.account.account, outcome.describe()));
                }
                let key = ProbeKey { host: before, account: request.account.clone() };
                let sequence = state.completed;
                state.results.insert(request.account.account, Stored { key, outcome, at_ms, sequence });
            }
            // The inputs changed (or became unreadable) during the run: the
            // outcome may describe neither key. Probe again under the new one.
            (Ok(after), Ok(at_ms)) => {
                state.host = Some((after, at_ms));
                state.enqueue(request.account, request.force);
            }
            _ => {
                state.host = None;
                state.enqueue(request.account, request.force);
            }
        }
    }
    shared.changed.notify_all();
    true
}

/// The background worker: passes until the evidence is dropped; waits for a
/// request (or at most [`WORKER_PERIOD_MS`]) whenever nothing ran.
pub fn worker_loop(host: &mut dyn Host, shared: &Shared) {
    loop {
        let ran = step(host, shared);
        let Ok(state) = shared.state.lock() else { return };
        if state.stopped { return; }
        // A request queued since the pass (with verified inputs) runs at once.
        if ran || (!state.queue.is_empty() && state.host.is_some()) { continue; }
        let Ok((state, _)) = shared.changed.wait_timeout(state, Duration::from_millis(WORKER_PERIOD_MS)) else { return };
        if state.stopped { return; }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{installed, run_probe, InstalledHost};

#[cfg(target_os = "linux")]
mod linux {
    use std::{path::Path, sync::Arc, time::Duration};
    use duduclaw_isolation::update::InstallTarget;
    use duduclaw_updated::linux::files::Directory;
    use sha2::{Digest, Sha256};
    use crate::systemd::{linux::Systemd, ISOLATION_PROBE_UNIT_TEXT};
    use super::*;

    fn unavailable(what: &str) -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, format!("{what} unavailable")) }
    /// [`unavailable`] carrying the underlying error (same kind).
    fn unavailable_because(what: &str, error: impl std::fmt::Display) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, format!("{what} unavailable: {error}"))
    }

    /// The live host key: the committed signed generation (updater probe),
    /// the boot id, the installed probe binary verified by descriptor against
    /// that generation's `IsolationProbe` artifact, and the installed template
    /// unit, which must be byte-for-byte the shipped text. This is the only
    /// place the signed generation and the probe binary's digest are checked:
    /// `run_probe` / `Systemd::run_fixed_unit` do not need a signed generation.
    pub fn host_key() -> io::Result<HostKey> {
        let (generation, manifest) = crate::installed::committed_generation()
            .map_err(|error| unavailable_because("committed generation", error))?;
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|error| io::Error::new(error.kind(), format!("boot id unavailable: {error}")))?.trim().to_owned();
        if boot_id.is_empty() { return Err(unavailable("boot id")); }
        let directory = Directory::open(Path::new(crate::selftest::TRAMPOLINE_DIRECTORY))
            .map_err(|error| unavailable_because("/usr/lib/duduclaw", error))?;
        let probe = crate::selftest::verified_artifact(&directory, &manifest, InstallTarget::IsolationProbe)
            .map_err(|error| unavailable_because("signed probe binary /usr/lib/duduclaw/duduclaw-isolation-probe", error))?;
        let unit = crate::systemd::linux::installed_probe_template()
            .map_err(|error| unavailable_because("probe template unit", error))?;
        if let Some(reason) = crate::systemd::linux::unit_file_mismatch(ISOLATION_PROBE_UNIT_TEXT.as_bytes(), &unit) {
            return Err(unavailable_because("shipped probe template unit", reason));
        }
        Ok(HostKey { boot_id, generation, probe, unit: Sha256Digest(Sha256::digest(&unit).into()) })
    }

    /// The production probe: one start of `duduclaw-isolation-probe@<account>.service`
    /// through PID 1 (`run_fixed_unit`: closed set, installed bytes, load
    /// record, effective properties, `Unit.Ref` held, strict outcome), mapped
    /// with [`unit_outcome`] (returned with PID 1's record it was read from).
    /// `Err`: refused before start (`InvalidData`), timed out, or a bus
    /// failure; the worker records those as inconclusive.
    pub fn run_probe(systemd: &Systemd, account: &str) -> io::Result<(ProbeOutcome, FixedUnitOutcome)> {
        let unit = FixedUnit::probe(account)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid unit policy: probe account name"))?;
        let outcome = systemd.run_fixed_unit(&unit.name(), Duration::from_millis(UNIT_DEADLINE_MS))?;
        Ok((unit_outcome(&unit, &outcome), outcome))
    }

    /// Production side effects, with the worker's own D-Bus connection (made
    /// on first use, dropped after a failure that is not a refusal).
    #[derive(Default)]
    pub struct InstalledHost { systemd: Option<Systemd> }
    impl Host for InstalledHost {
        fn host_key(&mut self) -> io::Result<HostKey> { host_key() }
        fn probe(&mut self, account: &str) -> ProbeOutcome {
            if self.systemd.is_none() {
                match Systemd::connect() {
                    Ok(systemd) => self.systemd = Some(systemd),
                    Err(error) => return ProbeOutcome::Inconclusive(format!("systemd unavailable: {error}")),
                }
            }
            let Some(systemd) = self.systemd.as_ref() else { return ProbeOutcome::Inconclusive("systemd unavailable".into()) };
            match run_probe(systemd, account) {
                Ok((outcome, _)) => outcome,
                Err(error) => {
                    if error.kind() != io::ErrorKind::InvalidData { self.systemd = None; }
                    let reason = if error.kind() == io::ErrorKind::TimedOut { "unit deadline expired".to_owned() }
                        else { format!("not run: {error}") };
                    ProbeOutcome::Inconclusive(reason)
                }
            }
        }
        fn now_ms(&mut self) -> io::Result<u64> { crate::host_state::boottime_ms() }
        fn log(&mut self, line: &str) { eprintln!("duduclaw-spawnd: {line}"); }
    }

    /// The worker thread (named, so it shows in diagnostics).
    fn start_worker(shared: Arc<Shared>) {
        let spawned = std::thread::Builder::new().name("duduclaw-ssh-probe".into())
            .spawn(move || worker_loop(&mut InstalledHost::default(), &shared));
        if spawned.is_err() { eprintln!("duduclaw-spawnd: SSH denial probe worker could not start; no account is proven"); }
    }

    /// The installed evidence: the worker starts on the first lookup (start-up
    /// recovery's account checks).
    pub fn installed() -> ProbeEvidence {
        ProbeEvidence::new(Box::new(start_worker), Box::new(crate::host_state::boottime_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systemd::{ExecCommand, PROBE_EXECUTABLE};
    use std::sync::atomic::{AtomicU64, Ordering};

    const ACCOUNT: &str = "duduclaw-emp-70000";
    fn unit() -> FixedUnit { FixedUnit::probe(ACCOUNT).unwrap() }
    /// A probe instance that ran once and exited `status`, as PID 1 records a
    /// referenced oneshot (exit ≠ 0: `failed`, `Result=exit-code`).
    fn ran(status: i32) -> FixedUnitOutcome {
        let failed = status != 0;
        FixedUnitOutcome { result: if failed { "exit-code" } else { "success" }.into(), exec_main_code: 1, exec_main_status: status,
            exec_main_pid: 4242, invocation: vec![9; 16], previous_invocation: vec![7; 16],
            active_state: if failed { "failed" } else { "inactive" }.into(),
            recorded: vec![ExecCommand { path: PROBE_EXECUTABLE.into(), argv: vec![PROBE_EXECUTABLE.into(), ACCOUNT.into()],
                ignore_failure: false, start_monotonic_us: 2_000, exit_monotonic_us: 3_000, pid: 4242, code: 1, status }],
            previous_start_us: 1_000 }
    }

    /// R7c item 1: every exit code class, bound to this invocation.
    #[test]
    fn ssh_probe_outcome_maps_every_exit_code_class() {
        assert_eq!(unit_outcome(&unit(), &ran(0)), ProbeOutcome::Denied);
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_NO_SSHD)), ProbeOutcome::NotApplicable);
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_LOGIN_NOT_DENIED)),
            ProbeOutcome::Reachable("login_not_denied (exit 10)".into()));
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_FORWARD_NOT_DENIED)),
            ProbeOutcome::Reachable("forward_not_denied (exit 13)".into()));
        // A failed oneshot of this invocation: the stage is named for the log.
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_IDENTITY)), ProbeOutcome::Inconclusive("identity (exit 3)".into()));
        for code in [2, 4, 6, 7, 8, 9, 11, 12, 14, 15, 16, 17] {
            assert!(matches!(unit_outcome(&unit(), &ran(code)), ProbeOutcome::Inconclusive(ref stage)
                if stage.ends_with(&format!("(exit {code})"))), "{code}");
        }
        // Codes the probe never uses (generic, panic, unsupported, systemd exec failures, ssh's).
        for code in [1, 101, 125, 200, 203, 243, 255] {
            assert_eq!(unit_outcome(&unit(), &ran(code)), ProbeOutcome::Inconclusive(format!("exit {code}")), "{code}");
        }
    }

    /// R7c item 1: the status is trusted only when the outcome is this start's.
    #[test]
    fn ssh_probe_outcome_is_bound_to_this_invocation() {
        let unbound = |outcome: &FixedUnitOutcome| matches!(unit_outcome(&unit(), outcome),
            ProbeOutcome::Inconclusive(ref reason) if reason.starts_with("outcome not bound to this start: "));
        for status in [0, probe_exit::EXIT_LOGIN_NOT_DENIED, probe_exit::EXIT_NO_SSHD, probe_exit::EXIT_IDENTITY] {
            let cases: [fn(&mut FixedUnitOutcome); 15] = [
                // A stale previous invocation (the same, none, or all zero).
                |o| o.invocation = o.previous_invocation.clone(),
                |o| o.invocation = vec![],
                |o| o.invocation = vec![0; 16],
                // Not finished.
                |o| o.active_state = "activating".into(),
                // Killed (timeout) or never exec'd: no exit status at all.
                |o| { o.exec_main_code = 2; o.result = "timeout".into(); },
                |o| o.exec_main_code = 0,
                |o| { o.exec_main_pid = 0; o.recorded[0].pid = 0; },
                // ExecStart record of the previous run, another process, another status.
                |o| o.recorded[0].start_monotonic_us = o.previous_start_us,
                |o| o.recorded[0].pid += 1,
                |o| o.recorded[0].status += 1,
                |o| o.recorded[0].exit_monotonic_us = 0,
                // Another account's instance, or another command.
                |o| o.recorded[0].argv[1] = "duduclaw-emp-70001".into(),
                |o| o.recorded[0].path = "/bin/true".into(),
                // State/result inconsistent with the status.
                |o| o.result = "signal".into(),
                |o| o.active_state = if o.exec_main_status == 0 { "failed" } else { "inactive" }.into(),
            ];
            for (index, change) in cases.iter().enumerate() {
                let mut outcome = ran(status);
                change(&mut outcome);
                assert!(unbound(&outcome), "status {status} case {index}: {:?}", unit_outcome(&unit(), &outcome));
            }
        }
        // A failed oneshot read as `inactive`/`success` (or the reverse) is never trusted.
        let mut odd = ran(probe_exit::EXIT_LOGIN_NOT_DENIED);
        odd.result = "success".into();
        assert!(unbound(&odd));
        // The self-test's strict rule is unchanged: a probe exit 0 is not a self-test pass.
        assert!(!ran(0).passed());
        assert_eq!(ran(0).failure_for(&unit()), None);
        assert_eq!(ran(3).failure_for(&unit()), Some("ActiveState"));
    }

    #[test]
    fn ssh_probe_refusal_mapping() {
        assert_eq!(refusal(Some(&ProbeOutcome::Denied)), None);
        assert_eq!(refusal(None), Some(SshRefusal::Pending));
        assert_eq!(refusal(Some(&ProbeOutcome::NotApplicable)), Some(SshRefusal::NotApplicable));
        assert_eq!(refusal(Some(&ProbeOutcome::Reachable("x".into()))), Some(SshRefusal::Reachable("x".into())));
        assert_eq!(refusal(Some(&ProbeOutcome::Inconclusive("y".into()))), Some(SshRefusal::Inconclusive("y".into())));
    }

    fn host_key(seed: u8) -> HostKey {
        HostKey { boot_id: "boot-a".into(), generation: Sha256Digest([seed; 32]),
            probe: ArtifactKey { sha256: Sha256Digest([2; 32]), size: 1000, mode: 0o755 }, unit: Sha256Digest([3; 32]) }
    }
    fn account(name: &str, config: u8) -> AccountKey {
        AccountKey { account: name.into(), uid: 70000, sshd_config: Sha256Digest([config; 32]) }
    }
    /// Scripted worker side effects sharing one clock with the refresh side.
    struct Fake {
        key: io::Result<HostKey>, after: Option<io::Result<HostKey>>, outcomes: BTreeMap<String, Vec<ProbeOutcome>>,
        probed: Vec<String>, keys: usize, clock: Arc<AtomicU64>, lines: Vec<String>,
    }
    impl Fake {
        fn new(clock: Arc<AtomicU64>) -> Self {
            Self { key: Ok(host_key(1)), after: None, outcomes: BTreeMap::new(), probed: vec![], keys: 0, clock, lines: vec![] }
        }
        fn answer(&mut self, name: &str, outcomes: Vec<ProbeOutcome>) { self.outcomes.insert(name.into(), outcomes); }
    }
    impl Host for Fake {
        fn host_key(&mut self) -> io::Result<HostKey> {
            self.keys += 1;
            if let Some(after) = self.after.take() { return after; }
            match &self.key { Ok(key) => Ok(key.clone()), Err(_) => Err(io::Error::other("generation probe failed")) }
        }
        fn probe(&mut self, account: &str) -> ProbeOutcome {
            self.probed.push(account.into());
            let queue = self.outcomes.get_mut(account).unwrap_or_else(|| panic!("unexpected probe of {account}"));
            if queue.is_empty() { panic!("unexpected probe of {account}") }
            queue.remove(0)
        }
        fn now_ms(&mut self) -> io::Result<u64> { Ok(self.clock.load(Ordering::SeqCst)) }
        fn log(&mut self, line: &str) { self.lines.push(line.into()); }
    }
    /// The refresh side with the worker replaced by the test (`step` by hand).
    fn evidence(clock: &Arc<AtomicU64>) -> (ProbeEvidence, Arc<Mutex<Option<Arc<Shared>>>>) {
        let started = Arc::new(Mutex::new(None));
        let (slot, time) = (started.clone(), clock.clone());
        let evidence = ProbeEvidence::new(Box::new(move |shared: Arc<Shared>| *slot.lock().unwrap() = Some(shared)),
            Box::new(move || -> io::Result<u64> { Ok(time.load(Ordering::SeqCst)) }));
        (evidence, started)
    }
    fn shared(started: &Arc<Mutex<Option<Arc<Shared>>>>) -> Arc<Shared> { started.lock().unwrap().clone().expect("worker started") }

    /// R7c item 2: no result ⇒ pending (and requested); Denied passes; a
    /// lookup never runs anything itself; a Denied result is reused.
    #[test]
    fn ssh_evidence_is_pending_until_probed_and_then_reuses_denied() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let key = account(ACCOUNT, 5);
        for _ in 0..5 { assert_eq!(evidence.lookup(&key), None); }
        let shared = shared(&started);
        assert_eq!(shared.state.lock().unwrap().queue.len(), 1, "one request per account");
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Denied]);
        assert!(step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
        assert_eq!(refusal(evidence.lookup(&key).as_ref()), None);
        // Later passes keep the host key fresh and never probe again.
        for _ in 0..10 {
            clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
            assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
            assert!(!step(&mut host, &shared));
        }
        assert_eq!(host.probed, [ACCOUNT]);
        assert_eq!(host.lines.iter().filter(|line| line.contains(ACCOUNT)).count(), 1, "{:?}", host.lines);
        assert_eq!(host.lines.last().unwrap(), "SSH denial probe duduclaw-emp-70000: denied");
        // A stuck worker: the host key goes stale and nothing is current.
        clock.fetch_add(HOST_FRESH_MS, Ordering::SeqCst);
        assert_eq!(evidence.lookup(&key), None);
    }

    /// R7c item 2: any key component change makes the result not current and
    /// the account is probed again; Reachable refuses.
    #[test]
    fn ssh_evidence_key_change_invalidates_and_reprobes() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Denied, ProbeOutcome::Reachable("forward_not_denied (exit 13)".into()),
            ProbeOutcome::Denied, ProbeOutcome::Denied]);
        let key = account(ACCOUNT, 5);
        assert_eq!(evidence.lookup(&key), None);
        let shared = shared(&started);
        assert!(step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
        // sshd's effective configuration for the account changed.
        let changed = account(ACCOUNT, 6);
        assert_eq!(evidence.lookup(&changed), None);
        assert!(step(&mut host, &shared));
        let reachable = evidence.lookup(&changed);
        assert!(matches!(refusal(reachable.as_ref()), Some(SshRefusal::Reachable(_))), "{reachable:?}");
        // The old key's result is not resurrected.
        assert_eq!(evidence.lookup(&key), None);
        assert!(step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
        // The uid changed: not the same account.
        assert_eq!(evidence.lookup(&AccountKey { uid: 70001, ..key.clone() }), None);
        // A new signed generation (host key) invalidates every result.
        clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
        host.key = Ok(host_key(9));
        assert!(step(&mut host, &shared), "re-measured, then probed the queued uid change");
        assert_eq!(evidence.lookup(&key), None);
        assert_eq!(host.probed.len(), 4);
        // Unverifiable inputs: nothing is current and nothing runs.
        clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
        host.key = Err(io::Error::other("x"));
        assert!(!step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), None);
        assert!(host.lines.iter().any(|line| line.starts_with("SSH denial probe inputs unavailable")), "{:?}", host.lines);
        assert!(host.lines.iter().all(|line| !line.contains("denyusers")), "never sshd output");
    }

    /// R7c item 2: one probe at a time, in request order, every account.
    #[test]
    fn ssh_evidence_worker_runs_one_probe_at_a_time() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let mut host = Fake::new(clock.clone());
        let names = ["duduclaw-emp-70000", "duduclaw-emp-70001", "duduclaw-emp-70002"];
        for name in names { host.answer(name, vec![ProbeOutcome::Denied]); assert_eq!(evidence.lookup(&account(name, 5)), None); }
        let shared = shared(&started);
        for (index, name) in names.iter().enumerate() {
            assert!(step(&mut host, &shared));
            assert_eq!(host.probed.len(), index + 1, "exactly one probe per pass");
            assert_eq!(host.probed[index], *name);
        }
        assert!(!step(&mut host, &shared));
        for name in names { assert_eq!(evidence.lookup(&account(name, 5)), Some(ProbeOutcome::Denied)); }
    }

    /// R7c item 2: nothing is persisted: a new evidence (spawnd restart)
    /// probes every account again.
    #[test]
    fn ssh_evidence_restart_reprobes_every_account() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Denied, ProbeOutcome::Denied]);
        for _ in 0..2 {
            let (evidence, started) = evidence(&clock);
            assert_eq!(evidence.lookup(&account(ACCOUNT, 5)), None, "not runnable until probed");
            assert!(step(&mut host, &shared(&started)));
            assert_eq!(evidence.lookup(&account(ACCOUNT, 5)), Some(ProbeOutcome::Denied));
        }
        assert_eq!(host.probed, [ACCOUNT, ACCOUNT]);
    }

    /// An inconclusive result refuses and is retried only after its window;
    /// a host key change during the run discards the result.
    #[test]
    fn ssh_evidence_retries_inconclusive_and_discards_a_run_under_changed_inputs() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Inconclusive("deadline (exit 15)".into()), ProbeOutcome::Denied, ProbeOutcome::Denied]);
        let key = account(ACCOUNT, 5);
        assert_eq!(evidence.lookup(&key), None);
        let shared = shared(&started);
        assert!(step(&mut host, &shared));
        assert!(matches!(refusal(evidence.lookup(&key).as_ref()), Some(SshRefusal::Inconclusive(_))));
        // Within the retry window: kept, not re-run.
        for _ in 0..(INCONCLUSIVE_RETRY_MS / WORKER_PERIOD_MS - 1) {
            clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
            assert!(evidence.lookup(&key).is_some());
            assert!(!step(&mut host, &shared));
        }
        clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
        assert!(!step(&mut host, &shared), "re-measured; nothing requested yet");
        // Due again: still refused, and requested.
        assert!(matches!(evidence.lookup(&key), Some(ProbeOutcome::Inconclusive(_))));
        // The inputs change while it runs: discarded, queued again, run under the new key.
        host.after = Some(Ok(host_key(4)));
        assert!(step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), None);
        host.key = Ok(host_key(4));
        assert!(step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
        assert_eq!(host.probed.len(), 3);
    }

    /// R7c allocation: a forced request goes ahead of background requests and
    /// replaces the account's queued one.
    #[test]
    fn ssh_evidence_allocation_request_goes_first() {
        let mut state = State::default();
        for name in ["duduclaw-emp-70001", "duduclaw-emp-70002", ACCOUNT] { state.enqueue(account(name, 5), false); }
        state.enqueue(account(ACCOUNT, 6), true);
        let order: Vec<(&str, bool)> = state.queue.iter().map(|request| (request.account.account.as_str(), request.force)).collect();
        assert_eq!(order, [(ACCOUNT, true), ("duduclaw-emp-70001", false), ("duduclaw-emp-70002", false)]);
        assert_eq!(state.queue[0].account.sshd_config, Sha256Digest([6; 32]), "the newest key");
        // A later background request keeps it forced and first.
        state.enqueue(account(ACCOUNT, 6), false);
        assert_eq!((state.queue[0].account.account.as_str(), state.queue[0].force, state.queue.len()), (ACCOUNT, true, 3));
    }

    /// R7c allocation: `wait` forces a fresh run after an inconclusive result,
    /// returns a Denied result at once, and gives up at its deadline (pending).
    #[test]
    fn ssh_evidence_wait_forces_a_probe_and_is_bounded() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let worker_clock = clock.clone();
        let (record, done) = (Arc::new(Mutex::new(Vec::<String>::new())), Arc::new(Mutex::new(false)));
        let (probed, finished) = (record.clone(), done.clone());
        let start: StartWorker = Box::new(move |shared: Arc<Shared>| {
            std::thread::spawn(move || {
                let mut host = Fake::new(worker_clock);
                host.answer(ACCOUNT, vec![ProbeOutcome::Inconclusive("login_journal (exit 11)".into()), ProbeOutcome::Denied]);
                worker_loop(&mut host, &shared);
                *probed.lock().unwrap() = host.probed;
                *finished.lock().unwrap() = true;
            });
        });
        let time = clock.clone();
        let evidence = ProbeEvidence::new(start, Box::new(move || -> io::Result<u64> { Ok(time.load(Ordering::SeqCst)) }));
        let key = account(ACCOUNT, 5);
        let first = evidence.wait(&key, Duration::from_secs(10));
        assert!(matches!(first, Some(ProbeOutcome::Inconclusive(_))), "{first:?}");
        // A retry (resumed allocation) does not wait out the 60 s window.
        assert_eq!(evidence.wait(&key, Duration::from_secs(10)), Some(ProbeOutcome::Denied));
        // Denied is returned at once, without another run.
        assert_eq!(evidence.wait(&key, Duration::from_secs(10)), Some(ProbeOutcome::Denied));
        // Missing evidence: a bounded wait ends pending.
        assert_eq!(MissingSshEvidence.wait(&key, Duration::from_millis(1)), None);
        drop(evidence);
        let until = Instant::now() + Duration::from_secs(10);
        while !*done.lock().unwrap() { assert!(Instant::now() < until, "worker did not stop"); std::thread::sleep(Duration::from_millis(10)); }
        assert_eq!(*record.lock().unwrap(), [ACCOUNT, ACCOUNT]);
    }

    #[test]
    fn ssh_evidence_wait_times_out_pending_while_the_inputs_are_unverifiable() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, _started) = evidence(&clock);
        // No worker runs at all (the test never steps): the wait ends at its deadline.
        let begun = Instant::now();
        assert_eq!(evidence.wait(&account(ACCOUNT, 5), Duration::from_millis(50)), None);
        assert!(begun.elapsed() >= Duration::from_millis(50));
        assert!(UNIT_DEADLINE_MS < ALLOCATION_WAIT_MS);
    }
}
