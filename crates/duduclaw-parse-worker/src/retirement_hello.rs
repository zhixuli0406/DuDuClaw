//! Private lifecycle handshake before any untrusted content is sent.
use std::{fs::File, os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd}, time::Instant};
use nix::sys::socket::{self, sockopt};
use duduclaw_isolation::{ipc, worker::ParseError};

const MAGIC: &[u8] = b"DUDUCLAW-PARSE-LEASE-1\0";
pub fn is_hello(payload: &[u8]) -> bool { payload.len() == MAGIC.len() + 16 && payload.get(..MAGIC.len()) == Some(MAGIC) }

pub fn answer(socket: &impl AsFd, payload: &[u8], deadline: Instant) -> Result<(), ParseError> {
    if !is_hello(payload) { return Err(ParseError::InvalidContent); }
    // SAFETY: pidfd_open returns a new descriptor for this still-running worker.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
    if raw < 0 { return Err(ParseError::Unavailable); }
    let file = unsafe { File::from_raw_fd(raw as i32) };
    crate::transport::send(socket, payload, &[file], deadline)
}

pub struct Hello { pub pidfd: OwnedFd, pub pid: u32, pub uid: u32 }

pub fn begin(socket: &OwnedFd, deadline: Instant) -> Result<Hello, ParseError> {
    socket::setsockopt(socket, sockopt::PassCred, &true).map_err(|_| ParseError::Unavailable)?;
    let mut payload = MAGIC.to_vec();
    payload.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    crate::transport::send(socket, &payload, &[], deadline)?;
    let result = receive(socket, &payload, deadline);
    socket::setsockopt(socket, sockopt::PassCred, &false).map_err(|_| ParseError::Unavailable)?;
    result
}

fn receive(socket: &OwnedFd, expected: &[u8], deadline: Instant) -> Result<Hello, ParseError> {
    crate::transport::wait(socket, nix::poll::PollFlags::POLLIN, deadline)?;
    let mut bytes = [0u8; 128];
    let mut control = [0usize; 512];
    let mut iov = libc::iovec { iov_base: bytes.as_mut_ptr().cast(), iov_len: bytes.len() };
    // Own all delivered rights before validating any packet, including CTRUNC.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov; msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast(); msg.msg_controllen = std::mem::size_of_val(&control);
        let n = libc::recvmsg(socket.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT);
        if n < 0 { return Err(ParseError::Unavailable); }
        let mut rights = Vec::new();
        let mut credential = None;
        let mut invalid = msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0;
        let base = msg.msg_control as usize;
        let end = base + msg.msg_controllen;
        let mut header = libc::CMSG_FIRSTHDR(&msg);
        while !header.is_null() {
            let start = header as usize;
            if start < base || start + std::mem::size_of::<libc::cmsghdr>() > end { invalid = true; break; }
            let minimum = libc::CMSG_LEN(0) as usize;
            let length = (*header).cmsg_len;
            if length < minimum || length > end.saturating_sub(start) { invalid = true; break; }
            let size = length - minimum;
            let data = libc::CMSG_DATA(header);
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                if size % std::mem::size_of::<i32>() != 0 { invalid = true; }
                for i in 0..size / std::mem::size_of::<i32>() {
                    rights.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.cast::<i32>().add(i))));
                }
            } else if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_CREDENTIALS
                && size == std::mem::size_of::<libc::ucred>() && credential.is_none() {
                credential = Some(std::ptr::read_unaligned(data.cast::<libc::ucred>()));
            } else { invalid = true; }
            header = libc::CMSG_NXTHDR(&msg, header);
        }
        if invalid || rights.len() != 1 || ipc::payload(&bytes[..n as usize]).ok() != Some(expected) {
            return Err(ParseError::InvalidContent);
        }
        let cred = credential.ok_or(ParseError::Unavailable)?;
        if cred.pid <= 0 || cred.uid == 0 { return Err(ParseError::Unavailable); }
        let pidfd = rights.pop().ok_or(ParseError::Unavailable)?;
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd()))
            .map_err(|_| ParseError::Unavailable)?;
        if !info.lines().any(|line| line.strip_prefix("Pid:").is_some_and(|p| p.trim().parse::<i32>() == Ok(cred.pid)))
            || exited(&pidfd)? { return Err(ParseError::Unavailable); }
        Ok(Hello { pidfd, pid: cred.pid as u32, uid: cred.uid })
    }
}

pub fn exited(pidfd: &OwnedFd) -> Result<bool, ParseError> {
    let mut poll = libc::pollfd { fd: pidfd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let n = unsafe { libc::poll(&mut poll, 1, 0) };
    if n < 0 || poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 { return Err(ParseError::Unavailable); }
    Ok(n != 0 && poll.revents & libc::POLLIN != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hello_is_exact_and_never_accepts_a_parser_request() {
        let mut message = MAGIC.to_vec(); message.extend_from_slice(&[7; 16]);
        assert!(is_hello(&message)); message.push(0); assert!(!is_hello(&message));
        assert!(!is_hello(b"{}")); assert!(!is_hello(MAGIC));
    }
}
