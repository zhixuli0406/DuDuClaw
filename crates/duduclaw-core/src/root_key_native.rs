//! Native systemd attestation facts and Linux probes. No configuration fallback.

#[cfg(any(target_os = "linux", test))]
#[path = "root_key_unix_diag.rs"]
mod unix_diag;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CgroupState { Unit, V1, #[default] Invalid }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutableState {
    Protected, #[default] Unreadable, NotRegular, Owner, Writable,
    AncestorType, AncestorOwner, AncestorWritable,
}

#[derive(Debug, Clone, Default)]
pub struct NativeInputs {
    pub initial_user_namespace: bool,
    pub container_environment_absent: bool,
    pub ppid: u32,
    pub cgroup: CgroupState,
    pub executable: ExecutableState,
    pub supplementary_groups: Option<u32>,
    pub engine_sockets_denied: bool,
    pub engine_scan_complete: bool,
    pub engine_socket_detail: Option<String>,
}

pub(super) fn attest(i: &NativeInputs) -> Result<(), &'static str> {
    if !i.initial_user_namespace { return Err("user_namespace"); }
    if !i.container_environment_absent { return Err("container_environment"); }
    if i.ppid != 1 { return Err("not_systemd_child"); }
    match i.cgroup {
        CgroupState::Unit => (),
        CgroupState::V1 => return Err("cgroup_v1"),
        CgroupState::Invalid => return Err("cgroup_unit"),
    }
    match i.executable {
        ExecutableState::Protected => (),
        ExecutableState::Unreadable => return Err("exe_stat"),
        ExecutableState::NotRegular => return Err("exe_type"),
        ExecutableState::Owner => return Err("exe_owner"),
        ExecutableState::Writable => return Err("exe_writable"),
        ExecutableState::AncestorType => return Err("exe_ancestor_type"),
        ExecutableState::AncestorOwner => return Err("exe_ancestor_owner"),
        ExecutableState::AncestorWritable => return Err("exe_ancestor_writable"),
    }
    if i.supplementary_groups != Some(0) { return Err("supplementary_groups"); }
    if !i.engine_scan_complete { return Err("engine_socket_probe_failed"); }
    if !i.engine_sockets_denied { return Err("engine_socket_reachable"); }
    Ok(())
}

/// The initial namespace has one full-range identity mapping, not root mapped
/// to a subordinate uid. Do not normalize multiple lines into a single mapping.
pub fn initial_user_namespace_map(input: Option<&str>) -> bool {
    let Some(input) = input else { return false; };
    let mut lines = input.lines();
    let Some(line) = lines.next() else { return false; };
    lines.next().is_none() && line.split_ascii_whitespace().eq(["0", "0", "4294967295"])
}

#[cfg(target_os = "linux")]
fn initial_user_namespace() -> bool {
    ["/proc/self/uid_map", "/proc/self/gid_map"].iter().all(|path| {
        initial_user_namespace_map(std::fs::read_to_string(path).ok().as_deref())
    })
}

#[cfg(target_os = "linux")]
fn container_environment_absent() -> bool {
    for path in ["/run/.containerenv", "/run/systemd/container"] {
        if !matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
            return false;
        }
    }
    match std::fs::read("/proc/1/environ") {
        Ok(bytes) => !bytes.split(|byte| *byte == 0).any(|entry| entry.starts_with(b"container=")),
        // Linux normally denies the unprivileged gateway access to PID 1's
        // environment. The root launcher already checked it before key intake.
        Err(error) => (unsafe { libc::geteuid() }) != 0
            && error.kind() == std::io::ErrorKind::PermissionDenied,
    }
}

/// Shared installer/launcher-contract gate. Root must inspect PID 1's environment.
#[cfg(target_os = "linux")]
pub fn native_host_check() -> Result<(), &'static str> {
    if !initial_user_namespace() { return Err("user_namespace"); }
    if !container_environment_absent() || crate::ns_guard::is_containerized() {
        return Err("container_environment");
    }
    Ok(())
}

