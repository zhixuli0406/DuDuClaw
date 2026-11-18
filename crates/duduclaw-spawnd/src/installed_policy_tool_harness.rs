//! R8r-4 launch harness of `accept` (root, disposable Linux host only;
//! feature `policy-tool`, never a release build).
//!
//! The launch follows `Host::start` (host.rs) step by step with the
//! production pieces: `RootPolicy::derive` on a publication of the generated
//! record, `LoopStorage` (provision, the R8r-2 provider write, usage check,
//! attach), a `RevocablePair` for descriptor 3 (and one for egress),
//! `BootstrapPlan::from_runtime`, the publication's `prepare_mcp_projection` /
//! `anchors` / `record_mcp_projection` / `native_profile` (here
//! [`AcceptancePublication`]: every check of `InstalledPublication` except
//! requiring an acceptance record, which is what this run produces),
//! `bootstrap_mounts::prepare`, `plan.seal`, `NativeTrampoline::prepare`,
//! `fds::validate_bootstrap`, `UnitPolicy::new`, PID 1's transient unit
//! (`Systemd::start_acceptance` = `start` with the harness's trampoline copy
//! and without the spawnd dependency) and `NativeTrampoline::bind_unit` /
//! `poll`. Not executed (no installation exists): the admission core
//! (readiness report, SSH evidence, the run journal and UID monitor, the
//! egress service registration), the R7a trampoline self-test readiness, the
//! helper table, the work-broker root and the gateway's `McpRunReceipt`.
//!
//! Network: the run has its own network namespace with loopback only (the
//! trampoline's `enter_namespaces` / `bring_loopback_up`); with `network:
//! egress` the only way out is the forwarder's egress socket, whose other end
//! the harness holds and never reads or forwards (no egress service exists on
//! this host). The provider endpoint `https://acceptance.invalid/v1` is never
//! reachable.
use super::*;
use std::{ffi::CString, io::{self, Read as _, Write as _}, os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
    path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use crate::bootstrap::{BootstrapPlan, Trampoline as _};
use crate::host::Publication as _;
use crate::storage::{Storage as _, AttachedStorage, Attachment, LoopStorage, ProvidersChange, ProvisionRequest};

const PROVIDER_NAME: &str = "acceptance";
const PROVIDER_URL: &str = "https://acceptance.invalid/v1";
/// duduclaw-api-worker `PROTOCOL` (no dependency on that crate).
const WORKER_PROTOCOL: &str = "openai-compat-v1";
const NOT_MAIN_THREAD: &str = "accept: not supported on this host: the harness must run on the policy tool's main thread";
const INTERRUPTED: &str = "accept: interrupted by a signal (no record written)";

/// What the run produced; [`super::decide`] judges it.
pub(super) struct Outcome {
    pub frames: Vec<Frame>,
    pub observations: Observations,
    pub trampoline_sha256: Sha256Digest,
    pub kernel: String,
}

fn os_error(what: &str) -> impl Fn(io::Error) -> String + '_ { move |error| format!("accept: {what}: {error}") }
fn refused(what: &str) -> impl Fn(ErrorCode) -> String + '_ { move |code| format!("accept: {what} refused ({code:?})") }

/// sha256 of everything an opened file yields.
fn digest_file(mut file: File) -> io::Result<Sha256Digest> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
    }
    Ok(Sha256Digest(hasher.finalize().into()))
}

/// sha256 of the running policy tool (`/proc/self/exe`, opened once).
pub(super) fn self_digest() -> Checked<Sha256Digest> {
    File::open("/proc/self/exe").and_then(digest_file).map_err(|e| format!("accept: /proc/self/exe: {e}"))
}

fn random16() -> Checked<[u8; 16]> { crate::server::random_id().map_err(|e| format!("accept: getrandom: {e}")) }
fn random32() -> Checked<[u8; 32]> {
    let mut value = [0u8; 32];
    value[..16].copy_from_slice(&random16()?);
    value[16..].copy_from_slice(&random16()?);
    Ok(value)
}

// ── signals ─────────────────────────────────────────────────────────────

/// SIGINT/SIGTERM/SIGHUP/SIGQUIT are blocked for the whole run (before any
/// thread exists) and polled, so an interruption always runs the tear-down.
struct Signals { set: libc::sigset_t }
const SIGNALS: [libc::c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];
impl Signals {
    fn block() -> Checked<Self> {
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::sigemptyset(&mut set); }
        for signal in SIGNALS { unsafe { libc::sigaddset(&mut set, signal); } }
        if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) } != 0 {
            return Err("accept: cannot block termination signals".into());
        }
        Ok(Self { set })
    }
    fn pending(&self) -> bool {
        let mut pending: libc::sigset_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigpending(&mut pending) } != 0 { return true; }
        SIGNALS.iter().any(|signal| unsafe { libc::sigismember(&self.set, *signal) == 1 && libc::sigismember(&pending, *signal) == 1 })
    }
}

// ── processes ───────────────────────────────────────────────────────────

/// `(ppid, start ticks)` of `/proc/<pid>/stat`.
fn proc_stat(pid: u32) -> io::Result<(u32, u64)> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "proc stat");
    let fields: Vec<&str> = text.rsplit_once(')').ok_or_else(invalid)?.1.split_whitespace().collect();
    let ppid = fields.get(1).and_then(|value| value.parse().ok()).ok_or_else(invalid)?;
    let start = fields.get(19).and_then(|value| value.parse().ok()).ok_or_else(invalid)?;
    Ok((ppid, start))
}

