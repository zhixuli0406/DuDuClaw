//! Late host mounts must not propagate into a moved, recursively private clone.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use duduclaw_run::linux::{fd, mounts};

const EMPLOYEE: u32 = 70000;

fn path(value: &Path) -> CString { CString::new(value.as_os_str().as_bytes()).unwrap() }

struct Child { pid: libc::pid_t, reaped: bool }
impl Child {
    fn spawn(body: impl FnOnce()) -> Self {
        let parent = unsafe { libc::getpid() };
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "propagation fixture fork");
        if pid == 0 {
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong,
                0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong) }, 0);
            if unsafe { libc::getppid() } != parent { unsafe { libc::_exit(125); } }
            unsafe { libc::alarm(30); }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
            unsafe { libc::_exit(if result.is_ok() { 0 } else { 101 }); }
        }
        Self { pid, reaped: false }
    }
    fn success(&mut self) {
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(self.pid, &mut status, 0) }, self.pid);
        self.reaped = true;
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "propagation worker wait status {status}");
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.reaped {
            unsafe { libc::kill(self.pid, libc::SIGKILL); libc::waitpid(self.pid, std::ptr::null_mut(), 0); }
        }
    }
}

fn pipe() -> (File, File) {
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    unsafe { (File::from_raw_fd(descriptors[0]), File::from_raw_fd(descriptors[1])) }
}

fn tmpfs(target: &CStr) {
    assert_eq!(unsafe { libc::mount(c"tmpfs".as_ptr(), target.as_ptr(), c"tmpfs".as_ptr(),
        libc::MS_NOSUID | libc::MS_NODEV, c"size=1048576,nr_inodes=64,mode=0755".as_ptr().cast()) }, 0,
        "fixture tmpfs");
}

#[repr(C)]
struct MountAttr { set: u64, clear: u64, propagation: u64, userns_fd: u64 }

// Deliberately keep shared propagation on the home clone. build_root must apply
// the production recursive-private defense before moving it into the new root.
fn clone(pathname: &Path, attributes: u64) -> mounts::DetachedMount {
    let raw = unsafe { libc::syscall(libc::SYS_open_tree, libc::AT_FDCWD,
        path(pathname).as_ptr(), 1u32 | libc::O_CLOEXEC as u32 | 0x8000u32) };
    assert!(raw >= 0, "detached fixture clone");
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
    let attr = MountAttr { set: attributes, clear: 0, propagation: 0, userns_fd: 0 };
    assert_eq!(unsafe { libc::syscall(libc::SYS_mount_setattr, descriptor.as_raw_fd(), c"".as_ptr(),
        libc::AT_EMPTY_PATH as u32 | 0x8000u32, &attr, std::mem::size_of_val(&attr)) }, 0);
    let observed = fd::observe(descriptor.as_raw_fd()).unwrap();
    mounts::DetachedMount { fd: descriptor, expected_mount_id: observed.identity.mount,
        expected_fs_type: observed.fs_type }
}

fn mount_id(pathname: &Path) -> u64 {
    let file = File::open(pathname).unwrap();
    fd::observe(file.as_raw_fd()).unwrap().identity.mount
}

pub fn run(root: &Path) {
    let source = root.join("propagation-source");
    let control = root.join("propagation-control");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&control).unwrap();
    let original_mount = mount_id(&source);
    let mut coordinator = Child::spawn(|| {
        // No mount or propagation change below reaches the invoking host.
        assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWNS) }, 0);
        assert_eq!(unsafe { libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) }, 0);
        tmpfs(&path(&source));
        std::fs::create_dir(source.join("nested")).unwrap();
        tmpfs(&path(&source.join("nested")));
        for suffix in ["late", "nested/late"] {
            std::fs::create_dir(source.join(suffix)).unwrap();
            std::fs::write(source.join(suffix).join("sentinel"), b"underlying").unwrap();
        }
        assert_eq!(unsafe { libc::mount(std::ptr::null(), path(&source).as_ptr(), std::ptr::null(),
            libc::MS_REC | libc::MS_SHARED, std::ptr::null()) }, 0);
        assert_eq!(unsafe { libc::mount(path(&source).as_ptr(), path(&control).as_ptr(), std::ptr::null(),
            libc::MS_BIND | libc::MS_REC, std::ptr::null()) }, 0);
        let readonly = 1 | 2 | 4 | 0x0020_0000;
        let inputs = mounts::MountInputs {
            home: clone(&source, 2 | 4), work: clone(&root.join("work"), 2 | 4),
            projection: clone(&root.join("config"), readonly | 8),
            runtime: clone(&root.join("runtime"), readonly),
            etc: clone(&root.join("etc"), readonly | 8),
        };
        let expected_home = inputs.home.expected_mount_id;
        let (mut ready_read, mut ready_write) = pipe();
        let (mut changed_read, mut changed_write) = pipe();
        let mut receiver = Child::spawn(|| {
            assert_eq!(unsafe { libc::close(ready_read.as_raw_fd()) }, 0);
            assert_eq!(unsafe { libc::close(changed_write.as_raw_fd()) }, 0);
            mounts::enter_namespaces().expect("receiver private namespaces");
            // build_root uses a fixed staging name in /tmp; match PrivateTmp so
            // even that empty staging directory never appears on the host.
            tmpfs(c"/tmp");
            let config = mounts::RootConfig { employee_uid: EMPLOYEE, employee_gid: EMPLOYEE,
                pty: false, pty_limit: 2, work_noexec: false, tmp_noexec: true,
                tmpfs_bytes: 1048576, tmpfs_inodes: 64 };
            let prepared = mounts::build_root(inputs, &config).expect("production private detached mount attachment");
            prepared.pivot().expect("production pivot");
            assert_eq!(mount_id(Path::new("/home/employee")), expected_home);
            ready_write.write_all(&[1]).unwrap();
            let mut byte = [0]; changed_read.read_exact(&mut byte).unwrap();
            for suffix in ["late", "nested/late"] {
                assert_eq!(std::fs::read(Path::new("/home/employee").join(suffix).join("sentinel")).unwrap(),
                    b"underlying", "host late submount must not reach the run, including recursive child mounts");
            }
        });
        drop(ready_write); drop(changed_read);
        // spawn drops its closure in the parent, closing the captured mount fds;
        // no parent descriptor keeps the moved mounts alive after receiver exit.
        let mut byte = [0]; ready_read.read_exact(&mut byte).unwrap();
        for suffix in ["late", "nested/late"] {
            tmpfs(&path(&source.join(suffix)));
            std::fs::write(source.join(suffix).join("sentinel"), b"propagated").unwrap();
            assert_eq!(std::fs::read(control.join(suffix).join("sentinel")).unwrap(), b"propagated",
                "positive shared-peer control must receive the late submount");
        }
        changed_write.write_all(&[1]).unwrap();
        receiver.success();
        assert!(!Path::new(&format!("/proc/{}/ns/mnt", receiver.pid)).exists());
    });
    coordinator.success();
    // All fixture namespaces have exited. Host directories and mount identity
    // are unchanged; no mounts or sentinel files escaped the disposable tree.
    assert_eq!(mount_id(&source), original_mount);
    assert_eq!(std::fs::read_dir(&source).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&control).unwrap().count(), 0);
}
