//! Bounded single-owner event loop; no employee program runs in this process.
use std::{io, os::fd::{AsFd, FromRawFd, OwnedFd}, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind, PtySize}, ids::{EmployeeId, RequestId, RunId},
    ipc::{self, CleanupReadback, ControlConnectionId, Envelope, ErrorCode, Opcode, SpawnRequest, SpawnResponse}};
use crate::{admission::{Coordinator, Host, HostProgress, LaunchPolicy, Limits, Ticket, BROKER_REPLY_MS},
    audit::{Detail, Event, Record}, fds::{self, SnapshotAuthority},
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

/// Audit subject of one request, taken before the message is consumed.
#[derive(Clone, Copy)]
struct Subject { op: Opcode, employee: Option<EmployeeId>, run: Option<RunId> }
impl Subject {
    fn of(message: &SpawnRequest) -> Self {
        let (employee, run) = match message {
            SpawnRequest::OpenWorkFile { run, employee_id, .. } => (Some(*employee_id), Some(run.0)),
            SpawnRequest::WorkAnchor { employee_id } | SpawnRequest::StopEmployeeRuns { employee_id }
                | SpawnRequest::EmployeeIdle { employee_id } => (Some(*employee_id), None),
            SpawnRequest::HelperCall { run, employee_id, .. } => (Some(*employee_id), Some(run.0)),
            SpawnRequest::HelperWait { run, .. } | SpawnRequest::HelperStop { run, .. }
                | SpawnRequest::HelperCancel { run, .. } => (None, Some(run.0)),
            _ => (None, None),
        };
        Self { op: message.opcode(), employee, run }
    }
    fn work(self) -> bool { Opcode::WORK.contains(&self.op) }
    fn helper(self) -> bool { Opcode::HELPER.contains(&self.op) }
}
/// H4/HX4: a Package H request (lease check, snapshot copy, stop
/// confirmation) or a `HelperCall` (slot wait, root start) whose reply waits
/// for root work. The client gets no other reply until this one is sent, and
/// it is sent within `BROKER_REPLY_MS`.
struct Deferred { request_id: RequestId, ticket: Ticket, deadline: u64, subject: Subject }
struct Client { socket: Seqpacket, id: ControlConnectionId, last_request: u64, deferred: Option<Deferred> }
enum Dispatched { Now(SpawnResponse, Vec<OwnedFd>), Later(Ticket) }
enum Reply { Now(Envelope<SpawnResponse>, Vec<OwnedFd>, Subject, Option<Detail>), Later(Deferred) }

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
                    clients.push(Client { socket, id, last_request: now, deferred: None });
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied) => {}
                Err(error) => return Err(error),
            }
        }
        let mut index = 0;
        while index < clients.len() {
            let client = &mut clients[index];
            let mut disconnected = false;
            if let Some(deferred) = client.deferred.take() {
                // A deferred reply is pending: answer it (or keep waiting)
                // before reading anything else from this connection.
                client.last_request = now;
                match finish(&mut coordinator, &deferred, now) {
                    Some((response, descriptors, detail)) =>
                        disconnected = send(client, &audit, now, response, descriptors, deferred.subject, detail),
                    None => client.deferred = Some(deferred),
                }
            } else if now.saturating_sub(client.last_request) >= IDLE_CONNECTION_MS {
                disconnected = true;
            } else {
                match client.socket.recv() {
                    Ok(packet) => {
                        client.last_request = now;
                        disconnected = match dispatch(&mut coordinator, client.id, packet, snapshots, now) {
                            Ok(Reply::Now(response, descriptors, subject, detail)) =>
                                send(client, &audit, now, response, descriptors, subject, detail),
                            Ok(Reply::Later(deferred)) => { client.deferred = Some(deferred); false }
                            Err(_) => true,
                        };
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => disconnected = true,
                }
            }
            if disconnected {
                if let Some(deferred) = client.deferred.take() { coordinator.abandon_ticket(deferred.ticket); }
                coordinator.disconnect(client.id, now);
                clients.swap_remove(index);
            } else { index += 1; }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Encode, audit and send one reply. `true` means the connection must close.
fn send(client: &mut Client, audit: &crate::audit::disk::AuditWorker, now: u64, response: Envelope<SpawnResponse>,
    descriptors: Vec<OwnedFd>, subject: Subject, detail: Option<Detail>) -> bool {
    // Exactly the fds the reply's plan names (one for WorkFile, none for an error).
    if response.message.fd_plan().len() != descriptors.len() { return true; }
    if let Some(record) = audit_record(now, &response.message, subject, detail) { let _ = audit.emit(0, record); }
    let borrowed: Vec<_> = descriptors.iter().map(AsFd::as_fd).collect();
    match ipc::encode(&response) {
        Ok(bytes) => client.socket.send(&bytes, &borrowed).is_err(),
        Err(_) => true,
    }
}

/// Event-only audit (no path, file name or content ever reaches it).
fn audit_record(now: u64, message: &SpawnResponse, subject: Subject, detail: Option<Detail>) -> Option<Record> {
    let record = |event: Event, employee: Option<EmployeeId>, run: Option<RunId>, detail: Option<Detail>|
        Some(Record { at_unix_ms: now, event, employee, run, suppressed: 0, detail });
    match message {
        SpawnResponse::Reserved { run, .. } => record(Event::Reserved, None, Some(run.0), None),
        SpawnResponse::Accepted { run, .. } => record(Event::Queued, None, Some(run.0), None),
        SpawnResponse::WorkFile { run, .. } => record(Event::WorkFileOpened, subject.employee, Some(run.0), detail),
        SpawnResponse::EmployeeRunsStopped { employee_id, runs, confirmed } => record(Event::EmployeeRunsStopped,
            Some(*employee_id), None, Some(Detail::Stopped { runs: runs.as_slice().len() as u16, confirmed: *confirmed })),
        SpawnResponse::Error { code } if subject.op == Opcode::OpenWorkFile =>
            record(Event::WorkFileRefused, subject.employee, subject.run, Some(Detail::Code { code: *code })),
        SpawnResponse::Error { code } if subject.work() =>
            record(Event::Refused, subject.employee, subject.run, Some(Detail::Code { code: *code })),
        // HX4: a refused HelperCall is audited; a HelperWait poll that is
        // simply not finished yet is not (the host audits every start and end).
        SpawnResponse::Error { code } if subject.op == Opcode::HelperCall =>
            record(Event::HelperRefused, subject.employee, subject.run, Some(Detail::Code { code: *code })),
        SpawnResponse::Error { code: ErrorCode::NotReady } if subject.op == Opcode::HelperWait => None,
        SpawnResponse::Error { code } if subject.helper() =>
            record(Event::Refused, None, subject.run, Some(Detail::Code { code: *code })),
        SpawnResponse::Error { .. } => record(Event::Refused, None, None, None),
        _ => None,
    }
}

/// Poll a deferred H4 reply. `None` = still pending and within its deadline.
/// Past the deadline the ticket is abandoned (a late snapshot is dropped by
/// the host) and the caller gets `workspace_broker_unavailable`.
fn finish<H: Host<Inputs = Vec<OwnedFd>, Io = Vec<OwnedFd>>>(coordinator: &mut Coordinator<H>, deferred: &Deferred,
    now: u64) -> Option<(Envelope<SpawnResponse>, Vec<OwnedFd>, Option<Detail>)> {
    if deferred.subject.op == Opcode::HelperCall {
        // HX4: `HelperStarted` carries exactly `[StdoutRead, StderrRead]`. The
        // coordinator answers a queued call `helper_busy` at HELPER_QUEUE_MS,
        // before this deadline; this fallback covers a stalled host start.
        let (message, descriptors) = match coordinator.poll_helper_ticket(deferred.ticket, now) {
            Ok(Some(reply)) => (reply.response, reply.pipes.unwrap_or_default()),
            Ok(None) if now < deferred.deadline => return None,
            Ok(None) => {
                coordinator.abandon_ticket(deferred.ticket);
                (SpawnResponse::Error { code: ErrorCode::HelperBusy }, vec![])
            }
            Err(code) => {
                coordinator.abandon_ticket(deferred.ticket);
                (SpawnResponse::Error { code }, vec![])
            }
        };
        return Some((Envelope { version: ipc::PROTOCOL_VERSION, request_id: deferred.request_id, message }, descriptors, None));
    }
    let (message, descriptors, detail) = match coordinator.poll_ticket(deferred.ticket, now) {
        Ok(Some(outcome)) => {
            let descriptors: Vec<OwnedFd> = outcome.file.map(OwnedFd::from).into_iter().collect();
            (outcome.response, descriptors, outcome.detail)
        }
        Ok(None) if now < deferred.deadline => return None,
        Ok(None) => {
            coordinator.abandon_ticket(deferred.ticket);
            (SpawnResponse::Error { code: ErrorCode::WorkspaceBrokerUnavailable }, vec![], None)
        }
        Err(code) => {
            coordinator.abandon_ticket(deferred.ticket);
            (SpawnResponse::Error { code }, vec![], None)
        }
    };
    Some((Envelope { version: ipc::PROTOCOL_VERSION, request_id: deferred.request_id, message }, descriptors, detail))
}

fn dispatch<H: Host<Inputs = Vec<OwnedFd>, Io = Vec<OwnedFd>>>(coordinator: &mut Coordinator<H>,
    connection: ControlConnectionId, packet: crate::transport::Packet, snapshots: SnapshotAuthority,
    now: u64) -> io::Result<Reply> {
    let frame = ipc::decode::<SpawnRequest>(&packet.bytes).map_err(|_| unavailable())?;
    let request_id = frame.request_id;
    let subject = Subject::of(&frame.message);
    let result = (|| -> Result<Dispatched> {
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
                return Ok(Dispatched::Now(SpawnResponse::Io { run, delivery }, descriptors));
            }
            // Package H (H4). The gateway connection is authenticated by the
            // listener (SO_PEERCRED gateway uid/gid) like every other op; the
            // coordinator checks run ownership/state, the host the rest. All
            // four answer asynchronously within BROKER_REPLY_MS.
            SpawnRequest::OpenWorkFile { run, employee_id, path, .. } =>
                return Ok(Dispatched::Later(coordinator.open_work_file(connection, run, employee_id, path, now)?)),
            SpawnRequest::WorkAnchor { employee_id } =>
                return Ok(Dispatched::Later(coordinator.work_anchor(employee_id, now)?)),
            SpawnRequest::StopEmployeeRuns { employee_id } =>
                return Ok(Dispatched::Later(coordinator.stop_employee_runs(employee_id, now)?)),
            SpawnRequest::EmployeeIdle { employee_id } =>
                return Ok(Dispatched::Later(coordinator.employee_idle(employee_id, now)?)),
            // HX4 helper calls. Same listener authentication; the coordinator
            // checks run ownership, `entry.employee == employee_id` [F7],
            // `Running` [F4], remaining run time and the per-run FIFO (Q4); the
            // host re-derives the helper from the run's employee, revalidates the
            // memfd (already checked by `validate_start`) and starts it. Only
            // HelperCall is deferred (≤ BROKER_REPLY_MS; a queued call waits at
            // most HELPER_QUEUE_MS); the call id is drawn here, never the gateway's.
            request @ SpawnRequest::HelperCall { .. } => {
                let call = random_id().map_err(|_| ErrorCode::Internal)?;
                return Ok(Dispatched::Later(coordinator.helper_call(connection, request, packet.fds, call, now)?));
            }
            SpawnRequest::HelperWait { run, call } => coordinator.helper_wait(run, call, connection)?,
            // [F8] per-call only: never Cancel/revoke/kill of the run.
            SpawnRequest::HelperStop { run, call } => { coordinator.helper_stop(run, call, connection, now)?; SpawnResponse::Ack }
            SpawnRequest::HelperCancel { run, call } => { coordinator.helper_cancel(run, call, connection, now)?; SpawnResponse::Ack }
        };
        Ok(Dispatched::Now(response, vec![]))
    })();
    Ok(match result {
        Ok(Dispatched::Now(message, descriptors)) =>
            Reply::Now(Envelope { version: ipc::PROTOCOL_VERSION, request_id, message }, descriptors, subject, None),
        Ok(Dispatched::Later(ticket)) => Reply::Later(Deferred { request_id, ticket,
            deadline: now.saturating_add(BROKER_REPLY_MS), subject }),
        Err(code) => Reply::Now(Envelope { version: ipc::PROTOCOL_VERSION, request_id,
            message: SpawnResponse::Error { code } }, vec![], subject, None),
    })
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