/// A process bound by a pidfd: the pidfd was opened between two reads of the
/// same start time (and parent, when given), so it names the inspected process.
struct Pinned { pid: u32, start: u64, fd: OwnedFd }
fn pin(pid: u32, parent: Option<u32>) -> io::Result<Option<Pinned>> {
    use rustix::process::{pidfd_open, Pid, PidfdFlags};
    let Ok((ppid, start)) = proc_stat(pid) else { return Ok(None) };
    let raw = i32::try_from(pid).ok().and_then(Pid::from_raw).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let fd = match pidfd_open(raw, PidfdFlags::empty()) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::SRCH => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    match proc_stat(pid) {
        Ok((again, same)) if same == start && again == ppid && parent.is_none_or(|parent| parent == ppid) =>
            Ok(Some(Pinned { pid, start, fd })),
        _ => Ok(None),
    }
}
fn readable(fd: RawFd, milliseconds: i32) -> bool {
    let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut poll, 1, milliseconds) == 1 }
}
/// The pinned process is still the same live process.
fn alive(process: &Pinned) -> bool {
    !readable(process.fd.as_raw_fd(), 0) && proc_stat(process.pid).is_ok_and(|(_, start)| start == process.start)
}
fn children(pid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else { return found };
    for task in tasks.flatten() {
        if let Ok(text) = fs::read_to_string(task.path().join("children")) {
            found.extend(text.split_whitespace().filter_map(|value| value.parse::<u32>().ok()));
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}
fn exe_digest(pid: u32) -> io::Result<Sha256Digest> { File::open(format!("/proc/{pid}/exe")).and_then(digest_file) }
/// `(inode, mount id)` of a descriptor's target when it is a socket.
fn fd_socket(pid: u32, fd: i32) -> io::Result<Option<SocketId>> {
    let path = CString::new(format!("/proc/{pid}/fd/{fd}")).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    let mask = libc::STATX_TYPE | libc::STATX_INO | libc::STATX_MNT_ID;
    duduclaw_run::linux::syscalls::statx(libc::AT_FDCWD, &path, 0, mask, &mut stat)?;
    if stat.stx_mask & mask != mask { return Err(io::Error::from(io::ErrorKind::InvalidData)); }
    if u32::from(stat.stx_mode) & libc::S_IFMT != libc::S_IFSOCK { return Ok(None); }
    Ok(Some(SocketId { inode: stat.stx_ino, mount_id: stat.stx_mnt_id }))
}
/// Every descriptor of `pid` naming `socket`, and what its descriptor 3 is.
fn socket_fds(pid: u32, socket: SocketId) -> io::Result<(Vec<i32>, Fd3)> {
    let mut found = Vec::new();
    let mut fd3 = Fd3::Absent;
    for (count, entry) in fs::read_dir(format!("/proc/{pid}/fd"))?.enumerate() {
        if count >= 4096 { return Err(io::Error::from(io::ErrorKind::InvalidData)); }
        let Some(fd) = entry?.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else { continue };
        match fd_socket(pid, fd) {
            Ok(Some(id)) if id == socket => { found.push(fd); if fd == 3 { fd3 = Fd3::ExpectedSocket; } }
            Ok(_) => { if fd == 3 { fd3 = Fd3::Other; } }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    found.sort_unstable();
    Ok((found, fd3))
}
fn cgroup_member(pid: u32, unit_cgroup: &str) -> bool {
    fs::read_to_string(format!("/proc/{pid}/cgroup")).is_ok_and(|text|
        crate::accounts::unified_cgroup(&text).is_ok_and(|actual| crate::accounts::cgroup_member(actual, unit_cgroup)))
}
fn process_facts(process: &Pinned, depth: u32, unit_cgroup: &str, socket: SocketId) -> io::Result<(ProcessFacts, Fd3)> {
    let uids = crate::accounts::process_uids(&fs::read_to_string(format!("/proc/{}/status", process.pid))?)?;
    let member = cgroup_member(process.pid, unit_cgroup);
    let exe = exe_digest(process.pid)?;
    let (fds, fd3) = socket_fds(process.pid, socket)?;
    let alive = alive(process);
    Ok((ProcessFacts { pid: process.pid, depth, uids, exe_sha256: exe, cgroup_member: member, socket_fds: fds,
        alive_through_observation: alive }, fd3))
}

/// The process tree below the unit's main process (pinned at start), each
/// node pinned by a pidfd with its parent link re-checked: the runtime is the
/// one node with the runtime digest, the bridge the one node below it with
/// the bridge digest. Observed while the `initialize` reply is withheld, so
/// both are alive and blocked on the harness.
struct Tree { runtime: Option<ProcessFacts>, fd3: Fd3, bridge: Option<ProcessFacts> }
fn observe_tree(launcher: &Pinned, unit_cgroup: &str, socket: SocketId, runtime_digest: Sha256Digest,
    bridge_digest: Sha256Digest) -> io::Result<Tree> {
    let mut nodes: Vec<(Pinned, u32, Option<usize>)> = Vec::new();
    let root = pin(launcher.pid, None)?.filter(|again| again.start == launcher.start)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "launcher"))?;
    nodes.push((root, 0, None));
    let mut next = 0;
    while next < nodes.len() && nodes.len() < 256 {
        let (pid, depth) = (nodes[next].0.pid, nodes[next].1);
        for child in children(pid) {
            if nodes.len() >= 256 || depth >= 8 { break; }
            if let Some(pinned) = pin(child, Some(pid))? { nodes.push((pinned, depth + 1, Some(next))); }
        }
        next += 1;
    }
    let digests: Vec<Option<Sha256Digest>> = nodes.iter().map(|(node, _, _)| exe_digest(node.pid).ok()).collect();
    let runtimes: Vec<usize> = (0..nodes.len()).filter(|index| digests[*index] == Some(runtime_digest)).collect();
    let mut tree = Tree { runtime: None, fd3: Fd3::Other, bridge: None };
    let [runtime] = runtimes.as_slice() else { return Ok(tree) };
    let below = |mut index: usize| -> Option<u32> {
        let mut distance = 0;
        while let Some(parent) = nodes[index].2 {
            distance += 1;
            if parent == *runtime { return Some(distance); }
            index = parent;
        }
        None
    };
    let bridges: Vec<(usize, u32)> = (0..nodes.len()).filter(|index| digests[*index] == Some(bridge_digest))
        .filter_map(|index| below(index).map(|distance| (index, distance))).collect();
    let (facts, fd3) = process_facts(&nodes[*runtime].0, nodes[*runtime].1, unit_cgroup, socket)?;
    tree.runtime = Some(facts);
    tree.fd3 = fd3;
    if let [(bridge, distance)] = bridges.as_slice() {
        tree.bridge = Some(process_facts(&nodes[*bridge].0, *distance, unit_cgroup, socket)?.0);
    }
    Ok(tree)
}

/// Every process ever seen in the run cgroup, by `(pid, start)`, and those
/// with the bridge digest (scanned on every turn of the conversation loop).
#[derive(Default)]
struct Census { seen: BTreeMap<(u32, u64), Sha256Digest>, bridges: Vec<(u32, u64)> }
fn cgroup_pids(directory: &Path, depth: u32, out: &mut Vec<u32>) {
    if let Ok(text) = fs::read_to_string(directory.join("cgroup.procs")) {
        out.extend(text.split_whitespace().filter_map(|value| value.parse::<u32>().ok()));
    }
    if depth >= 4 { return; }
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) { cgroup_pids(&entry.path(), depth + 1, out); }
        }
    }
}
impl Census {
    fn scan(&mut self, cgroup: &Path, bridge: Sha256Digest) {
        let mut pids = Vec::new();
        cgroup_pids(cgroup, 0, &mut pids);
        for pid in pids {
            let Ok((_, start)) = proc_stat(pid) else { continue };
            if self.seen.contains_key(&(pid, start)) || self.seen.len() >= 1024 { continue; }
            let Ok(digest) = exe_digest(pid) else { continue };
            if !proc_stat(pid).is_ok_and(|(_, again)| again == start) { continue; }
            self.seen.insert((pid, start), digest);
            if digest == bridge { self.bridges.push((pid, start)); }
        }
    }
}

