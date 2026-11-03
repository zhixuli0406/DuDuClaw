use super::*;

fn valid() -> AttestationInputs {
    AttestationInputs {
        pid: 1, uids: Some([1000; 3]), gids: Some([1000; 3]),
        no_new_privs: Some(1), capabilities: [Some(0); 5], dumpable: 0,
        key_path_denied: true, sockets_absent: true, docker_host_absent: true,
    }
}

#[test]
fn all_required_attestations_pass() { assert_eq!(attest(&valid()), Ok(())); }

#[test]
fn each_attestation_failure_is_closed_and_specific() {
    let cases: &[(&str, fn(&mut AttestationInputs))] = &[
        ("not_pid_one", |i| i.pid = 2),
        ("user_ids", |i| i.uids = Some([0; 3])),
        ("user_ids", |i| i.uids = Some([1000, 1000, 0])),
        ("user_ids", |i| i.uids = Some([1000, 1001, 1000])),
        ("user_ids", |i| i.uids = None),
        ("group_ids", |i| i.gids = Some([0; 3])),
        ("group_ids", |i| i.gids = Some([1000, 1000, 0])),
        ("group_ids", |i| i.gids = Some([1000, 1001, 1000])),
        ("group_ids", |i| i.gids = None),
        ("proc_status", |i| i.no_new_privs = None),
        ("no_new_privs", |i| i.no_new_privs = Some(0)),
        ("no_new_privs", |i| i.no_new_privs = Some(2)),
        ("dumpable", |i| i.dumpable = 1),
        ("dumpable", |i| i.dumpable = -1),
        ("key_path_accessible", |i| i.key_path_denied = false),
        ("engine_socket", |i| i.sockets_absent = false),
        ("docker_host", |i| i.docker_host_absent = false),
    ];
    for (reason, mutate) in cases {
        let mut inputs = valid();
        mutate(&mut inputs);
        assert_eq!(attest(&inputs), Err(*reason));
    }
    for (n, reason) in ["cap_inh", "cap_prm", "cap_eff", "cap_bnd", "cap_amb"].into_iter().enumerate() {
        let mut inputs = valid();
        inputs.capabilities[n] = None;
        assert_eq!(attest(&inputs), Err("proc_status"));
        for bit in 0..64 {
            inputs.capabilities[n] = Some(1 << bit);
            assert_eq!(attest(&inputs), Err(reason));
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn refusal_closes_fd_three_even_when_hardening_is_inactive_or_contract_invalid() {
    // Isolate descriptor 3 and the one-shot global from the test harness.
    const CHILD: &str = "DUDUCLAW_ROOT_KEY_TEST_CHILD";
    if let Ok(case) = std::env::var(CHILD) {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(unsafe { libc::dup2(file.as_raw_fd(), 3) }, 3);
        // Only descriptor 3 is intentionally transferred to intake.
        if file.as_raw_fd() == 3 { std::mem::forget(file); }
        intake(Some("inactive"));
        assert_eq!(state_kind(), StateKind::Refused);
        assert_eq!(refused_reason(), Some(if case == "invalid" { "fd_contract" } else { "inactive" }));
        assert_eq!(unsafe { libc::fcntl(3, libc::F_GETFD) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        assert!(with_key(|_| ()).is_none());
        return;
    }
    for (case, offered) in [("inactive", "3"), ("invalid", "03")] {
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "root_key::tests::refusal_closes_fd_three_even_when_hardening_is_inactive_or_contract_invalid"])
            .env(CHILD, case).env(FD_ENV, offered).status().unwrap().success());
    }
}

fn mount_line(path: &str) -> String {
    format!("42 21 0:31 / {path} rw,nosuid shared:5 - tmpfs tmpfs rw\n")
}

#[test]
fn mountinfo_allows_inspected_regular_mounts_and_optional_fields() {
    let mounts = format!("{}{}", mount_line("/"), mount_line("/var/lib/duduclaw-protected"));
    let mut inspected = Vec::new();
    assert!(mountinfo_sockets_absent(Some(&mounts), |path| {
        inspected.push(path.to_path_buf());
        Ok(false)
    }));
    assert_eq!(inspected, [std::path::PathBuf::from("/"), std::path::PathBuf::from("/var/lib/duduclaw-protected")]);
}

#[test]
fn mountinfo_refuses_custom_socket_and_engine_named_mounts() {
    let mounts = mount_line("/unusual/control");
    assert!(!mountinfo_sockets_absent(Some(&mounts), |path| {
        assert_eq!(path, std::path::Path::new("/unusual/control"));
        Ok(true)
    }));
    for path in ["/custom/engine.sock", "/custom/docker", "/custom/docker.socket",
        "/custom/docker-engine", "/custom/podman", "/custom/containerd", "/run/containerd/control"] {
        assert!(!mountinfo_sockets_absent(Some(&mount_line(path)), |_| Ok(false)), "{path}");
    }
}

#[cfg(unix)]
#[test]
fn mountinfo_refuses_real_unix_socket_with_an_unrelated_name() {
    use std::os::unix::{fs::FileTypeExt, net::UnixListener};
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("control");
    let _listener = UnixListener::bind(&socket).unwrap();
    let mount = mount_line(&socket.to_str().unwrap().replace('\\', "\\134").replace(' ', "\\040"));
    assert!(!mountinfo_sockets_absent(Some(&mount), |path| {
        std::fs::metadata(path).map(|metadata| metadata.file_type().is_socket())
    }));
}

#[test]
fn mountinfo_decodes_kernel_escaped_paths_before_inspection() {
    let mounts = mount_line(r"/odd\040name\011tab\012line\134slash/control");
    assert!(!mountinfo_sockets_absent(Some(&mounts), |path| {
        assert_eq!(path, std::path::Path::new("/odd name\ttab\nline\\slash/control"));
        Ok(true)
    }));
    assert!(!mountinfo_sockets_absent(Some(&mount_line(r"/odd\040name/control.sock")), |_| Ok(false)));
}

#[test]
fn mountinfo_unreadable_malformed_or_uninspectable_is_refused() {
    assert!(!mountinfo_sockets_absent(None, |_| Ok(false)));
    for input in ["", "\n", "42 21", "42 21 0:31 / / rw - tmpfs tmpfs",
        "x 21 0:31 / / rw - tmpfs tmpfs rw", "42 21 0:31 / relative rw - tmpfs tmpfs rw"] {
        assert!(!mountinfo_sockets_absent(Some(input), |_| Ok(false)), "{input:?}");
    }
    for path in [r"/bad\", r"/bad\04", r"/bad\999", r"/bad\000"] {
        assert!(!mountinfo_sockets_absent(Some(&mount_line(path)), |_| Ok(false)), "{path}");
    }
    for kind in [std::io::ErrorKind::PermissionDenied, std::io::ErrorKind::NotFound] {
        assert!(!mountinfo_sockets_absent(Some(&mount_line("/custom/control")), |_| Err(kind.into())));
    }
}

fn mountinfo_sockets_absent(
    input: Option<&str>,
    mut is_socket: impl FnMut(&std::path::Path) -> std::io::Result<bool>,
) -> bool {
    check_mountinfo(input, |path| is_socket(path).map(|socket| (socket, true))).is_ok()
}

#[test]
fn mountinfo_refuses_engine_source_root_and_source_device() {
    for input in [
        "42 21 8:1 /run/docker /control rw - ext4 /dev/sda1 rw",
        "42 21 8:1 /run/containerd /control rw - ext4 /dev/sda1 rw",
        "42 21 8:1 /var/run/docker-engine /control rw - ext4 /dev/sda1 rw",
        "42 21 8:1 /run/user/1000/podman /control rw - ext4 /dev/sda1 rw",
        "42 21 8:1 / /control rw - ext4 /run/docker rw",
        "42 21 8:1 / /control rw - ext4 /run/containerd rw",
        "42 21 8:1 / /control rw - ext4 /run/user/1000/podman rw",
        "42 21 8:1 / /control rw - ext4 /var/run/docker-engine rw",
    ] {
        assert_eq!(check_mountinfo(Some(input), |_| Ok((false, true))), Err("engine_socket"));
    }
}

#[test]
fn normal_container_overlay_and_standard_system_mounts_are_allowed() {
    let input = "29 1 0:25 / / rw,relatime - overlay overlay rw,lowerdir=/var/lib/docker/overlay2/l/ABC,upperdir=/var/lib/docker/overlay2/123/diff,workdir=/var/lib/docker/overlay2/123/work
30 29 0:26 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw
31 29 0:27 / /dev rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755
32 31 0:28 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,gid=5,mode=620,ptmxmode=666
33 29 0:29 / /sys ro,nosuid,nodev,noexec,relatime - sysfs sysfs ro
34 33 0:30 / /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw
35 31 0:31 / /dev/mqueue rw,nosuid,nodev,noexec,relatime - mqueue mqueue rw
36 31 0:32 / /dev/shm rw,nosuid,nodev,noexec,relatime - tmpfs shm rw,size=65536k
38 29 8:1 /var/lib/docker/volumes/duduclaw-data/_data /app/data rw,relatime - ext4 /dev/sda1 rw
39 29 8:1 /var/lib/docker/volumes/duduclaw-protected-key/_data /var/lib/duduclaw-protected rw,relatime - ext4 /dev/sda1 rw
37 29 8:1 /var/lib/docker/containers/abc/hostname /etc/hostname rw,relatime - ext4 /dev/sda1 rw
";
    let mut inspected = Vec::new();
    assert_eq!(check_mountinfo(Some(input), |path| {
        inspected.push(path.to_path_buf());
        Ok((false, path != std::path::Path::new("/etc/hostname")))
    }), Ok(()));
    assert_eq!(inspected, ["/", "/proc", "/dev", "/dev/pts", "/sys", "/sys/fs/cgroup",
        "/dev/mqueue", "/dev/shm", "/app/data", "/var/lib/duduclaw-protected", "/etc/hostname"]
        .map(std::path::PathBuf::from));
}
