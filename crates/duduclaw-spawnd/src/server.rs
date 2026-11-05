//! Bounded single-owner event loop; no employee program runs in this process.
use std::{io, os::fd::{AsFd, FromRawFd, OwnedFd}, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind, PtySize}, ids::{EmployeeId, RunId},
    ipc::{self, CleanupReadback, ControlConnectionId, Envelope, ErrorCode, SpawnRequest, SpawnResponse}};
use crate::{admission::{Coordinator, Host, HostProgress, LaunchPolicy, Limits}, fds::{self, SnapshotAuthority},
    readiness::Readiness, transport::{Listener, Seqpacket}, Result};

const MAX_CONNECTIONS: usize = 32;
const IDLE_CONNECTION_MS: u64 = 60_000;
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) { STOP.store(true, Ordering::Relaxed); }

fn unavailable() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "isolation unavailable") }

pub fn harden_process() -> io::Result<()> {
    // No uid conversion occurs in spawnd; the trampoline owns that transition.
    if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 { return Err(unavailable()); }
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0
        || unsafe { libc::prctl(libc::PR_GET_DUMPABLE) } != 0 { return Err(unavailable()); }
    Ok(())
}

pub(crate) fn random_id() -> io::Result<[u8; 16]> {
    let mut id = [0u8; 16];
    let mut done = 0;
    while done < id.len() {
        let size = unsafe { libc::getrandom(id.as_mut_ptr().add(done).cast(), id.len() - done, 0) };
        if size < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted { continue; }
            return Err(error);
        }
        if size == 0 { return Err(unavailable()); }
        done += size as usize;
    }
    Ok(id)
}

pub fn gateway_identity() -> io::Result<(u32, u32)> {
    named_identity(c"duduclaw")
}
pub(crate) fn named_identity(name: &std::ffi::CStr) -> io::Result<(u32, u32)> {
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 64 * 1024];
    let error = unsafe { libc::getpwnam_r(name.as_ptr(), &mut passwd,
        buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
    if error != 0 || result.is_null() || passwd.pw_uid == 0 || passwd.pw_gid == 0 {
        return Err(unavailable());
    }
    Ok((passwd.pw_uid, passwd.pw_gid))
}

pub fn listener(gateway_gid: u32) -> io::Result<Listener> {
    match (std::env::var_os("LISTEN_FDS"), std::env::var_os("LISTEN_PID")) {
        (None, None) => Listener::bind_fixed(gateway_gid),
        (Some(count), Some(pid)) if count == "1" && pid == std::process::id().to_string().as_str() => {
            // LISTEN_PID selects inherited descriptors only; it grants no caller authority.
            if unsafe { libc::fcntl(3, libc::F_GETFD) } < 0 { return Err(unavailable()); }
            Listener::from_activation(unsafe { OwnedFd::from_raw_fd(3) }, gateway_gid)
        }
        _ => Err(unavailable()),
    }
}

struct Client { socket: Seqpacket, id: ControlConnectionId, last_request: u64 }

/// One monotonic epoch domain for coordinator deadlines and asynchronous host
/// receipts. Reading wall time independently in a worker can invent activity in
/// the future after clock adjustment, and boot-relative time is a different unit.
pub(crate) fn coordination_time_ms() -> io::Result<u64> {
    static CLOCK: std::sync::OnceLock<(u64, Instant)> = std::sync::OnceLock::new();
    if CLOCK.get().is_none() {
        let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| unavailable())?
            .as_millis().try_into().map_err(|_| unavailable())?;
        let _ = CLOCK.set((epoch, Instant::now()));
    }
    let (epoch, start) = CLOCK.get().ok_or_else(unavailable)?;
    epoch.checked_add(start.elapsed().as_millis().try_into().map_err(|_| unavailable())?).ok_or_else(unavailable)
}

