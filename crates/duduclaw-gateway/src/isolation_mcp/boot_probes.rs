//! R8n-1: the ParseWorker and RuntimeAdapters integration probes of the
//! assembled boot owner, installed by `install_probes` next to the MCP
//! boundary's probe (inside the attempt's shutdown-gate permit).
//!
//! Both run on every readiness evaluation (every admission, every
//! `system.status` read and the published view's 5 s tick) and read MEMORY
//! only: their producer checks are in-memory and their service check is the
//! last stored [`Observations`] entry. R8n-1 review F2: the service I/O
//! (`/proc/net/unix` up to 8 MiB, the socket/lease metadata, the root
//! publication) is done by ONE background observer per process
//! ([`start_observer`]), never by a probe: a Tokio task that, every
//! [`PERIOD`] while the service is assembled, runs both readbacks on the
//! blocking pool and stores each result with a monotonic timestamp taken
//! BEFORE that read started. Single-flight by construction (one task, each
//! pass awaited before the next); it stops at the shutdown gate. A probe uses
//! a stored entry while it is at most [`MAX_AGE`] old (the composite's 2 s
//! window), so the evidence an admission relies on is at most 2 s stale;
//! older is `observation_stale`, none yet `observation_pending`. A failed
//! read stores its fixed refusal. Nothing is sent to any service. Any error is
//! a refusal with one fixed reason (`published::PARSE_WORKER_REASONS` /
//! `RUNTIME_ADAPTER_REASONS`); an absent probe stays `PendingIntegration`.
use std::{path::Path, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use duduclaw_isolation::{execution::RuntimeKind, ids::Sha256Digest};
use duduclaw_spawnd::policy::PublishedRuntime;
use crate::isolation_readiness::{IntegrationProbe, ProbeError, PtyRequirement, ServiceObservation};
use super::OWNER;

/// The observer starts one pass at most this often (a slower pass delays the
/// next one; passes never overlap).
const PERIOD: Duration = Duration::from_secs(1);
/// A stored observation is used while at most this old, measured from the
/// timestamp taken before its read (the composite's observation window).
const MAX_AGE: Duration = Duration::from_secs(2);
/// `/proc/net/unix` is read up to this size; a larger table is a refusal.
const MAX_SOCKET_TABLE_BYTES: u64 = 8 * 1024 * 1024;

const OBSERVATION_PENDING: &str = "observation_pending";
const OBSERVATION_STALE: &str = "observation_stale";

const ASSEMBLY_INCOMPLETE: &str = "assembly_incomplete";
const LAUNCHER_NOT_INSTALLED: &str = "launcher_not_installed";
const GENERATION_MISMATCH: &str = "generation_mismatch";
const SOCKET_UNAVAILABLE: &str = "socket_unavailable";
const SOCKET_UNSAFE: &str = "socket_unsafe";
const SOCKET_NOT_LISTENING: &str = "socket_not_listening";
const LEASE_UNAVAILABLE: &str = "retirement_lease_unavailable";
const HELPER_PRODUCER_MISSING: &str = "helper_producer_missing";
const FORK_READER_MISSING: &str = "fork_reader_missing";
const PUBLICATION_UNAVAILABLE: &str = "publication_unavailable";
const NO_MAIN_RUNTIME: &str = "no_main_runtime";
const MAIN_ADAPTER_UNAVAILABLE: &str = "main_adapter_unavailable";

type Readback = Result<Option<PtyRequirement>, &'static str>;

/// One stored readback: `observed` was taken BEFORE the read started.
#[derive(Clone, Copy)]
struct Stored { generation: Sha256Digest, observed: Instant, result: Readback }

/// The observer's latest readback of one probe (memory only).
struct Slot(Mutex<Option<Stored>>);
impl Slot {
    const fn new() -> Self { Self(Mutex::new(None)) }
    fn store(&self, value: Option<Stored>) {
        // A poisoned slot is still overwritten: never keep older evidence.
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = value;
    }
    /// `(timestamp taken before the read, its result)` while fresh for
    /// `generation`; otherwise the fixed reason (fail closed).
    fn fresh(&self, generation: Sha256Digest, now: Instant) -> Result<(Instant, Readback), &'static str> {
        let slot = self.0.lock().map_err(|_| OBSERVATION_PENDING)?;
        let stored = slot.as_ref().ok_or(OBSERVATION_PENDING)?;
        if stored.generation != generation
            || !now.checked_duration_since(stored.observed).is_some_and(|age| age <= MAX_AGE) {
            return Err(OBSERVATION_STALE);
        }
        Ok((stored.observed, stored.result))
    }
}

