//! Bounded seqpacket RPC and fd ownership. Only this module receives SCM_RIGHTS.
use std::{io::{self, IoSlice}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::{FileTypeExt, MetadataExt}}, path::Path, time::Duration};
use duduclaw_isolation::{execution::BackendError, ids::{RequestId, SPAWND_SOCKET}, ipc::{self, Envelope, ErrorCode, FdRole, RunStatus, SpawnRequest, SpawnResponse}};
use nix::{fcntl::{fcntl, FcntlArg, FdFlag, OFlag}, sys::{socket::{self, AddressFamily, ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr, sockopt}, stat::{fstat, SFlag}}};
use std::sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, Ordering}};
#[cfg(test)]
use tokio::io::unix::AsyncFd;
type LiveCheck = dyn Fn() -> Result<(), BackendError> + Send + Sync;

pub(super) struct Connection {
    io: Arc<ControlIo>,
    // Gateway-local identity for this authenticated connection, never sent by
    // an employee and never reused after reconnecting to the fixed endpoint.
    pub(super) id: ipc::ControlConnectionId,
}
pub(super) struct Packet { pub response: SpawnResponse, pub fds: Vec<OwnedFd> }

pub(super) fn connect() -> Result<Connection, BackendError> {
    // /run and the service directory must be root-owned, non-symlink directories
    // without group/other write. The socket itself may grant the gateway group
    // write access, but never world access.
    for directory in [Path::new("/run"), Path::new("/run/duduclaw-spawnd")] {
        let metadata = std::fs::symlink_metadata(directory).map_err(|_| BackendError::Unavailable)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(BackendError::Denied);
        }
    }
    let metadata = std::fs::symlink_metadata(SPAWND_SOCKET).map_err(|_| BackendError::Unavailable)?;
    if !metadata.file_type().is_socket() || metadata.uid() != 0 || metadata.mode() & 0o007 != 0 {
        return Err(BackendError::Denied);
    }
    let fd = socket::socket(AddressFamily::Unix, SockType::SeqPacket,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None).map_err(|_| BackendError::Unavailable)?;
    let address = UnixAddr::new(SPAWND_SOCKET).map_err(|_| BackendError::Unavailable)?;
    // AF_UNIX nonblocking connect fails with EAGAIN when the listener is full;
    // there is no unauthenticated blocking connect or path-based fallback.
    socket::connect(fd.as_raw_fd(), &address).map_err(|_| BackendError::Unavailable)?;
    authenticate_peer(&fd, 0, 0)?;
    Ok(Connection::from_fd(fd, ipc::ControlConnectionId(*uuid::Uuid::new_v4().as_bytes())))
}

fn authenticate_peer(fd: &OwnedFd, uid: u32, gid: u32) -> Result<(), BackendError> {
    let peer = socket::getsockopt(fd, sockopt::PeerCredentials).map_err(|_| BackendError::Unavailable)?;
    let domain = socket_domain(fd)?;
    let kind = socket::getsockopt(fd, sockopt::SockType).map_err(|_| BackendError::Unavailable)?;
    if peer.uid() != uid || peer.gid() != gid || domain != libc::AF_UNIX || kind != SockType::SeqPacket {
        return Err(BackendError::Denied);
    }
    Ok(())
}

fn socket_domain(fd: &OwnedFd) -> Result<libc::c_int, BackendError> {
    let mut domain: libc::c_int = 0;
    let mut length = std::mem::size_of_val(&domain) as libc::socklen_t;
    // nix 0.29 has no SO_DOMAIN option. Both pointers are valid for their exact
    // typed lengths; the kernel can only write within the supplied domain slot.
    let result = unsafe { libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_DOMAIN,
        (&mut domain as *mut libc::c_int).cast(), &mut length) };
    if result != 0 || length as usize != std::mem::size_of_val(&domain) { return Err(BackendError::Protocol); }
    Ok(domain)
}

