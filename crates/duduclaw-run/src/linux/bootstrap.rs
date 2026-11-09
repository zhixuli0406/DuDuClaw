//! Private root-to-trampoline bootstrap. This is NOT gateway/spawnd wire input.
//! B1 supplies RootRegistry; offer bytes carry no paths, identities or policies.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use serde::{Deserialize, Serialize};
use duduclaw_isolation::{ids::{RequestId, RunId}, ipc::{self, Envelope}};
use crate::{Error, Result};

pub const MAX_BOOTSTRAP_FDS: usize = 80;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge { pub nonce: [u8; 32] }

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer { pub nonce: [u8; 32], pub run: RunId }

/// The implementation must resolve a single-use run from the root-owned,
/// signature-verified registry. It must never forward gateway SpawnPolicy fields.
pub trait RootRegistry {
    fn take_run(&mut self, run: RunId) -> Result<super::runner::TrustedRun>;
    /// Called only after root-peer and fresh nonce authentication. Production
    /// consumes the first descriptor as a sealed root plan; fixtures may use an
    /// in-memory registry without a manifest descriptor.
    fn take_received_run(&mut self, run: RunId, _nonce: [u8; 32], _fds: &mut Vec<OwnedFd>)
        -> Result<super::runner::TrustedRun> { self.take_run(run) }

}

/// Explicit cross-package seam. Missing B1 integration is a terminal refusal.
pub struct UnavailableRegistry;
impl RootRegistry for UnavailableRegistry {
    fn take_run(&mut self, _: RunId) -> Result<super::runner::TrustedRun> { Err(Error::RegistryUnavailable) }
}

pub fn verify_peer(fd: RawFd) -> Result<()> {
    if super::fd::socket_option(fd, libc::SO_DOMAIN)? != libc::AF_UNIX
        || super::fd::socket_option(fd, libc::SO_TYPE)? != libc::SOCK_SEQPACKET { return Err(Error::Bootstrap); }
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED,
        (&mut cred as *mut libc::ucred).cast(), &mut len) } != 0
        || len as usize != std::mem::size_of_val(&cred) || cred.uid != 0 { return Err(Error::Bootstrap); }
    // Peer PID is deliberately never used for authentication.
    super::fd::set_cloexec(fd)?;
    Ok(())
}

fn deadline(fd: RawFd) -> Result<()> {
    let timeout = libc::timeval { tv_sec: (ipc::CONTROL_TIMEOUT_MS / 1000) as libc::time_t,
        tv_usec: ((ipc::CONTROL_TIMEOUT_MS % 1000) * 1000) as libc::suseconds_t };
    for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        if unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, option,
            (&timeout as *const libc::timeval).cast(), std::mem::size_of_val(&timeout) as libc::socklen_t) } != 0 {
            return Err(Error::Bootstrap);
        }
    }
    Ok(())
}

/// B1's root-only sender half. Authenticate the connected trampoline and reject
/// any ancillary data on its nonce challenge. Deadlines apply to both halves.
pub fn receive_challenge(fd: RawFd) -> Result<Challenge> {
    verify_peer(fd)?;
    deadline(fd)?;
    let (packet, rights) = receive(fd)?;
    if !rights.is_empty() { return Err(Error::Descriptors); }
    let challenge = ipc::decode::<Challenge>(&packet).map_err(|_| Error::Bootstrap)?;
    if challenge.request_id != RequestId([0;16]) { return Err(Error::Bootstrap); }
    Ok(challenge.message)
}

/// B1 sends only a registry key and exact ordered rights; authority remains in
/// RootRegistry. The sender retains ownership and must close its copies itself.
pub fn send_offer(fd: RawFd, challenge: Challenge, run: RunId, rights: &[RawFd]) -> Result<()> {
    verify_peer(fd)?;
    deadline(fd)?;
    let offer = Envelope { version: ipc::PROTOCOL_VERSION, request_id: RequestId([0;16]),
        message: Offer { nonce: challenge.nonce, run } };
    let packet = ipc::encode(&offer).map_err(|_| Error::Bootstrap)?;
    send_rights(fd, &packet, rights)
}

