//! Host-wide single-slot worker admission with durable uncertain retirement.
//! The installer creates the fixed root-owned lease; a dirty record is never
//! expired by a timer. Only unit/cgroup/pidfd readback releases the reservation.
use std::{fs::{File, OpenOptions}, io::{Read, Seek, SeekFrom, Write},
    os::{fd::{AsRawFd, OwnedFd}, unix::fs::{MetadataExt, OpenOptionsExt}},
    path::{Component, Path, PathBuf}, time::{Duration, Instant}};
use duduclaw_isolation::worker::{ParseError, ParseRequest, ParseResponse};
use zbus::{blocking::{connection::Builder, Connection, Proxy}, proxy::CacheProperties,
    zvariant::OwnedObjectPath};
use crate::client::{WorkerLauncher, WorkerSession};

pub use crate::retirement_hello::{is_hello, answer as answer_hello};
pub const LEASE_PATH: &str = "/run/duduclaw-parse/retirement.lease";
const BUS: &str = "org.freedesktop.systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
fn unavailable<T>(_: T) -> ParseError { ParseError::Unavailable }

struct Lease(File);
impl Lease {
    fn acquire() -> Result<Self, ParseError> {
        for path in ["/run", "/run/duduclaw-parse"] {
            let m = std::fs::symlink_metadata(path).map_err(unavailable)?;
            if !m.is_dir() || m.uid() != 0 || m.mode() & 0o022 != 0 { return Err(ParseError::Unavailable); }
        }
        let mut file = OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(LEASE_PATH).map_err(unavailable)?;
        let m = file.metadata().map_err(unavailable)?;
        if !m.is_file() || m.uid() != 0 || m.nlink() != 1 || m.mode() & 0o7777 != 0o660
            || m.len() > 64 { return Err(ParseError::Unavailable); }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(ParseError::Unavailable);
        }
        let mut state = String::new(); (&mut file).take(65).read_to_string(&mut state).map_err(unavailable)?;
        if state != "clean\n" { return Err(ParseError::Unavailable); }
        let mut lease = Self(file);
        // Persist before connect: a crash or unidentifiable queued activation
        // must consume the slot until operator recovery proves all work absent.
        lease.write(b"dirty\n")?;
        Ok(lease)
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), ParseError> {
        self.0.seek(SeekFrom::Start(0)).map_err(unavailable)?;
        self.0.write_all(bytes).map_err(unavailable)?;
        self.0.set_len(bytes.len() as u64).map_err(unavailable)?;
        self.0.sync_all().map_err(unavailable)
    }
    fn release(mut self) -> Result<(), ParseError> { self.write(b"clean\n") }
}

/// No caller-selected socket, unit, cgroup or accounting ceiling is accepted.
/// Protected boot must install this launcher, not SocketActivatedLauncher.
pub struct RetiringLauncher;
impl WorkerLauncher for RetiringLauncher {
    /// Its session runs the shared checked exchange (transport::exchange_checked)
    /// and retires the worker on every failure of it.
    fn supports_live_authority(&self) -> bool { true }

    fn launch(&self, request: &ParseRequest, input_reservation_bytes: u64)
        -> Result<Box<dyn WorkerSession>, ParseError> {
        crate::policy::validate(request)?;
        if input_reservation_bytes > crate::policy::MAX_FILE_BYTES
            || request.inputs.as_slice().iter().map(|input| input.size_bytes).sum::<u64>() > input_reservation_bytes {
            return Err(ParseError::LimitExceeded);
        }
        // One global slot reserves the full 64 MiB input and fixed 512 MiB
        // worker ceiling even when the caller's initial input length is zero.
        let lease = Lease::acquire()?;
        let connection = Builder::system().map_err(unavailable)?
            .method_timeout(Duration::from_secs(2)).build().map_err(unavailable)?;
        let fd = crate::transport::connect()?;
        let hello = crate::retirement_hello::begin(&fd, Instant::now() + Duration::from_secs(5))?;
        let owner = WorkerOwner::identify(connection, hello)?;
        Ok(Box::new(Session { fd: Some(fd), owner: Some(owner), lease: Some(lease),
            deadline: Instant::now() + Duration::from_millis(request.limits.wall_time_ms.get() as u64),
            exchanged: false }))
    }
}