// Only the transaction owner touches packet I/O. The original authenticated
// socket stays alive in this Arc; shutdown is safe during another transaction
// and immediately fences root without racing a recycled raw descriptor number.
struct ControlIo {
    fd: OwnedFd,
    busy: Mutex<bool>,
    available: Condvar,
    closed: AtomicBool,
}
impl ControlIo {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = socket::shutdown(self.fd.as_raw_fd(), socket::Shutdown::Both);
        }
        // Pair notification with the gate mutex so cancellation cannot be lost
        // between a waiter's predicate check and its condvar sleep.
        let _busy = self.busy.lock().unwrap_or_else(|error| error.into_inner());
        self.available.notify_all();
    }
    fn acquire(&self, deadline: std::time::Instant) -> Result<Transaction<'_>, BackendError> {
        let mut busy = self.busy.lock().map_err(|_| BackendError::Unavailable)?;
        loop {
            if self.closed.load(Ordering::Acquire) { return Err(BackendError::Unavailable); }
            let remaining = deadline.checked_duration_since(std::time::Instant::now())
                .ok_or(BackendError::Timeout)?;
            if !*busy {
                *busy = true;
                return Ok(Transaction { io: self, completed: false, cancel: None });
            }
            let (next, _) = self.available.wait_timeout(busy, remaining).map_err(|_| BackendError::Unavailable)?;
            busy = next;
        }
    }
    fn transact<'a>(&'a self, request: SpawnRequest, input_fds: Vec<OwnedFd>, deadline: std::time::Instant, check: &LiveCheck,
        cancel: Option<&'a CancelControl>) -> Result<Packet, BackendError> {
        let result = (|| {
            let mut transaction = self.acquire(deadline)?;
            transaction.cancel = cancel;
            match cancel { Some(control) => control.verify()?, None => check()? };
            let request_id = RequestId(*uuid::Uuid::new_v4().as_bytes());
            let frame = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id, message: &request })
                .map_err(|_| BackendError::Protocol)?;
            let raw: Vec<_> = input_fds.iter().map(AsRawFd::as_raw_fd).collect();
            let controls = if raw.is_empty() { vec![] } else { vec![ControlMessage::ScmRights(&raw)] };
            loop {
                if self.closed.load(Ordering::Acquire) { return Err(BackendError::Unavailable); }
                if std::time::Instant::now() >= deadline { return Err(BackendError::Timeout); }
                let send = || socket::sendmsg::<UnixAddr>(self.fd.as_raw_fd(), &[IoSlice::new(&frame)], &controls,
                    MsgFlags::MSG_NOSIGNAL | MsgFlags::MSG_DONTWAIT, None);
                let sent = match cancel {
                    Some(control) => control.send(send)?,
                    None => { check()?; send() },
                };
                match sent {
                    Ok(bytes) if bytes == frame.len() => break,
                    Ok(_) => return Err(BackendError::Protocol),
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(nix::errno::Errno::EAGAIN) => poll_until(&self.fd, libc::POLLOUT, deadline)?,
                    Err(_) => return Err(BackendError::Io),
                }
            }
            let (bytes, fds) = loop {
                poll_until(&self.fd, libc::POLLIN, deadline)?;
                match receive_packet(&self.fd) {
                    Ok(packet) => break packet,
                    Err(error) if matches!(error.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => continue,
                    Err(error) if error.kind() == io::ErrorKind::InvalidData => return Err(BackendError::Protocol),
                    Err(_) => return Err(BackendError::Io),
                }
            };
            let envelope: Envelope<SpawnResponse> = ipc::decode(&bytes).map_err(|_| BackendError::Protocol)?;
            if envelope.request_id != request_id { return Err(BackendError::Protocol); }
            validate_response(&request, &envelope.message)?;
            validate_fds(&envelope.message.fd_plan(), &fds)?;
            if self.closed.load(Ordering::Acquire) { return Err(BackendError::Unavailable); }
            check()?;
            transaction.completed = true;
            Ok(Packet { response: envelope.message, fds })
        })();
        if result.is_err() && !cancel.is_some_and(CancelControl::preserve_connection) { self.close(); }
        result
    }
}
struct Transaction<'a> { io: &'a ControlIo, completed: bool, cancel: Option<&'a CancelControl> }
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        // Fence failure before releasing the gate; no waiter can send a new
        // packet in the gap between a bad response and shutdown.
        if !self.completed && !self.cancel.is_some_and(CancelControl::preserve_connection) { self.io.close(); }
        let mut busy = self.io.busy.lock().unwrap_or_else(|error| error.into_inner());
        *busy = false;
        self.io.available.notify_all();
    }
}
// Cancelling a request to cancel another job is different from abandoning a
// spawn: refusal before send must not terminate that job through control EOF.
// One mutex orders caller abandonment against the actual nonblocking send.
#[derive(Default)]
struct CancelState { abandoned: bool, rejected: bool, sent: bool }
struct CancelControl { state: Mutex<CancelState>, check: Arc<LiveCheck> }
impl CancelControl {
    fn verify_locked(&self, state: &mut CancelState) -> Result<(), BackendError> {
        if state.abandoned || state.rejected { return Err(BackendError::Denied); }
        if let Err(error) = (self.check)() { state.rejected = true; return Err(error); }
        Ok(())
    }
    fn verify(&self) -> Result<(), BackendError> {
        let mut state = self.state.lock().map_err(|_| BackendError::Unavailable)?;
        self.verify_locked(&mut state)
    }
    fn send(&self, send: impl FnOnce() -> Result<usize, nix::errno::Errno>)
        -> Result<Result<usize, nix::errno::Errno>, BackendError> {
        let mut state = self.state.lock().map_err(|_| BackendError::Unavailable)?;
        self.verify_locked(&mut state)?;
        let result = send();
        if result.is_ok() { state.sent = true; }
        Ok(result)
    }
    fn preserve_connection(&self) -> bool {
        self.state.lock().is_ok_and(|state| !state.sent)
    }
}
struct CancelControlGuard { io: Arc<ControlIo>, control: Arc<CancelControl>, completed: bool }
impl Drop for CancelControlGuard {
    fn drop(&mut self) {
        if self.completed { return; }
        let sent = match self.control.state.lock() {
            Ok(mut state) => { state.abandoned = true; state.sent },
            Err(_) => true,
        };
        if sent { self.io.close(); }
    }
}

struct CancelTransaction(Option<Arc<ControlIo>>);
impl Drop for CancelTransaction {
    fn drop(&mut self) { if let Some(io) = &self.0 { io.close(); } }
}