/// The two probes' stored observations: written only by the observer, read
/// (memory only) by the probes. A probe holds this and nothing else: it has
/// no access to a [`ServiceReader`].
pub(super) struct Observations { parse: Slot, runtime: Slot }
impl Observations {
    pub(super) const fn new() -> Self { Self { parse: Slot::new(), runtime: Slot::new() } }
    fn clear(&self) {
        self.parse.store(None);
        self.runtime.store(None);
    }
}

/// The process's observations (the one observer writes them).
static OBSERVATIONS: Observations = Observations::new();
/// Set once the process's one observer was started.
static OBSERVER_STARTED: AtomicBool = AtomicBool::new(false);

/// The blocking service readbacks of both probes. Only the observer holds one.
pub(super) trait ServiceReader: Send + Sync {
    /// The parse service's local evidence (`Ok(None)` when it passed).
    fn parse(&self, generation: Sha256Digest) -> Readback;
    /// The root publication's main runtimes (`Ok(Some(pty))` when it passed).
    fn runtime(&self, generation: Sha256Digest) -> Readback;
}

/// Production readbacks (see [`ParseWorkerReadiness`], [`RuntimeAdapterReadiness`]).
struct InstalledReader;
impl ServiceReader for InstalledReader {
    fn parse(&self, _generation: Sha256Digest) -> Readback {
        parse_service_evidence(Path::new(duduclaw_parse_worker::policy::SOCKET_PATH),
            Path::new(duduclaw_parse_worker::retirement::LEASE_PATH), 0, &gateway_groups(), unix_socket_table)
            .map(|()| None)
    }
    fn runtime(&self, generation: Sha256Digest) -> Readback {
        let owner = OWNER.get().ok_or(ASSEMBLY_INCOMPLETE)?;
        let publication = duduclaw_spawnd::installed::GatewayPublication::read(nix::unistd::getegid().as_raw())
            .map_err(|_| PUBLICATION_UNAVAILABLE)?;
        if publication.generation != generation { return Err(GENERATION_MISMATCH); }
        publication.policy().map_err(|_| PUBLICATION_UNAVAILABLE)?;
        let runtimes: Vec<PublishedRuntime> = publication.runtimes.iter().map(|record| record.to_runtime()).collect();
        main_runtime_evidence(&runtimes, |kind| owner.registry.registration(kind).is_ok()).map(Some)
    }
}

/// One observer pass: each read's timestamp is taken before it starts, and
/// its result (a success or its fixed refusal) replaces the stored one.
fn observe_once(reader: &dyn ServiceReader, observations: &Observations, generation: Sha256Digest) {
    let observed = Instant::now();
    let result = reader.parse(generation);
    observations.parse.store(Some(Stored { generation, observed, result }));
    let observed = Instant::now();
    let result = reader.runtime(generation);
    observations.runtime.store(Some(Stored { generation, observed, result }));
}

/// The observer loop: one pass on the blocking pool, awaited, then the rest
/// of [`PERIOD`]; never two passes at once. Ends once `closed` answers true
/// (checked before every pass and after every wait). A pass that panicked
/// clears both entries (pending: fail closed at once).
async fn observe(reader: Arc<dyn ServiceReader>, observations: &'static Observations, generation: Sha256Digest,
    closed: impl Fn() -> bool) {
    loop {
        if closed() { return; }
        let started = Instant::now();
        let pass = reader.clone();
        if tokio::task::spawn_blocking(move || observe_once(pass.as_ref(), observations, generation)).await.is_err() {
            observations.clear();
        }
        tokio::time::sleep(PERIOD.saturating_sub(started.elapsed())).await;
    }
}

/// Start the process's ONE observer for the assembled owner's `generation`
/// (a later call does nothing). Called after the probes were installed,
/// inside the boot attempt's shutdown-gate permit; `closed` is the shutdown
/// gate. Without a Tokio runtime nothing is observed and both probes stay
/// not ready (`observation_pending`).
pub(super) fn start_observer(generation: Sha256Digest, closed: fn() -> bool) {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::error!("隔離服務狀態的背景觀察無法啟動，團隊執行維持暫停。");
        return;
    }
    if OBSERVER_STARTED.swap(true, Ordering::AcqRel) { return; }
    drop(tokio::spawn(observe(Arc::new(InstalledReader), &OBSERVATIONS, generation, closed)));
}

/// The ParseWorker service observation from memory only.
fn parse_observation(observations: &Observations, generation: Sha256Digest, now: Instant)
    -> Result<ServiceObservation, ProbeError> {
    let (observed, result) = observations.parse.fresh(generation, now).map_err(ProbeError::Refused)?;
    result.map_err(ProbeError::Refused)?;
    Ok(ServiceObservation::checked(generation, observed))
}

