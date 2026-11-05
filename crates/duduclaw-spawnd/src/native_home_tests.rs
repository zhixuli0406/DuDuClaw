use super::*;
use std::{fs::{self, File}, os::unix::fs::{symlink, MetadataExt}, path::Path};

#[test]
fn external_codex_detection_refuses_any_project_entry_without_following_it() {
    let dir = tempfile::tempdir().unwrap();
    let fd = File::open(dir.path()).unwrap();
    assert!(no_external_codex(fd.as_fd()).is_ok());
    symlink("/definitely-missing-config", dir.path().join(".codex")).unwrap();
    assert!(no_external_codex(fd.as_fd()).is_err());
}

fn force_attached(_: BorrowedFd<'_>, _: BorrowedFd<'_>) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::EINVAL))
}

fn conflict_after_attach(_: BorrowedFd<'_>, home: BorrowedFd<'_>) -> io::Result<()> {
    // Force configure() to fail only after the compatibility mounts landed.
    directory(home, ".codex", 0, 0)?;
    directory(home, ".grok", 0, 0)?;
    Err(io::Error::from_raw_os_error(libc::EINVAL))
}

fn assert_home(home: BorrowedFd<'_>, projection: BorrowedFd<'_>, kind: &str) {
    verify(home, projection).unwrap();
    let path = std::path::PathBuf::from(format!("/proc/self/fd/{}", home.as_raw_fd()));
    let config = path.join(format!(".{kind}"));
    assert_eq!(fs::read_link(config.join("config.toml")).unwrap(),
        Path::new(&format!("/config/{kind}-home.toml")));
    assert_eq!(fs::read_link(config.join("auth.json")).unwrap(),
        Path::new(&format!("/home/employee/data/.{kind}/auth.json")));
    assert_eq!(fs::read(path.join("data/persistent-marker")).unwrap(), b"employee data");
    let caches = if kind == "codex" { vec!["sessions", "archived_sessions", "log", "tmp"] }
        else { vec!["sessions", "cache", "logs"] };
    for entry in caches.iter().map(|entry| config.join(entry)).chain([path.join(".cache"), path.join(".local")]) {
        let metadata = fs::metadata(&entry).unwrap();
        assert!(metadata.is_dir());
        assert_eq!((metadata.uid(), metadata.gid(), metadata.mode() & 0o7777), (70000, 70000, 0o755));
    }
    let names = |path: &Path| {
        let mut values = fs::read_dir(path).unwrap().map(|entry| entry.unwrap().file_name()).collect::<Vec<_>>();
        values.sort(); values
    };
    let mut expected = ["data".into(), format!(".{kind}"), ".cache".into(), ".local".into()]
        .map(std::ffi::OsString::from).to_vec();
    expected.sort();
    assert_eq!(names(&path), expected);
    let mut expected = caches.into_iter().chain(["config.toml", "auth.json"])
        .map(std::ffi::OsString::from).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(names(&config), expected);
    let flags = nix::sys::statfs::fstatfs(home).unwrap().flags().bits();
    assert_eq!(flags & (libc::ST_NOSUID | libc::ST_NODEV | libc::ST_NOEXEC),
        libc::ST_NOSUID | libc::ST_NODEV | libc::ST_NOEXEC);
    let data = File::open(path.join("data")).unwrap();
    let flags = nix::sys::statfs::fstatfs(data.as_fd()).unwrap().flags().bits();
    assert_eq!(flags & (libc::ST_NOSUID | libc::ST_NODEV), libc::ST_NOSUID | libc::ST_NODEV);
    // The complete tree, including data, remains detached until B2 receives it.
    let table = fs::read_to_string("/proc/thread-self/mountinfo").unwrap();
    for fd in [home, data.as_fd()] {
        let id = crate::fds::mount_id(fd).unwrap().to_string();
        assert!(!table.lines().any(|line| line.split_whitespace().next() == Some(id.as_str())));
    }
}

