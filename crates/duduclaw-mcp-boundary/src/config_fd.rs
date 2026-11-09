//! Descriptor transport for A's protected child launcher and acceptance probes.
use std::{io, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::{net::UnixStream, process::CommandExt}},
    process::Command, sync::Arc};
use crate::{config::MCP_FD, VerificationError};

/// Keeps a private CLOEXEC duplicate alive until `Command` is dropped. Only the
/// post-fork child changes fd 3; never clear CLOEXEC in the multithreaded gateway.
/// This is a transport helper, not a process isolation backend: callers must
/// already route employee execution through A/B's protected launcher.
pub struct McpChildFd { source: Arc<OwnedFd> }
impl McpChildFd {
    pub fn new(socket: &UnixStream) -> Result<Self, VerificationError> {
        crate::socket::validate(socket)?;
        // SAFETY: the borrowed source remains alive during this fcntl call.
        let raw = unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
        if raw < 0 { return Err(VerificationError::Unavailable); }
        // SAFETY: fcntl returned a newly owned descriptor.
        Ok(Self { source: Arc::new(unsafe { OwnedFd::from_raw_fd(raw) }) })
    }

    /// Installs exactly fd 3 without copying any environment identity or token.
    /// Capturing the owned source avoids reuse if the caller drops its socket
    /// before spawn. This deliberately does not spawn or offer a legacy fallback.
    pub fn configure(&self, command: &mut Command) {
        let source = self.source.clone();
        // SAFETY: the closure only invokes async-signal-safe fd syscalls. The
        // Arc keeps the source alive in the parent until spawn has completed.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(source.as_raw_fd(), MCP_FD) != MCP_FD {
                    return Err(io::Error::last_os_error());
                }
                let flags = libc::fcntl(MCP_FD, libc::F_GETFD);
                if flags < 0 { return Err(io::Error::last_os_error()); }
                if flags & libc::FD_CLOEXEC != 0 {
                    return Err(io::Error::from_raw_os_error(libc::EBADF));
                }
                Ok(())
            });
        }
    }
}
