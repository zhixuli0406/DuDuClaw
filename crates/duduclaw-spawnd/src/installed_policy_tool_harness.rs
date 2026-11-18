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
//! (`Systemd::start_acceptance` = `start` with the harness's trampoline copy,
//! whose bytes are `--trampoline-sha256` = the release manifest's trampoline,
//! and without the spawnd dependency) and `NativeTrampoline::bind_unit` /
//! `poll`. Observation enumerates the whole run cgroup on every turn and
//! classifies every member by executable digest ([`super::classify`]); any
//! gap is an incomplete observation and no record. The tear-down signals only
//! pidfd-pinned members of the run's own cgroups. Not executed (no installation exists): the admission core
//! (readiness report, SSH evidence, the run journal and UID monitor, the
//! egress service registration), the R7a trampoline self-test readiness, the
//! helper table, the work-broker root and the gateway's `McpRunReceipt`.
//!
//! Diagnosis (first real-host run): every run collects a [`Diagnosis`] before
//! the tear-down (the phase it gave up in, its first harness-side error,
//! PID 1's `Result`/`ExecMainCode`/`ExecMainStatus` of the run unit, the
//! trampoline's error record PEEKED from its status socket, the bounded output
//! tails, every failed tear-down step); `accept` prints it only when it exits
//! non-zero. The employee image is verified gone by its objects (file, loop
//! device, mount), and the tear-down destroys it with its own operation id.
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

// The first harness-side error of the launch, without its message (which may
// name a path): its fixed context and io kind/errno or refusal code, for the
// diagnosis. The harness runs on its single main thread.
thread_local! { static FIRST_ERROR: std::cell::Cell<Option<ErrorFact>> = const { std::cell::Cell::new(None) }; }
fn note(fact: ErrorFact) { FIRST_ERROR.with(|first| if first.get().is_none() { first.set(Some(fact)); }); }
fn take_noted() -> Option<ErrorFact> { FIRST_ERROR.with(std::cell::Cell::take) }

fn os_error(what: &'static str) -> impl Fn(io::Error) -> String {
    move |error| { note(ErrorFact::io(what, &error)); format!("accept: {what}: {error}") }
}
fn refused(what: &'static str) -> impl Fn(ErrorCode) -> String {
    move |code| { note(ErrorFact::code(what, code)); format!("accept: {what} refused ({code:?})") }
}
/// A fixed harness refusal (`accept: <reason>`), noted for the diagnosis.
fn fixed_error(reason: &'static str) -> String { note(ErrorFact::fixed(reason)); format!("accept: {reason}") }

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

// ── the run cgroup, enumerated completely (F2; pure rules in the parent) ─

/// Bounds of the sub-cgroup walk below the unit (`Delegate=yes`; without a
/// helper launcher the trampoline creates none and nothing in the run reaches
/// cgroupfs): exceeding them is an incomplete scan, never a truncated one.
const CGROUP_DIRECTORIES_MAX: usize = 64;
const CGROUP_DEPTH_MAX: u32 = 8;
/// Enumerations per snapshot until the reads before and after the
/// inspection agree.
const SNAPSHOT_ATTEMPTS: usize = 16;

/// The process (or its `/proc` entry) no longer exists.
fn gone(error: &io::Error) -> bool { error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH) }

/// Every pid in the unit cgroup and all its sub-cgroups, sorted; `None` when
/// the unit cgroup itself is gone.
fn cgroup_pid_list(unit: &Path) -> std::result::Result<Option<Vec<u32>>, &'static str> {
    let mut pids = Vec::new();
    let mut directories = 0usize;
    let mut pending = vec![(unit.to_path_buf(), 0u32)];
    while let Some((directory, depth)) = pending.pop() {
        directories += 1;
        if directories > CGROUP_DIRECTORIES_MAX || depth > CGROUP_DEPTH_MAX { return Err(incomplete::CGROUP_TREE_CAP); }
        let top = depth == 0;
        let text = match fs::read_to_string(directory.join("cgroup.procs")) {
            Ok(text) => text,
            // A sub-cgroup removed mid-walk: the second enumeration decides.
            Err(error) if error.kind() == io::ErrorKind::NotFound => if top { return Ok(None) } else { continue },
            Err(_) => return Err(incomplete::CGROUP_UNREADABLE),
        };
        for token in text.split_whitespace() {
            pids.push(token.parse::<u32>().map_err(|_| incomplete::CGROUP_UNREADABLE)?);
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => if top { return Ok(None) } else { continue },
            Err(_) => return Err(incomplete::CGROUP_UNREADABLE),
        };
        for entry in entries {
            let entry = entry.map_err(|_| incomplete::CGROUP_UNREADABLE)?;
            if entry.file_type().map_err(|_| incomplete::CGROUP_UNREADABLE)?.is_dir() { pending.push((entry.path(), depth + 1)); }
        }
    }
    pids.sort_unstable();
    Ok(Some(pids))
}

/// Executable identity → digest. A running executable cannot be written
/// (ETXTBSY) and a write changes ctime, so the key names unchanged bytes.
#[derive(Default)]
struct ExeCache(BTreeMap<(u64, u64, u64, i64, i64), Sha256Digest>);

enum Inspected { Read(MemberRead, Option<Pinned>), Changed }

