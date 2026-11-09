//! Nonblocking data-plane descriptors. Control cancellation never waits on I/O.
use std::{io, os::fd::{AsRawFd, OwnedFd}, sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}}};
use duduclaw_isolation::execution::*;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use tokio::io::unix::AsyncFd;

pub(super) enum DeliveredIo { Stdio(RunStdio), Pty(RunPty) }

pub(super) fn make_io(pty: bool, fds: Vec<OwnedFd>, max_bytes: u64, buffer_bytes: u32) -> Result<DeliveredIo, BackendError> {
    if fds.len() != if pty { 1 } else { 3 } || max_bytes == 0 || buffer_bytes == 0 {
        return Err(BackendError::Protocol);
    }
    let mut converted = Vec::with_capacity(fds.len());
    for fd in fds {
        let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL).map_err(|_| BackendError::Io)?;
        fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(OFlag::from_bits_retain(flags) | OFlag::O_NONBLOCK))
            .map_err(|_| BackendError::Io)?;
        converted.push(Arc::new(AsyncFd::new(fd).map_err(|_| BackendError::Io)?));
    }
    let mut fds = converted.into_iter();
    let budget = Arc::new(OutputBudget { remaining: AtomicU64::new(max_bytes), exceeded: AtomicBool::new(false) });
    let first = fds.next().ok_or(BackendError::Protocol)?;
    if pty {
        Ok(DeliveredIo::Pty(RunPty {
            input: Box::new(Writer { fd: Some(first.clone()) }),
            output: Box::new(Reader { fd: first, budget, buffer_bytes, pty: true }),
        }))
    } else {
        Ok(DeliveredIo::Stdio(RunStdio {
            stdin: Box::new(Writer { fd: Some(first) }),
            stdout: Box::new(Reader { fd: fds.next().ok_or(BackendError::Protocol)?, budget: budget.clone(), buffer_bytes, pty: false }),
            stderr: Box::new(Reader { fd: fds.next().ok_or(BackendError::Protocol)?, budget, buffer_bytes, pty: false }),
        }))
    }
}

/// HX5: an in-run helper call's `[StdoutRead, StderrRead]` pipes (no stdin;
/// the input is a sealed memfd), sharing one output budget like `make_io`.
pub(super) fn make_helper_io(fds: Vec<OwnedFd>, max_bytes: u64)
    -> Result<(Box<dyn RunReader>, Box<dyn RunReader>), BackendError> {
    if fds.len() != 2 || max_bytes == 0 { return Err(BackendError::Protocol); }
    let mut converted = Vec::with_capacity(2);
    for fd in fds {
        let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL).map_err(|_| BackendError::Io)?;
        fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(OFlag::from_bits_retain(flags) | OFlag::O_NONBLOCK))
            .map_err(|_| BackendError::Io)?;
        converted.push(Arc::new(AsyncFd::new(fd).map_err(|_| BackendError::Io)?));
    }
    let mut fds = converted.into_iter();
    let budget = Arc::new(OutputBudget { remaining: AtomicU64::new(max_bytes), exceeded: AtomicBool::new(false) });
    let buffer_bytes = 64 * 1024;
    let stdout = Reader { fd: fds.next().ok_or(BackendError::Protocol)?, budget: budget.clone(), buffer_bytes, pty: false };
    let stderr = Reader { fd: fds.next().ok_or(BackendError::Protocol)?, budget, buffer_bytes, pty: false };
    Ok((Box::new(stdout), Box::new(stderr)))
}

struct OutputBudget { remaining: AtomicU64, exceeded: AtomicBool }

struct Reader {
    fd: Arc<AsyncFd<OwnedFd>>,
    budget: Arc<OutputBudget>,
    buffer_bytes: u32,
    pty: bool,
}

