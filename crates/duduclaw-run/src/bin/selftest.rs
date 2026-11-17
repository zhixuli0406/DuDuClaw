//! R7a isolation self-test, shipped as `/usr/lib/duduclaw/duduclaw-run-selftest`
//! and started only by PID 1 through the fixed unit
//! `duduclaw-isolation-selftest.service` (spawnd `Systemd::run_fixed_unit`).
//!
//! It is NOT a mode of `duduclaw-run`: the trampoline keeps refusing any argv
//! but its own name and gains no code path. This binary accepts no argument,
//! reads no environment and no stdin, and keeps no descriptor beyond 0/1/2.
//! It reuses only library code the production trampoline runs, in the
//! trampoline's order: kernel probe, namespaces with private propagation, the
//! real `build_root` + `pivot`, loopback, a new PID namespace whose init mounts
//! its own `/proc`, the full privilege drop and readback, the runtime Landlock
//! table (`runner::restrict_filesystem`) and the common seccomp filter. Then it
//! asserts that the policy actually denies and allows what it must.
//!
//! Output: exit 0 and one JSON line on stdout only when every assertion held.
//! Any failure: one JSON line on stderr naming the stage, and that stage's
//! exit code (table in `Stage::code`). No path, errno text or account name is
//! ever printed.
#![allow(unsafe_code)]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("{{\"code\":\"not_ready\",\"reason\":\"unsupported_platform\"}}");
    std::process::exit(125);
}

#[cfg(target_os = "linux")]
fn main() { linux::main() }