impl Connection {
    fn from_fd(fd: OwnedFd, id: ipc::ControlConnectionId) -> Self {
        Self { io: Arc::new(ControlIo { fd, busy: Mutex::new(false), available: Condvar::new(),
            closed: AtomicBool::new(false) }), id }
    }
    #[cfg(test)]
    pub(super) fn test_connection(fd: OwnedFd) -> Self {
        Self::from_fd(fd, ipc::ControlConnectionId([17; 16]))
    }
    #[cfg(test)]
    pub(super) fn test_pair() -> (Self, OwnedFd) {
        let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK).unwrap();
        (Self::from_fd(left, ipc::ControlConnectionId([17; 16])), right)
    }

    pub async fn close(&self) { self.io.close(); }
    pub fn is_closed(&self) -> bool { self.io.closed.load(Ordering::Acquire) }

    /// Fresh Status serialized with async RPCs on the original control socket.
    /// The entire wait plus I/O budget is 250 ms. Async I/O runs independently
    /// in the blocking pool, so even a current-thread runtime can make progress.
    pub(super) fn live_status(&self, run: duduclaw_isolation::execution::RunHandle,
        reservation_expiry: u64) -> Result<RunStatus, BackendError> {
        let packet = self.io.transact(SpawnRequest::Status { run }, vec![],
            std::time::Instant::now() + Duration::from_millis(250), &|| Ok(()), None)?;
        let result = match packet.response {
            SpawnResponse::Status { status, .. } => {
                if let RunStatus::Reserved { expires_at_unix_ms } = status {
                    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                        Ok(value) => value.as_millis(),
                        Err(_) => { self.io.close(); return Err(BackendError::Unavailable); }
                    };
                    if expires_at_unix_ms != reservation_expiry || now >= u128::from(expires_at_unix_ms) {
                        self.io.close();
                        return Err(BackendError::Denied);
                    }
                }
                Ok(status)
            }
            SpawnResponse::Error { code } => Err(map_error(code)),
            _ => Err(BackendError::Protocol),
        };
        if result.is_err() { self.io.close(); }
        result
    }

    pub async fn request(&self, request: SpawnRequest, input_fds: &[OwnedFd]) -> Result<Packet, BackendError> {
        self.request_checked(request, input_fds, Arc::new(|| Ok(()))).await
    }

    pub async fn request_checked(&self, request: SpawnRequest, input_fds: &[OwnedFd],
        check: Arc<LiveCheck>) -> Result<Packet, BackendError> {
        let packet = self.request_packet(request, input_fds, check).await?;
        if let SpawnResponse::Error { code } = packet.response { return Err(map_error(code)); }
        Ok(packet)
    }

    /// A refused or abandoned Cancel that sent no bytes leaves the existing
    /// job's connection intact. Once sent, uncertainty still closes it.
    pub(super) async fn cancel_checked(&self, run: duduclaw_isolation::execution::RunHandle,
        check: Arc<LiveCheck>) -> Result<(), BackendError> {
        let control = Arc::new(CancelControl { state: Mutex::new(CancelState::default()), check });
        control.verify()?;
        let mut guard = CancelControlGuard { io: self.io.clone(), control: control.clone(), completed: false };
        let io = self.io.clone();
        let pending = control.clone();
        let deadline = std::time::Instant::now() + Duration::from_millis(ipc::CONTROL_TIMEOUT_MS);
        let packet = tokio::task::spawn_blocking(move || io.transact(SpawnRequest::Cancel { run }, vec![],
            deadline, pending.check.as_ref(), Some(pending.as_ref())))
            .await.map_err(|_| BackendError::Unavailable)??;
        control.verify()?;
        guard.completed = true;
        match packet.response {
            SpawnResponse::Ack => Ok(()),
            SpawnResponse::Error { code } => Err(map_error(code)),
            _ => Err(BackendError::Protocol),
        }
    }

    pub(super) async fn pty_activity(&self, run: duduclaw_isolation::execution::RunHandle) -> Result<(), BackendError> {
        match self.request_packet(SpawnRequest::PtyActivity { run }, &[], Arc::new(|| Ok(()))).await?.response {
            SpawnResponse::Ack | SpawnResponse::Error { code: ErrorCode::NotReady } => Ok(()),
            SpawnResponse::Error { code } => Err(map_error(code)),
            _ => Err(BackendError::Protocol),
        }
    }

    async fn request_packet(&self, request: SpawnRequest, input_fds: &[OwnedFd], check: Arc<LiveCheck>) -> Result<Packet, BackendError> {
        check()?;
        let roles = request.fd_plan().map_err(|_| BackendError::Denied)?;
        if roles.len() != input_fds.len() { return Err(BackendError::Protocol); }
        let inputs = input_fds.iter().map(|fd| fd.try_clone().map_err(|_| BackendError::Io))
            .collect::<Result<Vec<_>, _>>()?;
        let deadline = std::time::Instant::now() + Duration::from_millis(ipc::CONTROL_TIMEOUT_MS);
        let io = self.io.clone();
        // Dropping this future shuts down the socket immediately, including
        // while its blocking task is queued or waiting for the transaction gate.
        let mut cancellation = CancelTransaction(Some(io.clone()));
        let transaction_check = check.clone();
        let result = tokio::task::spawn_blocking(move || io.transact(request, inputs, deadline, transaction_check.as_ref(), None))
            .await.map_err(|_| BackendError::Unavailable)?;
        check()?;
        cancellation.0 = None;
        result
    }
}
impl Drop for Connection {
    fn drop(&mut self) { self.io.close(); }
}