// ── files ───────────────────────────────────────────────────────────────

fn make_dir(path: &Path, mode: u32) -> Checked<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new().mode(mode).create(path).map_err(os_error("harness directory"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(os_error("harness directory"))?;
    std::os::unix::fs::chown(path, Some(0), Some(0)).map_err(os_error("harness directory"))
}

/// A root-owned 0755 copy of a regular file; its sha256 (of the bytes copied)
/// must equal `expected` when given.
fn install_copy(source: &Path, destination: &Path, expected: Option<Sha256Digest>) -> Checked<Sha256Digest> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if !fs::symlink_metadata(source).is_ok_and(|metadata| metadata.is_file()) {
        return Err(format!("accept: {} is not a regular file", source.display()));
    }
    let mut input = File::open(source).map_err(os_error("copy source"))?;
    let mut output = fs::OpenOptions::new().write(true).create_new(true).mode(0o700)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(destination).map_err(os_error("copy destination"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let count = input.read(&mut buffer).map_err(os_error("copy"))?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count]).map_err(os_error("copy"))?;
    }
    output.sync_all().map_err(os_error("copy"))?;
    output.set_permissions(fs::Permissions::from_mode(0o755)).map_err(os_error("copy"))?;
    let digest = Sha256Digest(hasher.finalize().into());
    if expected.is_some_and(|expected| expected != digest) {
        return Err(format!("accept: {} changed: its sha256 is not the generated record's", source.display()));
    }
    Ok(digest)
}

fn c_name(name: &str) -> io::Result<CString> { CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput)) }
fn open_dir_at(parent: BorrowedFd<'_>, name: &str) -> io::Result<OwnedFd> {
    let name = c_name(name)?;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
fn employee_dir(parent: BorrowedFd<'_>, name: &str, uid: u32) -> io::Result<OwnedFd> {
    let c = c_name(name)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) } != 0 { return Err(io::Error::last_os_error()); }
    let fd = open_dir_at(parent, name)?;
    if unsafe { libc::fchown(fd.as_raw_fd(), uid, uid) } != 0 || unsafe { libc::fchmod(fd.as_raw_fd(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}
fn employee_file(parent: BorrowedFd<'_>, name: &str, bytes: &[u8], mode: u32, uid: u32) -> io::Result<()> {
    let c = c_name(name)?;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), c.as_ptr(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600 as libc::c_uint) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.write_all(bytes)?;
    if unsafe { libc::fchown(file.as_raw_fd(), uid, uid) } != 0 || unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    file.sync_all()
}

/// The decoys of the exclusive-config fact, planted in the employee home
/// (which is the run's `/home/employee`): `.mcp.json` and
/// `.config/duduclaw/mcp.json`, each naming a marker script in the home
/// (`/home/employee/.decoy/marker.sh <name>`, which would create
/// `.decoy/marker-<name>`; note the home is mounted noexec in the run, so
/// the stronger signal is the second server) and a second copy of the bridge
/// (`/runtime/duduclaw-mcp-bridge decoy-<name>`, which would show up as a
/// second bridge process and a second `initialize`). An environment-selected
/// configuration is impossible: the plan's environment is fixed and the
/// worker clears the bridge's environment. Returns the decoy count.
fn plant_decoys(home: BorrowedFd<'_>, uid: u32) -> io::Result<u32> {
    let decoy = employee_dir(home, ".decoy", uid)?;
    employee_file(decoy.as_fd(), "marker.sh", b"#!/bin/sh\n: > \"/home/employee/.decoy/marker-$1\"\n", 0o755, uid)?;
    let config = |name: &str| format!("{{\"mcpServers\":{{\"decoy-marker\":{{\"command\":\"/home/employee/.decoy/marker.sh\",\
        \"args\":[\"{name}\"]}},\"decoy-bridge\":{{\"command\":\"{BRIDGE_EXECUTABLE}\",\"args\":[\"decoy-{name}\"]}}}}}}\n");
    employee_file(home, ".mcp.json", config("home-mcp-json").as_bytes(), 0o644, uid)?;
    let dot_config = employee_dir(home, ".config", uid)?;
    let duduclaw = employee_dir(dot_config.as_fd(), "duduclaw", uid)?;
    employee_file(duduclaw.as_fd(), "mcp.json", config("config-duduclaw-mcp-json").as_bytes(), 0o644, uid)?;
    Ok(2)
}
/// Entries of `.decoy` other than the script: a marker means a decoy ran.
fn decoy_markers(home: BorrowedFd<'_>) -> io::Result<u32> {
    let decoy = open_dir_at(home, ".decoy")?;
    let mut count = 0;
    for entry in fs::read_dir(format!("/proc/self/fd/{}", decoy.as_raw_fd()))? {
        if entry?.file_name() != "marker.sh" { count += 1; }
    }
    Ok(count)
}

/// The R8r-2 provider document: one provider `acceptance` with a random
/// 32-hex key generated here (never printed, never on descriptor 3).
fn providers_document() -> Checked<Vec<u8>> {
    let mut key = hex(&random16()?).into_bytes();
    let mut document = Vec::with_capacity(160);
    document.extend_from_slice(br#"{"providers":[{"name":""#);
    document.extend_from_slice(PROVIDER_NAME.as_bytes());
    document.extend_from_slice(br#"","api_key":""#);
    document.extend_from_slice(&key);
    document.extend_from_slice(br#"","base_url":""#);
    document.extend_from_slice(PROVIDER_URL.as_bytes());
    document.extend_from_slice(br#""}]}"#);
    key.fill(0);
    Ok(document)
}

/// The worker's request (duduclaw-api-worker `Request`): stdin and the sealed
/// prompt carry the same bytes. The record has no model (open_ai_compat runs
/// carry none), so a fixed model name is used; it never reaches a provider.
fn worker_request() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"protocol": WORKER_PROTOCOL, "provider": PROVIDER_NAME, "model": "acceptance-model",
        "messages": [{"role": "user", "content": "MCP acceptance run: reply with one word."}],
        "max_tokens": 16, "timeout_secs": 10})).unwrap_or_default()
}

fn sealed_memfd(bytes: &[u8]) -> io::Result<OwnedFd> {
    let fd = duduclaw_run::linux::syscalls::memfd_create(c"duduclaw-acceptance-prompt", libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)?;
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(file.into())
}

fn pipe() -> Checked<(OwnedFd, OwnedFd)> {
    nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(|e| format!("accept: pipe: {e}"))
}
fn set_nonblocking(fd: RawFd) -> Checked<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err("accept: cannot make a descriptor non-blocking".into());
    }
    Ok(())
}
/// `Some(0)` at end of file, `None` when nothing is ready.
fn read_some(fd: RawFd, buffer: &mut [u8]) -> io::Result<Option<usize>> {
    let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if count >= 0 { return Ok(Some(count as usize)); }
    let error = io::Error::last_os_error();
    match error.raw_os_error() { Some(libc::EAGAIN) | Some(libc::EINTR) => Ok(None), _ => Err(error) }
}
fn send_all(fd: RawFd, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let mut sent = 0;
    while sent < bytes.len() {
        let count = unsafe { libc::send(fd, bytes[sent..].as_ptr().cast(), bytes.len() - sent, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
        if count > 0 { sent += count as usize; continue; }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EINTR)) { return Err(error); }
        if Instant::now() >= deadline { return Err(io::ErrorKind::TimedOut.into()); }
        let mut poll = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        unsafe { libc::poll(&mut poll, 1, 20); }
    }
    Ok(())
}

