//! R7c: per-account SSH denial evidence from the probe unit
//! `duduclaw-isolation-probe@<account>.<port>.service`, started through PID 1
//! (`Systemd::run_fixed_unit`, [`FixedUnit::Probe`]) instead of read from an
//! installer receipt.
//!
//! The probe (`/usr/lib/duduclaw/duduclaw-isolation-probe`) runs UNPRIVILEGED
//! as the isolation test account with journal read access; it attempts a real
//! public-key login and a real `-N -R` forwarding session against the live
//! sshd on the port spawnd took from its own `sshd -T`, and requires sshd's own
//! DenyUsers journal record for each (and sshd's own `Accepted publickey`
//! record before it ever says "NOT denied"). Its exit code
//! (`duduclaw_isolation::probe_exit`) is the only thing spawnd reads, and only
//! once it is bound to THIS invocation
//! ([`FixedUnitOutcome::exit_status_of_this_start`]): 0 ⇒ [`ProbeOutcome::Denied`],
//! "login/forwarding NOT denied" ⇒ `Reachable`, anything else (other codes
//! including the `*_unverified` ones, timeout, a refused start, PID 1 errors)
//! ⇒ `Inconclusive`. spawnd consults a result only after its own
//! `sshd -T` already denies the account (`accounts.rs` `verify_login`).
//!
//! A result is kept in memory under a [`ProbeKey`]: the host part (boot id,
//! committed signed generation, the installed probe binary verified against
//! that generation's `IsolationProbe` artifact (digest, size, mode), the
//! installed template unit's digest, and the LIVE sshd listener's identity
//! from PID 1 ([`ListenerIdentity`]: unit, main pid, start time, command)) and
//! the account part (name, uid, SHA-256 of
//! `sshd -T -C user=<account>,host=localhost,addr=127.0.0.1`, which
//! `verify_login` computes anyway, and the port taken from it). Any component
//! change makes the result not current: the account is probed again. A
//! Denied result is also current for at most [`DENIED_TTL`] (only
//! [`DENIED_TTL_NO_IDENTITY`] when the listener identity could not be
//! determined); the worker re-probes it in the background once 5/6 of that
//! has passed, so a healthy host never shows a gap, and an expired result is
//! "pending" (fail closed). Reachable results do not expire within the
//! process. Nothing is persisted, so after a spawnd restart every account is
//! probed again.
//!
//! Only the single worker thread ([`worker_loop`], its own D-Bus connection)
//! measures the host key (every [`WORKER_PERIOD_MS`]) and starts probe units,
//! one at a time, each bounded by [`UNIT_DEADLINE_MS`]. Nothing else ever
//! waits for it: the refresh path ([`SshEvidence::lookup`]) and the
//! allocation path ([`SshEvidence::request`], a forced request ahead of
//! background ones) only read the shared state and queue a request (bounded
//! by [`MAX_QUEUED`]); a host key older than [`HOST_FRESH_MS`] makes every
//! result not current. An inconclusive result is retried after
//! [`INCONCLUSIVE_RETRY_MS`] (after [`FORCED_RETRY_MS`] for an allocation).
//! The worker logs only when an account's outcome (or the host key's or the
//! listener identity's availability) changes, never sshd output.
use std::{collections::{BTreeMap, VecDeque}, io, sync::{Arc, Condvar, Mutex}, time::Duration};
use duduclaw_isolation::{ids::Sha256Digest, probe_exit};
use crate::{accounts::SshRefusal, selftest::ArtifactKey, systemd::{FixedUnit, FixedUnitOutcome}};

/// The unit's own `TimeoutStartSec=30` plus margin for PID 1 to record it.
pub const UNIT_DEADLINE_MS: u64 = 40_000;
/// The worker re-measures the host key at least this often.
pub const WORKER_PERIOD_MS: u64 = 2_000;
/// Results are current only under a host key measured less than this long ago.
pub const HOST_FRESH_MS: u64 = 8_000;
/// An inconclusive result is probed again after this long.
pub const INCONCLUSIVE_RETRY_MS: u64 = 60_000;
/// An allocation's forced request re-probes an inconclusive result only after
/// this long, so a gateway repeating the request cannot keep the worker busy.
pub const FORCED_RETRY_MS: u64 = 10_000;
/// Bound on queued requests (the registry holds at most 30 000 accounts).
pub const MAX_QUEUED: usize = 30_000;
/// F4: how long a Denied result stays current when the live sshd listener's
/// identity is part of its key (an sshd restart changes the key at once).
pub const DENIED_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// F4: how long a Denied result stays current when the listener identity
/// could not be determined (nothing would notice an sshd restart).
pub const DENIED_TTL_NO_IDENTITY: Duration = Duration::from_secs(10 * 60);
const DENIED_TTL_MS: u64 = DENIED_TTL.as_secs() * 1_000;
const DENIED_TTL_NO_IDENTITY_MS: u64 = DENIED_TTL_NO_IDENTITY.as_secs() * 1_000;
/// The worker re-probes a Denied result once this share of its TTL has passed.
pub const REFRESH_NUMERATOR: u64 = 5;
pub const REFRESH_DENOMINATOR: u64 = 6;
/// The units whose main process may be the sshd listener, in PID 1.
pub const SSH_SERVICE_UNITS: [&str; 2] = ["ssh.service", "sshd.service"];
/// The only sshd binary the listener's `ExecStart` may run.
pub const SSHD_PATH: &str = "/usr/sbin/sshd";
/// Bounds on the listener's `ExecStart` argv kept in the key.
const MAX_LISTENER_ARGS: usize = 64;
const MAX_LISTENER_ARGV_BYTES: usize = 4_096;