/// One cgroup member: pinned by a pidfd between two reads of its stat, its
/// executable opened and hashed, its start time re-checked. A vanished
/// process counts as exited only when the kernel confirms it (`pidfd_open`
/// ESRCH or a readable pidfd); every other failure is reported, never
/// treated as absence. `keep` decides whether the pidfd is returned.
fn inspect(pid: u32, cache: &mut ExeCache, keep: &dyn Fn(Sha256Digest) -> bool) -> Inspected {
    use rustix::process::{pidfd_open, Pid, PidfdFlags};
    use std::os::unix::fs::MetadataExt;
    let Some(raw) = i32::try_from(pid).ok().and_then(Pid::from_raw) else { return Inspected::Read(MemberRead::Unreadable, None) };
    let (ppid, start) = match proc_stat(pid) {
        Ok(value) => value,
        Err(error) if gone(&error) => {
            let confirmed = matches!(pidfd_open(raw, PidfdFlags::empty()), Err(errno) if errno == rustix::io::Errno::SRCH);
            return Inspected::Read(MemberRead::Exited { confirmed }, None);
        }
        Err(_) => return Inspected::Read(MemberRead::Unreadable, None),
    };
    let fd = match pidfd_open(raw, PidfdFlags::empty()) {
        Ok(fd) => fd,
        Err(errno) if errno == rustix::io::Errno::SRCH => return Inspected::Read(MemberRead::Exited { confirmed: true }, None),
        Err(_) => return Inspected::Read(MemberRead::Unreadable, None),
    };
    let pinned = Pinned { pid, start, fd };
    let exited = |process: &Pinned| readable(process.fd.as_raw_fd(), 0);
    match proc_stat(pid) {
        Ok((again_ppid, again)) if again == start && again_ppid == ppid => {}
        Ok(_) => return Inspected::Changed,
        Err(error) if gone(&error) && exited(&pinned) => return Inspected::Read(MemberRead::Exited { confirmed: true }, None),
        Err(_) => return Inspected::Read(MemberRead::Unreadable, None),
    }
    let file = match File::open(format!("/proc/{pid}/exe")) {
        Ok(file) => file,
        Err(_) if exited(&pinned) => return Inspected::Read(MemberRead::Exited { confirmed: true }, None),
        Err(error) if gone(&error) => return Inspected::Read(MemberRead::Exited { confirmed: false }, None),
        Err(_) => return Inspected::Read(MemberRead::Unreadable, None),
    };
    let identity = match file.metadata() {
        Ok(metadata) => (metadata.dev(), metadata.ino(), metadata.size(), metadata.ctime(), metadata.ctime_nsec()),
        Err(_) => return Inspected::Read(MemberRead::HashError, None),
    };
    let exe = match cache.0.get(&identity) {
        Some(digest) => *digest,
        None => match digest_file(file) {
            Ok(digest) => { cache.0.insert(identity, digest); digest }
            Err(_) => return Inspected::Read(MemberRead::HashError, None),
        },
    };
    match proc_stat(pid) {
        Ok((_, again)) if again == start => {
            let pinned = keep(exe).then_some(pinned);
            Inspected::Read(MemberRead::Live { start, ppid, exe }, pinned)
        }
        Ok(_) => Inspected::Changed,
        Err(error) if gone(&error) && exited(&pinned) => Inspected::Read(MemberRead::Exited { confirmed: true }, None),
        Err(_) => Inspected::Read(MemberRead::Unreadable, None),
    }
}

enum Scan { Taken(CgroupSnapshot, BTreeMap<u32, Pinned>), Gone }

/// The whole unit cgroup: enumerate, inspect every member, enumerate again;
/// only two agreeing enumerations around a clean inspection make a snapshot.
fn snapshot(unit: &Path, cache: &mut ExeCache, keep: &dyn Fn(Sha256Digest) -> bool) -> Scan {
    let partial = |reason: &'static str| Scan::Taken(CgroupSnapshot { members: Vec::new(), incomplete: Some(reason) }, BTreeMap::new());
    for _ in 0..SNAPSHOT_ATTEMPTS {
        let before = match cgroup_pid_list(unit) { Ok(Some(pids)) => pids, Ok(None) => return Scan::Gone, Err(reason) => return partial(reason) };
        let mut members = Vec::with_capacity(before.len());
        let mut pinned = BTreeMap::new();
        let mut changed = false;
        for pid in &before {
            match inspect(*pid, cache, keep) {
                Inspected::Read(read, pin) => {
                    if let Some(pin) = pin { pinned.insert(*pid, pin); }
                    members.push(Member { pid: *pid, read });
                }
                Inspected::Changed => { changed = true; break; }
            }
        }
        let after = match cgroup_pid_list(unit) { Ok(Some(pids)) => pids, Ok(None) => return Scan::Gone, Err(reason) => return partial(reason) };
        if !changed && after == before { return Scan::Taken(CgroupSnapshot { members, incomplete: None }, pinned); }
    }
    partial(incomplete::CGROUP_UNSTABLE)
}
fn gone_snapshot() -> CgroupSnapshot { CgroupSnapshot { members: Vec::new(), incomplete: Some(incomplete::CGROUP_UNREADABLE) } }