/// Abstract sockets are matched as raw bytes by slash-delimited components,
/// exactly an engine name or its . / - / _ suffix namespace.
#[cfg(any(target_os = "linux", test))]
fn abstract_engine_name(name: &[u8]) -> bool {
    name.strip_prefix(b"@").is_some_and(|name| name.split(|b| *b == b'/').any(|component| {
        [b"docker".as_slice(), b"containerd", b"podman", b"buildkit", b"lxd"].iter().any(|engine| {
            component == *engine || component.strip_prefix(*engine).is_some_and(|suffix| {
                matches!(suffix.first(), Some(b'.' | b'-' | b'_'))
            })
        })
    }))
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy)]
struct Peer { uid: u32 }

#[cfg(any(target_os = "linux", test))]
fn parse_number(bytes: &[u8], radix: u32) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(|b| match radix {
        16 => b.is_ascii_hexdigit(), _ => b.is_ascii_digit(),
    }) { return None; }
    u64::from_str_radix(std::str::from_utf8(bytes).ok()?, radix).ok()
}

#[cfg(any(target_os = "linux", test))]
fn abstract_socket_finding(input: Option<&[u8]>, mut peer_cred: impl FnMut(&[u8]) -> Option<Peer>) -> Result<Option<String>, String> {
    // Never retain a previous verdict, including while Held. /proc renders
    // raw name bytes (and newlines), so validate every row before ignoring it.
    // This conservative byte parser avoids UTF-8 decoding of the whole table.
    let Some(input) = input else { return Err("/proc/net/unix: table unreadable".into()); };
    let Some(input) = input.strip_suffix(b"\n") else {
        return Err("/proc/net/unix: truncated table".into());
    };
    let mut lines = input.split(|byte| *byte == b'\n');
    if lines.next() != Some(b"Num       RefCount Protocol Flags    Type St Inode Path".as_slice()) {
        return Err("/proc/net/unix: unexpected table header".into());
    }
    let mut reachable = None;
    for line in lines {
        // A name containing whitespace or a newline continuation is ambiguous;
        // reject instead of guessing whether it belongs to an engine row.
        let mut rest = line;
        let mut fields = Vec::with_capacity(7);
        for _ in 0..7 {
            rest = rest.trim_ascii_start();
            let end = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        let name = if rest.is_empty() { rest } else {
            let name = rest.strip_prefix(b" ").unwrap_or_default();
            if name.is_empty() || name.iter().any(u8::is_ascii_whitespace) {
                return Err("/proc/net/unix: malformed row or split socket name".into());
            }
            name
        };
        if !line.first().is_some_and(u8::is_ascii_hexdigit)
            || fields[0].strip_suffix(b":").and_then(|v| parse_number(v, 16)).is_none()
            || fields[1..6].iter().any(|v| parse_number(v, 16).is_none())
            || parse_number(fields[6], 10).is_none() {
            return Err("/proc/net/unix: malformed row or split socket name".into());
        }
        let flags = parse_number(fields[3], 16).filter(|v| *v <= u32::MAX as u64)
            .ok_or_else(|| "/proc/net/unix: invalid flags".to_owned())?;
        // The kernel prints embedded NUL as '@', losing address identity.
        if name.iter().any(|b| b.is_ascii_control()) || name.get(1..).is_some_and(|s| s.contains(&b'@')) {
            return Err("/proc/net/unix: ambiguous socket name".into());
        }
        if !abstract_engine_name(name) || flags & 0x10000 == 0 { continue; }
        if let Some(Peer { uid: 0 }) = peer_cred(name) {
            let label: String = name.iter().flat_map(|byte| std::ascii::escape_default(*byte)).map(char::from).collect();
            reachable.get_or_insert_with(|| format!("{label}: peer uid=0 (root)"));
        }
    }
    Ok(reachable)
}

#[cfg(test)]
fn abstract_sockets_denied(input: Option<&[u8]>, peer_cred: impl FnMut(&[u8]) -> Option<Peer>) -> Result<(), String> {
    match abstract_socket_finding(input, peer_cred)? { Some(detail) => Err(detail), None => Ok(()) }
}

#[cfg(target_os = "linux")]
fn abstract_peer_cred(name: &[u8]) -> Option<Peer> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let name = name.strip_prefix(b"@")?;
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if name.len() + 1 > address.sun_path.len() { return None; }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path[1..].iter_mut().zip(name) { *target = *byte as libc::c_char; }
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 { return None; }
    // SAFETY: a fresh owned socket; the abstract address starts with NUL and
    // its length includes exactly the name, without a terminating NUL.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    let length = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + 1 + name.len()) as libc::socklen_t;
    let connected = unsafe { libc::connect(socket.as_raw_fd(), (&address as *const libc::sockaddr_un).cast(), length) };
    if connected != 0 {
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINPROGRESS) { return None; }
        let mut poll = libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
        if unsafe { libc::poll(&mut poll, 1, 20) } <= 0 { return None; }
        let mut error: libc::c_int = 0;
        let mut size = std::mem::size_of_val(&error) as libc::socklen_t;
        if unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_ERROR,
            (&mut error as *mut libc::c_int).cast(), &mut size) } != 0 || error != 0 { return None; }
    }
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
    if unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED,
        (&mut credentials as *mut libc::ucred).cast(), &mut size) } != 0
        || size as usize != std::mem::size_of_val(&credentials) { return None; }
    Some(Peer { uid: credentials.uid })
}

