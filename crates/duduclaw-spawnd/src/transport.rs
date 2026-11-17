//! Linux seqpacket transport. Peer uid/gid authorize the connection; PID never does.
//! The egress control connection is the exception: its listener is created by
//! PID 1, so the service identity is the kernel-attached sender credential of
//! every received frame (`EgressSeqpacket`), never `SO_PEERCRED`.
use std::{fs, io::{self, IoSlice}, mem, os::{fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd}, unix::fs::{FileTypeExt, MetadataExt, PermissionsExt}}, path::Path};
use nix::{fcntl::{fcntl, FcntlArg, FdFlag, OFlag}, sys::{socket::{self, AddressFamily, Backlog, ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr, sockopt}, time::TimeVal}};
use duduclaw_isolation::{ids::{SPAWND_SOCKET, EGRESS_CONTROL_SOCKET}, ipc::{self, CONTROL_TIMEOUT_MS, MAX_FRAME_BYTES, MAX_INPUT_FDS}};

const MAX_RIGHTS: usize = MAX_INPUT_FDS + 2;
// Linux SCM_MAX_FD is 253. Receive all of them before rejecting excess descriptors.
const KERNEL_MAX_RIGHTS: usize = 253;

fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid isolation transport") }
fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "isolation peer denied") }

pub struct Packet { pub bytes: Vec<u8>, pub fds: Vec<OwnedFd> }

pub struct Seqpacket { fd: OwnedFd }
impl AsFd for Seqpacket { fn as_fd(&self) -> BorrowedFd<'_> { self.fd.as_fd() } }
impl AsRawFd for Seqpacket { fn as_raw_fd(&self) -> i32 { self.fd.as_raw_fd() } }

impl Seqpacket {
    pub fn pair() -> io::Result<(Self, Self)> {
        let (left, right) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC)?;
        Ok((Self::from_owned(left)?, Self::from_owned(right)?))
    }