/// One census turn of the conversation loop.
fn census_scan(census: &mut Census, unit: &Path, launcher: &Pinned, ids: &Identities, limits: &WalkLimits, cache: &mut ExeCache,
    after_conversation: bool) {
    let scan = snapshot(unit, cache, &|_: Sha256Digest| false);
    // Read after the snapshot: a missing main process is explained only by its confirmed exit.
    let launcher_exited = readable(launcher.fd.as_raw_fd(), 0);
    let snapshot = match scan { Scan::Taken(snapshot, _) => snapshot, Scan::Gone => gone_snapshot() };
    census.record(&snapshot, ids, limits, launcher_exited, after_conversation);
}

/// The runtime and the bridge, observed while the `initialize` reply is
/// withheld (both are alive and blocked on the harness), from one complete
/// snapshot of the run cgroup that is also a census turn.
struct Tree { runtime: Option<ProcessFacts>, fd3: Fd3, bridge: Option<ProcessFacts>, incomplete: Option<&'static str> }
#[allow(clippy::too_many_arguments)]
fn observe(launcher: &Pinned, unit: &Path, unit_cgroup: &str, socket: SocketId, ids: &Identities, limits: &WalkLimits,
    census: &mut Census, cache: &mut ExeCache) -> Tree {
    let keep = |digest: Sha256Digest| digest == ids.runtime || digest == ids.bridge;
    let scan = snapshot(unit, cache, &keep);
    let launcher_exited = readable(launcher.fd.as_raw_fd(), 0);
    let (snapshot, pinned) = match scan { Scan::Taken(snapshot, pinned) => (snapshot, pinned), Scan::Gone => (gone_snapshot(), BTreeMap::new()) };
    census.record(&snapshot, ids, limits, launcher_exited, false);
    let mut tree = Tree { runtime: None, fd3: Fd3::Other, bridge: None, incomplete: None };
    let classified = match classify(&snapshot, ids, limits) {
        Ok(classified) => classified,
        Err(reason) => { tree.incomplete = Some(reason); return tree; }
    };
    if let Some((pid, depth)) = classified.runtime {
        match pinned.get(&pid).map(|process| process_facts(process, depth, unit_cgroup, socket)) {
            Some(Ok((facts, fd3))) => { tree.runtime = Some(facts); tree.fd3 = fd3; }
            _ => { tree.incomplete = Some(incomplete::PROCESS_FACTS); return tree; }
        }
    }
    if let Some((pid, distance)) = classified.bridge {
        match pinned.get(&pid).map(|process| process_facts(process, distance, unit_cgroup, socket)) {
            Some(Ok((facts, _))) => tree.bridge = Some(facts),
            _ => tree.incomplete = Some(incomplete::PROCESS_FACTS),
        }
    }
    tree
}

// ── files ───────────────────────────────────────────────────────────────

/// Mode and root ownership of a directory the harness just created.
fn secure_dir(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    std::os::unix::fs::chown(path, Some(0), Some(0))
}
/// A directory below the (already registered) harness directory.
fn make_dir(path: &Path, mode: u32) -> Checked<()> {
    make_registered_dir(path, mode, |_| {}, |path| secure_dir(path, mode)).map_err(os_error("harness directory"))
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
        return Err(format!("accept: {} changed: its sha256 is not the expected (record / --trampoline-sha256) digest",
            source.display()));
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
/// Every process with `uid` in any uid or gid field (the temporary uid = gid),
/// with its unified cgroup (`None` when unparseable). A process that vanishes
/// mid-scan is skipped; any other read error fails the scan.
fn uid_processes(uid: u32) -> io::Result<Vec<UidProcess>> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        let inspect = || -> io::Result<Option<UidProcess>> {
            let (_, start) = proc_stat(pid)?;
            let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
            if !crate::accounts::process_uids(&status)?.contains(&uid) && !crate::accounts::process_gids(&status)?.contains(&uid) {
                return Ok(None);
            }
            let text = fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
            let cgroup = crate::accounts::unified_cgroup(&text).ok().map(str::to_owned);
            if proc_stat(pid)?.1 != start { return Err(io::Error::new(io::ErrorKind::InvalidData, "pid reused during the scan")); }
            Ok(Some(UidProcess { pid, start, cgroup }))
        };
        match inspect() {
            Ok(Some(process)) => found.push(process),
            Ok(None) => {}
            Err(error) if gone(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(found)
}

/// SIGKILL one process of the run through a pidfd, after re-checking (with
/// the pidfd held) its start time, the temporary uid and its membership of a
/// run cgroup. `Ok(false)` when it is gone or no longer qualifies (never
/// signalled then).
fn kill_member(process: &UidProcess, uid: u32, run_cgroups: &[&str]) -> io::Result<bool> {
    use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};
    if process.pid <= 1 { return Ok(false); }
    let raw = i32::try_from(process.pid).ok().and_then(Pid::from_raw).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let descriptor = match pidfd_open(raw, PidfdFlags::empty()) {
        Ok(fd) => fd,
        Err(errno) if errno == rustix::io::Errno::SRCH => return Ok(false),
        Err(errno) => return Err(errno.into()),
    };
    let verify = || -> io::Result<bool> {
        if proc_stat(process.pid)?.1 != process.start { return Ok(false); }
        let status = fs::read_to_string(format!("/proc/{}/status", process.pid))?;
        if !crate::accounts::process_uids(&status)?.contains(&uid) && !crate::accounts::process_gids(&status)?.contains(&uid) {
            return Ok(false);
        }
        let text = fs::read_to_string(format!("/proc/{}/cgroup", process.pid))?;
        let actual = crate::accounts::unified_cgroup(&text)?;
        if !run_cgroups.iter().any(|allowed| crate::accounts::cgroup_member(actual, allowed)) { return Ok(false); }
        Ok(proc_stat(process.pid)?.1 == process.start)
    };
    match verify() {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(error) if gone(&error) => return Ok(false),
        Err(error) => return Err(error),
    }
    match pidfd_send_signal(&descriptor, Signal::KILL) {
        Ok(()) => Ok(true),
        Err(errno) if errno == rustix::io::Errno::SRCH => Ok(false),
        Err(errno) => Err(errno.into()),
    }
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
    mountinfo_below(&table, root)
}

// ── what is left of the employee image (P2: objects, not return values) ─

/// The image objects below H's storage directory `storage` after the
/// tear-down: entries of the directory itself, loop devices backed by a file
/// below it (`/sys/block/loop*/loop/backing_file`), mounts below it in this
/// process's (private) mount namespace, where H mounted it.
fn image_objects(storage: &Path) -> ImageObjects {
    let file = match fs::read_dir(storage) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Presence::Absent,
        Err(_) => Presence::Unknown,
        Ok(entries) => match entries.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<Vec<String>>>() {
            Ok(names) => storage_entries_presence(&names),
            Err(_) => Presence::Unknown,
        },
    };
    let mount = match fs::read_to_string("/proc/self/mountinfo") {
        Ok(table) => if mountinfo_below(&table, storage) { Presence::Present } else { Presence::Absent },
        Err(_) => Presence::Unknown,
    };
    ImageObjects { file, loop_device: loop_presence(storage), mount }
}
fn loop_presence(storage: &Path) -> Presence {
    let Ok(entries) = fs::read_dir("/sys/block") else { return Presence::Unknown };
    let mut unknown = false;
    for entry in entries {
        let Ok(entry) = entry else { unknown = true; continue };
        let name = entry.file_name();
        let is_loop = name.to_str().and_then(|name| name.strip_prefix("loop"))
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()));
        if !is_loop { continue; }
        match fs::read_to_string(entry.path().join("loop").join("backing_file")) {
            Ok(text) if backing_below(&text, storage) => return Presence::Present,
            Ok(_) => {}
            // An unbound loop device has no `loop/` attribute group.
            Err(error) if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ENXIO) => {}
            Err(_) => unknown = true,
        }
    }
    if unknown { Presence::Unknown } else { Presence::Absent }
}