/// The RuntimeAdapters service observation from memory only.
fn runtime_observation(observations: &Observations, generation: Sha256Digest, now: Instant)
    -> Result<ServiceObservation, ProbeError> {
    let (observed, result) = observations.runtime.fresh(generation, now).map_err(ProbeError::Refused)?;
    let pty = result.map_err(ProbeError::Refused)?.ok_or(ProbeError::Refused(PUBLICATION_UNAVAILABLE))?;
    Ok(ServiceObservation::checked_runtime(generation, observed, pty))
}

fn healthy_owner() -> Result<&'static super::BootOwner, ProbeError> {
    OWNER.get().map(|owner| owner.as_ref()).filter(|owner| owner.boundary.healthy())
        .ok_or(ProbeError::Refused(ASSEMBLY_INCOMPLETE))
}

/// ParseWorker: the gateway can use the installed document-parse service for
/// an isolated employee. Producers: the assembled owner (its parse
/// integration: `InstalledWorkerServices`, the fd-table parser and the worker
/// operation route) and the retirement-aware launcher installed process-wide
/// with isolation required (no local parser fallback). Service: the fixed
/// socket `/run/duduclaw-parse/parse.sock` (unit `duduclaw-parse.socket`) is
/// a root-owned socket the gateway's group can use, under a root-owned
/// directory, bound and LISTENING (read from `/proc/net/unix`, never by
/// connecting: the socket is `Accept=yes`, so a connection would start a
/// parse worker outside the host-wide retirement lease), and the root-created
/// retirement lease exists with its fixed owner and mode. The worker
/// operation routes are checked by the McpBoundary probe (the CLI's route
/// inventory against the same `worker_plans`); spawnd's host evidence covers
/// containment. The service evidence is read by the observer
/// ([`InstalledReader::parse`]); this probe only reads its stored result.
pub(super) struct ParseWorkerReadiness { observations: &'static Observations }
impl Default for ParseWorkerReadiness {
    fn default() -> Self { Self { observations: &OBSERVATIONS } }
}
impl IntegrationProbe for ParseWorkerReadiness {
    fn verify_producers(&self) -> Result<(), ProbeError> {
        healthy_owner()?;
        if !duduclaw_parse_worker::client::launcher_installed() || !duduclaw_parse_worker::client::required() {
            return Err(ProbeError::Refused(LAUNCHER_NOT_INSTALLED));
        }
        Ok(())
    }

    fn verify_service(&self, generation: Sha256Digest) -> Result<ServiceObservation, ProbeError> {
        let owner = OWNER.get().ok_or(ProbeError::Refused(ASSEMBLY_INCOMPLETE))?;
        if owner.generation != generation { return Err(ProbeError::Refused(GENERATION_MISMATCH)); }
        parse_observation(self.observations, generation, Instant::now())
    }
}

/// The gateway's effective and supplementary groups.
fn gateway_groups() -> Vec<u32> {
    let mut groups: Vec<u32> = nix::unistd::getgroups().unwrap_or_default().into_iter().map(|gid| gid.as_raw()).collect();
    groups.push(nix::unistd::getegid().as_raw());
    groups
}

fn unix_socket_table() -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open("/proc/net/unix").ok()?.take(MAX_SOCKET_TABLE_BYTES + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= MAX_SOCKET_TABLE_BYTES).then_some(text)
}

/// The parse service's local evidence (see [`ParseWorkerReadiness`]).
/// `root_uid` is 0 in production; `groups` the gateway's groups; `table`
/// reads `/proc/net/unix` (only consulted after every file check passed).
fn parse_service_evidence(socket: &Path, lease: &Path, root_uid: u32, groups: &[u32],
    table: impl FnOnce() -> Option<String>) -> Result<(), &'static str> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(socket).map_err(|_| SOCKET_UNAVAILABLE)?;
    if !meta.file_type().is_socket() { return Err(SOCKET_UNAVAILABLE); }
    let parent = socket.parent().and_then(|parent| std::fs::symlink_metadata(parent).ok()).ok_or(SOCKET_UNSAFE)?;
    // `SocketUser=root`, `SocketGroup=duduclaw`, `SocketMode=0660` (the
    // transport's own connect check: root-owned, no other access).
    if meta.uid() != root_uid || meta.mode() & 0o007 != 0 || meta.mode() & 0o060 != 0o060
        || !groups.contains(&meta.gid()) || !parent.is_dir() || parent.uid() != root_uid || parent.mode() & 0o022 != 0 {
        return Err(SOCKET_UNSAFE);
    }
    // The lease the retiring launcher requires before every launch.
    let lease = std::fs::symlink_metadata(lease).map_err(|_| LEASE_UNAVAILABLE)?;
    if !lease.is_file() || lease.uid() != root_uid || lease.nlink() != 1 || lease.mode() & 0o7777 != 0o660 {
        return Err(LEASE_UNAVAILABLE);
    }
    let path = socket.to_str().ok_or(SOCKET_UNAVAILABLE)?;
    if !table().is_some_and(|table| listening(&table, path)) { return Err(SOCKET_NOT_LISTENING); }
    Ok(())
}

