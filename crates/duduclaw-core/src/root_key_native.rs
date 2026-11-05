//! Native systemd attestation facts and Linux probes. No configuration fallback.

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

/// Abstract sockets are matched by slash-delimited components, exactly an
/// engine name or its . / - / _ suffix namespace. Never arbitrary substrings.
#[cfg(any(target_os = "linux", test))]
fn abstract_engine_name(name: &str) -> bool {
    // docker/cli cmd/docker/docker.go creates this random coordination socket
    // via cli-plugins/socket/socket.go with a nil handler; clients only get
    // EOF when the CLI exits, so it cannot expose the container engine API.
    if let Some(random) = name.strip_prefix("@docker_cli_") {
        if random.len() == 32 && random.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
            return false;
        }
    }
    name.strip_prefix('@').is_some_and(|name| name.split('/').any(|component| {
        ["docker", "containerd", "podman", "buildkit", "lxd"].iter().any(|engine| {
            component == *engine || component.strip_prefix(*engine).is_some_and(|suffix| {
                suffix.starts_with('.') || suffix.starts_with('-') || suffix.starts_with('_')
            })
        })
    }))
}

#[cfg(any(target_os = "linux", test))]
fn abstract_sockets_denied(input: Option<&str>, periodic: bool, mut peer_uid: impl FnMut(&str) -> Option<u32>) -> Result<(), String> {
    // A held key proves this probe passed intake. On a transient table read
    // failure retain only that probe's previous safe verdict; still check paths
    // and groups. Intake has no previous verdict and must fail closed.
    let Some(input) = input else {
        return if periodic { Ok(()) } else { Err("/proc/net/unix: table unreadable".into()) };
    };
    let mut lines = input.lines();
    if lines.next() != Some("Num       RefCount Protocol Flags    Type St Inode Path") {
        return Err("/proc/net/unix: unexpected table header".into());
    }
    for line in lines {
        let mut rest = line;
        let mut fields = Vec::new();
        for _ in 0..7 {
            rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
            let end = rest.find(|c: char| c.is_ascii_whitespace()).unwrap_or(rest.len());
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        let name = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        // Malformed unrelated rows cannot establish engine reachability.
        if !abstract_engine_name(name) { continue; }
        let Ok(flags) = u32::from_str_radix(fields[3], 16) else { continue; };
        if flags & 0x10000 != 0 {
            if let Some(0) = peer_uid(name) { return Err(format!("{name}: peer uid=0 (root)")); }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn abstract_peer_uid(name: &str) -> Option<u32> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let name = name.strip_prefix('@')?.as_bytes();
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
    Some(credentials.uid)
}

#[cfg(target_os = "linux")]
pub(super) fn reattest(uid: u32, held: bool) -> Result<(), super::Refusal> {
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count != 0 { return Err("supplementary_groups".into()); }
    // After revocation there is no held-key verdict to retain on read failure.
    engine_sockets_denied(uid, held).map_err(|detail| super::Refusal {
        reason: "engine_socket_reachable", detail: Some(detail),
    })
}

#[cfg(target_os = "linux")]
fn engine_sockets_denied(uid: u32, periodic: bool) -> Result<(), String> {
    for path in [
        "/var/run/docker.sock".to_string(), "/run/docker.sock".to_string(),
        "/run/podman/podman.sock".to_string(), "/var/run/podman/podman.sock".to_string(),
        "/run/containerd/containerd.sock".to_string(),
        format!("/run/user/{uid}/docker.sock"), format!("/run/user/{uid}/podman/podman.sock"),
    ] { engine_socket_denied(&path)?; }
    let table = std::fs::read_to_string("/proc/net/unix");
    if periodic && table.is_err() {
        tracing::warn!(error = ?table.as_ref().err(), "Abstract socket table unreadable; retaining previous probe verdict");
    }
    abstract_sockets_denied(table.ok().as_deref(), periodic, abstract_peer_uid)
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
    let socket_check = engine_sockets_denied(uid, false);
    NativeInputs {
        initial_user_namespace: initial_user_namespace(),
        container_environment_absent: container_environment_absent(),
        ppid: unsafe { libc::getppid() } as u32,
        cgroup: parse_cgroup(std::fs::read_to_string("/proc/self/cgroup").ok().as_deref()),
        executable: executable_state(),
        supplementary_groups: (count >= 0).then_some(count as u32),
        engine_sockets_denied: socket_check.is_ok(),
        engine_socket_detail: socket_check.err(),
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
fn engine_socket_denied(path: &str) -> Result<(), String> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let label = path;
    let Ok(path) = std::ffi::CString::new(path) else { return Err(format!("{label}: invalid path")); };
    // Both DAC writability and an actual nonblocking connection must be denied.
    if unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0 {
        return Err(format!("{label}: writable via access(W_OK)"));
    }
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 { return Err(format!("{label}: socket creation failed, errno={:?}", std::io::Error::last_os_error().raw_os_error())); }
    // SAFETY: ownership of a newly created descriptor, closed on every path.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_bytes_with_nul();
    if bytes.len() > address.sun_path.len() { return Err(format!("{label}: path too long")); }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) { *target = *byte as libc::c_char; }
    let result = unsafe { libc::connect(socket.as_raw_fd(),
        (&address as *const libc::sockaddr_un).cast(),
        std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t) };
    // EAGAIN/EINPROGRESS/ECONNREFUSED and all unknown errors are not proof of isolation.
    connection_denied(label, result, std::io::Error::last_os_error().raw_os_error())
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn connection_denied(path: &str, result: i32, errno: Option<i32>) -> Result<(), String> {
    if result == -1 && matches!(errno, Some(libc::EACCES | libc::EPERM | libc::ENOENT)) {
        Ok(())
    } else if result == 0 {
        Err(format!("{path}: connect succeeded"))
    } else {
        Err(format!("{path}: connect unexpected errno={errno:?}"))
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
    const HEADER: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n";

    #[test]
    fn root_key_abstract_detail_identifies_exact_root_peer() {
        let table = format!("{HEADER}0000: 2 0 00010000 0001 01 42 @/containerd-shim/example\n");
        assert_eq!(abstract_sockets_denied(Some(&table), false, |_| Some(0)),
            Err("@/containerd-shim/example: peer uid=0 (root)".into()));
        assert_eq!(abstract_sockets_denied(Some("bad header"), true, |_| None),
            Err("/proc/net/unix: unexpected table header".into()));
    }

    #[cfg(unix)]
    #[test]
    fn root_key_path_connection_detail_keeps_errno_and_success() {
        assert_eq!(connection_denied("/run/docker.sock", 0, Some(libc::ENOENT)),
            Err("/run/docker.sock: connect succeeded".into()));
        assert_eq!(connection_denied("/run/docker.sock", -1, Some(libc::ECONNREFUSED)),
            Err(format!("/run/docker.sock: connect unexpected errno=Some({})", libc::ECONNREFUSED)));
        for errno in [libc::EACCES, libc::EPERM, libc::ENOENT] {
            assert!(connection_denied("/run/docker.sock", -1, Some(errno)).is_ok());
        }
    }

    #[test]
    fn root_key_abstract_peer_credentials_and_transient_reads() {
        let table = format!("{HEADER}bad row\n0000: 2 0 00010000 0001 01 42 @attacker/docker\n");
        assert!(abstract_sockets_denied(Some(&table), false, |_| Some(1000)).is_ok());
        assert!(abstract_sockets_denied(Some(&table), false, |_| None).is_ok());
        assert!(abstract_sockets_denied(Some(&table), false, |_| Some(0)).is_err());
        assert!(abstract_sockets_denied(None, true, |_| panic!("no table")).is_ok());
        assert!(abstract_sockets_denied(None, false, |_| panic!("no table")).is_err());
        let unrelated = format!("{HEADER}bad row\n0000: 2 0 broken 0001 01 42 @unrelated\n");
        assert!(abstract_sockets_denied(Some(&unrelated), true, |_| panic!("non-candidate")).is_ok());
    }

    #[test]
    fn abstract_engine_socket_names_are_component_anchored() {
        for name in ["@docker", "@docker.sock", "@run/podman/control", "@containerd-shim",
            "@buildkit_worker", "@lxd/unix.socket"] {
            assert!(abstract_engine_name(name), "{name}");
            let table = format!("{HEADER}0000: 00000002 00000000 00010000 0001 01 42 {name}\n");
            assert!(abstract_sockets_denied(Some(&table), false, |_| Some(0)).is_err());
            let connected = format!("{HEADER}0000: 00000002 00000000 00000000 0001 03 42 {name}\n");
            assert!(abstract_sockets_denied(Some(&connected), false, |_| Some(0)).is_ok());
        }
        for name in ["@notdocker", "@dockerish", "@my-containerd", "@notes-about-podman",
            "@path/dockerman", "/run/docker.sock"] {
            assert!(!abstract_engine_name(name), "{name}");
        }
        assert!(abstract_sockets_denied(None, false, |_| Some(0)).is_err());
        assert!(abstract_sockets_denied(Some(""), false, |_| Some(0)).is_err());
        assert!(abstract_sockets_denied(Some(&format!("{HEADER}bad row")), false, |_| panic!("non-candidate")).is_ok());
        assert!(abstract_sockets_denied(Some(HEADER), false, |_| Some(0)).is_ok());
    }

    #[test]
    fn docker_cli_coordination_socket_is_not_an_engine_socket() {
        assert!(!abstract_engine_name("@docker_cli_3e708842d9322cef25983565c19d0866"));
        for name in [
            "@docker_cli_x",
            "@docker_cli_3e708842d9322cef25983565c19d0866/x",
            "@docker_cli_3e708842d9322cef25983565c19d086",
            "@docker_cli_3e708842d9322cef25983565c19d08666",
            "@docker_cli_3E708842D9322CEF25983565C19D0866",
            "@/containerd-shim/...",
            "@docker",
            "@docker.sock",
            "@containerd-shim",
        ] {
            assert!(abstract_engine_name(name), "{name}");
        }
    }
}