/// PID 1's view of one sshd service unit (`Systemd::ssh_service_view`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceView {
    /// The unit's primary name (an alias resolves to the same `Id`).
    pub id: String,
    pub active_state: String,
    pub main_pid: u32,
    /// `ExecMainStartTimestampMonotonic` (µs).
    pub started_us: u64,
    /// `ExecStart` commands: path and argv.
    pub exec_start: Vec<(String, Vec<String>)>,
}
/// F4: the identity of the LIVE sshd listener, from PID 1 (spawnd cannot
/// read `/proc/<sshd>/exe`: sshd's capabilities are not a subset of spawnd's
/// bounded set, so the kernel's ptrace access check refuses). A restart
/// (new main pid, new start time) or another command line changes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerIdentity { pub unit: String, pub main_pid: u32, pub started_us: u64, pub argv: Vec<String> }
/// The listener identity from the views of [`SSH_SERVICE_UNITS`]: exactly one
/// active unit (an alias pair counts once, by `Id`) with a non-zero main pid
/// and start time and exactly one `ExecStart` command running [`SSHD_PATH`]
/// with a bounded argv. Anything else (none or two distinct active units, a
/// socket-activated sshd without a main process, another binary): `None`, and
/// results fall back to [`DENIED_TTL_NO_IDENTITY`].
pub fn listener_identity(views: &[(&str, Option<ServiceView>)]) -> Option<ListenerIdentity> {
    let mut active: Vec<&ServiceView> = Vec::new();
    for view in views.iter().filter_map(|(_, view)| view.as_ref()).filter(|view| view.active_state == "active") {
        if !active.iter().any(|seen| seen.id == view.id) { active.push(view); }
    }
    let [view] = active.as_slice() else { return None };
    if view.main_pid == 0 || view.started_us == 0 || !SSH_SERVICE_UNITS.contains(&view.id.as_str()) { return None; }
    let [(path, argv)] = view.exec_start.as_slice() else { return None };
    if path != SSHD_PATH || argv.len() > MAX_LISTENER_ARGS
        || argv.iter().map(String::len).sum::<usize>() > MAX_LISTENER_ARGV_BYTES { return None; }
    Some(ListenerIdentity { unit: view.id.clone(), main_pid: view.main_pid, started_us: view.started_us, argv: argv.clone() })
}

/// Host-wide inputs of every probe, measured by the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub boot_id: String, pub generation: Sha256Digest, pub probe: ArtifactKey, pub unit: Sha256Digest,
    /// `None`: not determinable (results then expire after [`DENIED_TTL_NO_IDENTITY`]).
    pub listener: Option<ListenerIdentity>,
}
/// The account's inputs, computed by `verify_login` (after its own
/// `sshd -T` denies the account): `port` is that output's first `port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountKey { pub account: String, pub uid: u32, pub sshd_config: Sha256Digest, pub port: u16 }
/// Everything one result is valid for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeKey { pub host: HostKey, pub account: AccountKey }