fn kernel_release() -> String {
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 { return "unknown".into(); }
    unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) }.to_string_lossy().into_owned()
}

/// No passwd or group entry with this number and no process with it.
fn number_free(id: u32) -> bool {
    let mut buffer = vec![0 as libc::c_char; 1 << 16];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut user: *mut libc::passwd = std::ptr::null_mut();
    if unsafe { libc::getpwuid_r(id, &mut passwd, buffer.as_mut_ptr(), buffer.len(), &mut user) } != 0 || !user.is_null() { return false; }
    let mut group_entry: libc::group = unsafe { std::mem::zeroed() };
    let mut group: *mut libc::group = std::ptr::null_mut();
    if unsafe { libc::getgrgid_r(id, &mut group_entry, buffer.as_mut_ptr(), buffer.len(), &mut group) } != 0 || !group.is_null() { return false; }
    crate::accounts::unexpected_processes(id, None).is_ok_and(|found| found.is_empty())
}
/// A temporary employee uid (= gid) in 70000..=99999 that no account, group
/// or process uses. It is never written to the host account databases: the
/// run's identity is the trampoline's own `/etc` projection, so nothing about
/// the account outlives the run except what the tear-down verifies is gone.
fn temporary_employee_uid() -> Checked<u32> {
    for _ in 0..64 {
        let bytes = random16()?;
        let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let uid = duduclaw_isolation::ids::EMPLOYEE_UID_MIN + value % (duduclaw_isolation::ids::EMPLOYEE_UID_MAX - duduclaw_isolation::ids::EMPLOYEE_UID_MIN + 1);
        if number_free(uid) { return Ok(uid); }
    }
    Err("accept: not supported on this host: no free employee uid in 70000..=99999".into())
}
/// Supervisor and gateway identities below 1000 (`BootstrapPlan::validate`,
/// `TrustedRun::validate`) that no account, group or process uses.
fn temporary_service_uids() -> Checked<(u32, u32)> {
    let mut free = (900..=999u32).rev().filter(|id| number_free(*id));
    match (free.next(), free.next()) {
        (Some(supervisor), Some(gateway)) => Ok((supervisor, gateway)),
        _ => Err("accept: not supported on this host: no two free system ids in 900..=999".into()),
    }
}

fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if check() { return true; }
        if Instant::now() >= deadline { return false; }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn cgroup_empty(cgroup: &Path) -> bool {
    match fs::read_to_string(cgroup.join("cgroup.events")) {
        Ok(events) => crate::systemd::cgroup_empty(&events).unwrap_or(false),
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}
/// Any mount point at or below `root` in this (private) namespace.
fn mounts_below(root: &Path) -> bool {
    let Ok(table) = fs::read_to_string("/proc/self/mountinfo") else { return true };
    let prefix = root.to_string_lossy().into_owned();
    table.lines().any(|line| line.split_whitespace().nth(4).is_some_and(|point|
        point == prefix || point.starts_with(&format!("{prefix}/"))))
}
fn anchors_of(root: &Path) -> io::Result<crate::bootstrap_mounts::RootPublicationAnchors> {
    crate::bootstrap_mounts::RootPublicationAnchors::new(root.join("runtime"), root.join("etc-state"), root.join("projections"))
}

// ── the publication ─────────────────────────────────────────────────────

/// NOT a production publication: the harness's root authority for this one
/// disposable run. Every check `InstalledPublication` makes, on the generated
/// policy and the artifacts' manifest, except that an MCP run does not need an
/// existing `mcp_acceptance` (it is what this run produces). It writes no
/// gateway publication and no `McpRunReceipt` (there is no gateway).
struct AcceptancePublication<'a> {
    policy: &'a SignedPolicy,
    manifest: &'a ReleaseManifest,
    arch: Arch,
    employee: EmployeeId,
    profile: ProfileId,
    root: PathBuf,
    supervisor: (u32, u32),
    gateway_uid: u32,
    projection_mount_id: Option<u64>,
}
impl AcceptancePublication<'_> {
    fn projection(&self, runtime: &crate::policy::PublishedRuntime) -> Result<duduclaw_mcp_boundary::config::RuntimeProjection> {
        let bridge = self.manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge).ok_or(ErrorCode::RuntimeDenied)?;
        let runtime_digest = match &runtime.executable {
            crate::policy::Executable::Elf { digest, .. } => *digest,
            crate::policy::Executable::Script { script_digest, .. } => *script_digest,
        };
        os(duduclaw_mcp_boundary::config::runtime_projection(duduclaw_mcp_boundary::config::RuntimeRegistration {
            runtime: runtime.runtime.clone(), version: runtime.pinned_version.as_str().into(), runtime_digest,
            bridge_digest: bridge.sha256, bridge_executable: BRIDGE_EXECUTABLE.into() }))
    }
}
impl crate::host::Publication for AcceptancePublication<'_> {
    fn load(&mut self) -> Result<RootPolicy> {
        let report = GatewayPublication { version: 1, generation: Sha256Digest([1; 32]), boot_id: String::new(),
            checked_boottime_ms: 0, valid_for_ms: 1000, runtimes: self.policy.runtimes.clone(),
            entitlements: vec![EntitlementRecord { employee: self.employee, profiles: vec![self.profile], max_turns: None }] };
        report.policy()?.with_launchers(self.arch, launchers(self.policy))
    }
    fn snapshot_authority(&mut self, gateway_uid: u32) -> Result<crate::fds::SnapshotAuthority> {
        if gateway_uid != self.gateway_uid || gateway_uid == 0 { return Err(ErrorCode::IsolationUnavailable); }
        let path = self.root.join("snapshots");
        os(crate::registry::root_path(&path, true))?;
        let file = os(File::open(path))?;
        Ok(crate::fds::SnapshotAuthority { gateway_uid, mount_id: os(crate::fds::mount_id(file.as_fd()))? })
    }
    fn supervisor_identity(&mut self) -> Result<(u32, u32)> { Ok(self.supervisor) }
    fn anchors(&mut self) -> Result<crate::bootstrap_mounts::RootPublicationAnchors> { os(anchors_of(&self.root)) }
    fn prepare_mcp_projection(&mut self, plan: &BootstrapPlan, runtime: &crate::policy::PublishedRuntime) -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        checked_runtime(&self.policy.runtimes, runtime)?;
        check_plan_runtime(plan, runtime)?;
        if !runtime.mcp { return Err(ErrorCode::RuntimeDenied); }
        let projection = self.projection(runtime)?;
        let anchor = self.root.join("projections");
        os(crate::registry::root_path(&anchor, true))?;
        let directory = anchor.join(hex(&plan.employee.0));
        os(fs::DirBuilder::new().mode(0o755).create(&directory))?;
        os(fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)))?;
        os(crate::registry::root_path(&directory, true))?;
        for file in &projection.files {
            let name = os(duduclaw_mcp_boundary::config::projection_name(file))?;
            let mut output = os(fs::OpenOptions::new().write(true).create_new(true).mode(0o444)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(directory.join(name)))?;
            os(output.write_all(file.contents.as_bytes()))?;
            os(output.set_permissions(fs::Permissions::from_mode(0o444)))?;
            os(output.sync_all())?;
        }
        os(File::open(&directory).and_then(|f| f.sync_all()))
    }
    fn record_mcp_projection(&mut self, plan: &BootstrapPlan, runtime: &crate::policy::PublishedRuntime,
        mounts: &crate::bootstrap_mounts::PreparedMounts) -> Result<()> {
        checked_runtime(&self.policy.runtimes, runtime)?;
        check_plan_runtime(plan, runtime)?;
        if !runtime.mcp { return Err(ErrorCode::RuntimeDenied); }
        let projection = self.projection(runtime)?;
        let arguments: Vec<_> = runtime.fixed_args.iter().map(|arg| arg.as_str()).collect();
        let expected: Vec<_> = projection.arguments.iter().map(String::as_str).collect();
        if !arguments.ends_with(&expected) { return Err(ErrorCode::RuntimeDenied); }
        for arg in arguments.iter().take(arguments.len() - expected.len()) {
            if matches!(*arg, "--mcp-config" | "--strict-mcp-config") { return Err(ErrorCode::RuntimeDenied); }
        }
        let mount_id = os(crate::bootstrap_mounts::verify_mcp_projection(mounts, &projection))?;
        os(crate::bootstrap_mounts::open_verified(mounts.runtime.as_fd(), "duduclaw-mcp-bridge",
            projection.registration.bridge_digest, true, Instant::now() + Duration::from_secs(15)))?;
        self.projection_mount_id = Some(mount_id);
        Ok(())
    }
    fn native_profile(&mut self, runtime: &crate::policy::PublishedRuntime) -> Result<crate::native_trampoline::NativeProfile> {
        checked_runtime(&self.policy.runtimes, runtime)?;
        let snapshot = self.snapshot_authority(self.gateway_uid)?;
        let (policy, manifest) = (self.policy, self.manifest);
        let native = policy.native.iter().find(|n| n.profile == runtime.profile).ok_or(ErrorCode::RuntimeDenied)?;
        let forwarder = native.forwarder.as_ref().map(|f| -> Result<_> {
            if !forwarder_artifact_matches(manifest, f) { return Err(ErrorCode::RuntimeDenied); }
            let argv = f.argv.iter().map(|arg| os(CString::new(arg.as_str()))).collect::<Result<Vec<_>>>()?;
            Ok(crate::native_trampoline::ForwarderProfile { entry: f.entry.clone(), executable_digest: f.executable_digest,
                argv, seccomp: f.seccomp.clone(), seccomp_digest: f.seccomp_digest })
        }).transpose()?;
        // No helper table: `prepare` refuses runtimes with a launcher record.
        Ok(crate::native_trampoline::NativeProfile { runtime: native.runtime.clone(), runtime_digest: native.runtime_digest,
            forwarder, snapshot, tmpfs_bytes: native.tmpfs_bytes, tmpfs_inodes: native.tmpfs_inodes,
            proc_subset_pid: native.proc_subset_pid, launcher: None })
    }
}

// ── tear-down ───────────────────────────────────────────────────────────

