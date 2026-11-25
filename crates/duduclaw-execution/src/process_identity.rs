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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessNode { identity: ProcessIdentity, parent: u32 }

#[cfg(target_os = "linux")]
fn read_node(pid: u32) -> io::Result<ProcessNode> {
    parse_linux_stat(pid, &std::fs::read_to_string(format!("/proc/{pid}/stat"))?)
}

#[cfg(target_os = "macos")]
fn read_identity(pid: u32) -> io::Result<ProcessIdentity> { read_node(pid).map(|node| node.identity) }

#[cfg(windows)]
fn read_node(pid: u32) -> io::Result<ProcessNode> {
    // Creation-time checks reject ordinary PID reuse, not deliberate Windows
    // PROC_THREAD_ATTRIBUTE_PARENT_PROCESS spoofing by the same OS account.
    use windows_sys::Win32::{Foundation::{INVALID_HANDLE_VALUE, ERROR_NO_MORE_FILES},
        System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW,
            Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS}};
    let process = RootProcess::capture(pid)?;
    // SAFETY: scalar flags; successful snapshot handle is owned below.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    // SAFETY: snapshot and correctly sized output buffer remain live.
    let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        if entry.th32ProcessID == pid {
            if !process.is_current() { return Err(invalid()); }
            return Ok(ProcessNode { identity: process.identity(), parent: entry.th32ParentProcessID });
        }
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) { Err(invalid()) } else { Err(error) }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn read_node(_pid: u32) -> io::Result<ProcessNode> { Err(invalid()) }

/// Fresh, bounded ancestry proof. Every retained node is re-read before success;
/// no PID result survives a request. Reparenting, inaccessible nodes and exits deny.
pub fn is_descendant(peer: &RootProcess, root: ProcessIdentity) -> bool {
    let mut pins = Vec::new();
    let valid = walk_ancestry(peer.identity(), root, |pid| {
        let pin = RootProcess::capture(pid)?;
        let node = read_node(pid)?;
        if node.identity != pin.identity() || !pin.is_current() { return Err(invalid()); }
        pins.push(pin);
        Ok(node)
    });
    valid && peer.is_current() && pins.iter().all(RootProcess::is_current) && root.is_current()
}

fn walk_ancestry(mut caller: ProcessIdentity, root: ProcessIdentity,
    mut read: impl FnMut(u32) -> io::Result<ProcessNode>) -> bool
{
    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for depth in 0..=crate::spawn_binding::MAX_ANCESTRY_DEPTH {
        if !seen.insert(caller.pid) { return false; }
        let Ok(node) = read(caller.pid) else { return false; };
        if node.identity != caller { return false; }
        chain.push(node);
        if caller.pid == root.pid {
            return caller == root && chain.iter().all(|node| read(node.identity.pid).is_ok_and(|now| now == *node));
        }
        if depth == crate::spawn_binding::MAX_ANCESTRY_DEPTH || node.parent == 0 { return false; }
        let Ok(parent) = read(node.parent) else { return false; };
        // A reused parent PID cannot have been born after its alleged child.
        if parent.identity.birth > caller.birth { return false; }
        caller = parent.identity;
    }
    false
}

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

    /// Socket credentials, never a PID supplied in the protocol. macOS has a
    /// PID-to-birth lookup window; Linux without SO_PEERPIDFD has the same limit.
    #[cfg(unix)]
    pub fn unix_peer(socket: std::os::fd::BorrowedFd<'_>) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;
            let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
            let mut length = std::mem::size_of_val(&cred) as libc::socklen_t;
            // SAFETY: live socket and correctly sized output buffers.
            if unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(), &mut length) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if length as usize != std::mem::size_of_val(&cred) || cred.pid <= 0
                || cred.uid != unsafe { libc::geteuid() } { return Err(invalid()); }
            let mut fd: libc::c_int = -1;
            let mut length = std::mem::size_of_val(&fd) as libc::socklen_t;
            // Linux uapi SO_PEERPIDFD = 77 (6.5+); older libc releases omit it.
            if unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, 77,
                (&mut fd as *mut libc::c_int).cast(), &mut length) } == 0 {
                if fd < 0 { return Err(invalid()); }
                let pidfd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
                if length as usize != std::mem::size_of_val(&fd) { return Err(invalid()); }
                let root = Self { identity: ProcessIdentity::read(cred.pid as u32)?, pidfd: Some(pidfd) };
                if !root.is_current() { return Err(invalid()); }
                return Ok(root);
            }
            let error = io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::ENOPROTOOPT) | Some(libc::EINVAL) | Some(libc::ENOSYS)) {
                return Err(error);
            }
            Self::capture(cred.pid as u32)
        }
        #[cfg(target_os = "macos")]
        {
            let mut pid: libc::pid_t = 0;
            let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
            // LOCAL_PEERPID identifies the connector; Tokio uses LOCAL_PEEREPID.
            if unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(), &mut length) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let (mut uid, mut gid) = (0, 0);
            if length as usize != std::mem::size_of_val(&pid) || pid <= 0
                || unsafe { libc::getpeereid(socket.as_raw_fd(), &mut uid, &mut gid) } != 0
                || uid != unsafe { libc::geteuid() } { return Err(invalid()); }
            Self::capture(pid as u32)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        { let _ = socket; Err(invalid()) }
    }

    #[cfg(windows)]
    pub fn pipe_peer(pipe: BorrowedHandle<'_>) -> io::Result<Self> {
        use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
        let mut pid = 0;
        // SAFETY: live connected server handle and writable scalar output.
        if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // The pipe API exposes a PID, not connection-time creation time; there
        // remains a PID-to-handle capture window (D14's documented limitation).
        let root = Self::capture(pid)?;
        let mut again = 0;
        if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut again) } == 0
            || again != pid || !root.is_current() { return Err(invalid()); }
        Ok(root)
    }

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
    read_node(pid).map(|node| node.identity)
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(pid: u32, stat: &str) -> io::Result<ProcessNode> {
    // comm may contain spaces, newlines and ')'; fields start after its LAST ')'.
    let (prefix, fields) = stat.rsplit_once(')').ok_or_else(invalid)?;
    let (number, _) = prefix.split_once(" (").ok_or_else(invalid)?;
    if number.parse::<u32>().ok() != Some(pid) || pid == 0 { return Err(invalid()); }
    let mut fields = fields.split_whitespace();
    match fields.next() {
        Some("R" | "S" | "D" | "T" | "t" | "W" | "K" | "P" | "I") => {},
        _ => return Err(invalid()),
    }
    // State is field 3, parent is field 4, starttime is field 22.
    let parent = fields.next().ok_or_else(invalid)?.parse().map_err(|_| invalid())?;
    let birth = fields.nth(17).ok_or_else(invalid)?.parse().map_err(|_| invalid())?;
    Ok(ProcessNode { identity: ProcessIdentity { pid, birth }, parent })
}