/// A listening `SOCK_SEQPACKET` socket bound at exactly `path` in a
/// `/proc/net/unix` table: `Flags` has `__SO_ACCEPTCON`, `Type` 0005, state
/// unconnected.
fn listening(table: &str, path: &str) -> bool {
    table.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.len() == 8 && fields[7] == path && fields[4] == "0005" && fields[5] == "01"
            && u32::from_str_radix(fields[3], 16).is_ok_and(|flags| flags & 0x0001_0000 != 0)
    })
}

/// RuntimeAdapters: every main runtime the signed policy offers has its
/// gateway-side adapter installed and resolvable, and the helper producers of
/// exactly the AVAILABLE helper families are installed (R8n-1 1.4, the same
/// facts `a_producers_ready` evaluates). Producers: the healthy owner, a
/// producer for every offered helper tool and the three fork readers.
/// Service: the fresh root publication of this generation parses as the
/// policy the main plan factory derives from; it signs at least one main
/// runtime; each main runtime kind is one the gateway launches (Claude,
/// Codex, Grok, the API worker, `generic-cli-v1.<id>`) and resolves in the
/// installed runtime registry (signed MCP bridge acceptance). The PTY
/// requirement comes from the same publication: `Required` when a main
/// runtime allows a PTY (then the separate PtyTransport proof is required).
/// The publication is read by the observer ([`InstalledReader::runtime`]);
/// this probe only reads its stored result.
pub(super) struct RuntimeAdapterReadiness { observations: &'static Observations }
impl Default for RuntimeAdapterReadiness {
    fn default() -> Self { Self { observations: &OBSERVATIONS } }
}
impl IntegrationProbe for RuntimeAdapterReadiness {
    fn verify_producers(&self) -> Result<(), ProbeError> {
        let owner = healthy_owner()?;
        helper_producers(|name| owner.execution_plans.offers(name), |name| owner.execution_plans.supports(name),
            |name| owner.fork_tools.supports(name)).map_err(ProbeError::Refused)
    }

    fn verify_service(&self, generation: Sha256Digest) -> Result<ServiceObservation, ProbeError> {
        let owner = OWNER.get().ok_or(ProbeError::Refused(ASSEMBLY_INCOMPLETE))?;
        if owner.generation != generation { return Err(ProbeError::Refused(GENERATION_MISMATCH)); }
        runtime_observation(self.observations, generation, Instant::now())
    }
}

/// `fork_policy::remaining_producers_ready` with its fixed reason.
fn helper_producers(mut offered: impl FnMut(&str) -> bool, mut helper: impl FnMut(&str) -> bool,
    mut reader: impl FnMut(&str) -> bool) -> Result<(), &'static str> {
    use duduclaw_mcp_boundary::fork_policy::{HELPER_TOOLS, READ_TOOLS};
    if HELPER_TOOLS.into_iter().any(|name| offered(name) && !helper(name)) { return Err(HELPER_PRODUCER_MISSING); }
    if !READ_TOOLS.into_iter().all(&mut reader) { return Err(FORK_READER_MISSING); }
    Ok(())
}

/// The main runtimes of a signed runtime list (everything but the helper
/// kinds): at least one, each with a gateway adapter that `resolvable`
/// accepts. Answers the PTY requirement of that same list.
fn main_runtime_evidence(runtimes: &[PublishedRuntime], mut resolvable: impl FnMut(&RuntimeKind) -> bool)
    -> Result<PtyRequirement, &'static str> {
    let mains: Vec<&PublishedRuntime> = runtimes.iter()
        .filter(|runtime| !crate::isolation_entitlement::helper_kind(&runtime.runtime)).collect();
    if mains.is_empty() { return Err(NO_MAIN_RUNTIME); }
    if mains.iter().any(|runtime| !gateway_adapter(&runtime.runtime) || !resolvable(&runtime.runtime)) {
        return Err(MAIN_ADAPTER_UNAVAILABLE);
    }
    Ok(if mains.iter().any(|runtime| runtime.pty_allowed) { PtyRequirement::Required } else { PtyRequirement::NotRequired })
}

/// A main runtime kind the gateway's protected launch paths serve
/// (`isolation_entitlement::isolation_kind`'s image).
fn gateway_adapter(kind: &RuntimeKind) -> bool {
    match kind {
        RuntimeKind::Claude | RuntimeKind::Codex | RuntimeKind::Grok | RuntimeKind::OpenAiCompat => true,
        RuntimeKind::Registered(name) => name.as_str().strip_prefix("generic-cli-v1.")
            .and_then(duduclaw_core::types::RuntimeType::parse)
            .and_then(crate::isolation_entitlement::isolation_kind)
            .is_some_and(|mapped| mapped == *kind),
    }
}

