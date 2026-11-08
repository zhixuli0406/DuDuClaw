#[cfg(target_os = "linux")]
fn run() -> duduclaw_import::Result<()> {
    use std::{io::Read, os::fd::{FromRawFd, OwnedFd}};
    use duduclaw_import::{ImportRequest, MAX_REQUEST_BYTES, Error};
    // SAFETY: this single-thread worker has not opened or taken ownership of any
    // inherited handles. Only stdio and the two fixed source slots may survive.
    if unsafe { libc::syscall(libc::SYS_close_range, 5u32, u32::MAX, 0u32) } != 0 {
        return Err(Error::Isolation);
    }
    duduclaw_import::linux::check_worker()?;
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() == 3 && args[1] == "--handoff" {
        // SAFETY: the socket has not been opened yet; no inherited source slots
        // belong to the SCM_RIGHTS mode and no Rust object owns these raw slots.
        if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } != 0 {
            return Err(Error::Isolation);
        }
        let (request, main, mut wal, stream) = duduclaw_import::linux::handoff(std::path::Path::new(&args[2]))?;
        duduclaw_import::import(&request, main, wal.as_mut().map(|f| f as &mut dyn Read), &stream)?;
        stream.shutdown(std::net::Shutdown::Write)?;
        return Ok(());
    }
    if args.len() != 1 { return Err(Error::InvalidRequest); }
    let mut request = Vec::new();
    std::io::stdin().take(MAX_REQUEST_BYTES as u64 + 1).read_to_end(&mut request)?;
    let request = ImportRequest::decode(&request)?;
    // SAFETY: raw fcntl accepts any integer, including a missing descriptor. No
    // Rust ownership is constructed until both fixed slots have been checked.
    let main_exists = unsafe { libc::fcntl(3, libc::F_GETFD) } >= 0;
    let wal_flags = unsafe { libc::fcntl(4, libc::F_GETFD) };
    let wal_error = (wal_flags < 0).then(std::io::Error::last_os_error);
    if !request.has_wal && wal_flags >= 0 {
        // SAFETY: fd 4 is an unclaimed inherited slot. Do not retry close on EINTR.
        unsafe { libc::close(4); }
        return Err(Error::InvalidRequest);
    }
    if !main_exists || (request.has_wal && wal_flags < 0) {
        return Err(Error::Isolation);
    }
    if !request.has_wal && wal_error.and_then(|error| error.raw_os_error()) != Some(libc::EBADF) {
        return Err(Error::Isolation);
    }
    // SAFETY: fcntl proved each required slot is open. This single-thread worker
    // consumes each slot once and has no other Rust owner for either descriptor.
    let main = duduclaw_import::linux::source_fd(unsafe { OwnedFd::from_raw_fd(3) })?;
    let mut wal = if request.has_wal { Some(duduclaw_import::linux::source_fd(unsafe { OwnedFd::from_raw_fd(4) })?) } else { None };
    duduclaw_import::import(&request, main, wal.as_mut().map(|f| f as &mut dyn Read), std::io::stdout().lock())?;
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn run() -> duduclaw_import::Result<()> { Err(duduclaw_import::Error::Isolation) }
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        // Names only the catalog differences (object kinds and names), never source text.
        if let Some(refusal) = duduclaw_import::take_schema_refusal() { eprintln!("{refusal}"); }
        let code = match error {
            duduclaw_import::Error::InvalidRequest | duduclaw_import::Error::Schema
                | duduclaw_import::Error::Content | duduclaw_import::Error::Limit => 4,
            duduclaw_import::Error::Io | duduclaw_import::Error::Isolation => 3,
        };
        std::process::exit(code);
    }
}