/// Everything the harness created, released in reverse order on every path
/// (success, failure, interruption; a panic runs it from `Drop`), then
/// verified gone.
#[derive(Default)]
struct Teardown {
    finished: bool,
    systemd: Option<crate::systemd::linux::Systemd>,
    run: Option<RunHandle>,
    slice: Option<String>,
    cgroup: Option<PathBuf>,
    cancelled: Option<Arc<AtomicBool>>,
    trampoline: Option<crate::native_trampoline::NativeTrampoline>,
    descriptors: Vec<OwnedFd>,
    attached: Option<AttachedStorage>,
    attachment: Option<Attachment>,
    storage: Option<LoopStorage>,
    provisioned: Option<(EmployeeId, duduclaw_isolation::ids::RequestId, Sha256Digest)>,
    uid: Option<u32>,
    root: Option<PathBuf>,
}
impl Teardown {
    fn finish(&mut self) -> Checked<()> {
        if self.finished { return Ok(()); }
        self.finished = true;
        let mut left: Vec<&'static str> = Vec::new();
        if let Some(flag) = &self.cancelled { flag.store(true, Ordering::Release); }
        if let (Some(systemd), Some(run)) = (&self.systemd, self.run) {
            if systemd.cleanup_test_run(run.0).is_err() { left.push("the systemd run unit"); }
        }
        if let (Some(trampoline), Some(run)) = (self.trampoline.as_mut(), self.run) { let _ = trampoline.close(run); }
        self.trampoline = None;
        self.descriptors.clear();
        if let Some(cgroup) = &self.cgroup {
            if !wait_until(Duration::from_secs(10), || !cgroup.exists()) { left.push("the run cgroup"); }
        }
        if let (Some(systemd), Some(slice)) = (&self.systemd, &self.slice) {
            let _ = systemd.acceptance_stop(slice);
            if !wait_until(Duration::from_secs(10), || systemd.acceptance_loaded(slice).is_ok_and(|loaded| !loaded)) {
                left.push("the employee slice unit");
            }
        }
        if let Some(uid) = self.uid {
            let none = || crate::accounts::unexpected_processes(uid, None).is_ok_and(|found| found.is_empty());
            if !wait_until(Duration::from_secs(5), none) {
                if let Ok(found) = crate::accounts::unexpected_processes(uid, None) {
                    for process in &found { let _ = crate::accounts::terminate_intruder(process, uid, None); }
                }
                if !wait_until(Duration::from_secs(5), none) { left.push("processes of the temporary employee uid"); }
            }
        }
        self.attached = None;
        if let Some(storage) = self.storage.as_mut() {
            if let Some(attachment) = &self.attachment {
                if storage.detach(attachment).is_err() { left.push("the attached employee image"); }
            }
            if let Some((employee, operation, digest)) = self.provisioned {
                if storage.destroy(employee, operation, digest).is_err()
                    || storage.traces().map_or(true, |traces| traces.contains(&employee)) {
                    left.push("the employee image");
                }
            }
        }
        self.storage = None;
        if let (Some(root), Some(run)) = (&self.root, self.run) {
            match anchors_of(root) {
                Ok(anchors) => if crate::bootstrap_mounts::cleanup_projection(&anchors, run).is_err() { left.push("the /etc projection"); },
                Err(_) => left.push("the harness directory"),
            }
        }
        if let Some(root) = &self.root {
            // Never delete through a mount (remove_dir_all would descend into it).
            if mounts_below(root) { left.push("a mount below the harness directory"); }
            else if fs::remove_dir_all(root).is_err() || fs::symlink_metadata(root).is_ok() { left.push("the harness directory"); }
        }
        // Verification of what must no longer exist.
        if let (Some(systemd), Some(run)) = (&self.systemd, self.run) {
            if systemd.list_runs().map_or(true, |runs| runs.contains(&run.0)) { left.push("the systemd run unit"); }
        }
        if let (Some(systemd), Some(slice)) = (&self.systemd, &self.slice) {
            if !systemd.acceptance_loaded(slice).is_ok_and(|loaded| !loaded) { left.push("the employee slice unit"); }
        }
        if let Some(cgroup) = &self.cgroup { if cgroup.exists() { left.push("the run cgroup"); } }
        if let Some(uid) = self.uid {
            if !crate::accounts::unexpected_processes(uid, None).is_ok_and(|found| found.is_empty()) {
                left.push("processes of the temporary employee uid");
            }
        }
        if let Some(root) = &self.root { if fs::symlink_metadata(root).is_ok() { left.push("the harness directory"); } }
        self.systemd = None;
        left.sort_unstable();
        left.dedup();
        if left.is_empty() { return Ok(()); }
        Err(format!("accept: tear-down left {} behind; inspect this host before any other use (no record written)", left.join(", ")))
    }
}
impl Drop for Teardown {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(message) = self.finish() { eprintln!("duduclaw-isolation-policy: {message}"); }
        }
    }
}

// ── the run ─────────────────────────────────────────────────────────────

/// Launch, converse, observe, tear down. `Err` for anything that prevented
/// observing (with tear-down already verified); `Ok` carries the facts.
pub(super) fn run(prepared: &Prepared, trampoline: &Path) -> Checked<Outcome> {
    // The gates once more against the real process (tests inject facts).
    if unsafe { libc::geteuid() } != 0 || unsafe { libc::getegid() } != 0 { return Err(refusal::NOT_ROOT.into()); }
    if std::env::var_os(DISPOSABLE_ENV).is_none_or(|value| value.as_os_str() != std::ffi::OsStr::new("1")) {
        return Err(refusal::NOT_DISPOSABLE.into());
    }
    if installed_under(Path::new("/")) || present(Path::new(PROTECTED_MODE_MARKER)) { return Err(refusal::INSTALLED.into()); }
    // The H storage reads /proc/self/mountinfo: the private namespace must be
    // the whole process's, so this must be the (still single) main thread.
    if unsafe { libc::syscall(libc::SYS_gettid) } != libc::c_long::from(unsafe { libc::getpid() }) {
        return Err(NOT_MAIN_THREAD.into());
    }
    let signals = Signals::block()?;
    // Every mount of the run's preparation stays in this private namespace
    // and disappears with the process at the latest.
    if unsafe { libc::unshare(libc::CLONE_FS | libc::CLONE_NEWNS) } != 0
        || unsafe { libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(), libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) } != 0 {
        return Err("accept: not supported on this host: no private mount namespace".into());
    }
    let mut teardown = Teardown::default();
    let result = launch(prepared, trampoline, &signals, &mut teardown);
    let cleaned = teardown.finish();
    match (result, cleaned) {
        (_, Err(leftover)) => Err(leftover),
        (result, Ok(())) => result,
    }
}

