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

// Existing parser/point checks use a no-op walker to keep their synthetic paths.
fn mountinfo_sockets_absent(
    input: Option<&str>,
    mut is_socket: impl FnMut(&std::path::Path) -> std::io::Result<bool>,
) -> bool {
    check_mountinfo(input, |path| is_socket(path).map(|socket| (socket, true)), |_, _| Ok(())).is_ok()
}

#[cfg(target_os = "linux")]
fn scan_tree(input: &str, cap: usize) -> Result<(), &'static str> {
    use std::os::unix::fs::FileTypeExt;
    let mut remaining = cap;
    check_mountinfo(Some(input), |path| {
        std::fs::symlink_metadata(path).map(|metadata| (metadata.file_type().is_socket(), metadata.is_dir()))
    }, |path, mounts| scan_mount_directory(path, mounts, &mut remaining))
}

#[cfg(target_os = "linux")]
fn tree_mount(path: &std::path::Path) -> String {
    mount_line(&path.to_str().unwrap().replace('\\', "\\134").replace(' ', "\\040"))
}

#[cfg(target_os = "linux")]
#[test]
fn mounted_directory_refuses_nested_engine_sockets_but_allows_runtime_sockets() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("one/two");
    std::fs::create_dir_all(&nested).unwrap();
    let input = tree_mount(dir.path());
    let _gateway = UnixListener::bind(nested.join("gateway.sock")).unwrap();
    let _view = UnixListener::bind(nested.join("live-view.sock")).unwrap();
    assert_eq!(scan_tree(&input, MOUNT_SCAN_ENTRIES), Ok(()));
    for name in ["podman.sock", "docker.sock", "containerd.sock", "buildkitd.sock",
        "crio.sock", "dockershim.sock", "my-docker-api.sock", "my-podman.sock"] {
        let path = nested.join(name);
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(scan_tree(&input, MOUNT_SCAN_ENTRIES), Err("engine_socket"), "{name}");
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
    for engine in ["docker", "podman", "containerd"] {
        let path = nested.join(engine);
        std::fs::create_dir(&path).unwrap();
        let listener = UnixListener::bind(path.join("control")).unwrap();
        assert_eq!(scan_tree(&input, MOUNT_SCAN_ENTRIES), Err("engine_socket"));
        drop(listener);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn mounted_directory_does_not_follow_socket_or_directory_symlinks() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _listener = UnixListener::bind(outside.path().join("podman.sock")).unwrap();
    symlink(outside.path().join("podman.sock"), dir.path().join("podman.sock")).unwrap();
    symlink(outside.path(), dir.path().join("linked-directory")).unwrap();
    assert_eq!(scan_tree(&tree_mount(dir.path()), MOUNT_SCAN_ENTRIES), Ok(()));
}

// These filesystem-walk tests exercise the Linux-only protected-key mount scan.
#[cfg(target_os = "linux")]
#[test]
fn mounted_directory_caps_entries_across_mounts_and_fails_closed_at_depth() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for dir in [first.path(), second.path()] {
        std::fs::write(dir.join("entry"), b"").unwrap();
    }
    let input = format!("{}{}", tree_mount(first.path()), tree_mount(second.path()));
    assert_eq!(scan_tree(&input, 2), Ok(()));
    assert_eq!(scan_tree(&input, 1), Err("mount_scan_incomplete"));
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/b/c/d")).unwrap();
    assert_eq!(scan_tree(&tree_mount(dir.path()), MOUNT_SCAN_ENTRIES), Ok(()));
    std::fs::write(dir.path().join("a/b/c/d/too-deep"), b"").unwrap();
    assert_eq!(scan_tree(&tree_mount(dir.path()), MOUNT_SCAN_ENTRIES), Err("mount_scan_incomplete"));
}

#[cfg(unix)]
#[test]
fn mounted_directory_unreadable_fails_closed_even_under_root_test_runner() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("unreadable");
    std::fs::create_dir(&nested).unwrap();
    let mut remaining = MOUNT_SCAN_ENTRIES;
    let mut reads = 0;
    // Inject EACCES at read_dir, so this also exercises the failure under root.
    assert_eq!(walk_mount_directory(dir.path(), &[],
        std::fs::symlink_metadata(dir.path()).unwrap().dev(), 0, &mut remaining,
        &mut |path| {
            reads += 1;
            if reads == 2 { Err(std::io::ErrorKind::PermissionDenied.into()) }
            else { std::fs::read_dir(path) }
        }), Err("mount_scan_incomplete"));
}

#[cfg(target_os = "linux")]
#[test]
fn nested_same_device_mount_is_scanned_separately() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let _listener = UnixListener::bind(nested.join("podman.sock")).unwrap();
    let mut remaining = MOUNT_SCAN_ENTRIES;
    assert_eq!(scan_mount_directory(dir.path(), &[nested.clone()], &mut remaining), Ok(()));
    assert_eq!(scan_tree(&format!("{}{}", tree_mount(dir.path()), tree_mount(&nested)),
        MOUNT_SCAN_ENTRIES), Err("engine_socket"));
}

#[test]
fn mountinfo_refuses_engine_source_root_and_source_device() {
    for input in [
        "42 21 8:1 /run/docker /control rw - ext4 /dev/sda1 rw",
        "42 21 8:1 /run/user/1000/podman /control rw - ext4 /dev/sda1 rw",
        "42 21 8:1 / /control rw - ext4 /run/docker rw",
    ] {
        assert_eq!(check_mountinfo(Some(input), |_| Ok((false, true)), |_, _| Ok(())), Err("engine_socket"));
    }
}

#[test]
fn normal_container_overlay_and_standard_system_mounts_are_not_walked() {
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
    let mut walked = Vec::new();
    assert_eq!(check_mountinfo(Some(input), |path| {
        Ok((false, path != std::path::Path::new("/etc/hostname")))
    }, |path, _| {
        walked.push(path.to_path_buf());
        Ok(())
    }), Ok(()));
    assert_eq!(walked, ["/app/data", "/etc/hostname"].map(std::path::PathBuf::from));
}

#[cfg(unix)]
#[test]
fn protected_key_mount_is_skipped_exactly_and_other_unreadable_mounts_fail_closed() {
    let protected = format!("{}{}", mount_line("/var/lib/duduclaw-protected"), mount_line("/app/data"));
    let mut scanned = Vec::new();
    assert_eq!(check_mountinfo(Some(&protected), |_| Ok((false, true)), |path, _| {
        scanned.push(path.to_path_buf());
        if path == std::path::Path::new("/app/data") { Ok(()) }
        else { Err("mount_scan_incomplete") }
    }), Ok(()));
    assert_eq!(scanned, [std::path::PathBuf::from("/app/data")]);

    for path in ["/var/lib/duduclaw-protected-extra", "/var/lib/duduclaw-protected/sub", "/other/unreadable"] {
        let input = mount_line(path);
        assert_eq!(check_mountinfo(Some(&input), |_| Ok((false, true)), |_, _| {
            Err("mount_scan_incomplete")
        }), Err("mount_scan_incomplete"), "{path}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn mounted_directory_refuses_the_production_entry_cap() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..=MOUNT_SCAN_ENTRIES {
        std::fs::write(dir.path().join(format!("entry-{n}")), b"").unwrap();
    }
    assert_eq!(scan_tree(&tree_mount(dir.path()), MOUNT_SCAN_ENTRIES), Err("mount_scan_incomplete"));
}
