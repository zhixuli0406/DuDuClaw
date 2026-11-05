//! Test-only single-threaded executable. Never install or ship this binary.
//! Concurrent same-uid workers intentionally bypass admission ONLY in this fixture.
#![allow(unsafe_code)]
#[cfg(not(debug_assertions))]
compile_error!("root-test-fixture is forbidden in release builds");

#[cfg(not(target_os = "linux"))]
fn main() { panic!("root fixture requires native Linux"); }

#[cfg(target_os = "linux")]
fn main() { linux::runtime_mode(); linux::run(); }

#[cfg(target_os = "linux")]
#[path = "root-fixture/propagation.rs"]
mod propagation;

#[cfg(target_os = "linux")]
mod linux {
    mod launch { include!("root-fixture/launch.rs"); }
    use std::ffi::{CStr, CString};
    use std::fs::{File, Permissions};
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use duduclaw_run::{fd_plan::{Destination, FdPlan, Source}, landlock::{self, PathClass, Policy},
        linux::{fd, mounts, privileges, probe}, seccomp::{self, Architecture}};

    // No account is created. These identities are used only by forked test workers.
    const EMPLOYEE: u32 = 70000;

    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new() -> Self {
            let mut template=b"/tmp/duduclaw-iso-root-XXXXXX\0".to_vec();
            let pointer=unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
            assert!(!pointer.is_null(), "mkdtemp failed");
            let root=PathBuf::from(unsafe { CStr::from_ptr(pointer) }.to_str().unwrap());
            let fixture=Self(root);
            for name in ["home","work","ephemeral","gateway","config","runtime","etc"] {
                std::fs::create_dir(fixture.0.join(name)).unwrap();
            }
            std::fs::create_dir_all(fixture.0.join("etc/ssl/certs")).unwrap();
            std::fs::write(fixture.0.join("etc/hosts"),b"127.0.0.1 localhost\n").unwrap();
            for name in ["home","work"] {
                let path=cpath(&fixture.0.join(name));
                assert_eq!(unsafe { libc::chown(path.as_ptr(),EMPLOYEE,EMPLOYEE) },0);
                std::fs::set_permissions(fixture.0.join(name),Permissions::from_mode(0o700)).unwrap();
            }
            std::fs::set_permissions(fixture.0.join("gateway"),Permissions::from_mode(0o700)).unwrap();
            std::fs::write(fixture.0.join("gateway/secret"),b"test-only-sentinel").unwrap();
            std::fs::set_permissions(&fixture.0,Permissions::from_mode(0o755)).unwrap();
            fixture
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            // Only this mkdtemp-owned fixture is removed, after worker guards exit.
            if let Err(error)=std::fs::remove_dir_all(&self.0) {
                if error.kind()==std::io::ErrorKind::NotFound {return;}
                eprintln!("fixture cleanup failed: {error}");
                if !std::thread::panicking() { panic!("fixture cleanup failed"); }
            }
        }
    }
    struct Child { pid: libc::pid_t, reaped: bool }
    impl Child {
        fn spawn(f: impl FnOnce()) -> Self {
            let parent=unsafe {libc::getpid()};
            let pid=unsafe { libc::fork() };
            assert!(pid>=0,"fixture fork failed");
            if pid==0 {
                // A nested PID-1 fixture must die if its supervising child exits.
                arm_parent_death(if unsafe {libc::getpid()}==1 {0} else {parent});
                unsafe { libc::alarm(20); }
                let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
                unsafe { libc::_exit(if result.is_ok() {0} else {101}); }
            }
            Self { pid,reaped:false }
        }
        fn success(&mut self) {
            let mut status=0;
            assert_eq!(unsafe { libc::waitpid(self.pid,&mut status,0) },self.pid);
            self.reaped=true;
            assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status)==0,
                "worker failed with wait status {status}");
        }
    }
    fn arm_parent_death(parent: i32) {
        assert_eq!(unsafe {libc::prctl(libc::PR_SET_PDEATHSIG,libc::SIGKILL,0,0,0)},0);
        if unsafe {libc::getppid()}!=parent {unsafe {libc::_exit(125);}}
    }
    impl Drop for Child {
        fn drop(&mut self) {
            if !self.reaped {
                unsafe { libc::kill(self.pid,libc::SIGKILL); libc::waitpid(self.pid,std::ptr::null_mut(),0); }
            }
        }
    }
    fn cpath(path: &Path) -> CString { CString::new(path.as_os_str().as_bytes()).unwrap() }
    fn errno() -> i32 { std::io::Error::last_os_error().raw_os_error().unwrap() }
    fn close_in_child(fd: RawFd) { assert_eq!(unsafe { libc::close(fd) },0); }
    fn pipe() -> (File,File) {
        let mut fds=[-1;2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(),libc::O_CLOEXEC) },0);
        unsafe { (File::from_raw_fd(fds[0]),File::from_raw_fd(fds[1])) }
    }
    fn open_dir(path: &Path) -> OwnedFd {
        let raw=unsafe { libc::open(cpath(path).as_ptr(),libc::O_PATH|libc::O_DIRECTORY|libc::O_CLOEXEC) };
        assert!(raw>=0,"open fixture directory failed");
        unsafe { OwnedFd::from_raw_fd(raw) }
    }
    fn sandbox(root: &Path) -> u64 {
        let parent=unsafe {libc::getppid()};
        mounts::enter_namespaces().expect("required mount/IPC/UTS/network namespaces");
        mounts::bring_loopback_up().expect("private loopback activation");
        // Test fixture mounts only its own directory, inside the private namespace.
        // Production root construction is tested separately through MountInputs.
        let target=cpath(&root.join("ephemeral"));
        let options=CString::new(format!("size=1048576,nr_inodes=64,mode=0700,uid={EMPLOYEE},gid={EMPLOYEE}")).unwrap();
        assert_eq!(unsafe { libc::mount(c"tmpfs".as_ptr(),target.as_ptr(),c"tmpfs".as_ptr(),
            libc::MS_NOSUID|libc::MS_NODEV,options.as_ptr().cast()) },0,"fixture private tmpfs");
        let home=open_dir(&root.join("home"));
        let work=open_dir(&root.join("work"));
        let ephemeral=open_dir(&root.join("ephemeral"));
        let mount_id=fd::observe(ephemeral.as_raw_fd()).unwrap().identity.mount;
        let policy=Policy::for_abi(landlock::probe_abi().expect("Landlock kernel ABI")).unwrap();
        privileges::drop_all(EMPLOYEE,EMPLOYEE).expect("full privilege drop and readback");
        // Credential changes clear PDEATHSIG; close that interval by rearming
        // and comparing the already-established parent after the drop.
        arm_parent_death(parent);
        landlock::restrict(&policy,&[(home.as_fd(),PathClass::Persistent),
            (work.as_fd(),PathClass::Persistent),(ephemeral.as_fd(),PathClass::Ephemeral)])
            .expect("strict Landlock enforcement");
        seccomp::common(Architecture::native().unwrap()).install().expect("common seccomp enforcement");
        mount_id
    }
    fn denied_socket_fifo(root: &Path) {
        // A positive ordinary-file control prevents a DAC-only false positive.
        for name in ["home","work"] {
            let base=root.join(name);
            std::fs::write(base.join("ordinary"),b"allowed").unwrap();
            let error=UnixListener::bind(base.join("forbidden.sock")).unwrap_err();
            assert_eq!(error.raw_os_error(),Some(libc::EACCES));
            assert_eq!(unsafe { libc::mkfifo(cpath(&base.join("forbidden.fifo")).as_ptr(),0o600) },-1);
            assert_eq!(errno(),libc::EACCES);
        }
    }
    fn impossible_rights_send(root: &Path) {
        // Try an actual SCM_RIGHTS send after the cross-run rendezvous fails.
        let socket=unsafe { libc::socket(libc::AF_UNIX,libc::SOCK_STREAM|libc::SOCK_CLOEXEC,0) };
        assert!(socket>=0);
        let socket=unsafe { OwnedFd::from_raw_fd(socket) };
        let path=cpath(&root.join("ephemeral/listener.sock"));
        let mut address:libc::sockaddr_un=unsafe { std::mem::zeroed() };
        address.sun_family=libc::AF_UNIX as u16;
        assert!(path.as_bytes_with_nul().len()<=address.sun_path.len());
        for (dst,src) in address.sun_path.iter_mut().zip(path.as_bytes_with_nul()) { *dst=*src as libc::c_char; }
        assert_eq!(unsafe { libc::connect(socket.as_raw_fd(),(&address as *const libc::sockaddr_un).cast(),
            (std::mem::size_of::<libc::sa_family_t>()+path.as_bytes_with_nul().len()) as libc::socklen_t) },-1);
        assert_eq!(errno(),libc::ENOENT);
        let payload=unsafe { libc::memfd_create(c"fixture-rights".as_ptr(),libc::MFD_CLOEXEC) };
        assert!(payload>=0);
        let payload=unsafe { OwnedFd::from_raw_fd(payload) };
        let byte=1u8;
        let mut iov=libc::iovec { iov_base:(&byte as *const u8).cast_mut().cast(),iov_len:1 };
        let mut control=[0usize;8];
        let mut msg:libc::msghdr=unsafe { std::mem::zeroed() };
        msg.msg_iov=&mut iov; msg.msg_iovlen=1;
        msg.msg_control=control.as_mut_ptr().cast();
        msg.msg_controllen=unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
        unsafe {
            let header=libc::CMSG_FIRSTHDR(&msg);
            assert!(!header.is_null());
            (*header).cmsg_level=libc::SOL_SOCKET; (*header).cmsg_type=libc::SCM_RIGHTS;
            (*header).cmsg_len=libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
            libc::CMSG_DATA(header).cast::<RawFd>().write(payload.as_raw_fd());
            assert_eq!(libc::sendmsg(socket.as_raw_fd(),&msg,libc::MSG_NOSIGNAL),-1);
        }
        assert!(matches!(errno(),libc::ENOTCONN|libc::EPIPE));
    }
    fn d8(root: &Path) {
        // Existing, non-listening socket models the hostile pre-existing inode case.
        let stale=root.join("work/stale.sock");
        drop(UnixListener::bind(&stale).unwrap());
        std::fs::set_permissions(&stale,Permissions::from_mode(0o666)).unwrap();
        let (mut ready_read,mut ready_write)=pipe();
        let (mut finish_read,mut finish_write)=pipe();
        let mut a=Child::spawn(|| {
            close_in_child(ready_read.as_raw_fd()); close_in_child(finish_write.as_raw_fd());
            let mount_id=sandbox(root);
            denied_socket_fifo(root);
            assert!(UnixListener::bind(&stale).is_err());
            let listener=UnixListener::bind(root.join("ephemeral/listener.sock")).unwrap();
            listener.set_nonblocking(true).unwrap();
            ready_write.write_all(&mount_id.to_ne_bytes()).unwrap();
            let mut done=[0]; finish_read.read_exact(&mut done).unwrap();
            assert_eq!(listener.accept().unwrap_err().kind(),std::io::ErrorKind::WouldBlock);
        });
        drop(ready_write); drop(finish_read);
        let mut a_mount=[0;8]; ready_read.read_exact(&mut a_mount).unwrap();
        let (mut b_read,mut b_write)=pipe();
        let mut b=Child::spawn(|| {
            close_in_child(b_read.as_raw_fd()); close_in_child(ready_read.as_raw_fd()); close_in_child(finish_write.as_raw_fd());
            let mount_id=sandbox(root);
            denied_socket_fifo(root);
            assert_eq!(UnixStream::connect(&stale).unwrap_err().raw_os_error(),Some(libc::ECONNREFUSED));
            assert_eq!(UnixStream::connect(root.join("ephemeral/listener.sock")).unwrap_err().raw_os_error(),Some(libc::ENOENT));
            impossible_rights_send(root);
            b_write.write_all(&mount_id.to_ne_bytes()).unwrap();
        });
        drop(b_write);
        let mut b_mount=[0;8]; b_read.read_exact(&mut b_mount).unwrap();
        b.success();
        assert_ne!(u64::from_ne_bytes(a_mount),u64::from_ne_bytes(b_mount));
        finish_write.write_all(&[1]).unwrap();
        a.success();
    }
    fn gateway(root: &Path) {
        let mut child=Child::spawn(|| {
            sandbox(root);
            std::fs::write(root.join("work/positive"),b"allowed").unwrap();
            assert_eq!(std::fs::read(root.join("gateway/secret")).unwrap_err().raw_os_error(),Some(libc::EACCES));
        });
        child.success();
    }
    fn metadata(root: &Path) {
        let mut child=Child::spawn(|| {
            sandbox(root);
            let socket=unsafe { libc::socket(libc::AF_INET,libc::SOCK_STREAM|libc::SOCK_CLOEXEC,0) };
            assert!(socket>=0);
            let socket=unsafe { OwnedFd::from_raw_fd(socket) };
            let mut address:libc::sockaddr_in=unsafe { std::mem::zeroed() };
            address.sin_family=libc::AF_INET as u16;
            address.sin_port=80u16.to_be();
            address.sin_addr.s_addr=u32::from_ne_bytes([169,254,169,254]);
            assert_eq!(unsafe { libc::connect(socket.as_raw_fd(),(&address as *const libc::sockaddr_in).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t) },-1);
            assert_eq!(errno(),libc::ENETUNREACH);
        });
        child.success();
    }
    fn ptrace(root: &Path) {
        let (mut ready_read,mut ready_write)=pipe();
        let (mut finish_read,mut finish_write)=pipe();
        let mut victim=Child::spawn(|| {
            close_in_child(ready_read.as_raw_fd()); close_in_child(finish_write.as_raw_fd());
            sandbox(root);
            ready_write.write_all(&[1]).unwrap();
            let mut byte=[0]; finish_read.read_exact(&mut byte).unwrap();
        });
        drop(ready_write); drop(finish_read);
        let mut byte=[0]; ready_read.read_exact(&mut byte).unwrap();
        let victim_pid=victim.pid;
        let mut attacker=Child::spawn(|| {
            close_in_child(ready_read.as_raw_fd()); close_in_child(finish_write.as_raw_fd());
            sandbox(root);
            assert_eq!(unsafe { libc::ptrace(libc::PTRACE_ATTACH,victim_pid,
                std::ptr::null_mut::<u8>(),std::ptr::null_mut::<u8>()) },-1);
            assert_eq!(errno(),libc::EPERM);
        });
        attacker.success(); finish_write.write_all(&[1]).unwrap(); victim.success();
    }
    fn fds() {
        let mut limit:libc::rlimit=unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE,&mut limit) },0);
        if limit.rlim_cur<4096 {
            assert!(limit.rlim_max>=4096,"fixture requires hard NOFILE >= 4096");
            limit.rlim_cur=4096;
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE,&limit) },0);
        }
        let mut child=Child::spawn(|| {
            let mut sources=Vec::new();
            // Put three different objects at 3,4,5; then actually rotate them.
            for target in 3..=5 {
                let raw=unsafe { libc::memfd_create(c"fixture-fd".as_ptr(),libc::MFD_CLOEXEC) };
                assert!(raw>=0);
                if raw!=target { assert_eq!(unsafe { libc::dup3(raw,target,libc::O_CLOEXEC) },target); unsafe { libc::close(raw); } }
                sources.push(unsafe { OwnedFd::from_raw_fd(target) });
            }
            let identities:Vec<_>=sources.iter().map(|source|fd::observe(source.as_raw_fd()).unwrap().identity).collect();
            let executable=unsafe { libc::open(c"/proc/self/exe".as_ptr(),libc::O_RDONLY|libc::O_CLOEXEC) };
            assert!(executable>=0);
            let executable=unsafe { OwnedFd::from_raw_fd(executable) };
            let exec_identity=fd::observe(executable.as_raw_fd()).unwrap().identity;
            let null=unsafe { libc::open(c"/dev/null".as_ptr(),libc::O_RDWR|libc::O_CLOEXEC) };
            assert!(null>=0);
            let null=unsafe { OwnedFd::from_raw_fd(null) };
            let mut entries=vec![Source {fd:3,destination:Destination::Runtime(vec![4])},
                Source {fd:4,destination:Destination::Runtime(vec![5])},
                Source {fd:5,destination:Destination::Runtime(vec![3])},
                Source {fd:executable.as_raw_fd(),destination:Destination::Executable},
                Source {fd:null.as_raw_fd(),destination:Destination::Runtime(vec![0,1,2])}];
            sources.push(executable); sources.push(null);
            let plan=FdPlan::new(std::mem::take(&mut entries),fd::soft_limit().unwrap()).unwrap();
            let executable=fd::reorder(plan,sources,None).unwrap();
            assert_eq!(fd::observe(4).unwrap().identity,identities[0]);
            assert_eq!(fd::observe(5).unwrap().identity,identities[1]);
            assert_eq!(fd::observe(3).unwrap().identity,identities[2]);
            assert_eq!(fd::observe(executable.as_raw_fd()).unwrap().identity,exec_identity);
            for descriptor in 0..=executable.as_raw_fd()+1 {
                let flags=unsafe { libc::fcntl(descriptor,libc::F_GETFD) };
                if descriptor<=5 { assert_eq!(flags,0); }
                else if descriptor==executable.as_raw_fd() { assert_eq!(flags,libc::FD_CLOEXEC); }
                else { assert_eq!(flags,-1); assert_eq!(errno(),libc::EBADF); }
            }
            let _=executable.into_raw_fd();
        });
        child.success();
    }
    #[repr(C)]
    struct MountAttr { set:u64,clear:u64,propagation:u64,userns_fd:u64 }
    fn detached(path: &Path, attributes: u64) -> mounts::DetachedMount {
        let mount=unsafe { libc::syscall(libc::SYS_open_tree,libc::AT_FDCWD,
            cpath(path).as_ptr(),1u32|libc::O_CLOEXEC as u32|0x8000u32) };
        assert!(mount>=0,"clone fixture anchor");
        let mount=unsafe { OwnedFd::from_raw_fd(mount as RawFd) };
        let private=MountAttr {set:0,clear:0,propagation:libc::MS_PRIVATE,userns_fd:0};
        let attrs=MountAttr {set:attributes,clear:0,propagation:0,userns_fd:0};
        for attr in [&private,&attrs] {
            assert_eq!(unsafe { libc::syscall(libc::SYS_mount_setattr,mount.as_raw_fd(),c"".as_ptr(),
                libc::AT_EMPTY_PATH as u32|0x8000u32,attr,std::mem::size_of::<MountAttr>()) },0);
        }
        let observed=fd::observe(mount.as_raw_fd()).unwrap();
        assert_eq!(observed.fs_flags&(attributes&15),attributes&15);
        mounts::DetachedMount {fd:mount,expected_mount_id:observed.identity.mount,
            expected_fs_type:observed.fs_type}
    }
    fn mount_fixture_tmpfs(target: &CStr, options: &CStr) {
        assert_eq!(unsafe { libc::mount(c"tmpfs".as_ptr(),target.as_ptr(),c"tmpfs".as_ptr(),
            libc::MS_NOSUID|libc::MS_NODEV,options.as_ptr().cast()) },0,"fixture tmpfs mount");
    }
    fn observe_path(path: &str) -> duduclaw_run::validation::Observation {
        let descriptor=open_dir(Path::new(path));
        fd::observe(descriptor.as_raw_fd()).unwrap()
    }
    fn mount_tree(root: &Path) {
        let mut child=Child::spawn(|| {
            mounts::enter_namespaces().expect("private namespaces for root construction");
            // A child mount in /usr verifies recursive RO, not just the top mount.
            // All mounts here are private; no host directory or file is modified.
            mount_fixture_tmpfs(c"/usr/share",c"size=1048576,nr_inodes=64,mode=0755");
            std::fs::write("/usr/share/recursive-readonly-probe",b"fixture").unwrap();
            let readonly=1|2|4|0x0020_0000;
            let inputs=mounts::MountInputs {
                home:detached(&root.join("home"),2|4),
                work:detached(&root.join("work"),2|4|8),
                projection:detached(&root.join("config"),readonly|8),
                runtime:detached(&root.join("runtime"),readonly),
                etc:detached(&root.join("etc"),readonly|8),
            };
            let expected_runtime=inputs.runtime.expected_mount_id;
            // Simulate systemd PrivateTmp only after retaining detached anchors.
            mount_fixture_tmpfs(c"/tmp",c"size=4194304,nr_inodes=128,mode=1777");
            let config=mounts::RootConfig {employee_uid:EMPLOYEE,employee_gid:EMPLOYEE,
                pty:true,pty_limit:2,work_noexec:true,tmp_noexec:true,
                tmpfs_bytes:1048576,tmpfs_inodes:64};
            let mut prepared=mounts::build_root(inputs,&config).expect("build actual root tree");
            let pty=prepared.take_pty().expect("requested PTY must exist");
            assert!(prepared.take_pty().is_none(),"PTY handoff must be one-shot");
            let master=fd::observe(pty.master.as_raw_fd()).unwrap();
            let slave=fd::observe(pty.slave.as_raw_fd()).unwrap();
            assert_eq!(master.kind,duduclaw_run::validation::FileKind::Character);
            assert_eq!((master.rdev_major,master.rdev_minor),(5,2));
            assert_eq!(slave.kind,duduclaw_run::validation::FileKind::Character);
            assert!((136..=143).contains(&slave.rdev_major));
            assert_eq!(slave.uid,EMPLOYEE);
            assert_eq!(slave.mode&0o7777,0o620);
            assert_eq!(master.identity.mount,pty.mount_id);
            assert_eq!(slave.identity.mount,pty.mount_id);
            assert_eq!(master.fs_type,0x1cd1);
            assert!(master.cloexec && slave.cloexec);
            // The control PTY consumes one of this devpts instance's two slots.
            let extra=unsafe { libc::open(c"/tmp/duduclaw-root/dev/pts/ptmx".as_ptr(),
                libc::O_RDWR|libc::O_NOCTTY|libc::O_CLOEXEC) };
            assert!(extra>=0,"second PTY within per-run limit");
            let extra=unsafe { OwnedFd::from_raw_fd(extra) };
            assert_eq!(fd::observe(extra.as_raw_fd()).unwrap().identity.mount,pty.mount_id);
            let nodes=std::fs::read_dir("/tmp/duduclaw-root/dev/pts").unwrap().map(|entry|entry.unwrap())
                .filter(|entry|entry.file_name().to_str().and_then(|name|name.parse::<u32>().ok()).is_some()).count();
            assert_eq!(nodes,2,"per-run devpts contains exactly the allocated slaves");
            let excess=unsafe { libc::open(c"/tmp/duduclaw-root/dev/pts/ptmx".as_ptr(),
                libc::O_RDWR|libc::O_NOCTTY|libc::O_CLOEXEC) };
            assert_eq!(excess,-1,"devpts max must reject the third PTY");
            assert_eq!(errno(),libc::ENOSPC);
            drop(extra);
            let pty_mount=pty.mount_id;
            drop(pty.master);
            let readback=prepared.pivot().expect("actual pivot_root and old-root detach");
            assert_eq!(readback.runtime_mount_id,expected_runtime);
            assert_eq!(readback.devpts_mount_id,pty_mount);
            assert_eq!(observe_path("/runtime").identity.mount,expected_runtime);
            assert_eq!(observe_path("/dev/pts").identity.mount,pty_mount);
            assert_eq!(observe_path("/home/employee").uid,EMPLOYEE);
            for (path,flags) in [("/",7),("/usr",7),("/usr/share",7),("/work",14),
                ("/config",15),("/runtime",7),("/etc",15),("/tmp",14),("/dev/shm",14),("/dev",11)] {
                assert_eq!(observe_path(path).fs_flags&flags,flags,"mount flags for {path}");
            }
            assert_ne!(observe_path("/usr").identity.mount,observe_path("/usr/share").identity.mount);
            assert_eq!(std::fs::write("/usr/share/refused",b"x").unwrap_err().raw_os_error(),Some(libc::EROFS));
            assert_eq!(std::fs::read("/etc/hosts").unwrap(),b"127.0.0.1 localhost\n");
            assert!(!Path::new("/etc/passwd").exists(),"host /etc must not leak into projection");
            assert!(!Path::new("/.old").exists());
            assert_eq!(observe_path("/dev/pts").fs_flags&1,0,"devpts must remain writable");
            assert_eq!(observe_path("/dev").fs_flags&4,0,"device nodes must remain usable");
            drop(pty.slave);
            mounts::bring_loopback_up().expect("private loopback after pivot");
            assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWPID) },0,"new PID namespace");
            let mut init=Child::spawn(|| {
                assert_eq!(unsafe { libc::getpid() },1,"proc is mounted by namespace PID 1");
                mounts::mount_proc(true).expect("actual restricted proc mount");
                let proc=observe_path("/proc");
                assert_eq!(proc.fs_type,0x9fa0);
                assert_eq!(proc.fs_flags&14,14);
                assert_ne!(proc.identity.mount,observe_path("/").identity.mount);
                assert_eq!(observe_path("/proc/1").identity.mount,proc.identity.mount);
                assert!(!Path::new("/proc/meminfo").exists(),"subset=pid must hide non-process files");
                assert!(!Path::new("/proc/cpuinfo").exists());
                assert!(Path::new("/proc/1/status").is_file());
            });
            init.success();
        });
        child.success();
    }
    pub fn runtime_mode() { launch::runtime_mode(); }
    pub fn run() {
        assert_eq!(unsafe {libc::geteuid()},0,"run iso_root_ tests as root");
        privileges::require_root_capabilities().expect("required root capabilities");
        let parent=unsafe {libc::getpid()};
        let parent_fd=unsafe {libc::syscall(libc::SYS_pidfd_open,parent,0)} as i32;
        assert!(parent_fd>=0,"fixture supervisor pidfd");
        assert_eq!(unsafe {libc::unshare(libc::CLONE_NEWPID)},0,"fixture cleanup PID namespace");
        let child=unsafe {libc::fork()};
        assert!(child>=0);
        if child==0 {
            arm_parent_death(0);
            let mut poll=libc::pollfd {fd:parent_fd,events:libc::POLLIN,revents:0};
            assert_eq!(unsafe {libc::poll(&mut poll,1,0)},0,"fixture supervisor must still be alive");
            unsafe {libc::close(parent_fd);}
            mounts::enter_namespaces().expect("fixture private mounts before proc projection");
            // /proc must describe this namespace, so cleanup never interprets a
            // namespace-local PID as a host PID or walks host init's children.
            assert_eq!(unsafe {libc::mount(c"proc".as_ptr(),c"/proc".as_ptr(),c"proc".as_ptr(),
                libc::MS_NOSUID|libc::MS_NODEV|libc::MS_NOEXEC,std::ptr::null())},0);
            // All fixture files live on this bounded namespace-owned filesystem.
            // Namespace death also removes them on panic, timeout or _exit.
            mount_fixture_tmpfs(c"/tmp",c"size=268435456,nr_inodes=8192,mode=1777");
            let root=TempRoot::new();
            run_fixture(&root.0);
            drop(root);
            unsafe {libc::_exit(0);}
        }
        unsafe {libc::close(parent_fd);}
        // Killing this namespace's PID 1 kills every fixture descendant, even
        // those that called setsid or changed credentials. No host process is a
        // member. The outer process remains available to enforce the deadline.
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(45);
        loop {
            let mut status=0;
            let result=unsafe {libc::waitpid(child,&mut status,libc::WNOHANG)};
            if result==child {
                assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status)==0,
                    "fixture namespace failed with wait status {status}");
                break;
            }
            assert!(result>=0,"fixture namespace wait failed");
            if std::time::Instant::now()>=deadline {
                unsafe {libc::kill(child,libc::SIGKILL); libc::waitpid(child,std::ptr::null_mut(),0);}
                panic!("fixture namespace deadline exceeded");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    fn run_fixture(root: &Path) {
        assert_eq!(unsafe { libc::geteuid() },0,"run iso_root_ tests as root");
        let mut limit:libc::rlimit=unsafe {std::mem::zeroed()};
        assert_eq!(unsafe {libc::getrlimit(libc::RLIMIT_NOFILE,&mut limit)},0);
        assert!(limit.rlim_max>=4096,"fixture requires hard NOFILE >= 4096");
        limit.rlim_cur=4096;
        assert_eq!(unsafe {libc::setrlimit(libc::RLIMIT_NOFILE,&limit)},0);
        privileges::require_root_capabilities().expect("required root capabilities");
        probe::probe().expect("kernel readiness must pass; tests do not skip unsupported hosts");
        let scenario=std::env::var("DUDUCLAW_ROOT_TEST").expect("fixed test scenario");
        match scenario.as_str() {
            "d8"=>d8(root),"gateway"=>gateway(root),"metadata"=>metadata(root),
            "ptrace"=>ptrace(root),"fds"=>fds(),"mounts"=>mount_tree(root),
            "propagation"=>crate::propagation::run(root),
            "launch-elf"|"launch-script"|"launch-pty"|"launch-exec-failure"|
            "launch-forwarder-failure"|"launch-cancel"|"launch-cancel-startup"|"launch-teardown"|
            "launch-supervisor-failure"|"launch-fast-exit"=>launch::run(root,&scenario),
            _=>panic!("unknown test scenario"),
        }
    }
}
