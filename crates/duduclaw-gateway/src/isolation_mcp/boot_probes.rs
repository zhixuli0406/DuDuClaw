//! R8n-1: the ParseWorker and RuntimeAdapters integration probes of the
//! assembled boot owner, installed by `install_probes` next to the MCP
//! boundary's probe (inside the attempt's shutdown-gate permit).
//!
//! Both run on every readiness evaluation (every admission, every
//! `system.status` read and the published view's 5 s tick). Their producer
//! checks are in-memory; their service checks read local files only and are
//! cached for [`TTL`] (a success or a refusal), keyed by generation, with the
//! timestamp taken BEFORE the readback, so a cached success always expires
//! inside the composite's two-second observation window. No thread, task or
//! process is started, and nothing is sent to any service. Any error is a
//! refusal with one fixed reason (`published::PARSE_WORKER_REASONS` /
//! `RUNTIME_ADAPTER_REASONS`); an absent probe stays `PendingIntegration`.
use std::{path::Path, sync::Mutex, time::{Duration, Instant}};
use duduclaw_isolation::{execution::RuntimeKind, ids::Sha256Digest};
use duduclaw_spawnd::policy::PublishedRuntime;
use crate::isolation_readiness::{IntegrationProbe, ProbeError, PtyRequirement, ServiceObservation};
use super::OWNER;

/// How long one service readback is reused (half the observation window).
const TTL: Duration = Duration::from_secs(1);
/// `/proc/net/unix` is read up to this size; a larger table is a refusal.
const MAX_SOCKET_TABLE_BYTES: u64 = 8 * 1024 * 1024;

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

#[derive(Clone, Copy)]
struct Cached { generation: Sha256Digest, observed: Instant, result: Readback }

/// The last readback, reused while younger than [`TTL`] for the same generation.
#[derive(Default)]
struct TtlCache(Mutex<Option<Cached>>);
impl TtlCache {
    /// `(timestamp taken before the readback, its result)`.
    fn get_or(&self, generation: Sha256Digest, readback: impl FnOnce() -> Readback) -> (Instant, Readback) {
        let now = Instant::now();
        if let Ok(slot) = self.0.lock() {
            if let Some(cached) = slot.as_ref().filter(|cached| cached.generation == generation
                && now.checked_duration_since(cached.observed).is_some_and(|age| age < TTL)) {
                return (cached.observed, cached.result);
            }
        }
        let observed = Instant::now();
        let result = readback();
        // A poisoned slot only loses the cache, never the check.
        if let Ok(mut slot) = self.0.lock() { *slot = Some(Cached { generation, observed, result }); }
        (observed, result)
    }
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
/// containment.
#[derive(Default)]
pub(super) struct ParseWorkerReadiness { cache: TtlCache }
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
        let (observed, result) = self.cache.get_or(generation, || {
            parse_service_evidence(Path::new(duduclaw_parse_worker::policy::SOCKET_PATH),
                Path::new(duduclaw_parse_worker::retirement::LEASE_PATH), 0, &gateway_groups(), unix_socket_table)
                .map(|()| None)
        });
        result.map_err(ProbeError::Refused)?;
        Ok(ServiceObservation::checked(generation, observed))
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
#[derive(Default)]
pub(super) struct RuntimeAdapterReadiness { cache: TtlCache }
impl IntegrationProbe for RuntimeAdapterReadiness {
    fn verify_producers(&self) -> Result<(), ProbeError> {
        let owner = healthy_owner()?;
        helper_producers(|name| owner.execution_plans.offers(name), |name| owner.execution_plans.supports(name),
            |name| owner.fork_tools.supports(name)).map_err(ProbeError::Refused)
    }

    fn verify_service(&self, generation: Sha256Digest) -> Result<ServiceObservation, ProbeError> {
        let owner = OWNER.get().ok_or(ProbeError::Refused(ASSEMBLY_INCOMPLETE))?;
        if owner.generation != generation { return Err(ProbeError::Refused(GENERATION_MISMATCH)); }
        let (observed, result) = self.cache.get_or(generation, || {
            let publication = duduclaw_spawnd::installed::GatewayPublication::read(nix::unistd::getegid().as_raw())
                .map_err(|_| PUBLICATION_UNAVAILABLE)?;
            if publication.generation != generation { return Err(GENERATION_MISMATCH); }
            publication.policy().map_err(|_| PUBLICATION_UNAVAILABLE)?;
            let runtimes: Vec<PublishedRuntime> = publication.runtimes.iter().map(|record| record.to_runtime()).collect();
            main_runtime_evidence(&runtimes, |kind| owner.registry.registration(kind).is_ok()).map(Some)
        });
        let pty = result.map_err(ProbeError::Refused)?.ok_or(ProbeError::Refused(PUBLICATION_UNAVAILABLE))?;
        Ok(ServiceObservation::checked_runtime(generation, observed, pty))
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
const PARSE_REASONS: [&str; 7] = [ASSEMBLY_INCOMPLETE, LAUNCHER_NOT_INSTALLED, GENERATION_MISMATCH,
    SOCKET_UNAVAILABLE, SOCKET_UNSAFE, SOCKET_NOT_LISTENING, LEASE_UNAVAILABLE];
#[cfg(test)]
const RUNTIME_REASONS: [&str; 7] = [ASSEMBLY_INCOMPLETE, HELPER_PRODUCER_MISSING, FORK_READER_MISSING,
    GENERATION_MISMATCH, PUBLICATION_UNAVAILABLE, NO_MAIN_RUNTIME, MAIN_ADAPTER_UNAVAILABLE];

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

    /// The cache reuses one readback inside the TTL (with its original
    /// timestamp), never across generations, and caches refusals too.
    #[test]
    fn r8n_probe_readback_is_cached_briefly_per_generation() {
        let cache = TtlCache::default();
        let (one, two) = (Sha256Digest([1; 32]), Sha256Digest([2; 32]));
        let mut calls = 0;
        let (first, result) = cache.get_or(one, || { calls += 1; Ok(None) });
        assert_eq!(result, Ok(None));
        let (again, _) = cache.get_or(one, || { calls += 1; Ok(None) });
        assert_eq!((again, calls), (first, 1), "reused with its pre-readback timestamp");
        let (_, refused) = cache.get_or(two, || { calls += 1; Err(SOCKET_UNAVAILABLE) });
        assert_eq!((refused, calls), (Err(SOCKET_UNAVAILABLE), 2), "another generation reads again");
        let (_, cached) = cache.get_or(two, || { calls += 1; Ok(None) });
        assert_eq!((cached, calls), (Err(SOCKET_UNAVAILABLE), 2));
        // Expired: read again.
        *cache.0.lock().unwrap() = Some(Cached { generation: one, observed: Instant::now() - TTL, result: Ok(None) });
        let (_, _) = cache.get_or(one, || { calls += 1; Ok(None) });
        assert_eq!(calls, 3);
    }
}