    pub fn from_owned(fd: OwnedFd) -> io::Result<Self> {
        validate_socket(&fd)?;
        fcntl(fd.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let timeout = TimeVal::new((CONTROL_TIMEOUT_MS / 1000) as i64, ((CONTROL_TIMEOUT_MS % 1000) * 1000) as i64);
        socket::setsockopt(&fd, sockopt::ReceiveTimeout, &timeout)?;
        socket::setsockopt(&fd, sockopt::SendTimeout, &timeout)?;
        socket::setsockopt(&fd, sockopt::SndBuf, &(MAX_FRAME_BYTES + 32768))?;
        if socket::getsockopt(&fd, sockopt::SndBuf)? < MAX_FRAME_BYTES + 4096 { return Err(invalid()); }
        Ok(Self { fd })
    }

    /// Connect only to C's fixed control endpoint. The installation pins its
    /// account; neither destination nor identity may come from a gateway frame.
    ///
    /// `duduclaw-egress.socket` is socket-activated: PID 1 creates and listens on
    /// the endpoint, so both the socket file owner and `SO_PEERCRED` are root and
    /// say nothing about the service. Endpoint trust is therefore the unit's
    /// exact shape (root-owned, group/other-unwritable ancestors; a `root:root
    /// 0600` socket, not a symlink). Service identity is `SO_PASSCRED`, enabled
    /// before connecting: every frame received through the returned
    /// `EgressSeqpacket` must carry exactly one kernel-attached `SCM_CREDENTIALS`
    /// of the egress account (`expected_uid`/`expected_gid`) with a non-zero pid,
    /// otherwise the connection is shut down and the frame refused.
    pub fn connect_fixed_egress(expected_uid: u32, expected_gid: u32) -> io::Result<EgressSeqpacket> {
        connect_egress_at(Path::new(EGRESS_CONTROL_SOCKET), expected_uid, expected_gid)
    }

    pub fn set_nonblocking(&self, enabled: bool) -> io::Result<()> {
        let mut flags = OFlag::from_bits_truncate(fcntl(self.as_raw_fd(), FcntlArg::F_GETFL)?);
        flags.set(OFlag::O_NONBLOCK, enabled);
        fcntl(self.as_raw_fd(), FcntlArg::F_SETFL(flags))?;
        Ok(())
    }

    pub fn peer_ids(&self) -> io::Result<(u32, u32)> {
        let peer = socket::getsockopt(&self.fd, sockopt::PeerCredentials)?;
        Ok((peer.uid(), peer.gid()))
    }

    pub fn authenticate(&self, uid: u32, gid: u32) -> io::Result<()> {
        if self.peer_ids()? != (uid, gid) { return Err(denied()); }
        Ok(())
    }

    pub fn recv(&self) -> io::Result<Packet> {
        self.recv_with_capacity(MAX_FRAME_BYTES + 4)
    }

    fn recv_with_capacity(&self, capacity: usize) -> io::Result<Packet> {
        self.recv_checked(capacity, None)
    }

    /// `sender == None`: any credentials message is refused (gateway, updater).
    /// `sender == Some((uid, gid))`: exactly one kernel-attached SCM_CREDENTIALS
    /// of that account with a non-zero pid is required (egress control only).
    fn recv_checked(&self, capacity: usize, sender: Option<(u32, u32)>) -> io::Result<Packet> {
        let mut bytes = vec![0u8; capacity];
        // usize storage gives cmsghdr its required alignment. Extra space permits
        // unexpected credentials to be parsed and rejected without losing rights.
        let control_bytes = unsafe { libc::CMSG_SPACE((KERNEL_MAX_RIGHTS * mem::size_of::<i32>()) as u32) } as usize + 256;
        let mut control = vec![0usize; control_bytes.div_ceil(mem::size_of::<usize>())];
        let mut iov = libc::iovec { iov_base: bytes.as_mut_ptr().cast(), iov_len: bytes.len() };
        let mut message: libc::msghdr = unsafe { mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() * mem::size_of::<usize>();
        // nix's cmsgs() rejects MSG_CTRUNC before exposing rights. Parse the
        // kernel-generated headers here so every delivered fd has an RAII owner.
        let received = unsafe { libc::recvmsg(self.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
        if received < 0 { return Err(io::Error::last_os_error()); }
        let mut fds = Vec::new();
        let mut senders: Vec<libc::ucred> = Vec::new();
        let mut rejected = message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
        let mut rights_messages = 0;
        let mut offset = 0usize;
        let header_len = unsafe { libc::CMSG_LEN(0) } as usize;
        while offset + mem::size_of::<libc::cmsghdr>() <= message.msg_controllen {
            // Offsets are aligned and bounded by the kernel-returned control size.
            let header = unsafe { &*(control.as_ptr().cast::<u8>().add(offset).cast::<libc::cmsghdr>()) };
            if header.cmsg_len < header_len || header.cmsg_len > message.msg_controllen - offset {
                rejected = true;
                break;
            }
            let data_len = header.cmsg_len - header_len;
            if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
                rights_messages += 1;
                if data_len % mem::size_of::<i32>() != 0 { rejected = true; }
                for index in 0..data_len / mem::size_of::<i32>() {
                    let raw = unsafe { std::ptr::read_unaligned(control.as_ptr().cast::<u8>()
                        .add(offset + header_len + index * mem::size_of::<i32>()).cast::<i32>()) };
                    if raw < 0 { rejected = true; continue; }
                    // recvmsg created a distinct descriptor owned by this process.
                    fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
                }
            } else if sender.is_some() && header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_CREDENTIALS {
                if data_len != mem::size_of::<libc::ucred>() { rejected = true; } else {
                    // Length checked above; the kernel wrote one struct ucred here.
                    senders.push(unsafe { std::ptr::read_unaligned(control.as_ptr().cast::<u8>()
                        .add(offset + header_len).cast::<libc::ucred>()) });
                }
            } else { rejected = true; }
            let aligned = header.cmsg_len.div_ceil(mem::size_of::<usize>()) * mem::size_of::<usize>();
            offset += aligned;
        }
        if rights_messages > 1 || fds.len() > MAX_RIGHTS { rejected = true; }
        if received == 0 { return Err(io::Error::from(io::ErrorKind::UnexpectedEof)); }
        if rejected || received as usize > bytes.len() { return Err(invalid()); }
        if let Some((uid, gid)) = sender { egress_sender(&senders, uid, gid)?; }
        bytes.truncate(received as usize);
        ipc::payload(&bytes).map_err(|_| invalid())?;
        Ok(Packet { bytes, fds })
    }

    pub fn send(&self, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        ipc::payload(bytes).map_err(|_| invalid())?;
        if fds.len() > MAX_RIGHTS { return Err(invalid()); }
        let descriptors: Vec<_> = fds.iter().map(AsRawFd::as_raw_fd).collect();
        let messages = if descriptors.is_empty() { vec![] } else { vec![ControlMessage::ScmRights(&descriptors)] };
        let count = socket::sendmsg::<UnixAddr>(self.as_raw_fd(), &[IoSlice::new(bytes)], &messages, MsgFlags::MSG_NOSIGNAL, None)?;
        if count != bytes.len() { return Err(io::Error::from(io::ErrorKind::WriteZero)); }
        Ok(())
    }
}

/// The egress control connection (§8 R7e). Its only receive path checks the
/// kernel-attached sender credentials of every frame; the inner `Seqpacket` is
/// never exposed, so no caller can read an unchecked frame. The generic
/// `Seqpacket::recv` (gateway, updater) keeps refusing any credentials message.
pub struct EgressSeqpacket { socket: Seqpacket, sender: (u32, u32) }
impl EgressSeqpacket {
    pub fn send(&self, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        self.socket.send(bytes, fds)
    }

    /// Any refusal (missing, duplicate or foreign credentials, truncated
    /// control data, bad frame, timeout, EOF) shuts the connection down: it is
    /// treated as compromised and the owner must reconnect from scratch.
    pub fn recv(&self) -> io::Result<Packet> {
        let result = self.socket.recv_checked(MAX_FRAME_BYTES + 4, Some(self.sender));
        if result.is_err() { let _ = socket::shutdown(self.socket.as_raw_fd(), socket::Shutdown::Both); }
        result
    }

    /// Test-only: wrap one end of a socketpair; SO_PASSCRED is enabled now, so
    /// frames queued before this call carry no sender credentials.
    #[cfg(test)]
    pub(crate) fn with_passcred(socket: Seqpacket, uid: u32, gid: u32) -> io::Result<Self> {
        socket::setsockopt(&socket.fd, sockopt::PassCred, &true)?;
        Ok(Self { socket, sender: (uid, gid) })
    }
}

/// Exactly one sender credential, of the expected account, from a real process.
/// The kernel reports pid 0 (and the overflow uid/gid) for a frame that was
/// queued before SO_PASSCRED was enabled.
fn egress_sender(senders: &[libc::ucred], uid: u32, gid: u32) -> io::Result<()> {
    match senders {
        [one] if one.pid > 0 && one.uid == uid && one.gid == gid => Ok(()),
        _ => Err(denied()),
    }
}

fn connect_egress_at(path: &Path, expected_uid: u32, expected_gid: u32) -> io::Result<EgressSeqpacket> {
    if expected_uid == 0 { return Err(denied()); }
    trusted_egress_endpoint(path)?;
    let fd = socket::socket(AddressFamily::Unix, SockType::SeqPacket,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None)?;
    // Before connect: no frame can be queued for this end without credentials.
    socket::setsockopt(&fd, sockopt::PassCred, &true)?;
    if !socket::getsockopt(&fd, sockopt::PassCred)? { return Err(denied()); }
    match socket::connect(fd.as_raw_fd(), &UnixAddr::new(path)?) {
        Ok(()) => (),
        Err(nix::errno::Errno::EINPROGRESS) => {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(CONTROL_TIMEOUT_MS);
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
                let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
                let result = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) };
                if result < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted { continue; }
                    return Err(error);
                }
                if result == 0 { return Err(io::ErrorKind::TimedOut.into()); }
                let error = socket::getsockopt(&fd, sockopt::SocketError)?;
                if error != 0 { return Err(io::Error::from_raw_os_error(error)); }
                if poll.revents & libc::POLLOUT == 0 { return Err(invalid()); }
                break;
            }
        }
        // AF_UNIX reports a full listen backlog as EAGAIN. Refuse promptly;
        // no reconnect loop may extend the control deadline.
        Err(error) => return Err(error.into()),
    }
    let connection = Seqpacket::from_owned(fd)?;
    connection.set_nonblocking(false)?;
    Ok(EgressSeqpacket { socket: connection, sender: (expected_uid, expected_gid) })
}

