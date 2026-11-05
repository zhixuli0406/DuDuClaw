//! Unprivileged byte pump. Authority resides exclusively at the gateway endpoint.
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::{io, net::Shutdown, os::fd::FromRawFd};
    // SAFETY: fcntl accepts a possibly-invalid raw descriptor and returns EBADF.
    // Do not create a BorrowedFd before proving that the inherited slot exists.
    let fd = unsafe { libc::fcntl(duduclaw_mcp_boundary::config::MCP_FD, libc::F_DUPFD_CLOEXEC, 5) };
    if fd < 0 { return std::process::ExitCode::FAILURE; }
    // SAFETY: dup returned a unique owned descriptor, transferred exactly once.
    let mut socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    if duduclaw_mcp_boundary::socket::validate(&socket).is_err() {
        return std::process::ExitCode::FAILURE;
    }
    // SAFETY: this shim owns the inherited slot; its CLOEXEC duplicate above is
    // the only authority handle retained before the pump threads are started.
    unsafe { libc::close(duduclaw_mcp_boundary::config::MCP_FD); }
    let Ok(mut writer) = socket.try_clone() else { return std::process::ExitCode::FAILURE; };
    // The main thread returns on host EOF, even if stdin is still open. No join
    // on the input pump: a runtime holding stdin must not keep this shim alive.
    std::thread::spawn(move || {
        if io::copy(&mut io::stdin().lock(), &mut writer).is_err() {
            let _ = writer.shutdown(Shutdown::Both);
        } else {
            let _ = writer.shutdown(Shutdown::Write);
        }
    });
    match io::copy(&mut socket, &mut io::stdout().lock()) {
        Ok(_) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