pub fn serve<H: Host<Inputs = Vec<OwnedFd>, Io = Vec<OwnedFd>>>(listener: Listener,
    gateway: (u32, u32), snapshots: SnapshotAuthority, host: H, limits: Limits,
    audit: crate::audit::disk::AuditWorker) -> io::Result<()> {
    harden_process()?;
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    let handler = SigAction::new(SigHandler::Handler(stop), SaFlags::empty(), SigSet::empty());
    // The handler performs only a lock-free atomic store; no allocation or I/O.
    unsafe { sigaction(Signal::SIGTERM, &handler)?; sigaction(Signal::SIGINT, &handler)?; }
    if snapshots.gateway_uid != gateway.0 { return Err(unavailable()); }
    let mut coordinator = Coordinator::new(host, limits).map_err(|_| unavailable())?;
    coordination_time_ms()?;
    let mut clients: Vec<Client> = Vec::new();
    loop {
        let now = coordination_time_ms()?;
        if STOP.load(Ordering::Relaxed) {
            coordinator.begin_shutdown(now);
            let until = Instant::now() + Duration::from_secs(10);
            while coordinator.has_active_runs() && Instant::now() < until {
                let now = coordination_time_ms()?;
                let _ = coordinator.tick(now);
                std::thread::sleep(Duration::from_millis(25));
            }
            return if coordinator.has_active_runs() { Err(unavailable()) } else { Ok(()) };
        }
        // Host adapters may not block this loop: slow probes/startup belong in
        // adapter-owned jobs with finite deadlines, observed by poll().
        let _ = coordinator.tick(now);
        if clients.len() < MAX_CONNECTIONS {
            match listener.accept_gateway(gateway.0, gateway.1) {
                Ok(socket) => {
                    socket.set_nonblocking(true)?;
                    let id = ControlConnectionId(random_id()?);
                    if clients.iter().any(|client| client.id == id) { return Err(unavailable()); }
                    clients.push(Client { socket, id, last_request: now });
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied) => {}
                Err(error) => return Err(error),
            }
        }
        let mut index = 0;
        while index < clients.len() {
            let client = &mut clients[index];
            let mut disconnected = now.saturating_sub(client.last_request) >= IDLE_CONNECTION_MS;
            if !disconnected {
                match client.socket.recv() {
                    Ok(packet) => {
                        client.last_request = now;
                        let reply = dispatch(&mut coordinator, client.id, packet, snapshots, now);
                        disconnected = match reply {
                            Ok((response, descriptors)) => {
                                use crate::audit::{Event, Record};
                                let (event, run) = match &response.message {
                                    SpawnResponse::Reserved { run, .. } => (Event::Reserved, Some(run.0)),
                                    SpawnResponse::Accepted { run, .. } => (Event::Queued, Some(run.0)),
                                    SpawnResponse::Error { .. } => (Event::Refused, None),
                                    _ => (Event::Suppressed, None),
                                };
                                if !matches!(event, Event::Suppressed) {
                                    let _ = audit.emit(0, Record { at_unix_ms: now, event, employee: None, run, suppressed: 0 });
                                }
                                let borrowed: Vec<_> = descriptors.iter().map(AsFd::as_fd).collect();
                                match ipc::encode(&response) {
                                    Ok(bytes) => client.socket.send(&bytes, &borrowed).is_err(),
                                    Err(_) => true,
                                }
                            }
                            Err(_) => true,
                        };
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => disconnected = true,
                }
            }
            if disconnected {
                coordinator.disconnect(client.id, now);
                clients.swap_remove(index);
            } else { index += 1; }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn dispatch<H: Host<Inputs = Vec<OwnedFd>, Io = Vec<OwnedFd>>>(coordinator: &mut Coordinator<H>,
    connection: ControlConnectionId, packet: crate::transport::Packet, snapshots: SnapshotAuthority,
    now: u64) -> io::Result<(Envelope<SpawnResponse>, Vec<OwnedFd>)> {
    let frame = ipc::decode::<SpawnRequest>(&packet.bytes).map_err(|_| unavailable())?;
    let request_id = frame.request_id;
    let result = (|| -> Result<(SpawnResponse, Vec<OwnedFd>)> {
        fds::validate_start(&frame.message, &packet.fds, snapshots).map_err(|_| ErrorCode::InvalidFds)?;
        let response = match frame.message {
            SpawnRequest::ReserveRun { employee_id } => coordinator.reserve(connection, employee_id,
                RunHandle(RunId(random_id().map_err(|_| ErrorCode::Internal)?)), now)?,
            request @ SpawnRequest::StartRun { .. } => coordinator.start(connection, request, packet.fds, now)?,
            SpawnRequest::Cancel { run } => { coordinator.cancel(run, connection, now)?; SpawnResponse::Ack }
            SpawnRequest::Status { run } => SpawnResponse::Status { run, status: coordinator.status(run, connection)? },
            SpawnRequest::Wait { run, caller_run } => SpawnResponse::Status { run, status: coordinator.wait(run, connection, caller_run)? },
            SpawnRequest::PtyActivity { run } => { coordinator.pty_activity(run, connection, now)?; SpawnResponse::Ack }
            SpawnRequest::Resize { run, size } => { coordinator.resize(run, connection, size)?; SpawnResponse::Ack }
            SpawnRequest::TakeIo { run } => {
                let (delivery, descriptors) = coordinator.take_io(run, connection)?;
                if delivery.fd_plan().len() != descriptors.len() {
                    let _ = coordinator.cancel(run, connection, now);
                    return Err(ErrorCode::InvalidFds);
                }
                return Ok((SpawnResponse::Io { run, delivery }, descriptors));
            }
        };
        Ok((response, vec![]))
    })();
    let (message, descriptors) = result.unwrap_or_else(|code| (SpawnResponse::Error { code }, vec![]));
    Ok((Envelope { version: ipc::PROTOCOL_VERSION, request_id, message }, descriptors))
}

/// Explicit B2/C/H composition seam. Production cannot set this to ready with a
/// configuration flag. Replace with verified adapters, not optimistic defaults.
pub struct UnavailableHost;
impl UnavailableHost { pub fn readiness(&self) -> Readiness { Readiness::default() } }
impl Host for UnavailableHost {
    type Inputs = Vec<OwnedFd>;
    type Io = Vec<OwnedFd>;
    fn ready(&mut self, _: EmployeeId) -> Result<()> { self.readiness().check() }
    fn policy(&mut self, _: EmployeeId, _: &RuntimeKind, _: bool) -> Result<LaunchPolicy> { Err(ErrorCode::IsolationUnavailable) }
    fn cleanup(&mut self, _: EmployeeId, _: Option<RunHandle>) -> Result<CleanupReadback> { Err(ErrorCode::IsolationUnavailable) }
    fn start(&mut self, _: RunHandle, _: EmployeeId, _: &SpawnRequest, _: &LaunchPolicy, _: Self::Inputs) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn poll(&mut self, _: RunHandle) -> Result<HostProgress> { Err(ErrorCode::IsolationUnavailable) }
    fn revoke(&mut self, _: RunHandle) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn kill(&mut self, _: RunHandle) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn take_io(&mut self, _: RunHandle) -> Result<Self::Io> { Err(ErrorCode::IsolationUnavailable) }
    fn resize(&mut self, _: RunHandle, _: PtySize) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> { Err(ErrorCode::IsolationUnavailable) }
    fn forget(&mut self, _: RunHandle) -> Result<()> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    #[test]
    fn missing_adapters_refuse_every_launch_and_readiness() {
        assert_eq!(UnavailableHost.ready(EmployeeId([1; 16])), Err(ErrorCode::IsolationUnavailable));
        assert!(UnavailableHost.readiness().check().is_err());
        assert_ne!(random_id().unwrap(), random_id().unwrap());
    }
    #[test]
    #[ignore = "iso_root_: disposable Linux VM; root socket parent and duduclaw account; socket unit stopped"]
    fn iso_root_fixed_socket_bind() {
        use std::os::unix::fs::MetadataExt;
        harden_process().unwrap();
        crate::transport::prepare_runtime_directory().unwrap();
        let (_, gid) = gateway_identity().unwrap();
        let listener = Listener::bind_fixed(gid).unwrap();
        let created = std::fs::symlink_metadata(duduclaw_isolation::ids::SPAWND_SOCKET).unwrap();
        assert_eq!((created.uid(), created.gid(), created.mode() & 0o7777), (0, gid, 0o660));
        assert!(Listener::bind_fixed(gid).is_err());
        let activated = Listener::from_activation(listener.as_fd().try_clone_to_owned().unwrap(), gid).unwrap();
        drop(activated);
        assert!(listener.as_raw_fd() >= 0);
        drop(listener);
        let current = std::fs::symlink_metadata(duduclaw_isolation::ids::SPAWND_SOCKET).unwrap();
        assert_eq!((created.dev(), created.ino()), (current.dev(), current.ino()));
        let recovered = Listener::bind_fixed(gid).unwrap();
        drop(recovered);
        std::fs::remove_file(duduclaw_isolation::ids::SPAWND_SOCKET).unwrap();
    }
}