fn poll_until(fd: &OwnedFd, events: libc::c_short, deadline: std::time::Instant) -> Result<(), BackendError> {
    loop {
        let remaining = deadline.checked_duration_since(std::time::Instant::now())
            .ok_or(BackendError::Timeout)?;
        if remaining.is_zero() { return Err(BackendError::Timeout); }
        let timeout = remaining.as_millis().min(250) as libc::c_int;
        // Round down rather than exceed the budget; a sub-ms remainder refuses.
        if timeout == 0 { return Err(BackendError::Timeout); }
        let mut pollfd = libc::pollfd { fd: fd.as_raw_fd(), events, revents: 0 };
        // SAFETY: one live typed pollfd; poll cannot access beyond this slot.
        let result = unsafe { libc::poll(&mut pollfd, 1, timeout) };
        if result < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted { continue; }
            return Err(BackendError::Io);
        }
        if result == 0 { continue; }
        if pollfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 { return Err(BackendError::Io); }
        if pollfd.revents & events != 0 { return Ok(()); }
        if pollfd.revents & libc::POLLHUP != 0 { return Err(BackendError::Io); }
    }
}

fn map_error(code: ErrorCode) -> BackendError {
    match code {
        ErrorCode::WouldDeadlock => BackendError::WouldDeadlock,
        ErrorCode::NotFound => BackendError::NotFound,
        ErrorCode::QueueExpired => BackendError::Timeout,
        ErrorCode::IsolationUnavailable | ErrorCode::NotReady | ErrorCode::QueueFull | ErrorCode::ResourceLimit => BackendError::Unavailable,
        ErrorCode::Unauthorized | ErrorCode::RuntimeDenied | ErrorCode::AlreadyTaken => BackendError::Denied,
        ErrorCode::UnsupportedVersion | ErrorCode::InvalidRequest | ErrorCode::InvalidFds | ErrorCode::Internal => BackendError::Protocol,
    }
}

fn validate_response(request: &SpawnRequest, response: &SpawnResponse) -> Result<(), BackendError> {
    let valid = match (request, response) {
        (_, SpawnResponse::Error { .. }) => true,
        (SpawnRequest::ReserveRun { .. }, SpawnResponse::Reserved { .. }) => true,
        (SpawnRequest::StartRun { run, .. }, SpawnResponse::Accepted { run: actual, status }) =>
            run == actual && !matches!(status, RunStatus::Reserved { .. }),
        (SpawnRequest::Status { run } | SpawnRequest::Wait { run, .. }, SpawnResponse::Status { run: actual, .. }) => run == actual,
        (SpawnRequest::TakeIo { run }, SpawnResponse::Io { run: actual, .. }) => run == actual,
        (SpawnRequest::Cancel { .. } | SpawnRequest::Resize { .. } | SpawnRequest::PtyActivity { .. }, SpawnResponse::Ack) => true,
        _ => false,
    };
    if valid { Ok(()) } else { Err(BackendError::Protocol) }
}

#[cfg(test)]
async fn send_checked(fd: &AsyncFd<OwnedFd>, frame: &[u8], input_fds: &[OwnedFd],
    check: &(dyn Fn() -> Result<(), BackendError> + Send + Sync)) -> Result<(), BackendError> {
    let raw_fds: Vec<_> = input_fds.iter().map(AsRawFd::as_raw_fd).collect();
    loop {
        let mut ready = fd.writable().await.map_err(|_| BackendError::Io)?;
        check()?;
        let sent = ready.try_io(|fd| {
            let controls = if raw_fds.is_empty() { vec![] } else { vec![ControlMessage::ScmRights(&raw_fds)] };
            socket::sendmsg::<UnixAddr>(fd.get_ref().as_raw_fd(), &[IoSlice::new(frame)], &controls,
                MsgFlags::MSG_NOSIGNAL | MsgFlags::MSG_DONTWAIT, None).map_err(io::Error::from)
        });
        match sent {
            Ok(Ok(bytes)) if bytes == frame.len() => return Ok(()),
            Ok(Ok(_)) => return Err(BackendError::Protocol),
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(_)) => return Err(BackendError::Io),
            Err(_) => continue,
        }
    }
}

#[cfg(test)]
async fn receive(fd: &AsyncFd<OwnedFd>) -> Result<(Vec<u8>, Vec<OwnedFd>), BackendError> {
    loop {
        let mut ready = fd.readable().await.map_err(|_| BackendError::Io)?;
        match ready.try_io(|fd| receive_packet(fd.get_ref())) {
            Ok(Ok(packet)) => return Ok(packet),
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) if error.kind() == io::ErrorKind::InvalidData => return Err(BackendError::Protocol),
            Ok(Err(_)) => return Err(BackendError::Io),
            Err(_) => continue,
        }
    }
}