/// One probe's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Exit 0 of this invocation: login and forwarding denied.
    Denied,
    /// The login or the forwarding session was NOT denied, corroborated by
    /// sshd's own Accepted record (stage, code).
    Reachable(String),
    /// Anything else (stage/code, or why the outcome is not this start's).
    Inconclusive(String),
}
impl ProbeOutcome {
    fn describe(&self) -> String {
        match self {
            Self::Denied => "denied".into(),
            Self::Reachable(stage) => format!("NOT denied: {stage}"),
            Self::Inconclusive(reason) => format!("inconclusive: {reason}"),
        }
    }
}
/// `verify_login`'s answer for a lookup: `None` only for Denied; no current
/// result (none yet, expired, or under another key) is [`SshRefusal::Pending`].
pub fn refusal(outcome: Option<&ProbeOutcome>) -> Option<SshRefusal> {
    match outcome {
        Some(ProbeOutcome::Denied) => None,
        None => Some(SshRefusal::Pending),
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

/// The account side's view of the evidence (`LinuxAccounts`). Neither method
/// ever blocks on a probe.
pub trait SshEvidence: Send + Sync {
    /// The current result for `account`, or `None`; a missing, stale,
    /// expired, refresh-due or retry-due result is requested from the worker.
    fn lookup(&self, account: &AccountKey) -> Option<ProbeOutcome>;
    /// Allocation: [`Self::lookup`], except that the request goes ahead of
    /// background requests and an inconclusive result is re-requested after
    /// [`FORCED_RETRY_MS`] instead of [`INCONCLUSIVE_RETRY_MS`]. Returns at
    /// once; the caller asks again later.
    fn request(&self, account: &AccountKey) -> Option<ProbeOutcome>;
}
/// No evidence at all (the default component): every account is pending.
pub struct MissingSshEvidence;
impl SshEvidence for MissingSshEvidence {
    fn lookup(&self, _: &AccountKey) -> Option<ProbeOutcome> { None }
    fn request(&self, _: &AccountKey) -> Option<ProbeOutcome> { None }
}

#[derive(Debug, Clone)]
struct Stored { key: ProbeKey, outcome: ProbeOutcome, at_ms: u64 }
impl Stored {
    /// How long the result stays current; `None`: never expires in-process.
    fn ttl_ms(&self) -> Option<u64> {
        match self.outcome {
            ProbeOutcome::Denied => Some(if self.key.host.listener.is_some() { DENIED_TTL_MS } else { DENIED_TTL_NO_IDENTITY_MS }),
            _ => None,
        }
    }
    /// A clock read before the result was stored is treated as expired.
    fn expired(&self, now: u64) -> bool {
        self.ttl_ms().is_some_and(|ttl| now < self.at_ms || now - self.at_ms >= ttl)
    }
    /// 5/6 of the TTL has passed: the worker re-probes in the background.
    fn refresh_due(&self, now: u64) -> bool {
        self.ttl_ms().is_some_and(|ttl| now < self.at_ms || now - self.at_ms >= ttl / REFRESH_DENOMINATOR * REFRESH_NUMERATOR)
    }
}
#[derive(Debug, Clone)]
struct Request { account: AccountKey, force: bool }
/// State shared by the refresh side and the worker. The lock is held only to
/// read or update it, never across a host-key measurement or a probe run.
#[derive(Default)]
pub struct State {
    host: Option<(HostKey, u64)>,
    results: BTreeMap<String, Stored>,
    queue: VecDeque<Request>,
    /// The host key's availability last logged (`Some(None)`: available).
    logged_host: Option<Option<String>>,
    /// The listener identity's availability last logged.
    logged_listener: Option<bool>,
    stopped: bool,
}
impl State {
    fn host_at(&self, now: u64) -> Option<&HostKey> {
        self.host.as_ref().filter(|(_, at)| now >= *at && now - *at < HOST_FRESH_MS).map(|(key, _)| key)
    }
    /// The stored result for exactly `account` under the fresh host key, not expired.
    fn current(&self, account: &AccountKey, now: u64) -> Option<&Stored> {
        let host = self.host_at(now)?;
        self.results.get(&account.account)
            .filter(|stored| stored.key.host == *host && stored.key.account == *account && !stored.expired(now))
    }
    /// No current result, a Denied one whose refresh is due, or an
    /// inconclusive one due again (`force`: after [`FORCED_RETRY_MS`]).
    /// A Reachable result is never probed again in-process.
    fn needs_probe(&self, account: &AccountKey, now: u64, force: bool) -> bool {
        match self.current(account, now) {
            None => true,
            Some(stored) => match stored.outcome {
                ProbeOutcome::Denied => stored.refresh_due(now),
                ProbeOutcome::Reachable(_) => false,
                ProbeOutcome::Inconclusive(_) => now.saturating_sub(stored.at_ms)
                    >= if force { FORCED_RETRY_MS } else { INCONCLUSIVE_RETRY_MS },
            },
        }
    }
    /// One queued request per account (the newest key); a forced one goes
    /// first. At most [`MAX_QUEUED`] requests.
    fn enqueue(&mut self, account: AccountKey, force: bool) {
        let force = match self.queue.iter().position(|request| request.account.account == account.account) {
            Some(index) => self.queue.remove(index).is_some_and(|old| old.force) || force,
            None if self.queue.len() >= MAX_QUEUED => return,
            None => force,
        };
        let request = Request { account, force };
        if force { self.queue.push_front(request) } else { self.queue.push_back(request) }
    }
    /// The worker's background refresh: every Denied result under `host`
    /// whose refresh is due is queued (behind every other request) unless the
    /// account already has a request, which may carry a newer key.
    fn queue_refreshes(&mut self, host: &HostKey, now: u64) {
        let due: Vec<AccountKey> = self.results.values()
            .filter(|stored| stored.key.host == *host && stored.refresh_due(now)
                && !self.queue.iter().any(|request| request.account.account == stored.key.account.account))
            .map(|stored| stored.key.account.clone()).collect();
        for account in due { self.enqueue(account, false); }
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
    /// The current result, queuing a request when one is needed; never waits.
    fn query(&self, account: &AccountKey, force: bool) -> Option<ProbeOutcome> {
        let now = (self.clock)().ok();
        let (current, requested) = {
            let mut state = self.shared.state.lock().ok()?;
            let current = now.and_then(|now| state.current(account, now)).map(|stored| stored.outcome.clone());
            let requested = now.is_none_or(|now| state.needs_probe(account, now, force));
            if requested { state.enqueue(account.clone(), force); }
            (current, requested)
        };
        if requested { self.shared.changed.notify_all(); }
        self.start_worker();
        current
    }
}
impl SshEvidence for ProbeEvidence {
    fn lookup(&self, account: &AccountKey) -> Option<ProbeOutcome> { self.query(account, false) }
    fn request(&self, account: &AccountKey) -> Option<ProbeOutcome> { self.query(account, true) }
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
    /// template unit cannot be verified (no probe runs then). The listener
    /// identity is part of it when determinable.
    fn host_key(&mut self) -> io::Result<HostKey>;
    /// One start of the probe instance `<account>.<port>` through PID 1, mapped.
    fn probe(&mut self, account: &str, port: u16) -> ProbeOutcome;
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
    if let Ok(key) = &measured {
        let identified = key.listener.is_some();
        if state.logged_listener != Some(identified) {
            if identified {
                host.log("sshd listener identity read from PID 1; Denied results are re-proven when sshd restarts and every 6 h");
            } else {
                host.log("sshd listener identity unavailable from PID 1 (no single active ssh.service/sshd.service running /usr/sbin/sshd); \
                    Denied results expire after 10 min");
            }
            state.logged_listener = Some(identified);
        }
    }
    state.host = measured.ok().map(|key| (key, now));
    state.host.as_ref().map(|(key, _)| key.clone())
}

/// One worker pass: re-measure the host key when due, queue the background
/// refreshes that are due, then run at most one queued probe that is still
/// needed. The result is kept only when the host key re-measured after the
/// run is the one it ran under; otherwise it is discarded and the request
/// queued again. Returns whether a probe ran.
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
        state.queue_refreshes(&before, now);
        loop {
            match state.queue.pop_front() {
                None => break None,
                Some(request) if state.needs_probe(&request.account, now, request.force) => break Some(request),
                Some(_) => {}
            }
        }
    };
    let Some(request) = request else { return false };
    let outcome = host.probe(&request.account.account, request.account.port);
    let after = host.host_key();
    let at_ms = host.now_ms();
    if let Ok(mut state) = shared.state.lock() {
        match (after, at_ms) {
            (Ok(after), Ok(at_ms)) if after == before => {
                state.host = Some((after, at_ms));
                let previous = state.results.get(&request.account.account).map(|stored| stored.outcome.clone());
                if previous.as_ref() != Some(&outcome) {
                    host.log(&format!("SSH denial probe {}: {}", request.account.account, outcome.describe()));
                }
                let key = ProbeKey { host: before, account: request.account.clone() };
                state.results.insert(request.account.account, Stored { key, outcome, at_ms });
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

    /// The file-system part of the live host key: the committed signed
    /// generation (updater probe), the boot id, the installed probe binary
    /// verified by descriptor against that generation's `IsolationProbe`
    /// artifact, and the installed template unit, which must be byte-for-byte
    /// the shipped text. This is the only place the signed generation and the
    /// probe binary's digest are checked: `run_probe` /
    /// `Systemd::run_fixed_unit` do not need a signed generation. `listener`
    /// is `None` here; [`InstalledHost`] adds it from PID 1.
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
        Ok(HostKey { boot_id, generation, probe, unit: Sha256Digest(Sha256::digest(&unit).into()), listener: None })
    }

    /// The production probe: one start of
    /// `duduclaw-isolation-probe@<account>.<port>.service` through PID 1
    /// (`run_fixed_unit`: closed set, installed bytes, load record, effective
    /// properties including the unprivileged identity, `Unit.Ref` held,
    /// strict outcome), mapped with [`unit_outcome`] (returned with PID 1's
    /// record it was read from). `Err`: refused before start (`InvalidData`),
    /// timed out, or a bus failure; the worker records those as inconclusive.
    pub fn run_probe(systemd: &Systemd, account: &str, port: u16) -> io::Result<(ProbeOutcome, FixedUnitOutcome)> {
        let unit = FixedUnit::probe(account, port)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid unit policy: probe account name or port"))?;
        let outcome = systemd.run_fixed_unit(&unit.name(), Duration::from_millis(UNIT_DEADLINE_MS))?;
        Ok((unit_outcome(&unit, &outcome), outcome))
    }

    /// Production side effects, with the worker's own D-Bus connection (made
    /// on first use, dropped after a failure that is not a refusal).
    #[derive(Default)]
    pub struct InstalledHost { systemd: Option<Systemd> }
    impl InstalledHost {
        fn connected(&mut self) -> io::Result<&Systemd> {
            if self.systemd.is_none() { self.systemd = Some(Systemd::connect()?); }
            self.systemd.as_ref().ok_or_else(|| unavailable("systemd"))
        }
        /// F4: the live sshd listener's identity from PID 1
        /// ([`listener_identity`] over [`SSH_SERVICE_UNITS`]); `None` when not
        /// determinable. A bus failure drops the connection (made again next time).
        fn listener(&mut self) -> Option<ListenerIdentity> {
            let views = {
                let systemd = self.connected().ok()?;
                SSH_SERVICE_UNITS.iter().map(|name| systemd.ssh_service_view(name).map(|view| (*name, view)))
                    .collect::<io::Result<Vec<_>>>()
            };
            match views {
                Ok(views) => listener_identity(&views),
                Err(_) => { self.systemd = None; None }
            }
        }
    }
    impl Host for InstalledHost {
        fn host_key(&mut self) -> io::Result<HostKey> {
            let mut key = host_key()?;
            key.listener = self.listener();
            Ok(key)
        }
        fn probe(&mut self, account: &str, port: u16) -> ProbeOutcome {
            let systemd = match self.connected() {
                Ok(systemd) => systemd,
                Err(error) => return ProbeOutcome::Inconclusive(format!("systemd unavailable: {error}")),
            };
            match run_probe(systemd, account, port) {
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
    const INSTANCE: &str = "duduclaw-emp-70000.22";
    fn unit() -> FixedUnit { FixedUnit::probe(ACCOUNT, 22).unwrap() }
    /// A probe instance that ran once and exited `status`, as PID 1 records a
    /// referenced oneshot (exit ≠ 0: `failed`, `Result=exit-code`).
    fn ran(status: i32) -> FixedUnitOutcome {
        let failed = status != 0;
        FixedUnitOutcome { result: if failed { "exit-code" } else { "success" }.into(), exec_main_code: 1, exec_main_status: status,
            exec_main_pid: 4242, invocation: vec![9; 16], previous_invocation: vec![7; 16],
            active_state: if failed { "failed" } else { "inactive" }.into(),
            recorded: vec![ExecCommand { path: PROBE_EXECUTABLE.into(), argv: vec![PROBE_EXECUTABLE.into(), INSTANCE.into()],
                ignore_failure: false, start_monotonic_us: 2_000, exit_monotonic_us: 3_000, pid: 4242, code: 1, status }],
            previous_start_us: 1_000 }
    }

    /// R7c item 1: every exit code class, bound to this invocation. Only the
    /// two NOT-denied codes are Reachable; the unverified ones are not.
    #[test]
    fn ssh_probe_outcome_maps_every_exit_code_class() {
        assert_eq!(unit_outcome(&unit(), &ran(0)), ProbeOutcome::Denied);
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_LOGIN_NOT_DENIED)),
            ProbeOutcome::Reachable("login_not_denied (exit 10)".into()));
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_FORWARD_NOT_DENIED)),
            ProbeOutcome::Reachable("forward_not_denied (exit 13)".into()));
        // F1: a client success without sshd's Accepted record never burns the uid.
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_LOGIN_UNVERIFIED)),
            ProbeOutcome::Inconclusive("login_unverified (exit 18)".into()));
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_FORWARD_UNVERIFIED)),
            ProbeOutcome::Inconclusive("forward_unverified (exit 19)".into()));
        // A failed oneshot of this invocation: the stage is named for the log.
        assert_eq!(unit_outcome(&unit(), &ran(probe_exit::EXIT_IDENTITY)), ProbeOutcome::Inconclusive("identity (exit 3)".into()));
        for code in [2, 4, 6, 8, 9, 11, 12, 14, 15, 16, 17, 18, 19] {
            assert!(matches!(unit_outcome(&unit(), &ran(code)), ProbeOutcome::Inconclusive(ref stage)
                if stage.ends_with(&format!("(exit {code})"))), "{code}");
        }
        // Codes the probe never uses (generic, the retired 5/7, panic, unsupported, systemd exec failures, ssh's).
        for code in [1, 5, 7, 101, 125, 200, 203, 243, 255] {
            assert_eq!(unit_outcome(&unit(), &ran(code)), ProbeOutcome::Inconclusive(format!("exit {code}")), "{code}");
        }
    }

    /// R7c item 1: the status is trusted only when the outcome is this start's.
    #[test]
    fn ssh_probe_outcome_is_bound_to_this_invocation() {
        let unbound = |outcome: &FixedUnitOutcome| matches!(unit_outcome(&unit(), outcome),
            ProbeOutcome::Inconclusive(ref reason) if reason.starts_with("outcome not bound to this start: "));
        for status in [0, probe_exit::EXIT_LOGIN_NOT_DENIED, probe_exit::EXIT_FORWARD_NOT_DENIED, probe_exit::EXIT_IDENTITY] {
            let cases: [fn(&mut FixedUnitOutcome); 16] = [
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
                // Another account's or another port's instance, or another command.
                |o| o.recorded[0].argv[1] = "duduclaw-emp-70001.22".into(),
                |o| o.recorded[0].argv[1] = "duduclaw-emp-70000.2222".into(),
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
        assert_eq!(refusal(Some(&ProbeOutcome::Reachable("x".into()))), Some(SshRefusal::Reachable("x".into())));
        assert_eq!(refusal(Some(&ProbeOutcome::Inconclusive("y".into()))), Some(SshRefusal::Inconclusive("y".into())));
    }

    fn listener(pid: u32, started_us: u64) -> ListenerIdentity {
        ListenerIdentity { unit: "ssh.service".into(), main_pid: pid, started_us, argv: vec![SSHD_PATH.into(), "-D".into()] }
    }
    fn host_key(seed: u8) -> HostKey {
        HostKey { boot_id: "boot-a".into(), generation: Sha256Digest([seed; 32]),
            probe: ArtifactKey { sha256: Sha256Digest([2; 32]), size: 1000, mode: 0o755 }, unit: Sha256Digest([3; 32]),
            listener: Some(listener(812, 5_000_000)) }
    }
    fn account(name: &str, config: u8) -> AccountKey {
        AccountKey { account: name.into(), uid: 70000, sshd_config: Sha256Digest([config; 32]), port: 22 }
    }
    /// Scripted worker side effects sharing one clock with the refresh side.
    struct Fake {
        key: io::Result<HostKey>, after: Option<io::Result<HostKey>>, outcomes: BTreeMap<String, Vec<ProbeOutcome>>,
        probed: Vec<(String, u16)>, keys: usize, clock: Arc<AtomicU64>, lines: Vec<String>,
    }
    impl Fake {
        fn new(clock: Arc<AtomicU64>) -> Self {
            Self { key: Ok(host_key(1)), after: None, outcomes: BTreeMap::new(), probed: vec![], keys: 0, clock, lines: vec![] }
        }
        fn answer(&mut self, name: &str, outcomes: Vec<ProbeOutcome>) { self.outcomes.insert(name.into(), outcomes); }
        fn names(&self) -> Vec<&str> { self.probed.iter().map(|(name, _)| name.as_str()).collect() }
    }
    impl Host for Fake {
        fn host_key(&mut self) -> io::Result<HostKey> {
            self.keys += 1;
            if let Some(after) = self.after.take() { return after; }
            match &self.key { Ok(key) => Ok(key.clone()), Err(_) => Err(io::Error::other("generation probe failed")) }
        }
        fn probe(&mut self, account: &str, port: u16) -> ProbeOutcome {
            self.probed.push((account.into(), port));
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
    /// lookup never runs anything itself; a Denied result is reused within
    /// its TTL; the probe gets the account's port.
    #[test]
    fn ssh_evidence_is_pending_until_probed_and_then_reuses_denied() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let key = AccountKey { port: 2222, ..account(ACCOUNT, 5) };
        for _ in 0..5 { assert_eq!(evidence.lookup(&key), None); }
        let shared = shared(&started);
        assert_eq!(shared.state.lock().unwrap().queue.len(), 1, "one request per account");
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Denied]);
        assert!(step(&mut host, &shared));
        assert_eq!(host.probed, [(ACCOUNT.to_owned(), 2222)], "the probe instance carries the account's port");
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
        assert_eq!(refusal(evidence.lookup(&key).as_ref()), None);
        // Later passes keep the host key fresh and never probe again.
        for _ in 0..10 {
            clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
            assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
            assert!(!step(&mut host, &shared));
        }
        assert_eq!(host.probed.len(), 1);
        assert_eq!(host.lines.iter().filter(|line| line.contains(ACCOUNT)).count(), 1, "{:?}", host.lines);
        assert_eq!(host.lines.last().unwrap(), "SSH denial probe duduclaw-emp-70000: denied");
        // Another port is another key.
        assert_eq!(evidence.lookup(&account(ACCOUNT, 5)), None);
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

    /// F4: the key includes the LIVE sshd listener's identity: an sshd
    /// restart (new main pid / start time) or another command line, with the
    /// very same `sshd -T` output, makes every Denied result not current.
    #[test]
    fn ssh_evidence_key_includes_the_live_listener_identity() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Denied; 4]);
        let key = account(ACCOUNT, 5);
        assert_eq!(evidence.lookup(&key), None);
        let shared = shared(&started);
        assert!(step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
        let changes: [fn(&mut ListenerIdentity); 3] = [
            // Restarted: new pid and start time.
            |l| { l.main_pid += 1; l.started_us += 1; },
            // Restarted into the same pid number.
            |l| l.started_us += 1_000_000,
            // Started with another configuration (`-f`/`-o` in ExecStart).
            |l| l.argv.extend(["-f".to_owned(), "/etc/ssh/other_config".into()]),
        ];
        for (index, change) in changes.iter().enumerate() {
            clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
            let mut next = host.key.as_ref().unwrap().clone();
            change(next.listener.as_mut().unwrap());
            host.key = Ok(next);
            assert!(!step(&mut host, &shared), "case {index}: re-measured; nothing requested yet");
            assert_eq!(evidence.lookup(&key), None, "case {index}: the old Denied must not admit");
            assert!(step(&mut host, &shared), "case {index}");
            assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied), "case {index}");
        }
        assert_eq!(host.probed.len(), 4);
        // An undeterminable identity is a key of its own, too.
        clock.fetch_add(WORKER_PERIOD_MS, Ordering::SeqCst);
        let mut unknown = host.key.as_ref().unwrap().clone();
        unknown.listener = None;
        host.key = Ok(unknown);
        assert!(!step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), None);
        assert!(host.lines.iter().any(|line| line.starts_with("sshd listener identity unavailable")), "{:?}", host.lines);
    }

    /// F4: the listener identity is read only from one active sshd unit
    /// running /usr/sbin/sshd with a main process.
    #[test]
    fn ssh_listener_identity_requires_one_active_sshd_unit_with_a_main_process() {
        fn view(id: &str, state: &str, pid: u32, started_us: u64, exec: &[(&str, &[&str])]) -> Option<ServiceView> {
            Some(ServiceView { id: id.into(), active_state: state.into(), main_pid: pid, started_us,
                exec_start: exec.iter().map(|(path, argv)| ((*path).to_owned(), argv.iter().map(|arg| (*arg).to_owned()).collect())).collect() })
        }
        let sshd: &[(&str, &[&str])] = &[(SSHD_PATH, &[SSHD_PATH, "-D", "$SSHD_OPTS"])];
        let ubuntu = [("ssh.service", view("ssh.service", "active", 812, 5_000_000, sshd)), ("sshd.service", None)];
        assert_eq!(listener_identity(&ubuntu), Some(ListenerIdentity { unit: "ssh.service".into(), main_pid: 812,
            started_us: 5_000_000, argv: vec![SSHD_PATH.into(), "-D".into(), "$SSHD_OPTS".into()] }));
        // Debian's alias: both names resolve to the same unit.
        let alias = [("ssh.service", view("ssh.service", "active", 812, 5_000_000, sshd)),
            ("sshd.service", view("ssh.service", "active", 812, 5_000_000, sshd))];
        assert_eq!(listener_identity(&alias).map(|identity| identity.main_pid), Some(812));
        // RHEL/Fedora name.
        let rhel = [("ssh.service", None), ("sshd.service", view("sshd.service", "active", 900, 7, sshd))];
        assert_eq!(listener_identity(&rhel).map(|identity| identity.unit), Some("sshd.service".into()));
        for views in [
            vec![("ssh.service", None), ("sshd.service", None)],
            vec![("ssh.service", view("ssh.service", "inactive", 0, 0, sshd)), ("sshd.service", None)],
            vec![("ssh.service", view("ssh.service", "activating", 812, 5, sshd)), ("sshd.service", None)],
            // Two distinct active units: which one listens is unknown.
            vec![("ssh.service", view("ssh.service", "active", 812, 5, sshd)), ("sshd.service", view("sshd.service", "active", 813, 6, sshd))],
            // No main process or start time.
            vec![("ssh.service", view("ssh.service", "active", 0, 5, sshd)), ("sshd.service", None)],
            vec![("ssh.service", view("ssh.service", "active", 812, 0, sshd)), ("sshd.service", None)],
            // Another binary, no command, two commands.
            vec![("ssh.service", view("ssh.service", "active", 812, 5, &[("/usr/local/sbin/sshd", &["/usr/local/sbin/sshd", "-D"])])), ("sshd.service", None)],
            vec![("ssh.service", view("ssh.service", "active", 812, 5, &[])), ("sshd.service", None)],
            vec![("ssh.service", view("ssh.service", "active", 812, 5, &[sshd[0], sshd[0]])), ("sshd.service", None)],
            // A unit whose Id is not an sshd unit (an alias elsewhere).
            vec![("ssh.service", view("other.service", "active", 812, 5, sshd)), ("sshd.service", None)],
        ] {
            assert_eq!(listener_identity(&views), None, "{views:?}");
        }
        let long: Vec<String> = (0..=MAX_LISTENER_ARGS).map(|index| format!("-o{index}")).collect();
        let long: Vec<&str> = long.iter().map(String::as_str).collect();
        let too_many: &[(&str, &[&str])] = &[(SSHD_PATH, &long)];
        assert_eq!(listener_identity(&[("ssh.service", view("ssh.service", "active", 812, 5, too_many))]), None);
    }

    /// The fresh host key at `now` for a direct `State` test.
    fn fresh(state: &mut State, key: &HostKey, now: u64) { state.host = Some((key.clone(), now)); }
    fn stored(state: &mut State, key: &HostKey, account: &AccountKey, outcome: ProbeOutcome, at_ms: u64) {
        state.results.insert(account.account.clone(), Stored { key: ProbeKey { host: key.clone(), account: account.clone() }, outcome, at_ms });
    }

    /// F4: a Denied result is current for at most DENIED_TTL (with listener
    /// identity) or DENIED_TTL_NO_IDENTITY (without), refresh-due at 5/6 of
    /// it; an expired one is pending. Reachable never expires in-process.
    #[test]
    fn ssh_evidence_denied_ttl_expires_and_refresh_is_due_at_five_sixths() {
        assert_eq!((DENIED_TTL, DENIED_TTL_NO_IDENTITY), (Duration::from_secs(6 * 3600), Duration::from_secs(600)));
        assert_eq!((DENIED_TTL_MS, DENIED_TTL_NO_IDENTITY_MS), (21_600_000, 600_000));
        let mut unknown = host_key(1);
        unknown.listener = None;
        for (key, ttl) in [(host_key(1), DENIED_TTL_MS), (unknown, DENIED_TTL_NO_IDENTITY_MS)] {
            let mut state = State::default();
            let account = account(ACCOUNT, 5);
            let t0 = 1_000_000;
            stored(&mut state, &key, &account, ProbeOutcome::Denied, t0);
            let refresh = ttl / 6 * 5;
            for (now, current, due) in [(t0, true, false), (t0 + refresh - 1, true, false), (t0 + refresh, true, true),
                (t0 + ttl - 1, true, true), (t0 + ttl, false, true), (t0 + ttl * 10, false, true), (t0 - 1, false, true)] {
                fresh(&mut state, &key, now);
                assert_eq!(state.current(&account, now).is_some(), current, "ttl {ttl} now {now}");
                assert_eq!(state.needs_probe(&account, now, false), due, "ttl {ttl} now {now}");
                // An allocation's forced request does not force a Denied result early.
                assert_eq!(state.needs_probe(&account, now, true), due, "ttl {ttl} now {now}");
            }
            // Reachable: never expires, never re-probed.
            stored(&mut state, &key, &account, ProbeOutcome::Reachable("login_not_denied (exit 10)".into()), t0);
            for now in [t0, t0 + ttl, t0 + DENIED_TTL_MS * 4] {
                fresh(&mut state, &key, now);
                assert!(state.current(&account, now).is_some(), "{now}");
                assert!(!state.needs_probe(&account, now, true), "{now}");
            }
        }
    }

    /// F4: the worker re-probes a Denied result in the background at 5/6 of
    /// its TTL, without any lookup, so it never shows a gap; a stalled worker
    /// leaves an expired result pending.
    #[test]
    fn ssh_evidence_worker_reprobes_early_and_an_expired_result_is_pending() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Denied, ProbeOutcome::Denied]);
        let key = account(ACCOUNT, 5);
        assert_eq!(evidence.lookup(&key), None);
        let shared = shared(&started);
        assert!(step(&mut host, &shared));
        let refresh = DENIED_TTL_MS / 6 * 5;
        // Just before the refresh point: kept, nothing runs.
        clock.fetch_add(refresh - 1, Ordering::SeqCst);
        assert!(!step(&mut host, &shared));
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
        // At the refresh point the worker queues and runs it on its own.
        clock.fetch_add(1, Ordering::SeqCst);
        assert!(step(&mut host, &shared), "background re-probe");
        assert_eq!(host.probed.len(), 2);
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied), "no gap");
        // The worker stalls past the new TTL (host key kept fresh by hand): pending.
        let expiry = clock.load(Ordering::SeqCst) + DENIED_TTL_MS;
        clock.store(expiry, Ordering::SeqCst);
        if let Ok(mut state) = shared.state.lock() { let host = state.host.clone().unwrap().0; fresh(&mut state, &host, expiry); }
        assert_eq!(evidence.lookup(&key), None);
        assert_eq!(refusal(evidence.lookup(&key).as_ref()), Some(SshRefusal::Pending));
    }

    #[test]
    fn ssh_evidence_refresh_without_listener_identity_is_due_at_five_sixths_of_ten_minutes() {
        // Without listener identity the refresh comes after 5/6 of 10 min.
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let mut host = Fake::new(clock.clone());
        let mut unknown = host_key(1);
        unknown.listener = None;
        host.key = Ok(unknown);
        host.answer(ACCOUNT, vec![ProbeOutcome::Denied, ProbeOutcome::Denied]);
        let key = account(ACCOUNT, 5);
        assert_eq!(evidence.lookup(&key), None);
        let shared = shared(&started);
        assert!(step(&mut host, &shared));
        clock.fetch_add(DENIED_TTL_NO_IDENTITY_MS / 6 * 5, Ordering::SeqCst);
        assert!(step(&mut host, &shared));
        assert_eq!(host.probed.len(), 2);
        assert_eq!(evidence.lookup(&key), Some(ProbeOutcome::Denied));
    }

    /// The background refresh never replaces a queued request (which may
    /// carry a newer key) and goes behind it.
    #[test]
    fn ssh_evidence_refresh_keeps_queued_requests() {
        let mut state = State::default();
        let key = host_key(1);
        stored(&mut state, &key, &account(ACCOUNT, 5), ProbeOutcome::Denied, 0);
        stored(&mut state, &key, &account("duduclaw-emp-70001", 5), ProbeOutcome::Denied, 0);
        state.enqueue(account(ACCOUNT, 6), false);
        state.queue_refreshes(&key, DENIED_TTL_MS);
        let order: Vec<(&str, u8)> = state.queue.iter().map(|request| (request.account.account.as_str(), request.account.sshd_config.0[0])).collect();
        assert_eq!(order, [(ACCOUNT, 6), ("duduclaw-emp-70001", 5)]);
        // Not yet due, or under another host key: nothing queued.
        let mut state = State::default();
        stored(&mut state, &key, &account(ACCOUNT, 5), ProbeOutcome::Denied, 0);
        state.queue_refreshes(&key, DENIED_TTL_MS / 2);
        state.queue_refreshes(&host_key(2), DENIED_TTL_MS);
        assert!(state.queue.is_empty());
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
            assert_eq!(host.probed[index].0, *name);
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
        assert_eq!(host.names(), [ACCOUNT, ACCOUNT]);
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

    /// F6: `request` never waits: it returns the current result (or `None`)
    /// at once and queues a forced probe; an inconclusive result is re-run
    /// only after FORCED_RETRY_MS (a repeating gateway cannot keep the
    /// worker busy); Denied and Reachable are returned without another run.
    #[test]
    fn ssh_evidence_request_is_forced_and_never_waits() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let (evidence, started) = evidence(&clock);
        let key = account(ACCOUNT, 5);
        // Nothing runs the worker here: the call still returns at once.
        let begun = std::time::Instant::now();
        assert_eq!(evidence.request(&key), None);
        assert!(begun.elapsed() < Duration::from_secs(1));
        let shared = shared(&started);
        assert!(shared.state.lock().unwrap().queue[0].force);
        let mut host = Fake::new(clock.clone());
        host.answer(ACCOUNT, vec![ProbeOutcome::Inconclusive("login_unverified (exit 18)".into()), ProbeOutcome::Denied]);
        assert!(step(&mut host, &shared));
        assert!(matches!(evidence.request(&key), Some(ProbeOutcome::Inconclusive(_))));
        // Within FORCED_RETRY_MS a repeated request is dropped by the worker.
        assert!(!step(&mut host, &shared));
        clock.fetch_add(FORCED_RETRY_MS, Ordering::SeqCst);
        assert!(!step(&mut host, &shared), "re-measured; the earlier forced request was dropped");
        // After it, the request re-queues (the worker dropped the old one) and runs ahead of the 60 s window.
        assert!(matches!(evidence.request(&key), Some(ProbeOutcome::Inconclusive(_))));
        assert!(step(&mut host, &shared));
        assert_eq!(evidence.request(&key), Some(ProbeOutcome::Denied));
        assert!(!step(&mut host, &shared), "Denied is returned without another run");
        assert_eq!(host.probed.len(), 2);
        // Missing evidence: pending, never blocking.
        assert_eq!(MissingSshEvidence.request(&key), None);
        assert!(FORCED_RETRY_MS < INCONCLUSIVE_RETRY_MS);
    }

    /// The worker loop stops when the evidence is dropped.
    #[test]
    fn ssh_evidence_worker_loop_stops_with_the_evidence() {
        let clock = Arc::new(AtomicU64::new(10_000));
        let worker_clock = clock.clone();
        let done = Arc::new(Mutex::new(None::<Vec<String>>));
        let finished = done.clone();
        let start: StartWorker = Box::new(move |shared: Arc<Shared>| {
            std::thread::spawn(move || {
                let mut host = Fake::new(worker_clock);
                host.answer(ACCOUNT, vec![ProbeOutcome::Denied]);
                worker_loop(&mut host, &shared);
                *finished.lock().unwrap() = Some(host.names().into_iter().map(str::to_owned).collect());
            });
        });
        let time = clock.clone();
        let evidence = ProbeEvidence::new(start, Box::new(move || -> io::Result<u64> { Ok(time.load(Ordering::SeqCst)) }));
        let key = account(ACCOUNT, 5);
        assert_eq!(evidence.request(&key), None);
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while evidence.lookup(&key) != Some(ProbeOutcome::Denied) {
            assert!(std::time::Instant::now() < until, "worker did not probe");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(evidence);
        while done.lock().unwrap().is_none() { assert!(std::time::Instant::now() < until, "worker did not stop"); std::thread::sleep(Duration::from_millis(10)); }
        assert_eq!(done.lock().unwrap().clone().unwrap(), [ACCOUNT]);
    }
}