fn proxy<'a>(connection: &'a Connection, path: &'a str, interface: &'a str) -> Result<Proxy<'a>, ParseError> {
    zbus::blocking::proxy::Builder::new(connection).destination(BUS).map_err(unavailable)?
        .path(path).map_err(unavailable)?.interface(interface).map_err(unavailable)?
        .cache_properties(CacheProperties::No).build().map_err(unavailable)
}
fn unit_name(name: &str) -> bool {
    name.strip_prefix("duduclaw-parse@").and_then(|s| s.strip_suffix(".service"))
        .is_some_and(|instance| !instance.is_empty() && instance.len() <= 200
            && instance.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'\\' | b':')))
}
fn cgroup_path(raw: &str, unit: &str) -> Result<PathBuf, ParseError> {
    let path = Path::new(raw);
    if !path.is_absolute() || path.file_name().and_then(|s| s.to_str()) != Some(unit)
        || path.components().any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || !path.starts_with("/system.slice") { return Err(ParseError::Unavailable); }
    Ok(Path::new("/sys/fs/cgroup").join(path.strip_prefix("/").map_err(unavailable)?))
}
struct WorkerOwner { connection: Connection, path: OwnedObjectPath, name: String,
    invocation: Vec<u8>, cgroup: PathBuf, pidfd: OwnedFd }
impl WorkerOwner {
    fn identify(connection: Connection, hello: crate::retirement_hello::Hello) -> Result<Self, ParseError> {
        let manager = proxy(&connection, "/org/freedesktop/systemd1", MANAGER)?;
        let path: OwnedObjectPath = manager.call("GetUnitByPID", &(hello.pid,)).map_err(unavailable)?;
        let unit = proxy(&connection, path.as_str(), "org.freedesktop.systemd1.Unit")?;
        let service = proxy(&connection, path.as_str(), "org.freedesktop.systemd1.Service")?;
        let name: String = unit.get_property("Id").map_err(unavailable)?;
        let invocation: Vec<u8> = unit.get_property("InvocationID").map_err(unavailable)?;
        let pid: u32 = service.get_property("MainPID").map_err(unavailable)?;
        let dynamic: bool = service.get_property("DynamicUser").map_err(unavailable)?;
        let private_network: bool = service.get_property("PrivateNetwork").map_err(unavailable)?;
        let nnp: bool = service.get_property("NoNewPrivileges").map_err(unavailable)?;
        let tasks: u64 = service.get_property("TasksMax").map_err(unavailable)?;
        let memory: u64 = service.get_property("MemoryMax").map_err(unavailable)?;
        let cgroup: String = service.get_property("ControlGroup").map_err(unavailable)?;
        if !unit_name(&name) || invocation.len() != 16 || invocation.iter().all(|byte| *byte == 0)
            || pid != hello.pid || hello.uid == 0 || !dynamic || !private_network || !nnp
            || tasks != 1 || memory == 0 || memory > crate::policy::MAX_MEMORY_BYTES
            || crate::retirement_hello::exited(&hello.pidfd)? { return Err(ParseError::Unavailable); }
        let cgroup = cgroup_path(&cgroup, &name)?;
        // Do not add unit references: duplicate RefUnit accounting has wedged
        // systemd 249 in native acceptance. Retirement re-resolves by exact name.
        drop(service); drop(unit); drop(manager);
        Ok(Self { connection, path, name, invocation, cgroup, pidfd: hello.pidfd })
    }
    fn retired(&self) -> Result<bool, ParseError> {
        if !crate::retirement_hello::exited(&self.pidfd)? { return Ok(false); }
        // Verify the cgroup-v2 anchor itself remains mounted. ENOENT below an
        // unavailable/unmounted hierarchy must never be read as retirement.
        let fs = nix::sys::statfs::statfs("/sys/fs/cgroup").map_err(unavailable)?;
        if fs.filesystem_type().0 != 0x6367_7270 { return Err(ParseError::Unavailable); }
        let manager = proxy(&self.connection, "/org/freedesktop/systemd1", MANAGER)?;
        let path: OwnedObjectPath = match manager.call("GetUnit", &(self.name.as_str(),)) {
            Ok(path) => path,
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit" => {
                    // Explicit PID 1 absence is necessary but not sufficient:
                    // the kernel pidfd must be exited and the exact recorded
                    // cgroup directory must be gone, not just events unreadable.
                    return cgroup_removed(&self.cgroup);
                }
            Err(_) => return Err(ParseError::Unavailable),
        };
        if path != self.path { return Err(ParseError::Unavailable); }
        let unit = proxy(&self.connection, path.as_str(), "org.freedesktop.systemd1.Unit")?;
        let name: String = unit.get_property("Id").map_err(unavailable)?;
        let invocation: Vec<u8> = unit.get_property("InvocationID").map_err(unavailable)?;
        let state: String = unit.get_property("ActiveState").map_err(unavailable)?;
        if name != self.name || invocation != self.invocation { return Err(ParseError::Unavailable); }
        if !matches!(state.as_str(), "inactive" | "failed") { return Ok(false); }
        match std::fs::read_to_string(self.cgroup.join("cgroup.events")) {
            Ok(text) => parse_populated(&text).map(|populated| !populated),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => cgroup_removed(&self.cgroup),
            Err(_) => Err(ParseError::Unavailable),
        }
    }
}
fn cgroup_removed(path: &Path) -> Result<bool, ParseError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Ok(metadata) if metadata.is_dir() => Ok(false),
        _ => Err(ParseError::Unavailable),
    }
}
fn parse_populated(text: &str) -> Result<bool, ParseError> {
    let rows: Vec<_> = text.lines().filter_map(|line| line.strip_prefix("populated ")).collect();
    match rows.as_slice() { ["0"] => Ok(false), ["1"] => Ok(true), _ => Err(ParseError::Unavailable) }
}

