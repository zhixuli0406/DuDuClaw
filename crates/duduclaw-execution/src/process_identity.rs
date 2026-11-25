//! OS process generations. Never construct an identity from employee claims.
use std::io;

/// Birth is kernel start ticks (Linux), microseconds since epoch (macOS), or
/// FILETIME ticks (Windows). Values are comparable only on the same host/boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub birth: u64,
}

impl ProcessIdentity {
    pub fn read(pid: u32) -> io::Result<Self> { read_identity(pid) }

    /// No successful PID check is cached. Missing, exited and reused PIDs deny.
    pub fn is_current(self) -> bool { Self::read(self.pid).is_ok_and(|now| now == self) }
}

fn invalid() -> io::Error { io::Error::other("process identity unavailable or exited") }

/// Root evidence retained for the entire binding lifetime. Linux pins the
/// process with pidfd when supported; Windows always retains a process handle.
#[derive(Debug)]
pub struct RootProcess {
    identity: ProcessIdentity,
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
}

impl RootProcess {
    pub fn identity(&self) -> ProcessIdentity { self.identity }

    pub fn capture(pid: u32) -> io::Result<Self> {
        #[cfg(windows)]
        { Self::from_handle(open_process(pid)?.as_handle()) }
        #[cfg(not(windows))]
        {
            let identity = ProcessIdentity::read(pid)?;
            #[cfg(target_os = "linux")]
            let pidfd = pidfd_open(pid)?;
            let root = Self { identity, #[cfg(target_os = "linux")] pidfd };
            if !root.is_current() { return Err(invalid()); }
            Ok(root)
        }
    }

    #[cfg(windows)]
    pub fn from_handle(handle: std::os::windows::io::BorrowedHandle<'_>) -> io::Result<Self> {
        let handle = handle.try_clone_to_owned()?;
        let identity = windows_identity(handle.as_handle())?;
        Ok(Self { identity, handle })
    }

    pub fn is_current(&self) -> bool {
        #[cfg(windows)]
        { windows_identity(self.handle.as_handle()).is_ok_and(|now| now == self.identity) }
        #[cfg(not(windows))]
        {
            #[cfg(target_os = "linux")]
            if let Some(fd) = &self.pidfd {
                use std::os::fd::AsRawFd;
                let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: one initialized pollfd; zero timeout never blocks.
                if unsafe { libc::poll(&mut poll, 1, 0) } != 0 { return false; }
            }
            self.identity.is_current()
        }
    }
}

#[cfg(target_os = "linux")]
fn pidfd_open(pid: u32) -> io::Result<Option<std::os::fd::OwnedFd>> {
    use std::os::fd::FromRawFd;
    // SAFETY: pidfd_open takes scalar arguments, returns a CLOEXEC descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
    if fd >= 0 {
        // SAFETY: the successful syscall returned a new owned descriptor.
        return Ok(Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) }));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS) | Some(libc::EINVAL) => Ok(None),
        _ => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn read_identity(pid: u32) -> io::Result<ProcessIdentity> {
    parse_linux_stat(pid, &std::fs::read_to_string(format!("/proc/{pid}/stat"))?)
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(pid: u32, stat: &str) -> io::Result<ProcessIdentity> {
    // comm may contain spaces, newlines and ')'; fields start after its LAST ')'.
    let (prefix, fields) = stat.rsplit_once(')').ok_or_else(invalid)?;
    let (number, _) = prefix.split_once(" (").ok_or_else(invalid)?;
    if number.parse::<u32>().ok() != Some(pid) || pid == 0 { return Err(invalid()); }
    let mut fields = fields.split_whitespace();
    match fields.next() {
        Some("R" | "S" | "D" | "T" | "t" | "W" | "K" | "P" | "I") => {},
        _ => return Err(invalid()),
    }
    // State is field 3; after consuming it, starttime (22) is offset 18.
    let birth = fields.nth(18).ok_or_else(invalid)?.parse().map_err(|_| invalid())?;
    Ok(ProcessIdentity { pid, birth })
}

#[cfg(target_os = "macos")]
fn read_identity(pid: u32) -> io::Result<ProcessIdentity> {
    let native_pid = i32::try_from(pid).ok().filter(|pid| *pid > 0).ok_or_else(invalid)?;
    // SAFETY: proc_bsdinfo is a POD output buffer of exactly the advertised size.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe { libc::proc_pidinfo(native_pid, libc::PROC_PIDTBSDINFO, 0,
        (&mut info as *mut libc::proc_bsdinfo).cast(), size) };
    if read != size || info.pbi_pid != pid || matches!(info.pbi_status, 0 | libc::SZOMB) {
        return Err(invalid());
    }
    let birth = info.pbi_start_tvsec.checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec)).ok_or_else(invalid)?;
    Ok(ProcessIdentity { pid, birth })
}

#[cfg(windows)]
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle, BorrowedHandle};

#[cfg(windows)]
fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: scalar PID; non-inheritable, read-only process handle.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() { return Err(io::Error::last_os_error()); }
    // SAFETY: successful OpenProcess transfers ownership of the fresh handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

#[cfg(windows)]
fn windows_identity(handle: BorrowedHandle<'_>) -> io::Result<ProcessIdentity> {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::{GetProcessId, GetProcessTimes}};
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut birth, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the borrowed handle is live; all four output buffers are valid.
    if unsafe { GetProcessTimes(handle.as_raw_handle(), &mut birth, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if exit.dwLowDateTime != 0 || exit.dwHighDateTime != 0 { return Err(invalid()); }
    // SAFETY: handle remains live throughout the query.
    let pid = unsafe { GetProcessId(handle.as_raw_handle()) };
    if pid == 0 { return Err(invalid()); }
    Ok(ProcessIdentity { pid, birth: (u64::from(birth.dwHighDateTime) << 32) | u64::from(birth.dwLowDateTime) })
}

#[cfg(windows)]
fn read_identity(pid: u32) -> io::Result<ProcessIdentity> { windows_identity(open_process(pid)?.as_handle()) }

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn read_identity(_pid: u32) -> io::Result<ProcessIdentity> { Err(invalid()) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_birth_reader() {
        let root = RootProcess::capture(std::process::id()).unwrap();
        assert_eq!(root.identity(), ProcessIdentity::read(std::process::id()).unwrap());
        assert!(root.is_current());
        let mut other = root.identity();
        other.birth = other.birth.wrapping_add(1);
        assert!(!other.is_current());
        let mut stale = root;
        stale.identity = other;
        let binding = crate::spawn_binding::SpawnBinding::pending(std::time::Instant::now() + std::time::Duration::from_secs(10));
        assert!(binding.bind_spawn(stale).is_err());
        assert!(binding.root().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_comm_is_not_split_on_whitespace_or_first_parenthesis() {
        let fields = (4..=21).map(|_| "0").collect::<Vec<_>>().join(" ");
        let stat = format!("42 (a ) tricky\n名) S {fields} 12345 0");
        assert_eq!(parse_linux_stat(42, &stat).unwrap(), ProcessIdentity { pid: 42, birth: 12345 });
        assert!(parse_linux_stat(43, &stat).is_err());
        assert!(parse_linux_stat(42, "42 (gone) Z 0").is_err());
    }
}