/// Receives one bounded receipt. Stage markers never replace B1's live unit and
/// cgroup checks. On error, received descriptors are closed by their owners here.
/// Poll for readability using B1's phase-specific deadline before calling this:
/// the socket retains the five-second per-receive handshake I/O timeout, which
/// must not be confused with the lifetime of a running task.
pub fn receive_status(fd: RawFd) -> Result<(crate::status::StatusRecord, Option<OwnedFd>)> {
    let (packet, mut rights) = receive(fd)?;
    let status = crate::status::StatusRecord::decode(&packet, rights.len())?;
    Ok((status, rights.pop()))
}

pub fn handshake(fd: RawFd, registry: &mut dyn RootRegistry) -> Result<(super::runner::TrustedRun, Vec<OwnedFd>)> {
    verify_peer(fd)?;
    deadline(fd)?;
    let mut nonce = [0u8; 32];
    // Raw syscall: glibc's `getrandom` wrapper is above the release baseline.
    if super::syscalls::getrandom(&mut nonce, 0).ok() != Some(nonce.len()) { return Err(Error::Bootstrap); }
    let challenge = Envelope { version: ipc::PROTOCOL_VERSION, request_id: RequestId([0;16]), message: Challenge { nonce } };
    let packet = ipc::encode(&challenge).map_err(|_| Error::Bootstrap)?;
    send_packet(fd, &packet, None)?;
    let (packet, mut fds) = receive(fd)?;
    let run = decode_offer(&packet, nonce)?;
    let plan = registry.take_received_run(run, nonce, &mut fds)?;
    if fds.len() != plan.expected_fd_count() { return Err(Error::Descriptors); }
    Ok((plan, fds))
}

fn decode_offer(packet: &[u8], nonce: [u8;32]) -> Result<RunId> {
    let offer = ipc::decode::<Offer>(packet).map_err(|_| Error::Bootstrap)?;
    if offer.message.nonce != nonce || offer.request_id != RequestId([0;16]) { return Err(Error::Bootstrap); }
    Ok(offer.message.run)
}

pub fn receive(fd: RawFd) -> Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut packet = vec![0u8; ipc::MAX_FRAME_BYTES + 4];
    let capacity = unsafe { libc::CMSG_SPACE((MAX_BOOTSTRAP_FDS * std::mem::size_of::<i32>()) as u32) } as usize;
    // usize storage provides cmsghdr alignment; MSG_CMSG_CLOEXEC is mandatory.
    let mut control = vec![0usize; capacity.div_ceil(std::mem::size_of::<usize>())];
    let mut iov = libc::iovec { iov_base: packet.as_mut_ptr().cast(), iov_len: packet.len() };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov; msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast(); msg.msg_controllen = capacity;
    let count = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if count < 0 { return Err(Error::Bootstrap); }
    let mut received = Vec::new();
    let mut invalid = msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0;
    // Parse and own ALL installed SCM_RIGHTS, even when another cmsg is invalid.
    // Dropping `received` closes them on every rejection path.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            let header = &*cmsg;
            let minimum = libc::CMSG_LEN(0) as usize;
            if header.cmsg_len < minimum { invalid = true; break; }
            if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
                let bytes = header.cmsg_len - minimum;
                if bytes % std::mem::size_of::<i32>() != 0 { invalid = true; }
                for index in 0..bytes / std::mem::size_of::<i32>() {
                    let raw = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>().add(index));
                    if raw < 0 { invalid = true; }
                    else { received.push(OwnedFd::from_raw_fd(raw)); }
                }
            } else { invalid = true; }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    if invalid || received.len() > MAX_BOOTSTRAP_FDS || count == 0 { return Err(Error::Descriptors); }
    for fd in &received {
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        if flags < 0 || flags & libc::FD_CLOEXEC == 0 { return Err(Error::Descriptors); }
    }
    packet.truncate(count as usize);
    Ok((packet, received))
}

pub fn send_packet(fd: RawFd, packet: &[u8], right: Option<RawFd>) -> Result<()> {
    let mut iov = libc::iovec { iov_base: packet.as_ptr().cast_mut().cast(), iov_len: packet.len() };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov; msg.msg_iovlen = 1;
    let mut control = [0usize; 8];
    if let Some(right) = right {
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&msg);
            (*header).cmsg_level = libc::SOL_SOCKET; (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as usize;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), right);
        }
    }
    if unsafe { libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL) } != packet.len() as isize { return Err(Error::Status); }
    Ok(())
}