fn launch(prepared: &Prepared, trampoline_source: &Path, signals: &Signals, t: &mut Teardown) -> Checked<Outcome> {
    let record = prepared.record();
    let native = prepared.native();
    let egress = matches!(record.network, NetworkPolicy::Egress(_));
    let ExecutableRecord::Elf { entry, .. } = &record.executable else { return Err("accept: ELF runtimes only".into()) };
    // Identities and the harness directory.
    let uid = temporary_employee_uid()?;
    t.uid = Some(uid);
    let (supervisor, gateway_uid) = temporary_service_uids()?;
    let employee = EmployeeId(random16()?);
    let run = RunHandle(duduclaw_isolation::ids::RunId(random16()?));
    let root = PathBuf::from(format!("/var/lib/duduclaw-acceptance-{}", hex(&random16()?)));
    make_dir(&root, 0o700)?;
    t.root = Some(root.clone());
    for (name, mode) in [("runtime", 0o755), ("etc-state", 0o700), ("projections", 0o755), ("storage", 0o700),
        ("storage-journal", 0o700), ("launcher", 0o700), ("snapshots", 0o700)] {
        make_dir(&root.join(name), mode)?;
    }
    // The generation as the installed layout has it: /runtime/<entry>,
    // /runtime/duduclaw-mcp-bridge (and the forwarder), digests re-checked.
    install_copy(&prepared.artifacts.join(entry.as_str()), &root.join("runtime").join(entry.as_str()), Some(prepared.runtime_digest))?;
    install_copy(&prepared.artifacts.join("duduclaw-mcp-bridge"), &root.join("runtime").join("duduclaw-mcp-bridge"), Some(prepared.bridge_digest))?;
    if let Some(forwarder) = &native.forwarder {
        install_copy(&prepared.artifacts.join(&forwarder.entry), &root.join("runtime").join(&forwarder.entry), Some(forwarder.executable_digest))?;
    }
    let launcher_path = root.join("launcher").join("duduclaw-run");
    let trampoline_sha256 = install_copy(trampoline_source, &launcher_path, None)?;
    let launcher_text = launcher_path.to_str().ok_or("accept: harness path is not UTF-8")?.to_owned();
    t.systemd = Some(crate::systemd::linux::Systemd::connect().map_err(os_error("system bus"))?);

    // H storage: provision, the employee's provider document (R8r-2 path), usage, attach.
    t.storage = Some(LoopStorage::open_diagnosed(&root.join("storage"), &root.join("storage-journal"))
        .map_err(|reason| format!("accept: not supported on this host: employee storage ({reason})"))?);
    let operation = duduclaw_isolation::ids::RequestId(random16()?);
    let proposal = Sha256Digest(random32()?);
    let storage = t.storage.as_mut().ok_or("accept: storage")?;
    storage.provision(&ProvisionRequest { employee, uid, operation_id: operation, proposal_digest: proposal,
        quota: record.resources.disk.clone() }).map_err(refused("employee storage provision"))?;
    t.provisioned = Some((employee, operation, proposal));
    let mut providers = providers_document()?;
    let written = storage.write_providers(employee, uid, ProvidersChange::Write(&providers));
    providers.fill(0);
    written.map_err(|failure| format!("accept: provider document refused ({failure:?})"))?;

    let mut publication = AcceptancePublication { policy: &prepared.policy, manifest: &prepared.manifest, arch: prepared.arch,
        employee, profile: record.profile, root: root.clone(), supervisor: (supervisor, supervisor), gateway_uid,
        projection_mount_id: None };
    // Host::start from here, in its order.
    let root_policy = publication.load().map_err(refused("root policy"))?;
    let (runtime, launch_policy) = root_policy.derive(employee, &record.runtime, false).map_err(refused("RootPolicy::derive"))?;
    let runtime = runtime.clone();
    if runtime != record.to_runtime() { return Err("accept: the derived runtime is not the generated record".into()); }
    storage.usage(employee).map_err(refused("storage usage"))?.check(employee, &runtime.resources.disk).map_err(refused("storage quota"))?;
    let attached = storage.attach(employee, uid, run, &runtime.resources.disk).map_err(refused("storage attach"))?;
    attached.receipt.check(employee, run).map_err(refused("storage receipt"))?;
    t.attachment = Some(attached.receipt.clone());
    let receipt = attached.receipt.clone();
    let decoys_planted = plant_decoys(attached.home.as_fd(), uid).map_err(os_error("decoys"))?;
    t.attached = Some(attached);
    let pair = crate::channels::RevocablePair::new().map_err(os_error("MCP socketpair"))?;
    let (gateway_end, runtime_end) = (pair.gateway, pair.runtime);
    // The runtime-side endpoint's identity, recorded BEFORE the launch.
    let observed = duduclaw_run::linux::fd::observe(runtime_end.as_raw_fd()).map_err(|_| "accept: cannot observe the MCP socket".to_owned())?;
    let socket = SocketId { inode: observed.identity.inode, mount_id: observed.identity.mount };
    let (egress_gateway, egress_runtime) = if egress {
        let pair = crate::channels::RevocablePair::new().map_err(os_error("egress socketpair"))?;
        (Some(pair.gateway), Some(pair.runtime))
    } else { (None, None) };
    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    let (stdin_read, stdin_write) = pipe()?;
    let request = worker_request();
    File::from(stdin_write).write_all(&request).map_err(os_error("stdin"))?;
    let prompt = sealed_memfd(&request).map_err(os_error("sealed prompt"))?;
    let (supervisor_uid, supervisor_gid) = publication.supervisor_identity().map_err(refused("supervisor identity"))?;
    let nonce = random32()?;
    let mut plan = BootstrapPlan::from_runtime(run, employee, uid, uid, supervisor_uid, supervisor_gid, nonce, &runtime, false,
        request.len() as u64, &[], if egress { Some(crate::policy::RUNTIME_PROXY_PORT) } else { None }, None)
        .map_err(refused("BootstrapPlan::from_runtime"))?;
    plan.pty_size = launch_policy.pty_size;
    plan.validate().map_err(refused("BootstrapPlan::validate"))?;
    publication.prepare_mcp_projection(&plan, &runtime).map_err(refused("prepare_mcp_projection"))?;
    let anchors = publication.anchors().map_err(refused("anchors"))?;
    let attached = t.attached.as_ref().ok_or("accept: storage")?;
    let mounts = crate::bootstrap_mounts::prepare(&plan, attached, &anchors).map_err(os_error("bootstrap_mounts::prepare"))?;
    publication.record_mcp_projection(&plan, &runtime, &mounts).map_err(refused("record_mcp_projection"))?;
    let native_profile = publication.native_profile(&runtime).map_err(refused("native_profile"))?;
    let sealed = plan.seal().map_err(refused("plan seal"))?;
    t.run = Some(run);
    let trampoline = t.trampoline.insert(crate::native_trampoline::NativeTrampoline::default());
    let bootstrap = trampoline.prepare(plan, sealed, mounts, vec![prompt], Some(stdin_read), Some(runtime_end), egress_runtime,
        native_profile).map_err(refused("NativeTrampoline::prepare"))?;
    crate::fds::validate_bootstrap(bootstrap.bootstrap_for_unit.as_fd()).map_err(os_error("validate_bootstrap"))?;
    let mut unit_policy = crate::systemd::UnitPolicy::new(run.0, employee, &runtime.resources, &runtime.timeouts,
        receipt.device_major, receipt.device_minor).map_err(os_error("UnitPolicy::new"))?;
    unit_policy.properties.remove("BindsTo");
    unit_policy.properties.remove("After");
    t.slice = Some(crate::systemd::employee_slice(employee));
    let systemd = t.systemd.as_ref().ok_or("accept: system bus")?;
    let unit = systemd.start_acceptance(&unit_policy, &launcher_text, bootstrap.bootstrap_for_unit.as_fd(),
        stdout_write.as_fd(), stderr_write.as_fd()).map_err(os_error("transient run unit"))?;
    t.cgroup = Some(unit.cgroup.clone());
    let unit_cgroup = format!("/{}", unit.cgroup.strip_prefix("/sys/fs/cgroup").map_err(|_| "accept: run cgroup".to_owned())?.display());
    // The launcher process, bound by a pidfd: PID 1's MainPID of this unit,
    // in its cgroup, executing the trampoline copy.
    let main_pid = systemd.acceptance_main_pid(&unit).map_err(os_error("MainPID"))?;
    let launcher = pin(main_pid, None).map_err(os_error("launcher pidfd"))?.ok_or("accept: the trampoline exited before the bootstrap")?;
    let launcher_exe = exe_digest(main_pid).ok().filter(|_| cgroup_member(main_pid, &unit_cgroup) && alive(&launcher));
    let cancelled = Arc::new(AtomicBool::new(false));
    t.cancelled = Some(cancelled.clone());
    let trampoline = t.trampoline.as_mut().ok_or("accept: trampoline")?;
    trampoline.bind_unit(run, &unit, cancelled).map_err(refused("NativeTrampoline::bind_unit"))?;
    drop(bootstrap);
    drop((stdout_write, stderr_write));

    // The conversation: the harness is the gateway end of descriptor 3.
    for fd in [gateway_end.as_raw_fd(), stdout_read.as_raw_fd(), stderr_read.as_raw_fd()] { set_nonblocking(fd)?; }
    let deadline = Instant::now() + CONVERSATION_LIMIT;
    let mut responder = Responder::default();
    let mut census = Census::default();
    let mut tree: Option<Tree> = None;
    let mut run_end: Option<String> = None;
    let mut socket_closed = false;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut failure: Option<&'static str> = None;
    while run_end.is_none() {
        if signals.pending() { return Err(INTERRUPTED.into()); }
        if Instant::now() >= deadline { break; }
        match trampoline.poll(run) {
            Ok(crate::admission::HostProgress::Ended(outcome)) => run_end = Some(match outcome {
                duduclaw_isolation::execution::RunOutcome::Exited(code) => format!("exited {code}"),
                duduclaw_isolation::execution::RunOutcome::Signalled(signal) => format!("signalled {signal}"),
                other => format!("{other:?}"),
            }),
            Ok(_) => {}
            // After the conversation the worker's provider call fails by design;
            // a launcher error status then only ends the run.
            Err(_) if responder.complete() => run_end = Some("ended with a launcher error status after the conversation".into()),
            Err(_) => { failure = Some("the trampoline reported an error before the MCP conversation completed"); break; }
        }
        census.scan(&unit.cgroup, prepared.bridge_digest);
        readable(gateway_end.as_raw_fd(), 20);
        if !socket_closed {
            match read_some(gateway_end.as_raw_fd(), &mut buffer).map_err(os_error("MCP socket"))? {
                Some(0) => socket_closed = true,
                Some(count) => {
                    let reply = match responder.feed(&buffer[..count]) {
                        Ok(reply) => reply,
                        Err(limit) => { failure = Some(limit); break; }
                    };
                    if !reply.is_empty() { send_all(gateway_end.as_raw_fd(), &reply, deadline).map_err(os_error("MCP socket"))?; }
                    if responder.initialize_held() && tree.is_none() {
                        // Facts a and b, while the reply is withheld.
                        census.scan(&unit.cgroup, prepared.bridge_digest);
                        tree = Some(observe_tree(&launcher, &unit_cgroup, socket, prepared.runtime_digest, prepared.bridge_digest)
                            .map_err(os_error("process observation"))?);
                        let reply = match responder.release_initialize(true) {
                            Ok(reply) => reply,
                            Err(limit) => { failure = Some(limit); break; }
                        };
                        send_all(gateway_end.as_raw_fd(), &reply, deadline).map_err(os_error("MCP socket"))?;
                    }
                }
                None => {}
            }
        }
        for fd in [stdout_read.as_raw_fd(), stderr_read.as_raw_fd()] {
            // Drained and discarded: the worker's output is not evidence.
            while matches!(read_some(fd, &mut buffer), Ok(Some(count)) if count > 0) {}
        }
    }
    // The run must end: after the conversation by itself (its provider call
    // cannot succeed), otherwise the harness stops the unit.
    if run_end.is_none() {
        let _ = systemd.kill_run(run.0);
        run_end = Some(if responder.complete() { "killed by the harness after the MCP conversation".into() }
            else { "killed by the harness before the MCP conversation completed".into() });
    }
    if !wait_until(Duration::from_secs(10), || cgroup_empty(&unit.cgroup)) {
        return Err("accept: the run cgroup did not become empty".into());
    }
    if !wait_until(Duration::from_secs(10), || readable(launcher.fd.as_raw_fd(), 0)) {
        return Err("accept: the trampoline process did not exit".into());
    }
    if let Some(reason) = failure { return Err(format!("accept: no record written: {reason}")); }

    // Fact d's structural half: the projection file handed to B2 is the
    // byte-exact projection and the only configuration the worker accepts.
    let mcp_json = prepared.projection.files.iter()
        .find(|file| duduclaw_mcp_boundary::config::projection_name(file).is_ok_and(|name| name == "mcp.json"))
        .ok_or("accept: the projection has no mcp.json")?;
    let mounted = fs::read(root.join("projections").join(hex(&employee.0)).join("mcp.json")).map_err(os_error("projection readback"))?;
    let expected: Value = serde_json::from_str(API_WORKER_CONFIG).map_err(|_| "accept: worker configuration".to_owned())?;
    let attached = t.attached.as_ref().ok_or("accept: storage")?;
    let exclusive = ExclusiveFacts {
        projection_bytes_exact: mounted == mcp_json.contents.as_bytes(),
        projection_is_api_worker_config: serde_json::from_slice::<Value>(&mounted).is_ok_and(|value| value == expected),
        projection_mount_id: publication.projection_mount_id.unwrap_or(0),
        decoys_planted, decoy_markers_present: decoy_markers(attached.home.as_fd()).map_err(os_error("decoy readback"))?,
        bridge_processes_seen: census.bridges.len() as u32,
    };
    let tree = tree.unwrap_or(Tree { runtime: None, fd3: Fd3::Other, bridge: None });
    let observations = Observations { uid, socket, runtime_digest: prepared.runtime_digest, bridge_digest: prepared.bridge_digest,
        trampoline_digest: trampoline_sha256, launcher_exe_sha256: launcher_exe, runtime: tree.runtime, runtime_fd3: tree.fd3,
        bridge: tree.bridge, initialize: responder.facts(), exclusive, run_end: run_end.unwrap_or_default() };
    t.descriptors.extend([gateway_end, stdout_read, stderr_read]);
    t.descriptors.extend(egress_gateway);
    Ok(Outcome { frames: responder.frames().to_vec(), observations, trampoline_sha256, kernel: kernel_release() })
}