/// The shape `duduclaw-egress.socket` gives the endpoint: PID 1 creates it
/// `root:root 0600` in a root-owned, group/other-unwritable directory chain.
fn trusted_egress_endpoint(path: &Path) -> io::Result<()> {
    trusted_parent_of(path)?;
    trusted_socket_file(path, 0, 0)
}
/// A socket (not a symlink) owned by exactly `uid`/`gid` with mode exactly 0600.
fn trusted_socket_file(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() || metadata.uid() != uid || metadata.gid() != gid
        || metadata.mode() & 0o7777 != 0o600 {
        return Err(denied());
    }
    Ok(())
}

fn validate_socket(fd: &OwnedFd) -> io::Result<()> {
    if socket::getsockopt(fd, sockopt::SockType)? != SockType::SeqPacket { return Err(invalid()); }
    let _: UnixAddr = socket::getsockname(fd.as_raw_fd())?;
    Ok(())
}

fn trusted_parent() -> io::Result<()> {
    trusted_parent_of(Path::new(SPAWND_SOCKET))
}
/// The standalone mode has the same fixed parent as socket activation.
pub fn prepare_runtime_directory() -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let parent = Path::new(SPAWND_SOCKET).parent().ok_or_else(invalid)?;
    crate::registry::root_path(parent.parent().ok_or_else(invalid)?, true)?;
    match fs::DirBuilder::new().mode(0o755).create(parent) {
        Ok(()) => { fs::File::open(parent.parent().ok_or_else(invalid)?)?.sync_all()?; },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}, Err(e) => return Err(e),
    }
    trusted_parent()
}
fn trusted_parent_of(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    for ancestor in parent.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(denied());
        }
    }
    Ok(())
}
fn trusted_endpoint(gateway_gid: u32) -> io::Result<()> {
    trusted_parent()?;
    let metadata = fs::symlink_metadata(SPAWND_SOCKET)?;
    if !metadata.file_type().is_socket() || metadata.uid() != 0 || metadata.gid() != gateway_gid || metadata.mode() & 0o7777 != 0o660 {
        return Err(denied());
    }
    Ok(())
}