// Kept separate from the allocation-free supervisor sender above.
fn send_rights(fd: RawFd, packet: &[u8], rights: &[RawFd]) -> Result<()> {
    if packet.is_empty() || packet.len() > ipc::MAX_FRAME_BYTES + 4
        || rights.len() > MAX_BOOTSTRAP_FDS || rights.iter().any(|fd| *fd < 0) {
        return Err(Error::Descriptors);
    }
    let mut iov = libc::iovec { iov_base: packet.as_ptr().cast_mut().cast(), iov_len: packet.len() };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov; msg.msg_iovlen = 1;
    let size = std::mem::size_of_val(rights) as u32;
    let capacity = unsafe { libc::CMSG_SPACE(size) } as usize;
    let mut control = vec![0usize; capacity.div_ceil(std::mem::size_of::<usize>())];
    if !rights.is_empty() {
        msg.msg_control = control.as_mut_ptr().cast(); msg.msg_controllen = capacity;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&msg);
            (*header).cmsg_level = libc::SOL_SOCKET; (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size) as usize;
            std::ptr::copy_nonoverlapping(rights.as_ptr().cast::<u8>(), libc::CMSG_DATA(header), size as usize);
        }
    }
    if unsafe { libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL) } != packet.len() as isize { return Err(Error::Bootstrap); }
    Ok(())
}