// ── the trampoline's status socket, for the diagnosis only ──────────────

/// The head packet of the root end of the bootstrap/status socket, PEEKED
/// (never consumed: `NativeTrampoline::poll` still receives it), when it is a
/// 12-byte status record without descriptors.
fn peek_status(socket: BorrowedFd<'_>) -> Option<duduclaw_run::status::StatusRecord> {
    let mut packet = [0u8; 32];
    let mut iov = libc::iovec { iov_base: packet.as_mut_ptr().cast(), iov_len: packet.len() };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    let count = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
    if count != 12 || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 { return None; }
    duduclaw_run::status::StatusRecord::decode(&packet[..12], 0).ok()
}
/// Record an error status into the diagnosis (the first one wins).
fn note_status(d: &mut Diagnosis, record: Option<duduclaw_run::status::StatusRecord>) {
    if let Some(duduclaw_run::status::StatusRecord::Error(code)) = record { d.status_error.get_or_insert(code); }
}
/// After the run: receive what the trampoline left on its status socket
/// (bounded; nothing reads it any more).
fn drain_status(socket: BorrowedFd<'_>, d: &mut Diagnosis) {
    use duduclaw_run::status::StatusRecord;
    for _ in 0..16 {
        if !readable(socket.as_raw_fd(), 0) { return; }
        let Ok((record, rights)) = duduclaw_run::linux::bootstrap::receive_status(socket.as_raw_fd()) else { return };
        drop(rights);
        match record {
            StatusRecord::Error(code) => { d.status_error.get_or_insert(code); }
            StatusRecord::Exited(code) => { d.run_end.get_or_insert_with(|| format!("exited {code}")); }
            StatusRecord::Signalled(signal) => { d.run_end.get_or_insert_with(|| format!("signalled {signal}")); }
            _ => {}
        }
    }
}

// ── the run's output pipes (the harness's read ends) ────────────────────

/// Kept by [`run`] until after the tear-down, so the diagnosis can read the
/// last bytes whatever step failed.
#[derive(Default)]
struct Streams { stdout: Option<OwnedFd>, stderr: Option<OwnedFd> }
/// Everything readable now (non-blocking, bounded), into the tail.
fn read_tail(fd: Option<&OwnedFd>, tail: &mut Tail) {
    let Some(fd) = fd else { return };
    let _ = set_nonblocking(fd.as_raw_fd());
    let mut buffer = vec![0u8; 16 * 1024];
    for _ in 0..256 {
        match read_some(fd.as_raw_fd(), &mut buffer) {
            Ok(Some(count)) if count > 0 => tail.push(&buffer[..count]),
            _ => return,
        }
    }
}