pub struct Listener { fd: OwnedFd }
impl AsFd for Listener { fn as_fd(&self) -> BorrowedFd<'_> { self.fd.as_fd() } }
impl AsRawFd for Listener { fn as_raw_fd(&self) -> i32 { self.fd.as_raw_fd() } }
impl Listener {
    /// Caller holds RegistryStore's singleton lock. A live endpoint is never
    /// replaced; only an exact root-managed socket with ECONNREFUSED is stale.
    pub fn bind_fixed(gateway_gid: u32) -> io::Result<Self> {
        trusted_parent()?;
        match fs::symlink_metadata(SPAWND_SOCKET) {
            Ok(before) => {
                trusted_endpoint(gateway_gid)?;
                let probe = socket::socket(AddressFamily::Unix, SockType::SeqPacket,
                    SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None)?;
                match socket::connect(probe.as_raw_fd(), &UnixAddr::new(SPAWND_SOCKET)?) {
                    Err(nix::errno::Errno::ECONNREFUSED) => {
                        let current = fs::symlink_metadata(SPAWND_SOCKET)?;
                        if (current.dev(), current.ino()) != (before.dev(), before.ino()) { return Err(denied()); }
                        fs::remove_file(SPAWND_SOCKET)?;
                    }
                    _ => return Err(io::ErrorKind::AddrInUse.into()),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}, Err(e) => return Err(e),
        }
        let fd = socket::socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None)?;
        socket::bind(fd.as_raw_fd(), &UnixAddr::new(SPAWND_SOCKET)?)?;
        // The root-only writable parent excludes pathname replacement by callers.
        let path = std::ffi::CString::new(SPAWND_SOCKET).map_err(|_| invalid())?;
        if unsafe { libc::chown(path.as_ptr(), 0, gateway_gid) } != 0 { return Err(io::Error::last_os_error()); }
        fs::set_permissions(SPAWND_SOCKET, fs::Permissions::from_mode(0o660))?;
        socket::listen(&fd, Backlog::new(64)?)?;
        trusted_endpoint(gateway_gid)?;
        Ok(Self { fd })
    }