/// Fixed packet on the private status pipe and root bootstrap socket. The value
/// is a bounded ABI, exit status or signal; never arbitrary input/errno/text.
pub fn status(kind: u8, code: u8, value: u32) -> [u8; 12] {
    let mut bytes = [0u8; 12]; bytes[..4].copy_from_slice(b"DCR1");
    bytes[4] = kind; bytes[5] = code; bytes[8..].copy_from_slice(&value.to_be_bytes()); bytes
}
pub fn error_status(error: Error) -> [u8; 12] {
    status(0, error_code(error), 0)
}
pub(crate) fn error_code(error: Error) -> u8 {
    match error {
        Error::UnsupportedPlatform => 1, Error::UnsupportedKernel => 2, Error::RegistryUnavailable => 3,
        Error::Bootstrap => 4, Error::Descriptors => 5, Error::DescriptorLimit => 6, Error::Executable => 7,
        Error::Mount => 8, Error::Namespace => 9, Error::Privileges => 10, Error::Landlock => 11,
        Error::Seccomp => 12, Error::State => 13, Error::Process => 14, Error::Status => 15, Error::Exec => 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut pair = [-1; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0, pair.as_mut_ptr()) }, 0);
        unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) }
    }

    #[test]
    fn bootstrap_sender_roundtrips_ordered_cloexec_rights() {
        let (sender, receiver) = pair();
        let first = std::fs::File::open("/dev/null").unwrap();
        let second = std::fs::File::open("/dev/zero").unwrap();
        let offer = Envelope { version: ipc::PROTOCOL_VERSION, request_id: RequestId([0;16]),
            message: Offer { nonce: [1;32], run: RunId([2;16]) } };
        let bytes = ipc::encode(&offer).unwrap();
        send_rights(sender.as_raw_fd(), &bytes, &[first.as_raw_fd(), second.as_raw_fd()]).unwrap();
        let (received, rights) = receive(receiver.as_raw_fd()).unwrap();
        assert_eq!(received, bytes);
        assert_eq!(rights.len(), 2);
        for (actual, expected) in rights.iter().zip([first.as_raw_fd(), second.as_raw_fd()]) {
            assert_ne!(unsafe { libc::fcntl(actual.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
            assert_eq!(super::super::fd::observe(actual.as_raw_fd()).unwrap().identity,
                super::super::fd::observe(expected).unwrap().identity);
        }
        assert_eq!(send_rights(sender.as_raw_fd(), &bytes, &[-1]), Err(Error::Descriptors));
        assert_eq!(send_rights(sender.as_raw_fd(), &bytes, &[first.as_raw_fd();81]), Err(Error::Descriptors));
    }

    #[test]
    fn bootstrap_rejects_unknown_ancillary_and_closes_received_rights() {
        let (sender, receiver) = pair();
        let enabled = 1i32;
        assert_eq!(unsafe { libc::setsockopt(receiver.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PASSCRED,
            (&enabled as *const i32).cast(), std::mem::size_of_val(&enabled) as u32) }, 0);
        let mut pipe = [-1;2];
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) }, 0);
        let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
        send_rights(sender.as_raw_fd(), b"rejected", &[write.as_raw_fd()]).unwrap();
        drop(write);
        assert_eq!(receive(receiver.as_raw_fd()).err(), Some(Error::Descriptors));
        // If SCM_RIGHTS leaked, this nonblocking pipe would return EAGAIN, not EOF.
        let mut byte = 0u8;
        assert_eq!(unsafe { libc::read(read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) }, 0);
    }

    #[test]
    fn bootstrap_status_rejection_closes_unexpected_pty_right() {
        let (sender, receiver) = pair();
        let right = std::fs::File::open("/dev/null").unwrap();
        send_packet(sender.as_raw_fd(), &status(6, 0, 0), Some(right.as_raw_fd())).unwrap();
        assert_eq!(receive_status(receiver.as_raw_fd()).err(), Some(Error::Status));
        send_packet(sender.as_raw_fd(), &status(6, 0, 0), None).unwrap();
        let (record, right) = receive_status(receiver.as_raw_fd()).unwrap();
        assert_eq!(record, crate::status::StatusRecord::SupervisorReady);
        assert!(right.is_none());
    }

    #[test]
    fn bootstrap_control_truncation_closes_every_installed_right() {
        let (sender, receiver) = pair();
        let mut pipe = [-1;2];
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) }, 0);
        let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
        // Deliberately bypass send_offer's cap to exercise MSG_CTRUNC on receive.
        let rights = [write.as_raw_fd(); MAX_BOOTSTRAP_FDS + 1];
        let size = std::mem::size_of_val(&rights) as u32;
        let capacity = unsafe { libc::CMSG_SPACE(size) } as usize;
        let mut control = vec![0usize; capacity.div_ceil(std::mem::size_of::<usize>())];
        let mut byte = 1u8;
        let mut iov = libc::iovec { iov_base: (&mut byte as *mut u8).cast(), iov_len: 1 };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov; msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast(); msg.msg_controllen = capacity;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&msg);
            (*header).cmsg_level = libc::SOL_SOCKET; (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size) as usize;
            std::ptr::copy_nonoverlapping(rights.as_ptr().cast::<u8>(), libc::CMSG_DATA(header), size as usize);
            assert_eq!(libc::sendmsg(sender.as_raw_fd(), &msg, libc::MSG_NOSIGNAL), 1);
        }
        drop(write);
        assert_eq!(receive(receiver.as_raw_fd()).err(), Some(Error::Descriptors));
        assert_eq!(unsafe { libc::read(read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) }, 0);
    }
    #[test]
    fn bootstrap_offer_has_no_authority_overrides() {
        let offer = Offer { nonce: [1;32], run: RunId([2;16]) };
        for name in ["uid", "argv", "environment", "profile", "path", "mounts", "cwd"] {
            let mut json = serde_json::to_value(&offer).unwrap();
            json[name] = serde_json::json!("untrusted");
            assert!(serde_json::from_value::<Offer>(json).is_err());
        }
        let frame = Envelope { version: ipc::PROTOCOL_VERSION, request_id: RequestId([0;16]), message: offer };
        assert_eq!(ipc::decode::<Offer>(&ipc::encode(&frame).unwrap()).unwrap().message.nonce, [1;32]);
    }
    #[test]
    fn bootstrap_offer_is_bound_to_fresh_nonce_and_fixed_correlation() {
        let mut frame = Envelope { version: ipc::PROTOCOL_VERSION, request_id: RequestId([0;16]),
            message: Offer { nonce: [1;32], run: RunId([2;16]) } };
        let packet = ipc::encode(&frame).unwrap();
        assert_eq!(decode_offer(&packet, [1;32]), Ok(RunId([2;16])));
        assert_eq!(decode_offer(&packet, [3;32]), Err(Error::Bootstrap));
        frame.request_id = RequestId([1;16]);
        assert_eq!(decode_offer(&ipc::encode(&frame).unwrap(), [1;32]), Err(Error::Bootstrap));
        let mut trailing = packet; trailing.push(0);
        assert_eq!(decode_offer(&trailing, [1;32]), Err(Error::Bootstrap));
    }
    #[test]
    fn status_records_are_fixed_size_and_have_no_diagnostics() {
        for error in [Error::Descriptors, Error::Mount, Error::Privileges, Error::Exec] {
            let record = error_status(error);
            assert_eq!(record.len(), 12);
            assert_eq!(&record[..4], b"DCR1");
            assert_eq!(record[4], 0);
            assert_eq!(record[5], error_code(error));
            assert_eq!(&record[6..], &[0;6]);
        }
    }
}