impl RunReader for Reader {
    fn read<'a>(&'a mut self, buffer: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            if buffer.is_empty() { return Ok(0); }
            loop {
                if self.budget.exceeded.load(Ordering::Acquire) { return Err(BackendError::Io); }
                // Probe one byte beyond the remaining budget. Exact-limit EOF is
                // valid; any additional output is a sticky failure, never EOF.
                let available = self.budget.remaining.load(Ordering::Acquire).saturating_add(1).min(self.buffer_bytes as u64)
                    .min(buffer.len() as u64) as usize;
                let mut ready = self.fd.readable().await.map_err(|_| BackendError::Io)?;
                let read = ready.try_io(|fd| nix::unistd::read(fd.get_ref().as_raw_fd(), &mut buffer[..available]).map_err(io::Error::from));
                match read {
                    Ok(Ok(bytes)) => {
                        // The compare/exchange accounts both pipes atomically.
                        // A competing read may exhaust the budget while this read
                        // is pending; that surplus is an error, never truncation.
                        let mut previous = self.budget.remaining.load(Ordering::Acquire);
                        loop {
                            match self.budget.remaining.compare_exchange_weak(previous, previous.saturating_sub(bytes as u64),
                                Ordering::AcqRel, Ordering::Acquire) {
                                Ok(_) => break,
                                Err(current) => previous = current,
                            }
                        }
                        if bytes as u64 > previous { self.budget.exceeded.store(true, Ordering::Release); }
                        if self.budget.exceeded.load(Ordering::Acquire) { return Err(BackendError::Io); }
                        return Ok(bytes);
                    }
                    Ok(Err(error)) if self.pty && error.raw_os_error() == Some(libc::EIO) => return Ok(0),
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(Err(_)) => return Err(BackendError::Io),
                    Err(_) => continue,
                }
            }
        })
    }
}

struct Writer { fd: Option<Arc<AsyncFd<OwnedFd>>> }

impl RunWriter for Writer {
    fn write<'a>(&'a mut self, bytes: &'a [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            let fd = self.fd.as_ref().ok_or(BackendError::Io)?;
            loop {
                let mut ready = fd.writable().await.map_err(|_| BackendError::Io)?;
                match ready.try_io(|fd| nix::unistd::write(fd.get_ref(), bytes).map_err(io::Error::from)) {
                    Ok(Ok(bytes)) => return Ok(bytes),
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(Err(_)) => return Err(BackendError::Io),
                    Err(_) => continue,
                }
            }
        })
    }

    fn close(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            // PTYs have no half-close. Closing this writer drops its duplicate;
            // EOF/cancellation uses the run lifecycle while the reader remains.
            self.fd.take();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stdio_budget_overflow_is_shared_and_sticky() {
        let (stdin_r, stdin_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let (stdout_r, stdout_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let (stderr_r, stderr_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let DeliveredIo::Stdio(mut io) = make_io(false, vec![stdin_w, stdout_r, stderr_r], 5, 1024).unwrap() else { panic!() };
        nix::unistd::write(&stdout_w, b"1234").unwrap();
        nix::unistd::write(&stderr_w, b"abcd").unwrap();
        let mut bytes = [0; 16];
        assert_eq!(io.stdout.read(&mut bytes).await.unwrap(), 4);
        assert_eq!(io.stderr.read(&mut bytes).await, Err(BackendError::Io));
        assert_eq!(io.stdout.read(&mut bytes).await, Err(BackendError::Io));
        io.stdin.close().await.unwrap();
        assert_eq!(nix::unistd::read(stdin_r.as_raw_fd(), &mut bytes).unwrap(), 0);
    }

    #[tokio::test]
    async fn exact_output_limit_keeps_real_eof_and_empty_reads_consume_nothing() {
        let (_stdin_r, stdin_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let (stdout_r, stdout_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let (stderr_r, stderr_w) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
        let DeliveredIo::Stdio(mut io) = make_io(false, vec![stdin_w, stdout_r, stderr_r], 5, 1024).unwrap() else { panic!() };
        nix::unistd::write(&stdout_w, b"12345").unwrap();
        drop(stdout_w);
        drop(stderr_w);
        assert_eq!(io.stdout.read(&mut []).await.unwrap(), 0);
        let mut bytes = [0; 16];
        assert_eq!(io.stdout.read(&mut bytes).await.unwrap(), 5);
        assert_eq!(io.stdout.read(&mut bytes).await.unwrap(), 0);
        assert_eq!(io.stderr.read(&mut bytes).await.unwrap(), 0);
    }
}