#[cfg(target_os = "macos")]
fn read_node(pid: u32) -> io::Result<ProcessNode> {
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
    Ok(ProcessNode { identity: ProcessIdentity { pid, birth }, parent: info.pbi_ppid })
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
        assert_eq!(parse_linux_stat(42, &stat).unwrap().identity, ProcessIdentity { pid: 42, birth: 12345 });
        assert!(parse_linux_stat(43, &stat).is_err());
        assert!(parse_linux_stat(42, "42 (gone) Z 0").is_err());
        let fields = (5..=21).map(|_| "0").collect::<Vec<_>>().join(" ");
        let node = parse_linux_stat(42, &format!("42 (nested (and) spaces)) S 7 {fields} 900 0")).unwrap();
        assert_eq!(node.parent, 7);
        assert_eq!(node.identity.birth, 900);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn ancestry_depth_cycle_missing_permission_and_birth_mismatch_deny() {
        let id = |pid| ProcessIdentity { pid, birth: u64::from(pid) };
        let read = |pid| Ok(ProcessNode { identity: id(pid), parent: pid - 1 });
        assert!(walk_ancestry(id(65), id(1), read)); // exactly 64 edges
        assert!(!walk_ancestry(id(66), id(1), read));
        assert!(walk_ancestry(id(5), id(5), read));
        assert!(!walk_ancestry(id(5), ProcessIdentity { pid: 5, birth: 4 }, read));
        assert!(!walk_ancestry(id(5), id(1), |_| Err(io::ErrorKind::PermissionDenied.into())));
        assert!(!walk_ancestry(id(5), id(1), |pid| if pid == 3 { Err(invalid()) } else { read(pid) }));
        let cycle = |pid| Ok(ProcessNode { identity: ProcessIdentity { pid, birth: 1 },
            parent: if pid == 5 { 4 } else { 5 } });
        assert!(!walk_ancestry(ProcessIdentity { pid: 5, birth: 1 }, id(1), cycle));
        assert!(!walk_ancestry(id(5), id(1), |pid| {
            let mut node = read(pid)?;
            if pid == 4 { node.identity.birth = 6; }
            Ok(node)
        }));
        let mut count = 0;
        assert!(!walk_ancestry(id(2), id(1), |pid| {
            count += 1;
            let mut node = read(pid)?;
            if count > 3 { node.identity.birth += 100; }
            Ok(node)
        }));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn native_parent_query_matches_current_generation() {
        let node = read_node(std::process::id()).unwrap();
        assert_eq!(node.identity, ProcessIdentity::read(std::process::id()).unwrap());
        assert!(node.parent > 0);
        let parent = read_node(node.parent).unwrap();
        assert!(parent.identity.birth <= node.identity.birth);
    }
}