#[derive(Debug, PartialEq)]
#[cfg(any(target_os = "linux", all(test, unix)))]
struct ProbeFailure { reachable: bool, detail: String }
#[cfg(any(target_os = "linux", all(test, unix)))]
impl ProbeFailure {
    fn uncertain(detail: impl Into<String>) -> Self { Self { reachable: false, detail: detail.into() } }
    fn reachable(detail: String) -> Self { Self { reachable: true, detail } }
}

#[cfg(target_os = "linux")]
pub(super) fn reattest(uid: u32) -> Result<(), super::Refusal> {
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count != 0 { Err("supplementary_groups".into()) } else {
        engine_sockets_denied(uid).map_err(|failure| super::Refusal {
            reason: if failure.reachable { "engine_socket_reachable" } else { "engine_socket_probe_failed" },
            detail: Some(failure.detail),
        })
    }
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn periodic_decision(result: Result<(), super::Refusal>, action: super::policy::EngineSocketAction) -> Result<(), super::Refusal> {
    if action == super::policy::EngineSocketAction::Warn {
        if let Err(failure) = &result {
            if failure.reason == "engine_socket_reachable" {
                return Ok(());
            }
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn engine_sockets_denied(uid: u32) -> Result<(), ProbeFailure> {
    let mut reachable = None;
    for path in [
        "/var/run/docker.sock".to_string(), "/run/docker.sock".to_string(),
        "/run/podman/podman.sock".to_string(), "/var/run/podman/podman.sock".to_string(),
        "/run/containerd/containerd.sock".to_string(),
        format!("/run/user/{uid}/docker.sock"), format!("/run/user/{uid}/podman/podman.sock"),
    ] {
        match engine_socket_denied(&path) {
            Err(failure) if failure.reachable => { reachable.get_or_insert(failure); }
            Err(failure) => return Err(failure),
            Ok(()) => (),
        }
    }
    let table = std::fs::read("/proc/net/unix");
    if let Some(detail) = abstract_socket_finding(table.ok().as_deref(), abstract_peer_cred).map_err(ProbeFailure::uncertain)? {
        reachable.get_or_insert_with(|| ProbeFailure::reachable(detail));
    }
    // Inspect all kernel names even when a reachable socket was already found:
    // warn must never mask an unreadable table or an ambiguous name.
    unix_diag::validate_names().map_err(ProbeFailure::uncertain)?;
    match reachable { Some(failure) => Err(failure), None => Ok(()) }
}

/// Accept exactly one v2 unit line, with at most the kernel's trailing newline.
#[cfg(any(target_os = "linux", test))]
pub(super) fn parse_cgroup(input: Option<&str>) -> CgroupState {
    let Some(input) = input else { return CgroupState::Invalid; };
    if input.split_terminator('\n').any(|line| {
        let mut fields = line.splitn(3, ':');
        matches!((fields.next(), fields.next(), fields.next()),
            (Some(id), Some(controllers), Some(_))
                if id.parse::<u32>().is_ok_and(|id| id != 0) && !controllers.is_empty())
    }) { return CgroupState::V1; }
    match input.strip_suffix('\n').unwrap_or(input) {
        "0::/system.slice/duduclaw-gateway.service" => CgroupState::Unit,
        _ => CgroupState::Invalid,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn gather(uid: u32) -> NativeInputs {
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let socket_check = engine_sockets_denied(uid);
    NativeInputs {
        initial_user_namespace: initial_user_namespace(),
        container_environment_absent: container_environment_absent(),
        ppid: unsafe { libc::getppid() } as u32,
        cgroup: parse_cgroup(std::fs::read_to_string("/proc/self/cgroup").ok().as_deref()),
        executable: executable_state(),
        supplementary_groups: (count >= 0).then_some(count as u32),
        engine_sockets_denied: socket_check.is_ok(),
        engine_scan_complete: socket_check.as_ref().err().is_none_or(|failure| failure.reachable),
        engine_socket_detail: socket_check.err().map(|failure| failure.detail),
    }
}

#[cfg(target_os = "linux")]
fn executable_state() -> ExecutableState {
    use std::os::unix::fs::MetadataExt;
    let Ok(path) = std::fs::read_link("/proc/self/exe") else { return ExecutableState::Unreadable; };
    if !path.is_absolute() { return ExecutableState::Unreadable; }
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return ExecutableState::Unreadable; };
    // Compare the named inode to the executable actually mapped by this process.
    let Ok(running) = std::fs::metadata("/proc/self/exe") else { return ExecutableState::Unreadable; };
    if meta.dev() != running.dev() || meta.ino() != running.ino() { return ExecutableState::Unreadable; }
    if !meta.is_file() { return ExecutableState::NotRegular; }
    if meta.uid() != 0 { return ExecutableState::Owner; }
    if meta.mode() & 0o022 != 0 { return ExecutableState::Writable; }
    for parent in path.ancestors().skip(1) {
        let Ok(meta) = std::fs::symlink_metadata(parent) else { return ExecutableState::Unreadable; };
        if !meta.is_dir() { return ExecutableState::AncestorType; }
        if meta.uid() != 0 { return ExecutableState::AncestorOwner; }
        if meta.mode() & 0o022 != 0 { return ExecutableState::AncestorWritable; }
    }
    ExecutableState::Protected
}

#[cfg(target_os = "linux")]
fn engine_socket_denied(path: &str) -> Result<(), ProbeFailure> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let label = path;
    let Ok(path) = std::ffi::CString::new(path) else { return Err(ProbeFailure::uncertain(format!("{label}: invalid path"))); };
    // Both DAC writability and an actual nonblocking connection must be denied.
    if unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0 {
        return Err(ProbeFailure::reachable(format!("{label}: writable via access(W_OK)")));
    }
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 { return Err(ProbeFailure::uncertain(format!("{label}: socket creation failed, errno={:?}", std::io::Error::last_os_error().raw_os_error()))); }
    // SAFETY: ownership of a newly created descriptor, closed on every path.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_bytes_with_nul();
    if bytes.len() > address.sun_path.len() { return Err(ProbeFailure::uncertain(format!("{label}: path too long"))); }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) { *target = *byte as libc::c_char; }
    let result = unsafe { libc::connect(socket.as_raw_fd(),
        (&address as *const libc::sockaddr_un).cast(),
        std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t) };
    // EAGAIN/EINPROGRESS/ECONNREFUSED and all unknown errors are not proof of isolation.
    connection_denied(label, result, std::io::Error::last_os_error().raw_os_error())
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn connection_denied(path: &str, result: i32, errno: Option<i32>) -> Result<(), ProbeFailure> {
    if result == -1 && matches!(errno, Some(libc::EACCES | libc::EPERM | libc::ENOENT)) {
        Ok(())
    } else if result == 0 {
        Err(ProbeFailure::reachable(format!("{path}: connect succeeded")))
    } else {
        Err(ProbeFailure::uncertain(format!("{path}: connect unexpected errno={errno:?}")))
    }
}

#[cfg(all(target_os = "linux", test))]
mod tests {
    #[test]
    fn socket_probe_accepts_only_definite_denials() {
        for errno in [libc::EACCES, libc::EPERM, libc::ENOENT] {
            assert!(super::connection_denied("/run/docker.sock", -1, Some(errno)).is_ok());
            assert!(super::connection_denied("/run/docker.sock", 0, Some(errno)).is_err());
        }
        for errno in [None, Some(libc::EAGAIN), Some(libc::EINPROGRESS),
            Some(libc::ECONNREFUSED), Some(libc::ENOTDIR), Some(libc::EMFILE)] {
            assert!(super::connection_denied("/run/docker.sock", -1, errno).is_err());
        }
    }

    #[test]
    fn socket_probe_refuses_reachable_and_stale_sockets() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.sock");
        let path = path.to_str().unwrap();
        assert!(super::engine_socket_denied(path).is_ok());
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        assert!(super::engine_socket_denied(path).is_err());
        // Exercise connect after access(W_OK) fails, rather than only the
        // early writable-path refusal. Root bypasses DAC, so cannot test it.
        if unsafe { libc::geteuid() } != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert!(super::engine_socket_denied(path).is_ok());
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        drop(listener);
        assert!(super::engine_socket_denied(path).is_err());
    }
}

#[cfg(test)]
mod parser_tests {
    use super::*;
    const HEADER: &[u8] = b"Num       RefCount Protocol Flags    Type St Inode Path\n";
    const CLI: &[u8] = b"@docker_cli_3e708842d9322cef25983565c19d0866";
    const ROOT: Peer = Peer { uid: 0 };

    fn table(name: &[u8]) -> Vec<u8> {
        [HEADER, b"0000: 2 0 00010000 0001 01 42 ", name, b"\n"].concat()
    }

    #[test]
    fn root_key_reachable_socket_cannot_hide_later_table_errors() {
        let mut input = table(b"@docker.sock");
        input.extend_from_slice(b"malformed row\n");
        assert!(abstract_socket_finding(Some(&input), |_| Some(ROOT)).is_err());
        let reachable = abstract_socket_finding(Some(&table(b"@docker.sock")), |_| Some(ROOT)).unwrap();
        assert!(reachable.is_some());
    }

    #[test]
    fn root_key_abstract_detail_identifies_exact_root_peer() {
        assert_eq!(abstract_sockets_denied(Some(&table(b"@/containerd-shim/example")), |_| Some(ROOT)),
            Err("@/containerd-shim/example: peer uid=0 (root)".into()));
        assert!(abstract_sockets_denied(Some(b"bad header\n"), |_| None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn root_key_path_connection_detail_keeps_errno_and_success() {
        assert_eq!(connection_denied("/run/docker.sock", 0, Some(libc::ENOENT)),
            Err(ProbeFailure::reachable("/run/docker.sock: connect succeeded".into())));
        assert_eq!(connection_denied("/run/docker.sock", -1, Some(libc::ECONNREFUSED)),
            Err(ProbeFailure::uncertain(format!("/run/docker.sock: connect unexpected errno=Some({})", libc::ECONNREFUSED))));
        for errno in [libc::EACCES, libc::EPERM, libc::ENOENT] {
            assert!(connection_denied("/run/docker.sock", -1, Some(errno)).is_ok());
        }
    }

    #[test]
    fn root_key_abstract_peer_credentials_and_read_errors() {
        let table = table(b"@attacker/docker");
        assert!(abstract_sockets_denied(Some(&table), |_| Some(Peer { uid: 1000 })).is_ok());
        assert!(abstract_sockets_denied(Some(&table), |_| None).is_ok());
        assert!(abstract_sockets_denied(Some(&table), |_| Some(ROOT)).is_err());
        // Intake, Held and Refused all use this same fail-closed read path.
        assert!(abstract_sockets_denied(None, |_| panic!("no table")).is_err());
        assert!(abstract_sockets_denied(Some(HEADER), |_| panic!("no candidates")).is_ok());
    }

    #[test]
    fn root_key_non_utf8_names_do_not_blind_engine_probe() {
        let mut input = table(b"@unrelated-\xff");
        input.extend_from_slice(&table(b"@docker.sock")[HEADER.len()..]);
        let result = abstract_sockets_denied(Some(&input), |name| {
            assert_eq!(name, b"@docker.sock"); Some(ROOT)
        });
        assert_eq!(result, Err("@docker.sock: peer uid=0 (root)".into()));
        assert!(abstract_sockets_denied(Some(&table(b"@unrelated-\xff")), |_| panic!("non-engine")).is_ok());
        assert!(abstract_sockets_denied(Some(&table(b"@docker_\xff")), |name| {
            assert_eq!(name, b"@docker_\xff"); Some(ROOT)
        }).unwrap_err().contains("\\xff"));
    }

    #[test]
    fn root_key_malformed_rows_and_split_names_fail_closed() {
        for row in [b"bad row\n".as_slice(), b"\n", b"0000: 2 0 broken 0001 01 42 @unrelated\n",
            b"0000: 2 0 100000000 0001 01 42 @unrelated\n", b"0000: 2 0 10000 0001 01\n",
            b"0000: 2 0 10000 0001 01 NaN @unrelated\n", b"x: 2 0 10000 0001 01 42 @unrelated\n",
            b"0000: 2 0 10000 nope 01 42 @unrelated\n", b"0000: 2 0 10000 0001 01 42 @name extra\n",
            b"0000: 2 0 10000 0001 01 42 @unrelated\ncontinuation\n",
            b"0000: 2 0 10000 0001 01 42 @unrelated\r\n",
            b"0000: 2 0 10000 0001 01 42 @unrelated \n",
            b"0000: 2 0 10000 0001 01 42  @unrelated\n",
            b"0000: 2 0 10000 0001 01 42 @embedded@nul\n",
            b"0000: 2 0 10000 0001 01 42 @unterminated"] {
            let input = [HEADER, row].concat();
            assert!(abstract_sockets_denied(Some(&input), |_| None).is_err(), "{row:?}");
        }
        let unnamed = [HEADER, b"0000: 2 0 00000000 0001 03 42\n"].concat();
        assert!(abstract_sockets_denied(Some(&unnamed), |_| panic!("unnamed")).is_ok());
    }

    #[test]
    fn root_key_abstract_engine_socket_names_are_component_anchored() {
        for name in ["@docker", "@docker.sock", "@run/podman/control", "@containerd-shim",
            "@buildkit_worker", "@lxd/unix.socket"] {
            assert!(abstract_engine_name(name.as_bytes()), "{name}");
            assert!(abstract_sockets_denied(Some(&table(name.as_bytes())), |_| Some(ROOT)).is_err());
            let connected = format!("0000: 2 0 00000000 0001 03 42 {name}\n");
            let input = [HEADER, connected.as_bytes()].concat();
            assert!(abstract_sockets_denied(Some(&input), |_| panic!("not listening")).is_ok());
        }
        for name in ["@notdocker", "@dockerish", "@my-containerd", "@notes-about-podman",
            "@path/dockerman", "/run/docker.sock"] {
            assert!(!abstract_engine_name(name.as_bytes()), "{name}");
        }
    }

    #[test]
    fn root_key_docker_cli_root_peer_is_always_an_engine_socket() {
        assert!(abstract_engine_name(CLI));
        assert_eq!(abstract_sockets_denied(Some(&table(CLI)), |_| Some(ROOT)),
            Err("@docker_cli_3e708842d9322cef25983565c19d0866: peer uid=0 (root)".into()));
        assert!(abstract_sockets_denied(Some(&table(CLI)), |_| Some(Peer { uid: 1000 })).is_ok());
    }
}