    /// Caller owns the one systemd LISTEN_FDS descriptor; validate its actual kind,
    /// bound path, listening state and root-managed endpoint before accepting it.
    pub fn from_activation(fd: OwnedFd, gateway_gid: u32) -> io::Result<Self> {
        validate_socket(&fd)?;
        trusted_endpoint(gateway_gid)?;
        let address: UnixAddr = socket::getsockname(fd.as_raw_fd())?;
        if address.path() != Some(Path::new(SPAWND_SOCKET)) || !socket::getsockopt(&fd, sockopt::AcceptConn)? { return Err(invalid()); }
        fcntl(fd.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let flags = OFlag::from_bits_truncate(fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)?);
        fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(Self { fd })
    }

    pub fn accept_gateway(&self, gateway_uid: u32, gateway_gid: u32) -> io::Result<Seqpacket> {
        if gateway_uid == 0 { return Err(denied()); }
        let raw = socket::accept4(self.as_raw_fd(), SockFlag::SOCK_CLOEXEC)?;
        let connection = Seqpacket::from_owned(unsafe { OwnedFd::from_raw_fd(raw) })?;
        connection.authenticate(gateway_uid, gateway_gid)?;
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seqpacket_roundtrip_and_cloexec_rights() {
        let (a, b) = Seqpacket::pair().unwrap();
        let file = fs::File::open("/dev/null").unwrap();
        let bytes = ipc::frame_payload(b"{}").unwrap();
        a.send(&bytes, &[file.as_fd()]).unwrap();
        let packet = b.recv().unwrap();
        assert_eq!(packet.bytes, bytes);
        assert_eq!(packet.fds.len(), 1);
        assert_ne!(fcntl(packet.fds[0].as_raw_fd(), FcntlArg::F_GETFD).unwrap() & libc::FD_CLOEXEC, 0);
    }
    #[test]
    fn seqpacket_rejects_truncation_and_extra_rights() {
        let (a, b) = Seqpacket::pair().unwrap();
        let bytes = ipc::frame_payload(b"{}{}").unwrap();
        a.send(&bytes, &[]).unwrap();
        assert!(b.recv_with_capacity(4).is_err());
        let file = fs::File::open("/dev/null").unwrap();
        let rights = vec![file.as_raw_fd(); MAX_RIGHTS + 1];
        socket::sendmsg::<UnixAddr>(a.as_raw_fd(), &[IoSlice::new(&bytes)], &[ControlMessage::ScmRights(&rights)], MsgFlags::MSG_NOSIGNAL, None).unwrap();
        assert!(b.recv().is_err());
    }
    #[test]
    fn seqpacket_rejection_closes_received_rights() {
        for truncated in [false, true] {
            let (a, b) = Seqpacket::pair().unwrap();
            let (reader, writer) = nix::unistd::pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
            let bytes = ipc::frame_payload(b"{}").unwrap();
            let rights = vec![writer.as_raw_fd(); if truncated { 1 } else { MAX_RIGHTS + 1 }];
            socket::sendmsg::<UnixAddr>(a.as_raw_fd(), &[IoSlice::new(&bytes)], &[ControlMessage::ScmRights(&rights)], MsgFlags::MSG_NOSIGNAL, None).unwrap();
            drop(writer);
            assert!(b.recv_with_capacity(if truncated { 4 } else { MAX_FRAME_BYTES + 4 }).is_err());
            // Any leaked received writer would make this return EAGAIN, not EOF.
            assert_eq!(nix::unistd::read(reader.as_raw_fd(), &mut [0u8; 1]).unwrap(), 0);
        }
    }
    #[test]
    fn seqpacket_rejects_unknown_ancillary_and_closes_rights() {
        let (a, b) = Seqpacket::pair().unwrap();
        socket::setsockopt(&b.fd, sockopt::PassCred, &true).unwrap();
        let (reader, writer) = nix::unistd::pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
        a.send(&ipc::frame_payload(b"{}").unwrap(), &[writer.as_fd()]).unwrap();
        drop(writer);
        assert!(b.recv().is_err());
        assert_eq!(nix::unistd::read(reader.as_raw_fd(), &mut [0u8; 1]).unwrap(), 0);
    }
    #[test]
    fn seqpacket_authentication_does_not_accept_wrong_uid_or_gid() {
        let (a, _) = Seqpacket::pair().unwrap();
        let (uid, gid) = a.peer_ids().unwrap();
        assert!(a.authenticate(uid, gid).is_ok());
        assert!(a.authenticate(uid.wrapping_add(1), gid).is_err());
        assert!(a.authenticate(uid, gid.wrapping_add(1)).is_err());
    }
    #[test]
    fn seqpacket_rejects_stream_socket() {
        let (fd, _) = socket::socketpair(AddressFamily::Unix, SockType::Stream, None, SockFlag::SOCK_CLOEXEC).unwrap();
        assert!(Seqpacket::from_owned(fd).is_err());
    }

    fn own_ids() -> (u32, u32) { unsafe { (libc::getuid(), libc::getgid()) } }

    #[test]
    fn egress_frames_require_the_expected_sender_credentials() {
        let (uid, gid) = own_ids();
        let frame = ipc::frame_payload(b"{}").unwrap();
        for (expected, accepted) in [((uid, gid), true), ((uid.wrapping_add(1), gid), false), ((uid, gid.wrapping_add(1)), false)] {
            let (a, b) = Seqpacket::pair().unwrap();
            let b = EgressSeqpacket::with_passcred(b, expected.0, expected.1).unwrap();
            let (reader, writer) = nix::unistd::pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
            // SCM_RIGHTS may accompany the kernel's SCM_CREDENTIALS.
            a.send(&frame, &[writer.as_fd()]).unwrap();
            drop(writer);
            let result = b.recv();
            assert_eq!(result.is_ok(), accepted, "expected {expected:?}");
            if accepted {
                let packet = result.unwrap();
                assert_eq!(packet.bytes, frame);
                assert_eq!(packet.fds.len(), 1);
                a.send(&frame, &[]).unwrap();
                assert!(b.recv().is_ok());
            } else {
                assert_eq!(result.err().unwrap().kind(), io::ErrorKind::PermissionDenied);
                // The refused frame's rights were closed, and the connection is shut down.
                assert_eq!(nix::unistd::read(reader.as_raw_fd(), &mut [0u8; 1]).unwrap(), 0);
                assert!(a.send(&frame, &[]).is_err());
                assert!(b.recv().is_err());
            }
        }
    }

    #[test]
    fn egress_frame_without_kernel_sender_credentials_is_refused() {
        let (uid, gid) = own_ids();
        let frame = ipc::frame_payload(b"{}").unwrap();
        // No SO_PASSCRED on either end at send time: the kernel attaches no
        // sender; once enabled it reports pid 0 / overflow ids (or nothing).
        let (a, b) = Seqpacket::pair().unwrap();
        a.send(&frame, &[]).unwrap();
        let b = EgressSeqpacket::with_passcred(b, uid, gid).unwrap();
        assert_eq!(b.recv().err().unwrap().kind(), io::ErrorKind::PermissionDenied);
        assert!(a.send(&frame, &[]).is_err());
        // Receiving end without SO_PASSCRED at all: no credentials message.
        let (a, b) = Seqpacket::pair().unwrap();
        let b = EgressSeqpacket { socket: b, sender: (uid, gid) };
        a.send(&frame, &[]).unwrap();
        assert_eq!(b.recv().err().unwrap().kind(), io::ErrorKind::PermissionDenied);
        // The generic receive path still refuses any credentials message.
        let (a, b) = Seqpacket::pair().unwrap();
        socket::setsockopt(&b.fd, sockopt::PassCred, &true).unwrap();
        a.send(&frame, &[]).unwrap();
        assert!(b.recv().is_err());
    }

    #[test]
    fn egress_credential_rule_needs_exactly_one_matching_sender() {
        let one = libc::ucred { pid: 7, uid: 1001, gid: 1002 };
        assert!(egress_sender(&[one], 1001, 1002).is_ok());
        assert!(egress_sender(&[], 1001, 1002).is_err());
        assert!(egress_sender(&[one, one], 1001, 1002).is_err());
        assert!(egress_sender(&[libc::ucred { pid: 0, ..one }], 1001, 1002).is_err());
        assert!(egress_sender(&[one], 1003, 1002).is_err());
        assert!(egress_sender(&[one], 1001, 1003).is_err());
    }

    #[test]
    fn egress_refuses_root_as_the_service_account() {
        assert!(Seqpacket::connect_fixed_egress(0, 1).is_err());
    }

    #[test]
    fn egress_endpoint_file_must_be_an_owner_only_socket() {
        use std::os::unix::fs::symlink;
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let listener = socket::socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::SOCK_CLOEXEC, None).unwrap();
        socket::bind(listener.as_raw_fd(), &UnixAddr::new(&path).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        // The test user stands in for root as the endpoint owner.
        trusted_socket_file(&path, uid, gid).unwrap();
        assert!(trusted_socket_file(&path, uid.wrapping_add(1), gid).is_err());
        assert!(trusted_socket_file(&path, uid, gid.wrapping_add(1)).is_err());
        for mode in [0o640, 0o604, 0o660, 0o606, 0o400, 0o700] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(trusted_socket_file(&path, uid, gid).is_err(), "mode {mode:o}");
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("link.sock");
        symlink(&path, &link).unwrap();
        assert!(trusted_socket_file(&link, uid, gid).is_err());
        let file = directory.path().join("plain");
        fs::File::create(&file).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(trusted_socket_file(&file, uid, gid).is_err());
        // A non-root-owned directory chain is never a trusted endpoint parent.
        if uid != 0 { assert!(trusted_egress_endpoint(&path).is_err()); }
    }

    mod root_tests {
        use super::*;
        use crate::accounts::root_command;

        struct Cleanup { directory: std::path::PathBuf, socket: std::path::PathBuf, account: Option<String> }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.socket);
                let _ = fs::remove_dir(&self.directory);
                if let Some(name) = &self.account {
                    let _ = root_command("/usr/sbin/userdel", &[name]);
                    // `--user-group` created it; userdel normally removed it already.
                    let _ = root_command("/usr/sbin/groupdel", &[name]);
                }
            }
        }