/// Fixed stage vocabulary and pure parsers (host-testable).
#[cfg(any(target_os = "linux", test))]
mod stages {
    /// One exit code per stage. Parent stages are 2..=15; stages inside the
    /// forked namespace init are 20..=29. 0 is success, 125 is "unsupported
    /// platform" (same as `duduclaw-run`), 101 would be an unexpected panic.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Stage {
        Descriptors, Arguments, Capabilities, Probe, Account, Namespaces, Propagation,
        WorkArea, Root, Pivot, Loopback, PidNamespace, Supervise, Output,
        ChildPid, Privileges, Landlock, Seccomp, PositiveWrite, SocketAllowed,
        SocketDenied, FifoDenied, ReadAllowed, ReadDenied,
    }
    impl Stage {
        pub(crate) const ALL: [Stage; 24] = [Stage::Descriptors, Stage::Arguments, Stage::Capabilities,
            Stage::Probe, Stage::Account, Stage::Namespaces, Stage::Propagation, Stage::WorkArea, Stage::Root,
            Stage::Pivot, Stage::Loopback, Stage::PidNamespace, Stage::Supervise, Stage::Output,
            Stage::ChildPid, Stage::Privileges, Stage::Landlock, Stage::Seccomp, Stage::PositiveWrite,
            Stage::SocketAllowed, Stage::SocketDenied, Stage::FifoDenied, Stage::ReadAllowed, Stage::ReadDenied];
        pub(crate) fn code(self) -> i32 {
            match self {
                Stage::Descriptors => 2, Stage::Arguments => 3, Stage::Capabilities => 4, Stage::Probe => 5,
                Stage::Account => 6, Stage::Namespaces => 7, Stage::Propagation => 8, Stage::WorkArea => 9,
                Stage::Root => 10, Stage::Pivot => 11, Stage::Loopback => 12, Stage::PidNamespace => 13,
                Stage::Supervise => 14, Stage::Output => 15,
                Stage::ChildPid => 20, Stage::Privileges => 21, Stage::Landlock => 22, Stage::Seccomp => 23,
                Stage::PositiveWrite => 24, Stage::SocketAllowed => 25, Stage::SocketDenied => 26,
                Stage::FifoDenied => 27, Stage::ReadAllowed => 28, Stage::ReadDenied => 29,
            }
        }
        pub(crate) fn name(self) -> &'static str {
            match self {
                Stage::Descriptors => "descriptors", Stage::Arguments => "arguments",
                Stage::Capabilities => "capabilities", Stage::Probe => "probe", Stage::Account => "account",
                Stage::Namespaces => "namespaces", Stage::Propagation => "propagation",
                Stage::WorkArea => "work_area", Stage::Root => "root", Stage::Pivot => "pivot",
                Stage::Loopback => "loopback", Stage::PidNamespace => "pid_namespace",
                Stage::Supervise => "supervise", Stage::Output => "output", Stage::ChildPid => "child_pid",
                Stage::Privileges => "privileges", Stage::Landlock => "landlock", Stage::Seccomp => "seccomp",
                Stage::PositiveWrite => "positive_write", Stage::SocketAllowed => "socket_allowed",
                Stage::SocketDenied => "socket_denied", Stage::FifoDenied => "fifo_denied",
                Stage::ReadAllowed => "read_allowed", Stage::ReadDenied => "read_denied",
            }
        }
        /// Only codes the namespace init can report are accepted from it; any
        /// other exit of that process is a supervision failure.
        #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
        pub(crate) fn from_child(code: i32) -> Option<Stage> {
            Self::ALL.into_iter().find(|stage| stage.code() == code && stage.code() >= 20)
        }
    }
    /// `/proc/thread-self/mountinfo` after `enter_namespaces`: at least the
    /// root mount is listed, and no mount carries a shared or slave peer tag
    /// (MS_REC|MS_PRIVATE on `/` makes every mount of the namespace private).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn propagation_private(mountinfo: &str) -> bool {
        let mut root = false;
        for line in mountinfo.lines() {
            let Some((left, _)) = line.split_once(" - ") else { return false };
            let fields: Vec<&str> = left.split(' ').collect();
            if fields.len() < 6 { return false; }
            if fields[4] == "/" { root = true; }
            if fields[6..].iter().any(|tag| tag.starts_with("shared:") || tag.starts_with("master:")
                || tag.starts_with("propagate_from:")) { return false; }
        }
        root
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{CStr, CString};
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::{ffi::OsStrExt, fs::{DirBuilderExt, PermissionsExt}, net::UnixListener};
    use std::path::Path;
    use std::time::{Duration, Instant};
    use duduclaw_isolation::ids::{selftest_identity_allowed, SELFTEST_USER, SERVICE_USERS};
    use duduclaw_run::{linux::{fd, mounts, privileges, probe, runner}, seccomp};
    use super::stages::{propagation_private, Stage};

    /// The private work area: a fresh tmpfs over `/tmp`, inside this process's
    /// own mount namespace only. `build_root` stages the new root under it too.
    const AREA: &CStr = c"/tmp";
    const ANCHORS: &str = "/tmp/duduclaw-selftest";
    const HOSTS: &[u8] = b"127.0.0.1 localhost\n";
    const CHILD_DEADLINE: Duration = Duration::from_secs(20);
    const OPEN_TREE_CLONE: u32 = 1;
    const AT_RECURSIVE: u32 = 0x8000;
    const MOUNT_RDONLY: u64 = 1;
    const MOUNT_NOSUID: u64 = 2;
    const MOUNT_NODEV: u64 = 4;
    const MOUNT_NOEXEC: u64 = 8;
    const MOUNT_NOSYMFOLLOW: u64 = 0x0020_0000;

    fn fail(stage: Stage) -> ! {
        let _ = writeln!(std::io::stderr(), "{{\"code\":\"selftest_failed\",\"stage\":\"{}\"}}", stage.name());
        std::process::exit(stage.code())
    }
    fn check<T, E>(result: Result<T, E>, stage: Stage) -> T {
        match result { Ok(value) => value, Err(_) => fail(stage) }
    }
    fn errno() -> i32 { std::io::Error::last_os_error().raw_os_error().unwrap_or(0) }

    pub fn main() {
        // A panic never prints a message (no path or errno text); it exits 101.
        std::panic::set_hook(Box::new(|_| {}));
        // Same opening as `runner::serve`: non-dumpable, then no inherited fd.
        check(privileges::non_dumpable(), Stage::Descriptors);
        check(fd::close_range(3, u32::MAX), Stage::Descriptors);
        if std::env::args_os().count() != 1 { fail(Stage::Arguments); }
        check(privileges::require_root_capabilities(), Stage::Capabilities);
        let ready = check(probe::probe(), Stage::Probe);
        // NSS is consulted here, before any namespace or mount change.
        let (uid, gid) = check(identity(), Stage::Account);
        check(mounts::enter_namespaces(), Stage::Namespaces);
        let mountinfo = check(std::fs::read_to_string("/proc/thread-self/mountinfo"), Stage::Propagation);
        if !propagation_private(&mountinfo) { fail(Stage::Propagation); }
        let inputs = check(work_area(uid, gid), Stage::WorkArea);
        let runtime_mount = inputs.runtime.expected_mount_id;
        let config = mounts::RootConfig { employee_uid: uid, employee_gid: gid, pty: false, pty_limit: 1,
            work_noexec: true, tmp_noexec: true, tmpfs_bytes: 1024 * 1024, tmpfs_inodes: 64 };
        let mut root = check(mounts::build_root(inputs, &config), Stage::Root);
        if root.take_pty().is_some() { fail(Stage::Root); }
        let readback = check(root.pivot(), Stage::Pivot);
        if readback.runtime_mount_id != runtime_mount { fail(Stage::Pivot); }
        check(mounts::bring_loopback_up(), Stage::Loopback);
        // The runner's arrangement: only the next child enters the new PID
        // namespace and becomes its init.
        // SAFETY: single-threaded process; affects only children created below.
        if unsafe { libc::unshare(libc::CLONE_NEWPID) } != 0 { fail(Stage::PidNamespace); }
        // SAFETY: single-threaded process; the child only runs `child_main`.
        let child = unsafe { libc::fork() };
        if child < 0 { fail(Stage::PidNamespace); }
        if child == 0 { child_main(uid, gid, ready.landlock_ruleset_abi); }
        supervise(child);
        let line = format!("{{\"version\":1,\"kernel\":[{},{}],\"detected_abi\":{},\"selected_abi\":{}}}\n",
            ready.kernel_major, ready.kernel_minor, ready.landlock_abi, ready.landlock_ruleset_abi);
        let mut out = std::io::stdout().lock();
        if out.write_all(line.as_bytes()).and_then(|()| out.flush()).is_err() { fail(Stage::Output); }
        std::process::exit(0);
    }

    /// `Some((uid, gid))`, `None` if the account does not exist, `Err` if NSS failed.
    fn lookup(name: &str) -> Result<Option<(u32, u32)>, ()> {
        let name = CString::new(name).map_err(|_| ())?;
        // SAFETY: single-threaded; errno is reset so "not found" is distinguishable
        // from an NSS error, and the static entry is copied before the next call.
        unsafe {
            *libc::__errno_location() = 0;
            let entry = libc::getpwnam(name.as_ptr());
            if entry.is_null() {
                return match errno() { 0 | libc::ENOENT | libc::ESRCH => Ok(None), _ => Err(()) };
            }
            Ok(Some(((*entry).pw_uid, (*entry).pw_gid)))
        }
    }
    fn identity() -> Result<(u32, u32), ()> {
        let (uid, gid) = lookup(SELFTEST_USER)?.ok_or(())?;
        let mut services = Vec::new();
        for name in SERVICE_USERS {
            if let Some(service) = lookup(name)? { services.push(service); }
        }
        if !selftest_identity_allowed(uid, gid, &services) { return Err(()); }
        Ok((uid, gid))
    }

    fn directory(path: &Path, mode: u32) -> Result<(), ()> {
        std::fs::DirBuilder::new().mode(mode).create(path).map_err(|_| ())?;
        // The unit runs with UMask=0077: set the exact mode explicitly.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|_| ())
    }
    /// Root-owned anchors on the private tmpfs, cloned exactly as spawnd hands
    /// detached mounts to the trampoline: private propagation, then the
    /// attributes `build_root` re-verifies for each fixed destination.
    fn work_area(uid: u32, gid: u32) -> Result<mounts::MountInputs, ()> {
        // SAFETY: static C strings; the mount exists only in this namespace.
        if unsafe { libc::mount(c"tmpfs".as_ptr(), AREA.as_ptr(), c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV, c"size=4194304,nr_inodes=256,mode=0755".as_ptr().cast()) } != 0 {
            return Err(());
        }
        let base = Path::new(ANCHORS);
        directory(base, 0o755)?;
        for (name, mode) in [("home", 0o700), ("work", 0o700), ("config", 0o755), ("runtime", 0o755),
            ("etc", 0o755), ("etc/ssl", 0o755), ("etc/ssl/certs", 0o755)] {
            directory(&base.join(name), mode)?;
        }
        for name in ["home", "work"] {
            std::os::unix::fs::chown(base.join(name), Some(uid), Some(gid)).map_err(|_| ())?;
        }
        std::fs::write(base.join("etc/hosts"), HOSTS).map_err(|_| ())?;
        std::fs::set_permissions(base.join("etc/hosts"), std::fs::Permissions::from_mode(0o644)).map_err(|_| ())?;
        let readonly = MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOSYMFOLLOW;
        Ok(mounts::MountInputs {
            home: detached(&base.join("home"), MOUNT_NOSUID | MOUNT_NODEV)?,
            work: detached(&base.join("work"), MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC)?,
            projection: detached(&base.join("config"), readonly | MOUNT_NOEXEC)?,
            runtime: detached(&base.join("runtime"), readonly)?,
            etc: detached(&base.join("etc"), readonly | MOUNT_NOEXEC)?,
        })
    }
    #[repr(C)]
    struct MountAttr { set: u64, clear: u64, propagation: u64, userns_fd: u64 }
    fn detached(path: &Path, attributes: u64) -> Result<mounts::DetachedMount, ()> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| ())?;
        // SAFETY: valid C string; a successful call returns a new owned descriptor.
        let raw = unsafe { libc::syscall(libc::SYS_open_tree, libc::AT_FDCWD, path.as_ptr(),
            OPEN_TREE_CLONE | libc::O_CLOEXEC as u32 | AT_RECURSIVE) };
        if raw < 0 { return Err(()); }
        // SAFETY: the descriptor was just returned to this process only.
        let mount = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
        let private = MountAttr { set: 0, clear: 0, propagation: libc::MS_PRIVATE as u64, userns_fd: 0 };
        let attrs = MountAttr { set: attributes, clear: 0, propagation: 0, userns_fd: 0 };
        for attr in [&private, &attrs] {
            // SAFETY: exact mount_attr v0 layout, read synchronously by the kernel.
            if unsafe { libc::syscall(libc::SYS_mount_setattr, mount.as_raw_fd(), c"".as_ptr(),
                libc::AT_EMPTY_PATH as u32 | AT_RECURSIVE, attr as *const MountAttr,
                std::mem::size_of::<MountAttr>()) } != 0 { return Err(()); }
        }
        let observed = fd::observe(mount.as_raw_fd()).map_err(|_| ())?;
        let flags = attributes & (MOUNT_RDONLY | MOUNT_NOSUID | MOUNT_NODEV | MOUNT_NOEXEC);
        if observed.identity.mount == 0 || observed.fs_flags & flags != flags { return Err(()); }
        Ok(mounts::DetachedMount { fd: mount, expected_mount_id: observed.identity.mount,
            expected_fs_type: observed.fs_type })
    }

    /// The parent stays outside the run's PID namespace (like the runner's
    /// supervisor) and only waits, bounded, for the namespace init.
    fn supervise(child: libc::pid_t) {
        let deadline = Instant::now() + CHILD_DEADLINE;
        loop {
            let mut status = 0;
            // SAFETY: waits only for the child created above.
            let result = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
            if result == child {
                if libc::WIFEXITED(status) {
                    let code = libc::WEXITSTATUS(status);
                    if code == 0 { return; }
                    fail(Stage::from_child(code).unwrap_or(Stage::Supervise));
                }
                fail(Stage::Supervise);
            }
            if result < 0 && errno() != libc::EINTR { fail(Stage::Supervise); }
            if Instant::now() >= deadline {
                // SAFETY: our own child; killing the namespace init ends the namespace.
                unsafe { libc::kill(child, libc::SIGKILL); libc::waitpid(child, std::ptr::null_mut(), 0); }
                fail(Stage::Supervise);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn child_main(uid: u32, gid: u32, abi: u32) -> ! {
        let code = match child_checks(uid, gid, abi) { Ok(()) => 0, Err(stage) => stage.code() };
        // SAFETY: immediate exit of the forked child; no output, no destructors.
        unsafe { libc::_exit(code) }
    }
    /// Namespace init: the runner's `child_init` order up to the common filter
    /// (`runner.rs` mount_proc → privilege drop and readback → Landlock →
    /// common seccomp), then the assertions.
    fn child_checks(uid: u32, gid: u32, abi: u32) -> Result<(), Stage> {
        // SAFETY: argument-free query.
        if unsafe { libc::getpid() } != 1 { return Err(Stage::ChildPid); }
        mounts::mount_proc(true).map_err(|_| Stage::ChildPid)?;
        let me = std::fs::read_link("/proc/self").map_err(|_| Stage::ChildPid)?;
        if me.as_os_str().as_bytes() != b"1" { return Err(Stage::ChildPid); }
        // Same sequence as runner.rs (securebits, bounding set, identity, caps,
        // dumpable, no_new_privs) and the same uid/gid/caps readback.
        privileges::drop_all(uid, gid).map_err(|_| Stage::Privileges)?;
        runner::restrict_filesystem(abi).map_err(|_| Stage::Landlock)?;
        let arch = seccomp::Architecture::native().map_err(|_| Stage::Seccomp)?;
        seccomp::common(arch).install().map_err(|_| Stage::Seccomp)?;
        // (2) Positive control first: ordinary files in the persistent
        // directories must work, or the denials below prove nothing.
        for base in ["/work", "/home/employee"] {
            let path = format!("{base}/selftest-ordinary");
            std::fs::write(&path, b"allowed").map_err(|_| Stage::PositiveWrite)?;
            if std::fs::read(&path).map_err(|_| Stage::PositiveWrite)? != b"allowed" { return Err(Stage::PositiveWrite); }
        }
        // A socket in the per-run ephemeral /tmp is allowed: the denial below is
        // the persistent-directory rule, not a blanket AF_UNIX or seccomp refusal.
        UnixListener::bind("/tmp/selftest.sock").map_err(|_| Stage::SocketAllowed)?;
        // (1) Persistent directories never receive sockets or FIFOs (D8).
        for base in ["/work", "/home/employee"] {
            let denied = UnixListener::bind(format!("{base}/selftest.sock"));
            if !denied.is_err_and(|error| error.raw_os_error() == Some(libc::EACCES)) { return Err(Stage::SocketDenied); }
            let fifo = CString::new(format!("{base}/selftest.fifo")).map_err(|_| Stage::FifoDenied)?;
            // SAFETY: valid C string.
            if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } != -1 || errno() != libc::EACCES { return Err(Stage::FifoDenied); }
        }
        // Positive read control on the read-only projection.
        if std::fs::read("/etc/hosts").map_err(|_| Stage::ReadAllowed)? != HOSTS { return Err(Stage::ReadAllowed); }
        // (3) Outside every rule: the new root itself (mode 0755, so only
        // Landlock can refuse it).
        // SAFETY: static C string; a returned descriptor is closed at once.
        let raw = unsafe { libc::open(c"/".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if raw >= 0 {
            // SAFETY: just opened by this process.
            drop(unsafe { OwnedFd::from_raw_fd(raw) });
            return Err(Stage::ReadDenied);
        }
        if errno() != libc::EACCES { return Err(Stage::ReadDenied); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::stages::{propagation_private, Stage};

    #[test]
    fn selftest_stage_codes_are_distinct_small_and_never_success_or_platform() {
        let mut codes: Vec<i32> = Stage::ALL.iter().map(|stage| stage.code()).collect();
        let mut names: Vec<&str> = Stage::ALL.iter().map(|stage| stage.name()).collect();
        codes.sort(); codes.dedup(); names.sort(); names.dedup();
        assert_eq!(codes.len(), Stage::ALL.len());
        assert_eq!(names.len(), Stage::ALL.len());
        assert!(codes.iter().all(|code| (2..=29).contains(code)));
        assert!(names.iter().all(|name| name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')));
        // Only namespace-init stages are accepted from the child's exit code.
        assert_eq!(Stage::from_child(Stage::Landlock.code()), Some(Stage::Landlock));
        assert_eq!(Stage::from_child(Stage::Root.code()), None);
        assert_eq!(Stage::from_child(0), None);
        assert_eq!(Stage::from_child(125), None);
    }

    #[test]
    fn selftest_propagation_requires_root_and_no_peer_group() {
        let private = "22 1 252:1 / / rw,relatime - ext4 /dev/vda1 rw\n\
            23 22 0:5 / /dev rw,nosuid - devtmpfs udev rw\n";
        assert!(propagation_private(private));
        let shared = "22 1 252:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw\n";
        assert!(!propagation_private(shared));
        let slave = "22 1 252:1 / / rw,relatime - ext4 /dev/vda1 rw\n\
            23 22 0:5 / /dev rw master:3 - devtmpfs udev rw\n";
        assert!(!propagation_private(slave));
        assert!(!propagation_private("23 22 0:5 / /dev rw - devtmpfs udev rw\n"));
        assert!(!propagation_private(""));
        assert!(!propagation_private("garbage without separator\n"));
    }
}
