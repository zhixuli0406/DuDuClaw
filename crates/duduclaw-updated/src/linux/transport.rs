//! One bounded seqpacket per connection; no ancillary data is ever accepted.
use std::{io::{IoSlice, IoSliceMut}, os::fd::{AsRawFd, FromRawFd, OwnedFd}};
use nix::sys::{socket::{self, AddressFamily, MsgFlags, SockFlag, SockType, UnixAddr, sockopt}, time::TimeVal};
use serde::{Serialize, de::DeserializeOwned};
use duduclaw_isolation::ipc::{MAX_FRAME_BYTES, frame_payload, payload};
use crate::{Error, Result};

pub fn errno(error: nix::errno::Errno) -> Error { std::io::Error::from_raw_os_error(error as i32).into() }
pub fn socket() -> Result<OwnedFd> {
    socket::socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::SOCK_CLOEXEC, None).map_err(errno)
}
pub fn timeout(fd: &OwnedFd) -> Result<()> {
    let time = TimeVal::new(5, 0);
    socket::setsockopt(fd, sockopt::ReceiveTimeout, &time).map_err(errno)?;
    socket::setsockopt(fd, sockopt::SendTimeout, &time).map_err(errno)?;
    Ok(())
}
pub fn connect() -> Result<OwnedFd> {
    // The path and all parents are fixed and protected; peer identity is still
    // checked after connecting so socket replacement cannot impersonate root.
    super::files::Directory::open(std::path::Path::new("/run/duduclaw-updated"))?;
    let fd = socket()?;
    timeout(&fd)?;
    socket::connect(fd.as_raw_fd(), &UnixAddr::new(duduclaw_isolation::ids::UPDATED_SOCKET).map_err(errno)?).map_err(errno)?;
    let cred = socket::getsockopt(&fd, sockopt::PeerCredentials).map_err(errno)?;
    if cred.uid() != 0 || cred.gid() != 0 { return Err(Error::Maintenance); }
    Ok(fd)
}
pub fn send<T: Serialize>(fd: &OwnedFd, message: &T) -> Result<()> {
    let bytes = serde_json::to_vec(message)?;
    let packet = frame_payload(&bytes)?;
    let sent = socket::sendmsg::<UnixAddr>(fd.as_raw_fd(), &[IoSlice::new(&packet)], &[], MsgFlags::MSG_NOSIGNAL, None).map_err(errno)?;
    if sent != packet.len() { return Err(Error::Invalid); }
    Ok(())
}
pub fn receive<T: DeserializeOwned>(fd: &OwnedFd) -> Result<T> {
    let mut buffer = vec![0; MAX_FRAME_BYTES + 4];
    let mut iov = [IoSliceMut::new(&mut buffer)];
    // Zero control space is intentional: Linux closes SCM_RIGHTS descriptors it
    // cannot fit and sets MSG_CTRUNC. No received descriptor escapes a rejection.
    let msg = socket::recvmsg::<UnixAddr>(fd.as_raw_fd(), &mut iov, None, MsgFlags::MSG_CMSG_CLOEXEC).map_err(errno)?;
    if msg.flags.intersects(MsgFlags::MSG_CTRUNC | MsgFlags::MSG_TRUNC) || msg.bytes == 0 { return Err(Error::Invalid); }
    let length = msg.bytes;
    Ok(serde_json::from_slice(payload(&buffer[..length])?)?)
}
pub fn accept(listener: &OwnedFd, gateway_uid: u32, gateway_gid: u32) -> Result<OwnedFd> {
    let raw = socket::accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC).map_err(errno)?;
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    timeout(&fd)?;
    let cred = socket::getsockopt(&fd, sockopt::PeerCredentials).map_err(errno)?;
    if !((cred.uid() == 0 && cred.gid() == 0) || (cred.uid() == gateway_uid && cred.gid() == gateway_gid)) {
        return Err(Error::Invalid);
    }
    Ok(fd)
}
pub fn bind(gateway_gid: u32) -> Result<OwnedFd> {
    let dir = super::files::Directory::open(std::path::Path::new("/run/duduclaw-updated"))?;
    match dir.remove("control.sock") {
        Ok(()) => (),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    let fd = socket()?;
    let address = UnixAddr::new(duduclaw_isolation::ids::UPDATED_SOCKET).map_err(errno)?;
    socket::bind(fd.as_raw_fd(), &address).map_err(errno)?;
    let path = std::ffi::CString::new(duduclaw_isolation::ids::UPDATED_SOCKET).map_err(|_| Error::Invalid)?;
    // The parent is root-only writable and the daemon holds the global lock.
    if unsafe { libc::chown(path.as_ptr(), 0, gateway_gid) } != 0
        || unsafe { libc::chmod(path.as_ptr(), 0o660) } != 0 { return Err(std::io::Error::last_os_error().into()); }
    socket::listen(&fd, socket::Backlog::new(16).map_err(errno)?).map_err(errno)?;
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{ids::RequestId, update::{UpdateRequest, UpdateRequestV1}};
    #[test]
    fn seqpacket_roundtrip_rejects_ancillary_and_trailing_bytes() {
        let (a, b) = socket::socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
        timeout(&a).unwrap(); timeout(&b).unwrap();
        let request = UpdateRequest::V1(UpdateRequestV1::Status { request_id: RequestId([1; 16]) });
        send(&a, &request).unwrap();
        assert_eq!(receive::<UpdateRequest>(&b).unwrap(), request);
        let frame = frame_payload(&serde_json::to_vec(&request).unwrap()).unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        let rights = [file.as_raw_fd()];
        socket::sendmsg::<UnixAddr>(a.as_raw_fd(), &[IoSlice::new(&frame)],
            &[socket::ControlMessage::ScmRights(&rights)], MsgFlags::MSG_NOSIGNAL, None).unwrap();
        assert!(receive::<UpdateRequest>(&b).is_err());
        let mut trailing = frame; trailing.push(0);
        socket::sendmsg::<UnixAddr>(a.as_raw_fd(), &[IoSlice::new(&trailing)], &[], MsgFlags::MSG_NOSIGNAL, None).unwrap();
        assert!(receive::<UpdateRequest>(&b).is_err());
    }
}
