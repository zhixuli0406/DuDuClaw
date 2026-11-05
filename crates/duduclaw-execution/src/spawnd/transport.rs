//! Bounded seqpacket RPC and fd ownership. Only this module receives SCM_RIGHTS.
use std::{io::{self, IoSlice}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::{FileTypeExt, MetadataExt}}, path::Path, time::Duration};
use duduclaw_isolation::{execution::BackendError, ids::{RequestId, SPAWND_SOCKET}, ipc::{self, Envelope, ErrorCode, FdRole, RunStatus, SpawnRequest, SpawnResponse}};
use nix::{fcntl::{fcntl, FcntlArg, FdFlag, OFlag}, sys::{socket::{self, AddressFamily, ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr, sockopt}, stat::{fstat, SFlag}}};
use tokio::{io::unix::AsyncFd, sync::Mutex};

pub(super) struct Connection { fd: Mutex<Option<AsyncFd<OwnedFd>>> }
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
    Ok(Connection { fd: Mutex::new(Some(AsyncFd::new(fd).map_err(|_| BackendError::Unavailable)?)) })
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

impl Connection {
    #[cfg(test)]
    pub(super) fn test_connection(fd: OwnedFd) -> Self {
        Self { fd: Mutex::new(Some(AsyncFd::new(fd).unwrap())) }
    }

    pub async fn close(&self) { self.fd.lock().await.take(); }

    pub fn is_closed(&self) -> bool {
        self.fd.try_lock().is_ok_and(|fd| fd.is_none())
    }

    pub async fn request(&self, request: SpawnRequest, input_fds: &[OwnedFd]) -> Result<Packet, BackendError> {
        self.request_checked(request, input_fds, &|| Ok(())).await
    }

    pub async fn request_checked(&self, request: SpawnRequest, input_fds: &[OwnedFd],
        check: &(dyn Fn() -> Result<(), BackendError> + Send + Sync)) -> Result<Packet, BackendError> {
        check()?;
        let roles = request.fd_plan().map_err(|_| BackendError::Denied)?;
        if roles.len() != input_fds.len() { return Err(BackendError::Protocol); }
        let request_id = RequestId(*uuid::Uuid::new_v4().as_bytes());
        let frame = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id, message: &request })
            .map_err(|_| BackendError::Protocol)?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ipc::CONTROL_TIMEOUT_MS);
        let mut guard = tokio::time::timeout_at(deadline, self.fd.lock()).await.map_err(|_| BackendError::Timeout)?;
        // Take ownership before the first await: dropping this future, timing out,
        // or rejecting the response closes the socket and revokes the run on B.
        let fd = guard.take().ok_or(BackendError::Unavailable)?;
        check()?;
        let result = tokio::time::timeout_at(deadline, async {
            send_checked(&fd, &frame, input_fds, check).await?;
            let (bytes, fds) = receive(&fd).await?;
            let envelope: Envelope<SpawnResponse> = ipc::decode(&bytes).map_err(|_| BackendError::Protocol)?;
            if envelope.request_id != request_id { return Err(BackendError::Protocol); }
            validate_response(&request, &envelope.message)?;
            validate_fds(&envelope.message.fd_plan(), &fds)?;
            check()?;
            Ok(Packet { response: envelope.message, fds })
        }).await.map_err(|_| BackendError::Timeout)?;
        match result {
            Ok(packet) => {
                *guard = Some(fd);
                if let SpawnResponse::Error { code } = packet.response { return Err(map_error(code)); }
                Ok(packet)
            }
            Err(error) => Err(error),
        }
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
        (SpawnRequest::Cancel { .. } | SpawnRequest::Resize { .. }, SpawnResponse::Ack) => true,
        _ => false,
    };
    if valid { Ok(()) } else { Err(BackendError::Protocol) }
}

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
        let connection = Connection { fd: Mutex::new(Some(AsyncFd::new(left).unwrap())) };
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
        let connection = Connection { fd: Mutex::new(Some(AsyncFd::new(left).unwrap())) };
        let request = SpawnRequest::ReserveRun { employee_id: duduclaw_isolation::ids::EmployeeId([1; 16]) };
        assert!(tokio::time::timeout(Duration::from_millis(10), connection.request(request.clone(), &[])).await.is_err());
        assert!(matches!(connection.request(request, &[]).await, Err(BackendError::Unavailable)));
        // The queued request may remain readable, then peer EOF proves closure.
        let mut buffer = vec![0; ipc::MAX_FRAME_BYTES + 4];
        assert!(socket::recv(right.as_raw_fd(), &mut buffer, MsgFlags::empty()).unwrap() > 0);
        assert_eq!(socket::recv(right.as_raw_fd(), &mut buffer, MsgFlags::empty()).unwrap(), 0);
    }
    #[tokio::test]
    async fn checked_request_rechecks_after_transaction_lock_wait() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK).unwrap();
        let connection = Connection::test_connection(left);
        let live = AtomicBool::new(true);
        let check = || if live.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) };
        let lock = connection.fd.lock().await;
        let request = SpawnRequest::ReserveRun { employee_id: duduclaw_isolation::ids::EmployeeId([1; 16]) };
        let future = connection.request_checked(request, &[], &check);
        tokio::pin!(future);
        assert!(tokio::time::timeout(Duration::from_millis(5), &mut future).await.is_err());
        live.store(false, Ordering::SeqCst);
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
        let check = || if live.load(Ordering::SeqCst) { Ok(()) } else { Err(BackendError::Denied) };
        let request = SpawnRequest::ReserveRun { employee_id: duduclaw_isolation::ids::EmployeeId([1; 16]) };
        assert!(matches!(connection.request_checked(request, &[], &check).await, Err(BackendError::Denied)));
        assert!(connection.is_closed());
        server.await.unwrap();
    }

}