fn receive_packet(fd: &OwnedFd) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut bytes = vec![0u8; ipc::MAX_FRAME_BYTES + 4];
    // More than the maximum response count, so extra rights can be closed and
    // rejected. usize storage satisfies cmsghdr alignment. Linux closes rights
    // omitted by MSG_CTRUNC; parse and own the delivered subset before rejecting.
    let control_bytes = unsafe { libc::CMSG_SPACE((65 * std::mem::size_of::<libc::c_int>()) as u32) } as usize;
    let mut control = vec![0usize; control_bytes.div_ceil(std::mem::size_of::<usize>())];
    let mut iov = libc::iovec { iov_base: bytes.as_mut_ptr().cast(), iov_len: bytes.len() };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_bytes;
    // SAFETY: all storage is alive and initialized, lengths describe the buffers;
    // the kernel alone constructs the ancillary headers and received fd numbers.
    let read = unsafe { libc::recvmsg(fd.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT) };
    if read < 0 { return Err(io::Error::last_os_error()); }
    let mut received = Vec::new();
    let mut invalid = message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 || read == 0;
    // SAFETY: msg_controllen is kernel-produced and bounded to our allocation.
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !header.is_null() {
        let header_value = unsafe { &*header };
        let base = unsafe { libc::CMSG_LEN(0) } as usize;
        let offset = header as usize - control.as_ptr() as usize;
        let available = message.msg_controllen.saturating_sub(offset);
        if header_value.cmsg_len < base || header_value.cmsg_len > available {
            invalid = true;
            break;
        }
        let data_len = header_value.cmsg_len - base;
        if header_value.cmsg_level == libc::SOL_SOCKET && header_value.cmsg_type == libc::SCM_RIGHTS {
            if data_len % std::mem::size_of::<libc::c_int>() != 0 { invalid = true; }
            let data = unsafe { libc::CMSG_DATA(header) };
            for index in 0..data_len / std::mem::size_of::<libc::c_int>() {
                let raw = unsafe { std::ptr::read_unaligned(data.add(index * std::mem::size_of::<libc::c_int>()).cast::<libc::c_int>()) };
                if raw < 0 { invalid = true; } else {
                    // SAFETY: SCM_RIGHTS installs a new owned descriptor for each
                    // entry. Taking ownership now closes it on every later error.
                    received.push(unsafe { OwnedFd::from_raw_fd(raw) });
                }
            }
        } else { invalid = true; }
        header = unsafe { libc::CMSG_NXTHDR(&message, header) };
    }
    if invalid { return Err(io::ErrorKind::InvalidData.into()); }
    bytes.truncate(read as usize);
    Ok((bytes, received))
}