/// Every reason these probes can give (each must be in the published tables).
#[cfg(test)]
const PARSE_REASONS: [&str; 9] = [ASSEMBLY_INCOMPLETE, LAUNCHER_NOT_INSTALLED, GENERATION_MISMATCH,
    SOCKET_UNAVAILABLE, SOCKET_UNSAFE, SOCKET_NOT_LISTENING, LEASE_UNAVAILABLE, OBSERVATION_PENDING,
    OBSERVATION_STALE];
#[cfg(test)]
const RUNTIME_REASONS: [&str; 9] = [ASSEMBLY_INCOMPLETE, HELPER_PRODUCER_MISSING, FORK_READER_MISSING,
    GENERATION_MISMATCH, PUBLICATION_UNAVAILABLE, NO_MAIN_RUNTIME, MAIN_ADAPTER_UNAVAILABLE, OBSERVATION_PENDING,
    OBSERVATION_STALE];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolation_readiness::published::{PARSE_WORKER_REASONS, RUNTIME_ADAPTER_REASONS};

    #[test]
    fn r8n_probe_reasons_are_the_published_tables() {
        let mut parse = PARSE_REASONS.to_vec();
        parse.sort_unstable();
        let mut table = PARSE_WORKER_REASONS.to_vec();
        table.sort_unstable();
        assert_eq!(parse, table);
        let mut runtime = RUNTIME_REASONS.to_vec();
        runtime.sort_unstable();
        let mut table = RUNTIME_ADAPTER_REASONS.to_vec();
        table.sort_unstable();
        assert_eq!(runtime, table);
    }

    /// Before assembly both probes refuse with `assembly_incomplete`.
    #[test]
    fn r8n_probes_refuse_without_an_assembled_owner() {
        if OWNER.get().is_some() { return; }
        let generation = Sha256Digest([1; 32]);
        for probe in [&ParseWorkerReadiness::default() as &dyn IntegrationProbe, &RuntimeAdapterReadiness::default()] {
            assert_eq!(probe.verify_producers(), Err(ProbeError::Refused(ASSEMBLY_INCOMPLETE)));
            assert!(matches!(probe.verify_service(generation), Err(ProbeError::Refused(ASSEMBLY_INCOMPLETE))));
        }
    }

    /// The same decision as `fork_policy::remaining_producers_ready` for every
    /// offered subset, with the fixed reason of the first missing producer.
    #[test]
    fn r8n_helper_producers_agree_with_the_readiness_hook() {
        use duduclaw_mcp_boundary::fork_policy::{remaining_producers_ready, HELPER_TOOLS};
        for mask in 0u32..(1 << HELPER_TOOLS.len()) {
            let offered = |name: &str| HELPER_TOOLS.iter().position(|tool| *tool == name)
                .is_some_and(|index| mask & (1 << index) != 0);
            for (helper, reader) in [(true, true), (false, true), (true, false), (false, false)] {
                let helper_of = |name: &str| helper && offered(name);
                let result = helper_producers(offered, helper_of, |_| reader);
                assert_eq!(result.is_ok(), remaining_producers_ready(offered, helper_of, |_| reader), "{mask:b}");
                let expected = if !helper && mask != 0 { Err(HELPER_PRODUCER_MISSING) }
                    else if !reader { Err(FORK_READER_MISSING) } else { Ok(()) };
                assert_eq!(result, expected, "{mask:b} {helper} {reader}");
            }
        }
    }

    fn runtime(kind: RuntimeKind, pty_allowed: bool) -> PublishedRuntime {
        let mut runtime = crate::isolation_entitlement::tests::signed(1, kind);
        runtime.pty_allowed = pty_allowed;
        runtime
    }

    /// R8n-1 1.4/2.2: at least one main runtime; every main runtime has a
    /// gateway adapter that resolves; helpers alone are not enough; the PTY
    /// requirement follows the signed main runtimes.
    #[test]
    fn r8n_runtime_adapters_require_a_resolvable_main_runtime() {
        use crate::isolation_entitlement::tests::registered;
        let program = runtime(registered(duduclaw_execution::program_helper::PROGRAM_RUNTIME), false);
        let fork = runtime(registered(duduclaw_execution::fork_helper::RUNTIME), false);
        // Zero main runtimes: not ready, whatever helpers exist.
        assert_eq!(main_runtime_evidence(&[], |_| true), Err(NO_MAIN_RUNTIME));
        assert_eq!(main_runtime_evidence(&[program.clone(), fork.clone()], |_| true), Err(NO_MAIN_RUNTIME));
        // Main runtimes only (zero helpers): ready when every one resolves.
        let claude = runtime(RuntimeKind::Claude, false);
        let qwen = runtime(registered("generic-cli-v1.qwen"), false);
        assert_eq!(main_runtime_evidence(&[claude.clone(), qwen.clone()], |_| true), Ok(PtyRequirement::NotRequired));
        assert_eq!(main_runtime_evidence(&[claude.clone(), program, fork], |_| true), Ok(PtyRequirement::NotRequired));
        // One main runtime that does not resolve in the registry: refused.
        assert_eq!(main_runtime_evidence(&[claude.clone(), qwen.clone()], |kind| *kind == RuntimeKind::Claude),
            Err(MAIN_ADAPTER_UNAVAILABLE));
        // A signed main kind the gateway has no adapter for: refused.
        for unknown in ["something-else", "generic-cli-v1.no-such-cli", "generic-cli-v1.claude"] {
            assert_eq!(main_runtime_evidence(&[claude.clone(), runtime(registered(unknown), false)], |_| true),
                Err(MAIN_ADAPTER_UNAVAILABLE), "{unknown}");
        }
        // A PTY main runtime makes the PTY proof mandatory.
        assert_eq!(main_runtime_evidence(&[claude, runtime(RuntimeKind::Grok, true)], |_| true), Ok(PtyRequirement::Required));
        for kind in [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Grok, RuntimeKind::OpenAiCompat,
            registered("generic-cli-v1.qwen")] {
            assert!(gateway_adapter(&kind), "{kind:?}");
        }
    }

    const PATH: &str = "/run/duduclaw-parse/parse.sock";
    const HEADER: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n";

    #[test]
    fn r8n_parse_socket_must_be_listening_seqpacket_at_the_exact_path() {
        let line = |flags: &str, kind: &str, state: &str, path: &str|
            format!("{HEADER}0000000000000000: 00000002 00000000 {flags} {kind} {state} 4242 {path}\n");
        assert!(listening(&line("00010000", "0005", "01", PATH), PATH));
        // Not listening, wrong type, connected, other path, no path.
        assert!(!listening(&line("00000000", "0005", "01", PATH), PATH));
        assert!(!listening(&line("00010000", "0001", "01", PATH), PATH));
        assert!(!listening(&line("00010000", "0005", "03", PATH), PATH));
        assert!(!listening(&line("00010000", "0005", "01", "/run/duduclaw-parse/parse.sock.evil"), PATH));
        assert!(!listening(&format!("{HEADER}0000000000000000: 00000002 00000000 00010000 0005 01 4242\n"), PATH));
        // The header line is never a socket.
        assert!(!listening(&format!("Num RefCount Protocol 00010000 0005 01 4242 {PATH}\n"), PATH));
        assert!(!listening("", PATH));
    }

    /// The file checks over a temp directory owned by this test's account
    /// (standing in for root), then the listening check over a real temporary
    /// SOCK_SEQPACKET listener read back from `/proc/net/unix` (Linux).
    #[test]
    fn r8n_parse_service_evidence_checks_socket_lease_and_listener() {
        use std::os::unix::fs::PermissionsExt;
        let me = nix::unistd::geteuid().as_raw();
        let group = nix::unistd::getegid().as_raw();
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = dir.path().join("parse.sock");
        let lease = dir.path().join("retirement.lease");
        let (ours, foreign): (&[u32], &[u32]) = (&[group], &[group.wrapping_add(1)]);
        type Table = Box<dyn FnOnce() -> Option<String>>;
        let listed = || -> Table {
            let path = socket.to_str().unwrap().to_owned();
            Box::new(move || Some(format!("{HEADER}0000000000000000: 00000002 00000000 00010000 0005 01 4242 {path}\n")))
        };
        let check = |groups: &[u32], table: Table| parse_service_evidence(&socket, &lease, me, groups, table);
        // No socket yet.
        assert_eq!(check(ours, listed()), Err(SOCKET_UNAVAILABLE));
        // A regular file is not a socket.
        std::fs::write(&socket, b"").unwrap();
        assert_eq!(check(ours, listed()), Err(SOCKET_UNAVAILABLE));
        std::fs::remove_file(&socket).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660)).unwrap();
        // No lease yet.
        assert_eq!(check(ours, listed()), Err(LEASE_UNAVAILABLE));
        std::fs::write(&lease, b"clean\n").unwrap();
        std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert_eq!(check(ours, listed()), Ok(()));
        // Wrong owner, other access, no group access, a foreign group.
        assert_eq!(parse_service_evidence(&socket, &lease, me.wrapping_add(1), ours, listed()), Err(SOCKET_UNSAFE));
        for mode in [0o666, 0o600] {
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(check(ours, listed()), Err(SOCKET_UNSAFE), "{mode:o}");
        }
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert_eq!(check(foreign, listed()), Err(SOCKET_UNSAFE));
        // A group/other-writable parent directory.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        assert_eq!(check(ours, listed()), Err(SOCKET_UNSAFE));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // The lease: wrong mode, hard-linked.
        std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(check(ours, listed()), Err(LEASE_UNAVAILABLE));
        std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o660)).unwrap();
        std::fs::hard_link(&lease, dir.path().join("alias")).unwrap();
        assert_eq!(check(ours, listed()), Err(LEASE_UNAVAILABLE));
        std::fs::remove_file(dir.path().join("alias")).unwrap();
        // The socket table: unreadable, or no listener at that path.
        assert_eq!(check(ours, Box::new(|| None)), Err(SOCKET_NOT_LISTENING));
        assert_eq!(check(ours, Box::new(|| Some(HEADER.to_owned()))), Err(SOCKET_NOT_LISTENING));
        drop(listener);
        // A real SEQPACKET listener, found in the kernel's table without connecting.
        std::fs::remove_file(&socket).unwrap();
        let _seqpacket = seqpacket_listener(&socket);
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660)).unwrap();
        let table = unix_socket_table().unwrap();
        assert!(listening(&table, socket.to_str().unwrap()), "temporary listener not found in /proc/net/unix");
        assert_eq!(parse_service_evidence(&socket, &lease, me, ours, unix_socket_table), Ok(()));
    }

    fn seqpacket_listener(path: &Path) -> std::os::fd::OwnedFd {
        use nix::sys::socket::{bind, listen, socket, AddressFamily, Backlog, SockFlag, SockType, UnixAddr};
        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::SOCK_CLOEXEC, None).unwrap();
        bind(std::os::fd::AsRawFd::as_raw_fd(&fd), &UnixAddr::new(path).unwrap()).unwrap();
        listen(&fd, Backlog::new(4).unwrap()).unwrap();
        fd
    }

    use std::sync::atomic::AtomicUsize;

    /// A counting fake reader: `delay` per parse read, tracking how many
    /// reads run at once.
    struct FakeReader {
        calls: AtomicUsize, in_flight: AtomicUsize, max_in_flight: AtomicUsize,
        delay: Duration, parse: Readback, runtime: Readback,
    }
    impl FakeReader {
        fn new(delay: Duration, parse: Readback, runtime: Readback) -> Arc<Self> {
            Arc::new(Self { calls: AtomicUsize::new(0), in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0), delay, parse, runtime })
        }
        fn calls(&self) -> usize { self.calls.load(Ordering::Acquire) }
    }
    impl ServiceReader for FakeReader {
        fn parse(&self, _: Sha256Digest) -> Readback {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let now = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            self.max_in_flight.fetch_max(now, Ordering::AcqRel);
            std::thread::sleep(self.delay);
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
            self.parse
        }
        fn runtime(&self, _: Sha256Digest) -> Readback {
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.runtime
        }
    }
    fn leaked() -> &'static Observations { Box::leak(Box::new(Observations::new())) }

    /// R8n-1 review F2: the probes read memory only. A probe type holds only
    /// `&Observations` (no reader); reading it any number of times never
    /// calls the reader: only the observer's pass does. Nothing stored yet
    /// is `observation_pending`; a stored refusal keeps its own reason.
    #[test]
    fn r8n_probes_perform_no_io_and_only_the_observer_reads() {
        let generation = Sha256Digest([1; 32]);
        let observations = leaked();
        let reader = FakeReader::new(Duration::ZERO, Ok(None), Ok(Some(PtyRequirement::NotRequired)));
        for _ in 0..3 {
            assert!(matches!(parse_observation(observations, generation, Instant::now()),
                Err(ProbeError::Refused(OBSERVATION_PENDING))));
            assert!(matches!(runtime_observation(observations, generation, Instant::now()),
                Err(ProbeError::Refused(OBSERVATION_PENDING))));
        }
        assert_eq!(reader.calls(), 0, "a probe never reads");
        observe_once(reader.as_ref(), observations, generation);
        assert_eq!(reader.calls(), 2, "one pass: one parse and one runtime read");
        for _ in 0..5 {
            assert!(parse_observation(observations, generation, Instant::now()).is_ok());
            assert!(runtime_observation(observations, generation, Instant::now()).is_ok());
        }
        assert_eq!(reader.calls(), 2, "probes reuse the stored observation");
        // A failed read stores "not ready" with its fixed reason.
        let failing = FakeReader::new(Duration::ZERO, Err(SOCKET_NOT_LISTENING), Err(NO_MAIN_RUNTIME));
        observe_once(failing.as_ref(), observations, generation);
        assert!(matches!(parse_observation(observations, generation, Instant::now()),
            Err(ProbeError::Refused(SOCKET_NOT_LISTENING))));
        assert!(matches!(runtime_observation(observations, generation, Instant::now()),
            Err(ProbeError::Refused(NO_MAIN_RUNTIME))));
        // A runtime readback without a PTY requirement is never ready.
        let no_pty = FakeReader::new(Duration::ZERO, Ok(None), Ok(None));
        observe_once(no_pty.as_ref(), observations, generation);
        assert!(matches!(runtime_observation(observations, generation, Instant::now()),
            Err(ProbeError::Refused(PUBLICATION_UNAVAILABLE))));
        // The production probes are built over the process's observations.
        assert!(std::ptr::eq(ParseWorkerReadiness::default().observations, &OBSERVATIONS));
        assert!(std::ptr::eq(RuntimeAdapterReadiness::default().observations, &OBSERVATIONS));
    }

    /// Older than the 2 s window (measured from the pre-read timestamp), from
    /// the future, or of another generation: `observation_stale`.
    #[test]
    fn r8n_stale_observation_is_not_ready() {
        let generation = Sha256Digest([1; 32]);
        let observations = leaked();
        observe_once(FakeReader::new(Duration::ZERO, Ok(None), Ok(Some(PtyRequirement::NotRequired))).as_ref(),
            observations, generation);
        let observed_at = |slot: &Slot| slot.0.lock().unwrap().as_ref().map(|stored| stored.observed).unwrap();
        let (observed, runtime_observed) = (observed_at(&observations.parse), observed_at(&observations.runtime));
        assert!(parse_observation(observations, generation, observed + MAX_AGE).is_ok());
        assert!(runtime_observation(observations, generation, runtime_observed + MAX_AGE).is_ok());
        for now in [observed + MAX_AGE + Duration::from_millis(1), observed + Duration::from_secs(60)] {
            assert!(matches!(parse_observation(observations, generation, now), Err(ProbeError::Refused(OBSERVATION_STALE))));
        }
        assert!(matches!(runtime_observation(observations, generation, runtime_observed + Duration::from_secs(3)),
            Err(ProbeError::Refused(OBSERVATION_STALE))));
        // A stored timestamp after "now" is never fresh.
        if let Some(before) = observed.checked_sub(Duration::from_millis(1)) {
            assert!(matches!(parse_observation(observations, generation, before), Err(ProbeError::Refused(OBSERVATION_STALE))));
        }
        // Another generation's observation.
        assert!(matches!(parse_observation(observations, Sha256Digest([2; 32]), observed),
            Err(ProbeError::Refused(OBSERVATION_STALE))));
        // A cleared slot (a pass that panicked) is pending again.
        observations.clear();
        assert!(matches!(parse_observation(observations, generation, Instant::now()),
            Err(ProbeError::Refused(OBSERVATION_PENDING))));
    }

    /// Single-flight: a read slower than the period never overlaps a second
    /// pass; the next pass starts only after the slow one ended.
    #[tokio::test]
    async fn r8n_observer_is_single_flight_with_a_slow_read() {
        let generation = Sha256Digest([1; 32]);
        let observations = leaked();
        let reader = FakeReader::new(Duration::from_millis(1300), Ok(None), Ok(Some(PtyRequirement::NotRequired)));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let task = tokio::spawn(observe(reader.clone(), observations, generation, move || flag.load(Ordering::Acquire)));
        tokio::time::sleep(Duration::from_millis(2000)).await;
        // One 1.3 s pass done, the second one in progress (never two at once).
        assert_eq!(reader.max_in_flight.load(Ordering::Acquire), 1);
        assert!(reader.calls() <= 3, "at most one finished pass (2 reads) and one started parse read");
        stop.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(5), task).await.expect("observer stops").unwrap();
        assert_eq!(reader.max_in_flight.load(Ordering::Acquire), 1);
    }

    /// The observer stops at the shutdown gate: nothing is read once closed,
    /// and a gate that closes later ends the loop within one period.
    #[tokio::test]
    async fn r8n_observer_stops_on_shutdown() {
        let generation = Sha256Digest([1; 32]);
        let reader = FakeReader::new(Duration::ZERO, Ok(None), Ok(Some(PtyRequirement::NotRequired)));
        tokio::time::timeout(Duration::from_secs(1), observe(reader.clone(), leaked(), generation, || true))
            .await.expect("a closed gate ends the observer at once");
        assert_eq!(reader.calls(), 0);
        let observations = leaked();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let task = tokio::spawn(observe(reader.clone(), observations, generation, move || flag.load(Ordering::Acquire)));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(reader.calls(), 2, "the first pass ran at once");
        assert!(parse_observation(observations, generation, Instant::now()).is_ok());
        stop.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(3), task).await.expect("observer stops").unwrap();
        let after = reader.calls();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(reader.calls(), after, "no pass after shutdown");
    }
}