        /// Forked child: only async-signal-safe libc calls, no allocation.
        fn serve_one_as(listener: i32, uid: u32, gid: u32, answer: &[u8]) -> ! {
            let mut buffer = [0u8; 4096];
            let timeout = libc::timeval { tv_sec: 10, tv_usec: 0 };
            // Raw syscalls on fds this child owns after fork; buffers outlive each call.
            unsafe {
            if libc::setgroups(0, std::ptr::null()) != 0 || libc::setgid(gid) != 0 || libc::setuid(uid) != 0
                || libc::getuid() != uid || libc::geteuid() != uid || libc::getgid() != gid { libc::_exit(10); }
            let connection = libc::accept4(listener, std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_CLOEXEC);
            if connection < 0 { libc::_exit(11); }
            if libc::setsockopt(connection, libc::SOL_SOCKET, libc::SO_RCVTIMEO, (&timeout as *const libc::timeval).cast(),
                mem::size_of::<libc::timeval>() as libc::socklen_t) != 0 { libc::_exit(12); }
            if libc::recv(connection, buffer.as_mut_ptr().cast(), buffer.len(), 0) <= 0 { libc::_exit(13); }
            if libc::send(connection, answer.as_ptr().cast(), answer.len(), libc::MSG_NOSIGNAL) != answer.len() as isize { libc::_exit(14); }
            // Hold the connection until the client is done with it.
            let _ = libc::recv(connection, buffer.as_mut_ptr().cast(), buffer.len(), 0);
            libc::_exit(0);
            }
        }