fn validate_fds(roles: &[FdRole], fds: &[OwnedFd]) -> Result<(), BackendError> {
    if roles.len() != fds.len() { return Err(BackendError::Protocol); }
    for (role, fd) in roles.iter().zip(fds) {
        let raw = fd.as_raw_fd();
        let flags = fcntl(raw, FcntlArg::F_GETFD).map_err(|_| BackendError::Protocol)?;
        if !FdFlag::from_bits_retain(flags).contains(FdFlag::FD_CLOEXEC) { return Err(BackendError::Protocol); }
        let flags = fcntl(raw, FcntlArg::F_GETFL).map_err(|_| BackendError::Protocol)?;
        let access = OFlag::from_bits_retain(flags) & OFlag::O_ACCMODE;
        let stat = fstat(raw).map_err(|_| BackendError::Protocol)?;
        let kind = SFlag::from_bits_retain(stat.st_mode) & SFlag::S_IFMT;
        let valid = match role {
            FdRole::StdinWrite => kind == SFlag::S_IFIFO && access == OFlag::O_WRONLY,
            FdRole::StdoutRead | FdRole::StderrRead => kind == SFlag::S_IFIFO && access == OFlag::O_RDONLY,
            FdRole::McpGateway => {
                let domain = socket_domain(fd)?;
                let socket_type = socket::getsockopt(fd, sockopt::SockType).map_err(|_| BackendError::Protocol)?;
                let local = socket::getsockname::<UnixAddr>(raw).map_err(|_| BackendError::Protocol)?;
                let peer = socket::getpeername::<UnixAddr>(raw).map_err(|_| BackendError::Protocol)?;
                kind == SFlag::S_IFSOCK && access == OFlag::O_RDWR
                    && domain == libc::AF_UNIX && socket_type == SockType::Stream
                    && local.path().is_none() && local.as_abstract().is_none()
                    && peer.path().is_none() && peer.as_abstract().is_none()
            }
            FdRole::PtyMaster => {
                let mut number: libc::c_uint = 0;
                // SAFETY: ioctl writes exactly one unsigned integer to number.
                let is_master = unsafe { libc::ioctl(raw, libc::TIOCGPTN, &mut number) } == 0;
                kind == SFlag::S_IFCHR && access == OFlag::O_RDWR && is_master
            }
            _ => false,
        };
        if !valid { return Err(BackendError::Protocol); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{execution::RunHandle, ids::RunId, ipc::IoDelivery};

    fn status_peer(peer: OwnedFd, response: impl FnOnce(Envelope<SpawnRequest>) -> (Envelope<SpawnResponse>, Vec<OwnedFd>)
        + Send + 'static) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            poll_until(&peer, libc::POLLIN, std::time::Instant::now() + Duration::from_secs(2)).unwrap();
            let (bytes, fds) = receive_packet(&peer).unwrap();
            assert!(fds.is_empty());
            let request = ipc::decode(&bytes).unwrap();
            let (reply, rights) = response(request);
            let bytes = ipc::encode(&reply).unwrap();
            let raw: Vec<_> = rights.iter().map(AsRawFd::as_raw_fd).collect();
            let controls = if raw.is_empty() { vec![] } else { vec![ControlMessage::ScmRights(&raw)] };
            socket::sendmsg::<UnixAddr>(peer.as_raw_fd(), &[IoSlice::new(&bytes)], &controls,
                MsgFlags::MSG_NOSIGNAL, None).unwrap();
        })
    }

    #[tokio::test]
    async fn live_status_reads_matching_root_response_without_async_state_lock() {
        let (connection, peer) = Connection::test_pair();
        let run = RunHandle(RunId([1; 16]));
        let thread = status_peer(peer, move |request| {
            assert!(matches!(request.message, SpawnRequest::Status { run: actual } if actual == run));
            (Envelope { version: ipc::PROTOCOL_VERSION, request_id: request.request_id,
                message: SpawnResponse::Status { run, status: RunStatus::Running } }, vec![])
        });
        assert!(matches!(connection.live_status(run, 1), Ok(RunStatus::Running)));
        assert!(!connection.is_closed());
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn live_status_waits_for_existing_transaction_and_reads_its_own_reply() {
        let (connection, peer) = Connection::test_pair();
        let connection = Arc::new(connection);
        let run = RunHandle(RunId([1; 16]));
        let mut transaction = connection.io.acquire(std::time::Instant::now() + Duration::from_secs(1)).unwrap();
        transaction.completed = true;
        let peer = status_peer(peer, move |request| (Envelope { version: ipc::PROTOCOL_VERSION,
            request_id: request.request_id, message: SpawnResponse::Status { run,
                status: RunStatus::Running } }, vec![]));
        std::thread::scope(|scope| {
            let query = scope.spawn(|| connection.live_status(run, 1));
            std::thread::sleep(Duration::from_millis(10));
            drop(transaction);
            assert!(matches!(query.join().unwrap(), Ok(RunStatus::Running)));
        });
        assert!(!connection.is_closed());
        peer.join().unwrap();
    }

    #[tokio::test]
    async fn live_status_timeout_closes_control_and_cannot_reuse_late_reply() {
        let (connection, _peer) = Connection::test_pair();
        let started = std::time::Instant::now();
        assert!(matches!(connection.live_status(RunHandle(RunId([1; 16])), 1), Err(BackendError::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(connection.is_closed());
        assert!(matches!(connection.live_status(RunHandle(RunId([1; 16])), 1), Err(BackendError::Unavailable)));
    }

    #[tokio::test]
    async fn live_status_rejects_wrong_request_run_and_unexpected_rights() {
        let run = RunHandle(RunId([1; 16]));
        for mutation in 0..3 {
            let (connection, peer) = Connection::test_pair();
            let (read, write) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
            let before = open_copies(&read);
            let thread = status_peer(peer, move |request| {
                let rights = if mutation == 2 { vec![write] } else { vec![] };
                (Envelope { version: ipc::PROTOCOL_VERSION,
                    request_id: if mutation == 0 { RequestId([0; 16]) } else { request.request_id },
                    message: SpawnResponse::Status {
                        run: if mutation == 1 { RunHandle(RunId([2; 16])) } else { run },
                        status: RunStatus::Running } }, rights)
            });
            assert!(matches!(connection.live_status(run, 1), Err(BackendError::Protocol)));
            assert!(connection.is_closed());
            thread.join().unwrap();
            assert_eq!(open_copies(&read), before - 1, "received rights and peer copy must close");
        }
    }

    #[tokio::test]
    async fn live_status_reservation_must_match_authenticated_expiry_and_be_live() {
        let run = RunHandle(RunId([1; 16]));
        let future = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap()
            .as_millis() as u64 + 30_000;
        for (reported, expected, accepted) in [(future, future, true), (future, future + 1, false), (1, 1, false)] {
            let (connection, peer) = Connection::test_pair();
            let thread = status_peer(peer, move |request| (Envelope { version: ipc::PROTOCOL_VERSION,
                request_id: request.request_id, message: SpawnResponse::Status { run,
                    status: RunStatus::Reserved { expires_at_unix_ms: reported } } }, vec![]));
            assert_eq!(connection.live_status(run, expected).is_ok(), accepted);
            assert_eq!(connection.is_closed(), !accepted);
            thread.join().unwrap();
        }
    }

    #[test]
    fn replies_bind_operation_and_run() {
        let run = RunHandle(RunId([1; 16]));
        let other = RunHandle(RunId([2; 16]));
        let request = SpawnRequest::TakeIo { run };
        let io = |run| SpawnResponse::Io { run, delivery: IoDelivery { mcp: false, pty: false } };
        assert!(validate_response(&request, &io(run)).is_ok());
        assert!(validate_response(&request, &io(other)).is_err());
        assert!(validate_response(&request, &SpawnResponse::Ack).is_err());
        assert!(validate_response(&request, &SpawnResponse::Error { code: ErrorCode::NotReady }).is_ok());
    }

    #[test]
    fn received_rights_are_cloexec_and_typed() {
        let (sender, receiver) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
        let (read, write) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        socket::sendmsg::<UnixAddr>(sender.as_raw_fd(), &[IoSlice::new(b"test")],
            &[ControlMessage::ScmRights(&[read.as_raw_fd(), write.as_raw_fd()])], MsgFlags::empty(), None).unwrap();
        let (bytes, fds) = receive_packet(&receiver).unwrap();
        assert_eq!(bytes, b"test");
        assert!(validate_fds(&[FdRole::StdoutRead, FdRole::StdinWrite], &fds).is_ok());
        assert!(validate_fds(&[FdRole::StdinWrite, FdRole::StdoutRead], &fds).is_err());
        assert!(validate_fds(&[], &fds).is_err());
    }

    #[test]
    fn unknown_ancillary_is_refused() {
        let (sender, receiver) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
        socket::setsockopt(&receiver, sockopt::PassCred, &true).unwrap();
        socket::send(sender.as_raw_fd(), b"credentials", MsgFlags::empty()).unwrap();
        assert_eq!(receive_packet(&receiver).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    fn open_copies(fd: &OwnedFd) -> usize {
        let expected = fstat(fd.as_raw_fd()).unwrap();
        std::fs::read_dir("/proc/self/fd").unwrap().filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|raw| fstat(raw).ok())
            .filter(|stat| stat.st_dev == expected.st_dev && stat.st_ino == expected.st_ino).count()
    }

    #[test]
    fn ancillary_truncation_closes_every_received_descriptor() {
        let (sender, receiver) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
        let (read, _write) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let before = open_copies(&read);
        let rights = vec![read.as_raw_fd(); 80];
        socket::sendmsg::<UnixAddr>(sender.as_raw_fd(), &[IoSlice::new(b"truncated")],
            &[ControlMessage::ScmRights(&rights)], MsgFlags::empty(), None).unwrap();
        assert_eq!(receive_packet(&receiver).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(open_copies(&read), before);
    }

    #[test]
    fn mcp_accepts_only_connected_unnamed_stream_socket() {
        let (stream, _peer) = socket::socketpair(AddressFamily::Unix, SockType::Stream, None, SockFlag::SOCK_CLOEXEC).unwrap();
        assert!(validate_fds(&[FdRole::McpGateway], &[stream]).is_ok());
        let (packet, _peer) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
        assert!(validate_fds(&[FdRole::McpGateway], &[packet]).is_err());
        let disconnected = socket::socket(AddressFamily::Unix, SockType::Stream, SockFlag::SOCK_CLOEXEC, None).unwrap();
        assert!(validate_fds(&[FdRole::McpGateway], &[disconnected]).is_err());
    }

    #[tokio::test]
    async fn malformed_reply_closes_rights_and_poisons_connection() {
        let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK).unwrap();
        let (read, _write) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let before = open_copies(&read);
        socket::sendmsg::<UnixAddr>(right.as_raw_fd(), &[IoSlice::new(b"bad frame")],
            &[ControlMessage::ScmRights(&[read.as_raw_fd()])], MsgFlags::empty(), None).unwrap();
        let connection = Connection::from_fd(left, ipc::ControlConnectionId([7; 16]));
        let request = SpawnRequest::ReserveRun { employee_id: duduclaw_isolation::ids::EmployeeId([1; 16]) };
        assert!(matches!(connection.request(request.clone(), &[]).await, Err(BackendError::Protocol)));
        assert_eq!(open_copies(&read), before);
        assert!(matches!(connection.request(request, &[]).await, Err(BackendError::Unavailable)));
    }

    #[test]
    fn wrong_peer_identity_is_refused() {
        let (left, _) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        assert!(authenticate_peer(&left, uid, gid).is_ok());
        assert!(authenticate_peer(&left, uid.wrapping_add(1), gid).is_err());
        assert!(authenticate_peer(&left, uid, gid.wrapping_add(1)).is_err());
    }

    #[tokio::test]
    async fn cancelled_transaction_closes_control_connection() {
        let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK).unwrap();
        let connection = Connection::from_fd(left, ipc::ControlConnectionId([7; 16]));
        let request = SpawnRequest::ReserveRun { employee_id: duduclaw_isolation::ids::EmployeeId([1; 16]) };
        let (received, notice) = tokio::sync::oneshot::channel();
        let peer = std::thread::spawn(move || {
            poll_until(&right, libc::POLLIN, std::time::Instant::now() + Duration::from_secs(2)).unwrap();
            let (bytes, fds) = receive_packet(&right).unwrap();
            assert!(!bytes.is_empty()); assert!(fds.is_empty());
            received.send(()).unwrap();
            right
        });
        let mut pending = Box::pin(connection.request(request.clone(), &[]));
        tokio::select! {
            result = &mut pending => panic!("RPC completed before cancellation: {}", result.is_ok()),
            result = notice => result.unwrap(),
        }
        drop(pending);
        let right = peer.join().unwrap();
        assert!(connection.is_closed());
        assert!(matches!(connection.request(request, &[]).await, Err(BackendError::Unavailable)));
        // The first request was received, but no reply was sent. Cancellation
        // still shuts down control while the independent I/O owner is polling.
        let mut buffer = vec![0; ipc::MAX_FRAME_BYTES + 4];
        assert_eq!(socket::recv(right.as_raw_fd(), &mut buffer, MsgFlags::empty()).unwrap(), 0);
    }
    #[tokio::test]
    async fn checked_request_rechecks_after_transaction_lock_wait() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK).unwrap();
        let connection = Connection::test_connection(left);
        let live = Arc::new(AtomicBool::new(true));
        let authority = live.clone();
        let check: Arc<LiveCheck> = Arc::new(move || if authority.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) });
        let mut lock = connection.io.acquire(std::time::Instant::now() + Duration::from_secs(1)).unwrap();
        let request = SpawnRequest::ReserveRun { employee_id: duduclaw_isolation::ids::EmployeeId([1; 16]) };
        let future = connection.request_checked(request, &[], check);
        tokio::pin!(future);
        assert!(tokio::time::timeout(Duration::from_millis(5), &mut future).await.is_err());
        live.store(false, Ordering::SeqCst);
        lock.completed = true;
        drop(lock);
        assert!(matches!(future.await, Err(BackendError::Denied)));
        // No bytes crossed the root boundary, and taking then rejecting the
        // owned connection ensures the reservation is revoked by peer EOF.
        let mut bytes = [0; 16];
        assert_eq!(socket::recv(right.as_raw_fd(), &mut bytes, MsgFlags::empty()).unwrap(), 0);
    }

    #[tokio::test]
    async fn checked_request_refuses_fd_handover_after_reply_revocation() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK).unwrap();
        let connection = Connection::test_connection(left);
        let live = Arc::new(AtomicBool::new(true));
        let server_live = live.clone();
        let server = tokio::spawn(async move {
            let right = AsyncFd::new(right).unwrap();
            let (bytes, _) = receive(&right).await.unwrap();
            let request: Envelope<SpawnRequest> = ipc::decode(&bytes).unwrap();
            let run = duduclaw_isolation::execution::RunHandle(duduclaw_isolation::ids::RunId([9; 16]));
            let response = Envelope { version: ipc::PROTOCOL_VERSION, request_id: request.request_id,
                message: SpawnResponse::Reserved { run, expires_at_unix_ms: 100 } };
            server_live.store(false, Ordering::SeqCst);
            send_checked(&right, &ipc::encode(&response).unwrap(), &[], &|| Ok(())).await.unwrap();
        });
        let check: Arc<LiveCheck> = Arc::new(move || if live.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) });
        let request = SpawnRequest::ReserveRun { employee_id: duduclaw_isolation::ids::EmployeeId([1; 16]) };
        assert!(matches!(connection.request_checked(request, &[], check).await, Err(BackendError::Denied)));
        assert!(connection.is_closed());
        server.await.unwrap();
    }

    #[test]
    fn checked_cancel_timeout_before_send_keeps_target_connection() {
        let (connection, root) = Connection::test_pair();
        let mut lock = connection.io.acquire(std::time::Instant::now() + Duration::from_secs(1)).unwrap();
        let io = connection.io.clone();
        let pending = std::thread::spawn(move || {
            let control = CancelControl { state: Mutex::new(CancelState::default()), check: Arc::new(|| Ok(())) };
            io.transact(SpawnRequest::Cancel { run: duduclaw_isolation::execution::RunHandle(
                duduclaw_isolation::ids::RunId([1; 16])) }, vec![],
                std::time::Instant::now() + Duration::from_millis(10), control.check.as_ref(), Some(&control))
        });
        assert!(matches!(pending.join().unwrap(), Err(BackendError::Timeout)));
        assert!(!connection.is_closed());
        let mut bytes = [0; 16];
        assert_eq!(socket::recv(root.as_raw_fd(), &mut bytes, MsgFlags::MSG_DONTWAIT), Err(nix::errno::Errno::EAGAIN));
        lock.completed = true;
    }

    #[tokio::test]
    async fn checked_cancel_revoked_after_send_still_fences_uncertainty() {
        let (connection, root) = Connection::test_pair();
        let live = Arc::new(AtomicBool::new(true));
        let verifier = live.clone();
        let server = tokio::spawn(async move {
            let root = AsyncFd::new(root).unwrap();
            let (bytes, _) = receive(&root).await.unwrap();
            let request: Envelope<SpawnRequest> = ipc::decode(&bytes).unwrap();
            assert!(matches!(request.message, SpawnRequest::Cancel { .. }));
            live.store(false, Ordering::SeqCst);
            let response = Envelope { version: ipc::PROTOCOL_VERSION, request_id: request.request_id,
                message: SpawnResponse::Ack };
            send_checked(&root, &ipc::encode(&response).unwrap(), &[], &|| Ok(())).await.unwrap();
        });
        let check: Arc<LiveCheck> = Arc::new(move || if verifier.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) });
        let run = duduclaw_isolation::execution::RunHandle(duduclaw_isolation::ids::RunId([1; 16]));
        assert_eq!(connection.cancel_checked(run, check).await, Err(BackendError::Denied));
        assert!(connection.is_closed());
        server.await.unwrap();
    }

}
