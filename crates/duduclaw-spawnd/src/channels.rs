//! Nonblocking output relay and revocable socketpairs for the B2 adapter.
use std::{collections::VecDeque, io, os::fd::{AsFd, AsRawFd, OwnedFd}};
use nix::{errno::Errno, fcntl::{fcntl, FcntlArg, OFlag}, sys::socket::{self, AddressFamily, Shutdown, SockFlag, SockType}};

/// Keep an endpoint duplicate solely to revoke a handed-off connection.
pub struct RevocablePair { revoke: OwnedFd, pub gateway: OwnedFd, pub runtime: OwnedFd }
impl RevocablePair {
    pub fn new() -> io::Result<Self> {
        let (gateway, runtime) = socket::socketpair(AddressFamily::Unix, SockType::Stream, None, SockFlag::SOCK_CLOEXEC)?;
        let revoke = gateway.as_fd().try_clone_to_owned()?;
        Ok(Self { revoke, gateway, runtime })
    }
    pub fn revoker(&self) -> io::Result<ConnectionRevoker> {
        Ok(ConnectionRevoker(self.revoke.as_fd().try_clone_to_owned()?))
    }
}
pub struct ConnectionRevoker(OwnedFd);
impl ConnectionRevoker {
    /// Test-only: a revoker over an arbitrary fd (a non-socket makes `revoke` fail).
    #[cfg(test)]
    pub(crate) fn from_fd_for_test(fd: OwnedFd) -> Self { Self(fd) }
    pub fn revoke(&self) -> io::Result<()> {
        match socket::shutdown(self.0.as_raw_fd(), Shutdown::Both) {
            Ok(()) | Err(Errno::ENOTCONN) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
impl Drop for ConnectionRevoker {
    fn drop(&mut self) { let _ = self.revoke(); }
}

/// One pipe-to-pipe relay. Poll once per event-loop turn. Kernel pipes provide
/// backpressure before the total cap; after that cap, drain and discard output.
pub struct OutputRelay {
    input: OwnedFd,
    output: Option<OwnedFd>,
    buffer: VecDeque<u8>,
    capacity: usize,
    remaining: std::sync::Arc<std::sync::atomic::AtomicU64>,
    eof: bool,
    truncated: bool,
}
impl OutputRelay {
    /// Return (relay, runtime writer, gateway reader). Neither output enters journal.
    pub fn pipes(capacity: usize, total: u64) -> io::Result<(Self, OwnedFd, OwnedFd)> {
        if total == 0 { return Err(io::ErrorKind::InvalidInput.into()); }
        Self::pipes_shared(capacity, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(total)))
    }
    /// stdout and stderr debit one per-run budget, including buffered bytes.
    pub fn pipes_shared(capacity: usize, remaining: std::sync::Arc<std::sync::atomic::AtomicU64>) -> io::Result<(Self, OwnedFd, OwnedFd)> {
        if capacity == 0 || capacity > 1024 * 1024 { return Err(io::ErrorKind::InvalidInput.into()); }
        let (input, runtime) = nix::unistd::pipe2(OFlag::O_CLOEXEC)?;
        let (gateway, output) = nix::unistd::pipe2(OFlag::O_CLOEXEC)?;
        for fd in [&input, &output] {
            let flags = OFlag::from_bits_truncate(fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)?);
            fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        }
        Ok((Self { input, output: Some(output), buffer: VecDeque::new(), capacity, remaining,
            eof: false, truncated: false }, runtime, gateway))
    }
    pub fn truncated(&self) -> bool { self.truncated }
    pub fn finished(&self) -> bool { self.eof && self.buffer.is_empty() }
    pub fn buffered(&self) -> usize { self.buffer.len() }
    /// At most 64 KiB of work per tick; an infinite producer cannot monopolize root.
    pub fn pump(&mut self) -> io::Result<()> {
        if let Some(output) = &self.output {
            let bytes = self.buffer.make_contiguous();
            if !bytes.is_empty() {
                match nix::unistd::write(output, bytes) {
                    Ok(size) => { self.buffer.drain(..size); }
                    Err(Errno::EAGAIN | Errno::EINTR) => {},
                    Err(Errno::EPIPE) => { self.output = None; self.buffer.clear(); self.truncated = true; }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let mut scratch = [0u8; 8192];
        for _ in 0..8 {
            if self.eof { break; }
            let discarding = self.remaining.load(std::sync::atomic::Ordering::Relaxed) == 0 || self.output.is_none();
            let amount = if discarding { scratch.len() } else {
                scratch.len().min(self.capacity.saturating_sub(self.buffer.len()))
            };
            if amount == 0 { break; }
            match nix::unistd::read(self.input.as_raw_fd(), &mut scratch[..amount]) {
                Ok(0) => self.eof = true,
                Ok(size) => {
                    let accepted = if discarding { 0 } else {
                        let mut before = self.remaining.load(std::sync::atomic::Ordering::Relaxed);
                        loop {
                            let accepted = before.min(size as u64);
                            match self.remaining.compare_exchange_weak(before, before - accepted,
                                std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed) {
                                Ok(_) => break accepted as usize, Err(actual) => before = actual,
                            }
                        }
                    };
                    self.buffer.extend(scratch[..accepted].iter().copied());
                    self.truncated |= accepted < size;
                }
                Err(Errno::EAGAIN | Errno::EINTR) => break,
                Err(error) => return Err(error.into()),
            }
        }
        if self.finished() { self.output = None; }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stdout_and_stderr_share_one_total_limit() {
        let budget = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(5));
        let (mut out, out_writer, out_reader) = OutputRelay::pipes_shared(16, budget.clone()).unwrap();
        let (mut err, err_writer, err_reader) = OutputRelay::pipes_shared(16, budget).unwrap();
        nix::unistd::write(&out_writer, b"abc").unwrap();
        nix::unistd::write(&err_writer, b"defg").unwrap();
        drop(out_writer); drop(err_writer);
        for _ in 0..8 { out.pump().unwrap(); err.pump().unwrap(); }
        let mut bytes = [0; 16];
        let first = nix::unistd::read(out_reader.as_raw_fd(), &mut bytes).unwrap();
        let second = nix::unistd::read(err_reader.as_raw_fd(), &mut bytes).unwrap();
        assert_eq!(first + second, 5); assert!(err.truncated());
    }
    #[test]
    fn relay_truncates_total_and_closes_after_flush() {
        let (mut relay, writer, reader) = OutputRelay::pipes(4, 5).unwrap();
        nix::unistd::write(&writer, b"123456789").unwrap();
        drop(writer);
        for _ in 0..8 { relay.pump().unwrap(); }
        let mut bytes = [0u8; 20];
        let read = nix::unistd::read(reader.as_raw_fd(), &mut bytes).unwrap();
        assert_eq!(&bytes[..read], b"12345");
        assert!(relay.truncated());
        assert!(relay.finished());
        assert_eq!(nix::unistd::read(reader.as_raw_fd(), &mut bytes).unwrap(), 0);
    }
    #[test]
    fn relay_memory_is_bounded_when_gateway_does_not_read() {
        let (mut relay, writer, _reader) = OutputRelay::pipes(4, 100).unwrap();
        nix::unistd::write(&writer, b"12345678").unwrap();
        relay.pump().unwrap();
        assert_eq!(relay.buffered(), 4);
    }
    #[test]
    fn revocation_affects_already_transferred_socket_endpoint() {
        let pair = RevocablePair::new().unwrap();
        let revoker = pair.revoker().unwrap();
        let runtime = pair.runtime;
        let gateway = pair.gateway;
        nix::unistd::write(&runtime, b"a").unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(nix::unistd::read(gateway.as_raw_fd(), &mut byte).unwrap(), 1);
        revoker.revoke().unwrap();
        assert_eq!(nix::unistd::read(runtime.as_raw_fd(), &mut byte).unwrap(), 0);
    }
}
