//! Linux ancillary-aware I/O: stream reads also reject/close SCM_RIGHTS.
#![allow(unsafe_code)]
use std::{io, mem, os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd}, pin::Pin, task::{Context, Poll}};
use tokio::io::{unix::AsyncFd, AsyncRead, AsyncWrite, ReadBuf};

fn error() -> io::Error { io::ErrorKind::InvalidData.into() }

pub fn socket_info(fd: &OwnedFd, kind: i32, root_peer: bool, unnamed: bool) -> io::Result<()> {
    // All getsockopt buffers have their exact native sizes and live for the call.
    unsafe {
        for (option, expected) in [(libc::SO_DOMAIN, libc::AF_UNIX), (libc::SO_TYPE, kind)] {
            let mut value: i32 = 0;
            let mut len = mem::size_of_val(&value) as libc::socklen_t;
            if libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, option, (&mut value as *mut i32).cast(), &mut len) != 0
                || len as usize != mem::size_of_val(&value) || value != expected { return Err(error()); }
        }
        if root_peer {
            let mut cred: libc::ucred = mem::zeroed();
            let mut len = mem::size_of_val(&cred) as libc::socklen_t;
            if libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) != 0
                || len as usize != mem::size_of_val(&cred) || cred.uid != 0 || cred.gid != 0 { return Err(error()); }
        }
        if unnamed {
            for peer in [false, true] {
                let mut addr: libc::sockaddr_un = mem::zeroed();
                let mut len = mem::size_of_val(&addr) as libc::socklen_t;
                let rc = if peer { libc::getpeername(fd.as_raw_fd(), (&mut addr as *mut libc::sockaddr_un).cast(), &mut len) }
                    else { libc::getsockname(fd.as_raw_fd(), (&mut addr as *mut libc::sockaddr_un).cast(), &mut len) };
                if rc != 0 || len as usize != mem::size_of::<libc::sa_family_t>() { return Err(error()); }
            }
        }
    }
    Ok(())
}

/// Owned fd prevents accidentally closing someone else's inherited descriptor.
pub struct CheckedSocket { fd: AsyncFd<OwnedFd> }
impl CheckedSocket {
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        let flags = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)?;
        nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(
            nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK))?;
        nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
        Ok(Self { fd: AsyncFd::new(fd)? })
    }
    pub fn raw_fd(&self) -> RawFd { self.fd.get_ref().as_raw_fd() }
    pub fn validate(&self, kind: i32, root_peer: bool, unnamed: bool) -> io::Result<()> {
        socket_info(self.fd.get_ref(), kind, root_peer, unnamed)
    }
    pub async fn receive(&self, buffer: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        loop {
            let mut ready = self.fd.readable().await?;
            match ready.try_io(|fd| receive(fd.get_ref().as_raw_fd(), buffer)) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    }
    pub async fn send_packet(&self, bytes: &[u8]) -> io::Result<()> {
        loop {
            let mut ready = self.fd.writable().await?;
            match ready.try_io(|fd| send(fd.get_ref().as_raw_fd(), bytes)) {
                Ok(Ok(n)) if n == bytes.len() => return Ok(()),
                Ok(Ok(_)) => return Err(error()),
                Ok(Err(e)) => return Err(e),
                Err(_) => continue,
            }
        }
    }
    pub async fn send_packet_with_fd(&self, bytes: &[u8], endpoint: Option<&OwnedFd>) -> io::Result<()> {
        use nix::sys::socket::{sendmsg, ControlMessage, MsgFlags};
        loop {
            let mut ready = self.fd.writable().await?;
            match ready.try_io(|fd| {
                let rights = endpoint.map(|fd| [fd.as_raw_fd()]);
                let controls = rights.as_ref().map(|fds| [ControlMessage::ScmRights(fds)]);
                sendmsg::<()>(fd.get_ref().as_raw_fd(), &[io::IoSlice::new(bytes)],
                    controls.as_ref().map_or(&[], |c| c.as_slice()), MsgFlags::MSG_NOSIGNAL, None)
                    .map_err(io::Error::from)
            }) {
                Ok(Ok(n)) if n == bytes.len() => return Ok(()),
                Ok(Ok(_)) => return Err(error()),
                Ok(Err(e)) => return Err(e),
                Err(_) => continue,
            }
        }
    }
    pub async fn accept(&self) -> io::Result<OwnedFd> {
        loop {
            let mut ready = self.fd.readable().await?;
            match ready.try_io(|fd| {
                // accept4 returns a fresh owned fd; no sockaddr is needed.
                let raw = unsafe { libc::accept4(fd.get_ref().as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK) };
                if raw < 0 { Err(io::Error::last_os_error()) } else { Ok(unsafe { OwnedFd::from_raw_fd(raw) }) }
            }) { Ok(result) => return result, Err(_) => continue }
        }
    }
}
fn send(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    // MSG_NOSIGNAL prevents a peer disconnect from terminating the daemon.
    let n = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}
fn receive(fd: RawFd, bytes: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    // Aligned storage exceeds Linux's 253-fd SCM_RIGHTS ceiling. Walk cmsgs even
    // on truncation so every installed descriptor is closed on rejection.
    let mut ancillary = [0usize; 512];
    let mut iov = libc::iovec { iov_base: bytes.as_mut_ptr().cast(), iov_len: bytes.len() };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov; msg.msg_iovlen = 1;
    msg.msg_control = ancillary.as_mut_ptr().cast(); msg.msg_controllen = mem::size_of_val(&ancillary);
    let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 { return Err(io::Error::last_os_error()); }
    let mut rights = Vec::new();
    let mut invalid = msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&msg);
        while !header.is_null() {
            let minimum = libc::CMSG_LEN(0) as usize;
            let length = (*header).cmsg_len;
            let offset = header as usize - msg.msg_control as usize;
            if length < minimum || offset.saturating_add(length) > msg.msg_controllen { invalid = true; break; }
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let size = length - minimum;
                if size % mem::size_of::<RawFd>() != 0 { invalid = true; }
                for index in 0..size / mem::size_of::<RawFd>() {
                    let raw = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>().add(index));
                    if raw < 0 { invalid = true; } else { rights.push(OwnedFd::from_raw_fd(raw)); }
                }
            } else { invalid = true; }
            header = libc::CMSG_NXTHDR(&msg, header);
        }
    }
    if invalid { return Err(error()); }
    Ok((n as usize, rights))
}
impl AsyncRead for CheckedSocket {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 { return Poll::Ready(Ok(())); }
        loop {
            let mut ready = std::task::ready!(self.fd.poll_read_ready(cx))?;
            match ready.try_io(|fd| receive(fd.get_ref().as_raw_fd(), buf.initialize_unfilled())) {
                Ok(Ok((n, rights))) => {
                    if !rights.is_empty() { return Poll::Ready(Err(error())); }
                    buf.advance(n); return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_) => continue,
            }
        }
    }
}
impl AsyncWrite for CheckedSocket {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        loop {
            let mut ready = std::task::ready!(self.fd.poll_write_ready(cx))?;
            match ready.try_io(|fd| send(fd.get_ref().as_raw_fd(), bytes)) {
                Ok(result) => return Poll::Ready(result), Err(_) => continue,
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let rc = unsafe { libc::shutdown(self.raw_fd(), libc::SHUT_WR) };
        Poll::Ready(if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) })
    }
}