#[test]
#[ignore = "requires root with mount API privileges; auto and forced compatibility paths, no provider"]
fn iso_root_native_home_projects_root_configs_and_refuses_external_codex() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let root = tempfile::tempdir().unwrap();
    for kind in ["codex", "grok"] {
        for fallback in [false, true] {
            let base = root.path().join(format!("{kind}-{fallback}"));
            fs::create_dir(&base).unwrap();
            for name in ["home", "work", "projection"] { fs::create_dir(base.join(name)).unwrap(); }
            fs::write(base.join("projection").join(format!("{kind}-home.toml")), b"test-only-root-config").unwrap();
            fs::write(base.join("home/persistent-marker"), b"employee data").unwrap();
            let projection = File::open(base.join("projection")).unwrap();
            let work = File::open(base.join("work")).unwrap();
            let persistent = File::open(base.join("home")).unwrap();
            let namespace = fs::read_link("/proc/thread-self/ns/mnt").unwrap();
            let mount_id = crate::fds::mount_id(persistent.as_fd()).unwrap();
            let flags = nix::sys::statfs::fstatfs(persistent.as_fd()).unwrap().flags();
            let mount_row = || fs::read_to_string("/proc/thread-self/mountinfo").unwrap().lines()
                .find(|line| line.split_whitespace().next() == Some(mount_id.to_string().as_str())).unwrap().to_owned();
            let before = mount_row();
            let cloned = crate::bootstrap_mounts::detached_clone(persistent.as_fd(), 2 | 4).unwrap();
            let home = if fallback {
                prepare_with(cloned, work.as_fd(), projection.as_fd(), 70000, 70000, force_attached)
            } else {
                prepare(cloned, work.as_fd(), projection.as_fd(), 70000, 70000)
            }.unwrap();
            eprintln!("native home: {kind}, forced compatibility={fallback}");
            assert_home(home.as_fd(), projection.as_fd(), kind);
            assert_eq!(fs::read_link("/proc/thread-self/ns/mnt").unwrap(), namespace);
            assert_eq!(mount_row(), before, "original mount/propagation changed");
            assert_eq!(nix::sys::statfs::fstatfs(persistent.as_fd()).unwrap().flags(), flags);
            assert_eq!(fs::read_dir(base.join("home")).unwrap().count(), 1);
            // Exercise the actual handoff: a new namespace can attach the returned
            // mount after the compatibility worker has exited. Never run an employee.
            let projection = projection.try_clone().unwrap();
            std::thread::spawn(move || {
                assert_eq!(unsafe { libc::unshare(libc::CLONE_FS | libc::CLONE_NEWNS) }, 0);
                assert_eq!(unsafe { libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) }, 0);
                assert_eq!(unsafe { libc::syscall(libc::SYS_move_mount, home.as_raw_fd(), c"".as_ptr(),
                    libc::AT_FDCWD, c"/tmp".as_ptr(), 4) }, 0);
                verify(home.as_fd(), projection.as_fd()).unwrap();
                assert_eq!(fs::read("/tmp/data/persistent-marker").unwrap(), b"employee data");
            }).join().unwrap();
            if fallback {
                let cloned = crate::bootstrap_mounts::detached_clone(persistent.as_fd(), 2 | 4).unwrap();
                let projection = File::open(base.join("projection")).unwrap();
                assert_eq!(prepare_with(cloned, work.as_fd(), projection.as_fd(), 70000, 70000,
                    conflict_after_attach).unwrap_err().raw_os_error(), Some(libc::EEXIST));
                assert_eq!(fs::read_link("/proc/thread-self/ns/mnt").unwrap(), namespace);
                assert_eq!(mount_row(), before);
                assert_eq!(fs::read_dir(base.join("home")).unwrap().count(), 1);
            }
            if kind == "codex" {
                symlink("/missing", base.join("work/.codex")).unwrap();
                let cloned = crate::bootstrap_mounts::detached_clone(persistent.as_fd(), 2 | 4).unwrap();
                let projection = File::open(base.join("projection")).unwrap();
                assert_eq!(prepare_with(cloned, work.as_fd(), projection.as_fd(), 70000, 70000,
                    force_attached).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            }
        }
    }
}