struct Session { fd: Option<OwnedFd>, owner: Option<WorkerOwner>, lease: Option<Lease>, deadline: Instant, exchanged: bool }
impl WorkerSession for Session {
    fn exchange(&mut self, request: &ParseRequest, files: &[File]) -> Result<ParseResponse, ParseError> {
        if self.exchanged { return Err(ParseError::Unavailable); }
        self.exchanged = true;
        let fd = self.fd.as_ref().ok_or(ParseError::Unavailable)?;
        let payload = serde_json::to_vec(request).map_err(unavailable)?;
        crate::transport::send(fd, &payload, files, self.deadline)?;
        let (payload, rights) = crate::transport::receive(fd, 0, self.deadline)?;
        if !rights.is_empty() { return Err(ParseError::InvalidContent); }
        ParseResponse::decode_payload(&payload, request).map_err(|_| ParseError::InvalidContent)
    }
    /// Live authority: the same one-shot guard as `exchange`, then the shared
    /// checked sequence. Any failure (guard, revoked/expired verify, transport,
    /// decode) hands the worker to retirement at once; the lease stays dirty
    /// until readback proves the unit gone, exactly as on Drop.
    fn exchange_checked(&mut self, request: &ParseRequest, files: &[File],
        verify: &dyn Fn() -> Result<(), ParseError>) -> Result<ParseResponse, ParseError> {
        let result = self.exchange_checked_once(request, files, verify);
        if result.is_err() { self.retire(); }
        result
    }
}
impl Session {
    fn exchange_checked_once(&mut self, request: &ParseRequest, files: &[File],
        verify: &dyn Fn() -> Result<(), ParseError>) -> Result<ParseResponse, ParseError> {
        if self.exchanged { return Err(ParseError::Unavailable); }
        self.exchanged = true;
        let fd = self.fd.as_ref().ok_or(ParseError::Unavailable)?;
        crate::transport::exchange_checked(fd, request, files, self.deadline, verify)
    }
    /// Idempotent: closes the socket (EOF starts worker teardown) and moves the
    /// owner + dirty lease to the named "parse-retirement" thread, which
    /// releases the lease only on exact readback. Without an identified owner
    /// the lease is never released (it stays dirty when dropped).
    fn retire(&mut self) {
        self.fd.take(); // EOF starts teardown even if snapshot/exchange failed.
        let Some(owner) = self.owner.take() else { return; };
        let Some(lease) = self.lease.take() else { return; };
        // Cancellation does not wait on a Tokio executor. Thread creation,
        // readback failure or timeout leave the durable dirty reservation intact.
        let _ = std::thread::Builder::new().name("parse-retirement".into()).spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(40);
            while Instant::now() < deadline {
                match owner.retired() {
                    Ok(true) => { let _ = lease.release(); return; }
                    Ok(false) => std::thread::sleep(Duration::from_millis(50)),
                    // Transient GC/property races may recover on the next
                    // exact GetUnit lookup; no failed observation releases it.
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        });
    }
}
impl Drop for Session {
    fn drop(&mut self) { self.retire(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retirement_requires_exact_empty_readback() {
        assert_eq!(parse_populated("populated 0\nfrozen 0\n"), Ok(false));
        assert_eq!(parse_populated("populated 1\n"), Ok(true));
        for text in ["", "populated 0\npopulated 1", "populated 2", "notpopulated 0"] {
            assert!(parse_populated(text).is_err());
        }
    }
    #[test]
    fn missing_events_is_not_a_removed_cgroup_and_errors_keep_the_lease_dirty() {
        let directory = tempfile::tempdir().unwrap();
        // An existing cgroup with a missing events file cannot be released by
        // the NoSuchUnit branch; only removal of the directory is sufficient.
        assert_eq!(cgroup_removed(directory.path()), Ok(false));
        let path = directory.path().to_owned();
        directory.close().unwrap();
        assert_eq!(cgroup_removed(&path), Ok(true));

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"clean\n").unwrap();
        let mut lease = Lease(file.reopen().unwrap());
        lease.write(b"dirty\n").unwrap();
        drop(lease); // hello/reaper errors and thread-spawn failure take this path.
        assert_eq!(std::fs::read(file.path()).unwrap(), b"dirty\n");
    }
    #[test]
    fn worker_identity_and_cgroup_are_exact() {
        let name = "duduclaw-parse@3-peer.service";
        assert!(unit_name(name)); assert!(cgroup_path(&format!("/system.slice/{name}"), name).is_ok());
        for name in ["evil-duduclaw-parse@3.service", "duduclaw-parse@.service", "duduclaw-parse@../x.service"] { assert!(!unit_name(name)); }
        for path in ["/system.slice/../x", "/system.slice.evil/x", "relative", "/system.slice/other.service"] {
            assert!(cgroup_path(path, name).is_err());
        }
    }

    // --- live authority (R8n-2 A) -------------------------------------------
    // A real WorkerOwner needs PID 1 over the system bus, so these sessions
    // carry no owner: retirement then closes the socket (EOF to the worker)
    // and the dirty lease is dropped unreleased, the fail-closed branch of
    // `retire`. The readback-then-release thread itself needs systemd.
    use std::cell::RefCell;
    use duduclaw_isolation::{bounded::{List, Text}, worker::{ContentFormat, ParseOperation,
        ParseResult, ParsedItem, WORKER_PROTOCOL_VERSION}};
    use nix::sys::socket::{recv, socketpair, AddressFamily, MsgFlags, SockFlag, SockType};
    use std::os::fd::RawFd;

    fn session_with_lease() -> (Session, OwnedFd, tempfile::NamedTempFile) {
        let (local, peer) = socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
            SockFlag::SOCK_CLOEXEC).unwrap();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"clean\n").unwrap();
        let mut lease = Lease(file.reopen().unwrap());
        lease.write(b"dirty\n").unwrap();
        (Session { fd: Some(local), owner: None, lease: Some(lease),
            deadline: Instant::now() + Duration::from_secs(2), exchanged: false }, peer, file)
    }
    fn request_and_reply() -> (ParseRequest, Vec<u8>) {
        let request = crate::client::request(ParseOperation::ExtractDocument, ContentFormat::Text, 0);
        let response = ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Complete { items: List::new(vec![ParsedItem::Text {
                input_slot: 0, page: None, text: Text::new("ok".into()).unwrap() }]).unwrap() } };
        let reply = crate::response::encode(&response, &request).unwrap();
        (request, reply)
    }
    fn pending(fd: RawFd) -> bool {
        let mut byte = [0u8; 1];
        matches!(recv(fd, &mut byte, MsgFlags::MSG_PEEK | MsgFlags::MSG_DONTWAIT), Ok(n) if n > 0)
    }
    /// Records (request sent to the worker?, reply still unreceived?) at each
    /// verify; fails the `fail_at`-th call (1-based).
    fn recording(calls: &RefCell<Vec<(bool, bool)>>, local: RawFd, peer: RawFd, fail_at: Option<usize>)
        -> impl Fn() -> Result<(), ParseError> + '_ {
        move || {
            let mut calls = calls.borrow_mut();
            calls.push((pending(peer), pending(local)));
            if Some(calls.len()) == fail_at { Err(ParseError::Unavailable) } else { Ok(()) }
        }
    }
    /// Packet sizes the worker end received until EOF. Closing a seqpacket
    /// socket with an unread reply sets ECONNRESET on its peer once; skip it.
    fn drained(peer: &OwnedFd) -> Vec<usize> {
        let mut buffer = vec![0u8; 64 * 1024];
        let mut packets = Vec::new();
        loop {
            match recv(peer.as_raw_fd(), &mut buffer, MsgFlags::MSG_DONTWAIT) {
                Ok(0) => return packets,
                Ok(n) => packets.push(n),
                Err(nix::errno::Errno::ECONNRESET) => continue,
                Err(error) => panic!("worker end: {error}"),
            }
        }
    }
    fn expected_sequence() -> Vec<(bool, bool)> {
        // 3 before the send (wait start, after POLLOUT, before sendmsg), 3
        // before the receive (wait start, after POLLIN, before recvmsg), and
        // one after the reply is received, before it is decoded.
        [vec![(false, true); 3], vec![(true, true); 3], vec![(true, false)]].concat()
    }

    #[test]
    fn retiring_launcher_reports_live_authority() {
        assert!(RetiringLauncher.supports_live_authority());
    }

    #[test]
    fn checked_exchange_verifies_before_send_before_receive_and_after_reply() {
        let (mut session, peer, lease_file) = session_with_lease();
        let (request, reply) = request_and_reply();
        crate::transport::send(&peer, &reply, &[], Instant::now() + Duration::from_secs(2)).unwrap();
        let local = session.fd.as_ref().unwrap().as_raw_fd();
        let calls = RefCell::new(Vec::new());
        let response = session.exchange_checked(&request, &[],
            &recording(&calls, local, peer.as_raw_fd(), None)).unwrap();
        assert_eq!(response.request_id, request.request_id);
        assert_eq!(*calls.borrow(), expected_sequence());
        assert!(session.fd.is_some() && session.lease.is_some(), "success retires on Drop, not before");

        // One-shot guard: a second exchange is refused without any verify or
        // I/O, and the refusal itself hands the worker to retirement.
        let before = calls.borrow().len();
        assert!(matches!(session.exchange_checked(&request, &[],
            &recording(&calls, local, peer.as_raw_fd(), None)), Err(ParseError::Unavailable)));
        assert_eq!(calls.borrow().len(), before);
        assert!(session.fd.is_none(), "refused exchange closes the worker socket");
        drop(session);
        assert_eq!(std::fs::read(lease_file.path()).unwrap(), b"dirty\n");
    }

    #[test]
    fn checked_exchange_failing_verify_sends_or_accepts_nothing_and_retires() {
        for fail_at in 1..=expected_sequence().len() {
            let (mut session, peer, lease_file) = session_with_lease();
            let (request, reply) = request_and_reply();
            crate::transport::send(&peer, &reply, &[], Instant::now() + Duration::from_secs(2)).unwrap();
            let input = crate::transport::snapshot(&mut &b"private input"[..]).unwrap();
            let local = session.fd.as_ref().unwrap().as_raw_fd();
            let calls = RefCell::new(Vec::new());
            let result = session.exchange_checked(&request, std::slice::from_ref(&input),
                &recording(&calls, local, peer.as_raw_fd(), Some(fail_at)));
            // Fails closed with the socket session's error kind; no reply is returned.
            assert!(matches!(result, Err(ParseError::Unavailable)), "fail_at={fail_at}");
            assert_eq!(*calls.borrow(), expected_sequence()[..fail_at], "fail_at={fail_at}");
            // Retirement ran inside exchange_checked: the worker socket is closed
            // now, so the worker end reaches EOF without waiting for a Drop.
            assert!(session.fd.is_none(), "fail_at={fail_at}");
            let received = drained(&peer);
            if fail_at <= 3 {
                // Revoked before sendmsg: neither the request nor the input fd left.
                assert!(received.is_empty(), "fail_at={fail_at}: {received:?}");
            } else {
                assert_eq!(received.len(), 1, "fail_at={fail_at}: the request was sent before revocation");
            }
            drop(session);
            // No owner readback ⇒ the reservation is never released.
            assert_eq!(std::fs::read(lease_file.path()).unwrap(), b"dirty\n", "fail_at={fail_at}");
        }
    }
}