/// How long the diagnosis waits for PID 1 to record the main process's end.
const UNIT_SETTLE: Duration = Duration::from_secs(6);
/// The diagnosis of the run, read BEFORE the tear-down (which resets and
/// unloads the unit): the first harness-side error, what the trampoline left
/// on its status socket, PID 1's record of the main process, the output tails.
fn collect(d: &mut Diagnosis, t: &Teardown, streams: &Streams) {
    d.harness_error = take_noted();
    if let Some(run) = t.run { d.run = Some(run.0.0); }
    if let (Some(trampoline), Some(run)) = (t.trampoline.as_ref(), t.run) {
        if let Some(socket) = trampoline.acceptance_status_socket(run) { drain_status(socket, d); }
    }
    if let (Some(systemd), Some(run)) = (t.systemd.as_ref(), t.run) {
        let name = crate::systemd::run_unit(run.0);
        let deadline = Instant::now() + UNIT_SETTLE;
        d.unit = loop {
            match systemd.acceptance_result(&name) {
                Ok(Some((state, ..))) if matches!(state.as_str(), "active" | "activating" | "deactivating" | "reloading")
                    && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Ok(Some((active_state, result, exec_main_code, exec_main_status))) =>
                    break UnitReadback::Read(UnitResult { active_state, result, exec_main_code, exec_main_status }),
                Ok(None) => break UnitReadback::NotLoaded,
                Err(error) => break UnitReadback::Unreadable(ErrorFact::io("unit result readback", &error)),
            }
        };
    }
    read_tail(streams.stdout.as_ref(), &mut d.stdout);
    read_tail(streams.stderr.as_ref(), &mut d.stderr);
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
    /// `(employee, DESTROY operation id, destroy proposal digest)`: a fresh
    /// operation, never the provision's (H's ledger refuses a reused one).
    provisioned: Option<(EmployeeId, duduclaw_isolation::ids::RequestId, Sha256Digest)>,
    /// H's storage directory: the employee image's objects are verified gone
    /// below it ([`image_objects`]).
    image_storage: Option<PathBuf>,
    uid: Option<u32>,
    /// F3: the run unit's cgroup and the harness's own leaf slice
    /// ([`run_cgroups`]); the only places whose processes may be signalled.
    run_cgroups: Vec<String>,
    root: Option<PathBuf>,
    /// Every step that failed, for the diagnosis (A6).
    steps: Vec<ErrorFact>,
}
const LEFT_RUN_PROCESSES: &str = "processes of the temporary employee uid in the run cgroup";
const LEFT_UNSCANNED: &str = "processes of the temporary employee uid (the process table could not be read)";
impl Teardown {
    fn finish(&mut self) -> Checked<()> {
        if self.finished { return Ok(()); }
        self.finished = true;
        let mut left: Vec<&'static str> = Vec::new();
        let mut steps: Vec<ErrorFact> = Vec::new();
        if let Some(flag) = &self.cancelled { flag.store(true, Ordering::Release); }
        if let (Some(systemd), Some(run)) = (&self.systemd, self.run) {
            if let Err(error) = systemd.cleanup_test_run(run.0) {
                steps.push(ErrorFact::io("stop and unload the run unit", &error));
                left.push("the systemd run unit");
            }
        }
        if let (Some(trampoline), Some(run)) = (self.trampoline.as_mut(), self.run) { let _ = trampoline.close(run); }
        self.trampoline = None;
        self.descriptors.clear();
        if let Some(cgroup) = &self.cgroup {
            if !wait_until(Duration::from_secs(10), || !cgroup.exists()) {
                steps.push(ErrorFact::timeout("run cgroup removal"));
                left.push("the run cgroup");
            }
        }
        if let (Some(systemd), Some(slice)) = (&self.systemd, &self.slice) {
            if let Err(error) = systemd.acceptance_stop(slice) { steps.push(ErrorFact::io("stop the employee slice", &error)); }
            if !wait_until(Duration::from_secs(10), || systemd.acceptance_loaded(slice).is_ok_and(|loaded| !loaded)) {
                steps.push(ErrorFact::timeout("employee slice unload"));
                left.push("the employee slice unit");
            }
        }
        // F3: only members of the run cgroups are ever signalled; a process
        // with the temporary uid anywhere else is reported, never touched.
        let groups: Vec<&str> = self.run_cgroups.iter().map(String::as_str).collect();
        if let Some(uid) = self.uid {
            let settled = || uid_processes(uid).is_ok_and(|found| teardown_selection(&found, &groups).kill.is_empty());
            if !wait_until(Duration::from_secs(5), settled) {
                match uid_processes(uid) {
                    Ok(found) => {
                        let selection = teardown_selection(&found, &groups);
                        for process in found.iter().filter(|process| selection.kill.contains(&(process.pid, process.start))) {
                            if let Err(error) = kill_member(process, uid, &groups) {
                                steps.push(ErrorFact::io("signal a run process", &error));
                            }
                        }
                    }
                    Err(error) => steps.push(ErrorFact::io("scan the temporary uid's processes", &error)),
                }
                if !wait_until(Duration::from_secs(5), settled) {
                    steps.push(ErrorFact::timeout("run processes exit"));
                    left.push(LEFT_RUN_PROCESSES);
                }
            }
        }
        // P2: detach (the run's lease), then destroy with the harness's own
        // destroy operation. Their results are diagnostics only: whether the
        // image is left over is decided from the objects after the tear-down.
        self.attached = None;
        if let Some(storage) = self.storage.as_mut() {
            if let Some(attachment) = &self.attachment {
                if let Err(code) = storage.detach(attachment) { steps.push(ErrorFact::code("storage detach", code)); }
            }
            if let Some((employee, operation, digest)) = self.provisioned {
                if let Err(code) = storage.destroy(employee, operation, digest) { steps.push(ErrorFact::code("storage destroy", code)); }
                match storage.traces() {
                    Ok(traces) if traces.contains(&employee) => steps.push(ErrorFact::fixed("storage ledger still names the employee")),
                    Ok(_) => {}
                    Err(code) => steps.push(ErrorFact::code("storage traces", code)),
                }
            }
        }
        self.storage = None;
        if let (Some(root), Some(run)) = (&self.root, self.run) {
            match anchors_of(root) {
                Ok(anchors) => if let Err(error) = crate::bootstrap_mounts::cleanup_projection(&anchors, run) {
                    steps.push(ErrorFact::io("remove the /etc projection", &error));
                    left.push("the /etc projection");
                },
                Err(error) => { steps.push(ErrorFact::io("open the projection anchors", &error)); left.push("the harness directory"); }
            }
        }
        if let Some(root) = &self.root {
            // Never delete through a mount (remove_dir_all would descend into it).
            if mounts_below(root) {
                steps.push(ErrorFact::fixed("a mount below the harness directory"));
                left.push("a mount below the harness directory");
            } else if let Err(error) = fs::remove_dir_all(root) {
                steps.push(ErrorFact::io("remove the harness directory", &error));
                left.push("the harness directory");
            } else if fs::symlink_metadata(root).is_ok() { left.push("the harness directory"); }
        }
        // P2: the employee image is left over only if one of its objects still
        // exists (or cannot be shown absent) now.
        if let Some(storage) = &self.image_storage { left.extend(image_leftovers(&image_objects(storage))); }
        // Verification of what must no longer exist.
        if let (Some(systemd), Some(run)) = (&self.systemd, self.run) {
            if systemd.list_runs().map_or(true, |runs| runs.contains(&run.0)) { left.push("the systemd run unit"); }
        }
        if let (Some(systemd), Some(slice)) = (&self.systemd, &self.slice) {
            if !systemd.acceptance_loaded(slice).is_ok_and(|loaded| !loaded) { left.push("the employee slice unit"); }
        }
        if let Some(cgroup) = &self.cgroup { if cgroup.exists() { left.push("the run cgroup"); } }
        if let Some(uid) = self.uid {
            match uid_processes(uid) {
                Ok(found) => {
                    let selection = teardown_selection(&found, &groups);
                    if !selection.kill.is_empty() { left.push(LEFT_RUN_PROCESSES); }
                    if !selection.foreign.is_empty() { left.push(FOREIGN_UID_PROCESS); }
                }
                Err(error) => { steps.push(ErrorFact::io("verify the temporary uid's processes", &error)); left.push(LEFT_UNSCANNED); }
            }
        }
        if let Some(root) = &self.root { if fs::symlink_metadata(root).is_ok() { left.push("the harness directory"); } }
        self.systemd = None;
        self.steps = steps;
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
/// observing (with tear-down already verified); `Ok` carries the facts. The
/// [`Diagnosis`] is always collected (printed by `accept` only when it exits
/// non-zero); collecting it changes nothing the decision reads.
pub(super) fn run(prepared: &Prepared, trampoline: &Path, trampoline_sha256: Sha256Digest) -> (Checked<Outcome>, Diagnosis) {
    let mut diagnosis = Diagnosis::default();
    let result = run_diagnosed(prepared, trampoline, trampoline_sha256, &mut diagnosis);
    (result, diagnosis)
}
fn run_diagnosed(prepared: &Prepared, trampoline: &Path, trampoline_sha256: Sha256Digest, diagnosis: &mut Diagnosis)
    -> Checked<Outcome> {
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
    let _ = take_noted();
    let mut teardown = Teardown::default();
    let mut streams = Streams::default();
    let result = launch(prepared, trampoline, trampoline_sha256, &signals, &mut teardown, diagnosis, &mut streams);
    if let Ok(outcome) = &result { diagnosis.incomplete = outcome.observations.incomplete; }
    // Before the tear-down resets and unloads the unit.
    collect(diagnosis, &teardown, &streams);
    let cleaned = teardown.finish();
    diagnosis.teardown = std::mem::take(&mut teardown.steps);
    drop(streams);
    match (result, cleaned) {
        (Ok(_), Err(leftover)) => { diagnosis.phase = HarnessPhase::Teardown; Err(leftover) }
        (Err(_), Err(leftover)) => Err(leftover),
        (result, Ok(())) => result,
    }
}

const UID_TAKEN: &str = "accept: refused: a temporary uid (employee, supervisor or gateway) gained an account, a group \
    or a process before the launch";
const TRAMPOLINE_COPY: &str = "accept: the root-owned trampoline copy is not --trampoline-sha256";

#[allow(clippy::too_many_arguments)]
fn launch(prepared: &Prepared, trampoline_source: &Path, trampoline_sha256: Sha256Digest, signals: &Signals, t: &mut Teardown,
    d: &mut Diagnosis, streams: &mut Streams) -> Checked<Outcome> {
    d.phase = HarnessPhase::Prepare;
    let record = prepared.record();
    let native = prepared.native();
    let egress = matches!(record.network, NetworkPolicy::Egress(_));
    let ExecutableRecord::Elf { entry, .. } = &record.executable else { return Err(fixed_error("ELF runtimes only")) };
    // Identities and the harness directory.
    let uid = temporary_employee_uid()?;
    t.uid = Some(uid);
    let (supervisor, gateway_uid) = temporary_service_uids()?;
    let employee = EmployeeId(random16()?);
    let run = RunHandle(duduclaw_isolation::ids::RunId(random16()?));
    d.employee = Some(employee.0);
    d.run = Some(run.0.0);
    let root = PathBuf::from(format!("/var/lib/duduclaw-acceptance-{}", hex(&random16()?)));
    // F5: the guard owns the directory from the moment it exists.
    make_registered_dir(&root, 0o700, |path| t.root = Some(path.to_path_buf()), |path| secure_dir(path, 0o700))
        .map_err(os_error("harness directory"))?;
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
    // F1: the copy the unit executes is the trampoline the release manifest
    // names: copied with that expected digest, then hashed again from disk.
    let launcher_path = root.join("launcher").join("duduclaw-run");
    install_copy(trampoline_source, &launcher_path, Some(trampoline_sha256))?;
    if File::open(&launcher_path).and_then(digest_file).map_err(os_error("trampoline copy"))? != trampoline_sha256 {
        note(ErrorFact::fixed("the trampoline copy is not --trampoline-sha256"));
        return Err(TRAMPOLINE_COPY.into());
    }
    let launcher_text = launcher_path.to_str().ok_or("accept: harness path is not UTF-8")?.to_owned();
    t.systemd = Some(crate::systemd::linux::Systemd::connect().map_err(os_error("system bus"))?);

    // H storage: provision, the employee's provider document (R8r-2 path), usage, attach.
    d.phase = HarnessPhase::Storage;
    t.image_storage = Some(root.join("storage"));
    t.storage = Some(LoopStorage::open_diagnosed(&root.join("storage"), &root.join("storage-journal"))
        .map_err(|reason| { note(ErrorFact::fixed("employee storage open")); format!("accept: not supported on this host: employee storage ({reason})") })?);
    let operation = duduclaw_isolation::ids::RequestId(random16()?);
    let proposal = Sha256Digest(random32()?);
    // P2: the tear-down's destroy is its own operation. H's ledger refuses a
    // destroy that reuses the provision's operation id (`Journal::validate`:
    // every operation id once), which left the ledger entry behind.
    let destroy_operation = duduclaw_isolation::ids::RequestId(random16()?);
    let destroy_proposal = Sha256Digest(random32()?);
    if destroy_operation == operation { return Err(fixed_error("getrandom repeated an operation id")); }
    // Registered before provision: a provision that fails half-way is destroyed too.
    t.provisioned = Some((employee, destroy_operation, destroy_proposal));
    let storage = t.storage.as_mut().ok_or("accept: storage")?;
    storage.provision(&ProvisionRequest { employee, uid, operation_id: operation, proposal_digest: proposal,
        quota: record.resources.disk.clone() }).map_err(refused("employee storage provision"))?;
    let mut providers = providers_document()?;
    let written = storage.write_providers(employee, uid, ProvidersChange::Write(&providers));
    providers.fill(0);
    written.map_err(|failure| { note(ErrorFact::fixed("provider document write")); format!("accept: provider document refused ({failure:?})") })?;

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
    d.phase = HarnessPhase::Projection;
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
    // The read ends stay with `run` until after the tear-down (diagnosis).
    let (stdout_fd, stderr_fd) = (stdout_read.as_raw_fd(), stderr_read.as_raw_fd());
    streams.stdout = Some(stdout_read);
    streams.stderr = Some(stderr_read);
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
    d.phase = HarnessPhase::Seal;
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
    let slice_name = crate::systemd::employee_slice(employee);
    t.slice = Some(slice_name.clone());
    // F3: the numbers are re-checked immediately before the launch; a process
    // that took one meanwhile is never adopted (nor signalled) by this run.
    d.phase = HarnessPhase::StartUnit;
    if !number_free(uid) || !number_free(supervisor) || !number_free(gateway_uid) {
        note(ErrorFact::fixed("a temporary uid was taken before the launch"));
        return Err(UID_TAKEN.into());
    }
    let systemd = t.systemd.as_ref().ok_or("accept: system bus")?;
    let unit = systemd.start_acceptance(&unit_policy, &launcher_text, bootstrap.bootstrap_for_unit.as_fd(),
        stdout_write.as_fd(), stderr_write.as_fd()).map_err(os_error("transient run unit"))?;
    t.cgroup = Some(unit.cgroup.clone());
    let unit_cgroup = format!("/{}", unit.cgroup.strip_prefix("/sys/fs/cgroup").map_err(|_| fixed_error("run cgroup outside cgroupfs"))?.display());
    t.run_cgroups = run_cgroups(&unit_cgroup, &slice_name);
    // The launcher process, bound by a pidfd: PID 1's MainPID of this unit,
    // in its cgroup, executing the trampoline copy.
    let main_pid = systemd.acceptance_main_pid(&unit).map_err(os_error("MainPID"))?;
    let launcher = pin(main_pid, None).map_err(os_error("launcher pidfd"))?
        .ok_or_else(|| fixed_error("the trampoline exited before the bootstrap"))?;
    let launcher_exe = exe_digest(main_pid).ok().filter(|_| cgroup_member(main_pid, &unit_cgroup) && alive(&launcher));
    // F2: who may live in the run cgroup, and the bounds of a complete walk
    // (the unit's TasksMax is the record's task limit).
    let ids = Identities { launcher: (launcher.pid, launcher.start), trampoline: trampoline_sha256,
        runtime: prepared.runtime_digest, bridge: prepared.bridge_digest,
        forwarder: native.forwarder.as_ref().map(|forwarder| forwarder.executable_digest) };
    let limits = WalkLimits::for_tasks(record.resources.tasks);
    let mut cache = ExeCache::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    t.cancelled = Some(cancelled.clone());
    let trampoline = t.trampoline.as_mut().ok_or("accept: trampoline")?;
    d.phase = HarnessPhase::BindUnit;
    // Diagnosis: a trampoline that fails before its challenge sends an error
    // record instead, which `bind_unit` consumes as a bad challenge. Wait for
    // the first packet as `bind_unit` would (same bound) and PEEK it.
    if let Some(socket) = trampoline.acceptance_status_socket(run) {
        let bound = i32::try_from(duduclaw_isolation::ipc::CONTROL_TIMEOUT_MS).unwrap_or(i32::MAX);
        readable(socket.as_raw_fd(), bound);
        note_status(d, peek_status(socket));
    }
    trampoline.bind_unit(run, &unit, cancelled).map_err(refused("NativeTrampoline::bind_unit"))?;
    drop(bootstrap);
    drop((stdout_write, stderr_write));

    // The conversation: the harness is the gateway end of descriptor 3.
    d.phase = HarnessPhase::AwaitInitialize;
    for fd in [gateway_end.as_raw_fd(), stdout_fd, stderr_fd] { set_nonblocking(fd)?; }
    let deadline = Instant::now() + CONVERSATION_LIMIT;
    let mut responder = Responder::default();
    let mut census = Census::default();
    let mut tree: Option<Tree> = None;
    let mut run_end: Option<String> = None;
    let mut socket_closed = false;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut failure: Option<&'static str> = None;
    while run_end.is_none() {
        d.phase = if responder.complete() { HarnessPhase::AwaitExit }
            else if tree.is_some() { HarnessPhase::AwaitToolsList } else { HarnessPhase::AwaitInitialize };
        if signals.pending() { note(ErrorFact::fixed("interrupted by a signal")); return Err(INTERRUPTED.into()); }
        if Instant::now() >= deadline { break; }
        // Diagnosis: `poll` reduces an error record to `IsolationUnavailable`;
        // the head packet is peeked first (never consumed).
        if d.status_error.is_none() { note_status(d, trampoline.acceptance_status_socket(run).and_then(peek_status)); }
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
        if run_end.is_none() {
            census_scan(&mut census, &unit.cgroup, &launcher, &ids, &limits, &mut cache, responder.complete());
        }
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
                        d.phase = HarnessPhase::Observe;
                        tree = Some(observe(&launcher, &unit.cgroup, &unit_cgroup, socket, &ids, &limits, &mut census, &mut cache));
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
        for (fd, tail) in [(stdout_fd, &mut d.stdout), (stderr_fd, &mut d.stderr)] {
            // Drained; only the bounded tail is kept, for the diagnosis: the
            // worker's output is not evidence.
            while let Ok(Some(count)) = read_some(fd, &mut buffer) {
                if count == 0 { break; }
                tail.push(&buffer[..count]);
            }
        }
    }
    if failure.is_none() { d.phase = HarnessPhase::AwaitExit; }
    // The run must end: after the conversation by itself (its provider call
    // cannot succeed), otherwise the harness stops the unit.
    if run_end.is_none() {
        let _ = systemd.kill_run(run.0);
        run_end = Some(if responder.complete() { "killed by the harness after the MCP conversation".into() }
            else { "killed by the harness before the MCP conversation completed".into() });
    }
    d.run_end = run_end.clone();
    if !wait_until(Duration::from_secs(10), || cgroup_empty(&unit.cgroup)) {
        return Err(fixed_error("the run cgroup did not become empty"));
    }
    if !wait_until(Duration::from_secs(10), || readable(launcher.fd.as_raw_fd(), 0)) {
        return Err(fixed_error("the trampoline process did not exit"));
    }
    if let Some(reason) = failure { note(ErrorFact::fixed(reason)); return Err(format!("accept: no record written: {reason}")); }

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
        bridge_processes_seen: census.bridges(&ids),
    };
    let tree = tree.unwrap_or(Tree { runtime: None, fd3: Fd3::Other, bridge: None, incomplete: None });
    let incomplete = tree.incomplete.or_else(|| census.finish(&ids));
    let observations = Observations { uid, socket, runtime_digest: prepared.runtime_digest, bridge_digest: prepared.bridge_digest,
        trampoline_digest: trampoline_sha256, launcher_exe_sha256: launcher_exe, runtime: tree.runtime, runtime_fd3: tree.fd3,
        bridge: tree.bridge, initialize: responder.facts(), exclusive, run_end: run_end.unwrap_or_default(), incomplete };
    t.descriptors.push(gateway_end);
    t.descriptors.extend(egress_gateway);
    Ok(Outcome { frames: responder.frames().to_vec(), observations, trampoline_sha256, kernel: kernel_release() })
}