        fn wait_child(pid: libc::pid_t) -> i32 {
            let mut status = 0;
            loop {
                let result = unsafe { libc::waitpid(pid, &mut status, 0) };
                if result == pid { return status; }
                assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::Interrupted);
            }
        }

        #[test]
        #[ignore = "requires disposable root Linux VM; DUDUCLAW_ISO_DISPOSABLE=1; creates /run/duduclaw-r7e-<pid> and a temporary system account"]
        fn iso_root_egress_control_identity_over_socket_activation() {
            assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
            assert_eq!(unsafe { (libc::geteuid(), libc::getegid()) }, (0, 0));
            let pid = std::process::id();
            let name = format!("ddc-r7e-{pid}");
            let directory = std::path::PathBuf::from(format!("/run/duduclaw-r7e-{pid}"));
            let path = directory.join("control.sock");
            for database in ["passwd", "group"] {
                assert_eq!(root_command("/usr/bin/getent", &[database, &name]).unwrap().0, 2, "{database} {name} must not exist");
            }
            assert!(fs::symlink_metadata(&directory).is_err(), "{} must not exist", directory.display());
            let mut cleanup = Cleanup { directory: directory.clone(), socket: path.clone(), account: None };
            {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o755).create(&directory).unwrap();
            }
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
            let (status, _) = root_command("/usr/sbin/useradd", &["--system", "--user-group", "--no-create-home",
                "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin", &name]).unwrap();
            assert_eq!(status, 0, "useradd {name}");
            cleanup.account = Some(name.clone());
            let (uid, gid) = crate::server::named_identity(&std::ffi::CString::new(name.clone()).unwrap()).unwrap();

            // Root creates and listens, like PID 1 for `duduclaw-egress.socket`.
            let listener = socket::socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::SOCK_CLOEXEC, None).unwrap();
            socket::bind(listener.as_raw_fd(), &UnixAddr::new(&path).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            socket::listen(&listener, Backlog::new(4).unwrap()).unwrap();
            // Bounds the child's accept if the client never connects.
            socket::setsockopt(&listener, sockopt::ReceiveTimeout, &TimeVal::new(10, 0)).unwrap();
            trusted_egress_endpoint(&path).unwrap();
            let answer = ipc::frame_payload(b"{}").unwrap();
            let request = ipc::frame_payload(b"{}").unwrap();

            for (expected_uid, accepted) in [(uid, true), (uid.wrapping_add(1), false)] {
                assert_ne!(expected_uid, 0);
                let child = unsafe { libc::fork() };
                assert!(child >= 0, "fork");
                if child == 0 { unsafe { serve_one_as(listener.as_raw_fd(), uid, gid, &answer) } }
                let client = connect_egress_at(&path, expected_uid, gid);
                let outcome = client.as_ref().map_err(|e| e.kind()).and_then(|client| {
                    client.send(&request, &[]).map_err(|e| e.kind())?;
                    client.recv().map(|packet| packet.bytes).map_err(|e| e.kind())
                });
                let reusable = client.as_ref().map(|client| client.send(&request, &[]).is_ok()).unwrap_or(false);
                drop(client);
                // Reap before asserting so the account has no process at cleanup.
                let status = wait_child(child);
                assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, "listener child status {status:#x}");
                if accepted {
                    assert_eq!(outcome, Ok(answer.clone()));
                } else {
                    // SO_PEERCRED would say root here; the frame's sender says otherwise.
                    assert_eq!(outcome, Err(io::ErrorKind::PermissionDenied));
                    assert!(!reusable, "refused connection must be shut down");
                }
            }
            drop(listener);
            drop(cleanup);
            assert!(fs::symlink_metadata(&directory).is_err());
            assert_eq!(root_command("/usr/bin/getent", &["passwd", &name]).unwrap().0, 2);
        }
    }
}
