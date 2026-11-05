//! Linux seqpacket transport. Peer uid/gid authorize the connection; PID never does.
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
    pub fn connect_fixed_egress(expected_uid: u32, expected_gid: u32) -> io::Result<Self> {
        if expected_uid == 0 { return Err(denied()); }
        trusted_parent_of(Path::new(EGRESS_CONTROL_SOCKET))?;
        let metadata = fs::symlink_metadata(EGRESS_CONTROL_SOCKET)?;
        if !metadata.file_type().is_socket() || metadata.uid() != expected_uid
            || metadata.gid() != expected_gid || metadata.mode() & 0o007 != 0 {
            return Err(denied());
        }
        let fd = socket::socket(AddressFamily::Unix, SockType::SeqPacket,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None)?;
        match socket::connect(fd.as_raw_fd(), &UnixAddr::new(EGRESS_CONTROL_SOCKET)?) {
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
        let connection = Self::from_owned(fd)?;
        connection.authenticate(expected_uid, expected_gid)?;
        connection.set_nonblocking(false)?;
        Ok(connection)
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
            } else { rejected = true; }
            let aligned = header.cmsg_len.div_ceil(mem::size_of::<usize>()) * mem::size_of::<usize>();
            offset += aligned;
        }
        if rights_messages > 1 || fds.len() > MAX_RIGHTS { rejected = true; }
        if received == 0 { return Err(io::Error::from(io::ErrorKind::UnexpectedEof)); }
        if rejected || received as usize > bytes.len() { return Err(invalid()); }
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
}
