// Full serve/handshake/launch tests. This module exists only in the debug-only
// root fixture; its registry and syscall profile are never production policy.
use super::*;
use duduclaw_isolation::{ids::RunId, ipc::InputFd};
use duduclaw_run::{linux::{bootstrap, runner::{self, MountIdentity, Program, TrustedRun}},
    seccomp::RuntimeProfile, validation::Identity};
use std::time::{Duration, Instant};

const RUN: RunId = RunId([0x42;16]);
const SUPERVISOR: u32 = 997;
const PROMPT: &[u8] = b"sealed-prompt-fixture";
const ATTACHMENT: &[u8] = b"sealed-attachment-fixture";

struct Registry(Option<TrustedRun>);
impl bootstrap::RootRegistry for Registry {
    fn take_run(&mut self, run: RunId) -> duduclaw_run::Result<TrustedRun> {
        if run != RUN { return Err(duduclaw_run::Error::RegistryUnavailable); }
        self.0.take().ok_or(duduclaw_run::Error::RegistryUnavailable)
    }
}

fn socketpair(kind: i32) -> (OwnedFd,OwnedFd) {
    let mut descriptors=[-1;2];
    assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX,kind|libc::SOCK_CLOEXEC,0,descriptors.as_mut_ptr()) },0);
    unsafe { (OwnedFd::from_raw_fd(descriptors[0]),OwnedFd::from_raw_fd(descriptors[1])) }
}
fn memfd(contents: &[u8]) -> OwnedFd {
    let raw=unsafe { libc::memfd_create(c"launch-fixture".as_ptr(),libc::MFD_CLOEXEC|libc::MFD_ALLOW_SEALING) };
    assert!(raw>=0);
    let mut file=unsafe { File::from_raw_fd(raw) };
    file.write_all(contents).unwrap();
    assert_eq!(unsafe { libc::fcntl(raw,libc::F_ADD_SEALS,15) },0);
    // Deliberately leave the offset at EOF; launch must reset it.
    file.into()
}
fn open_program(registry: RawFd, name: &CStr) -> OwnedFd {
    fd::open_beneath(registry,name,libc::O_RDONLY).unwrap()
}
fn program(identity: Identity, mode: &str, script: Option<Identity>) -> Program {
    // Test-only compatibility envelope, not an approved runtime allowlist.
    // The common seccomp prohibitions remain in force even for this envelope.
    let seccomp=RuntimeProfile {allowed_syscalls:(0..512).collect()};
    let seccomp_digest=seccomp.digest(Architecture::native().unwrap()).unwrap();
    let mut argv=vec![CString::new("fixture").unwrap()];
    let script=script.map(|identity| {
        argv.push(CString::new("/runtime/fixture.script").unwrap());
        (identity,CString::new("fixture.script").unwrap())
    });
    Program {identity,script,argv,environment:vec![CString::new(format!("DUDUCLAW_FIXTURE_PAYLOAD={mode}")).unwrap(),
        CString::new("HOME=/home/employee").unwrap(),CString::new("XDG_RUNTIME_DIR=/run/duduclaw").unwrap()],
        seccomp,seccomp_digest}
}

fn prepare(root: &Path, scenario: &str) -> (TrustedRun,Vec<OwnedFd>,OwnedFd,Option<OwnedFd>) {
    // The test checkout may itself be beneath the /tmp now hidden by the private
    // tmpfs. Read the running image by fd, never reopen that masked pathname.
    let mut source=File::open("/proc/self/exe").unwrap();
    let mut payload=File::create(root.join("runtime/payload")).unwrap();
    std::io::copy(&mut source,&mut payload).unwrap();
    drop((source,payload));
    std::fs::set_permissions(root.join("runtime/payload"),Permissions::from_mode(0o755)).unwrap();
    let scripted=scenario=="launch-script";
    if scripted {
        std::fs::copy("/bin/dash",root.join("runtime/interpreter")).expect("fixture requires /bin/dash");
        std::fs::set_permissions(root.join("runtime/interpreter"),Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.join("runtime/fixture.script"),b"#!/deliberately/nonexistent\nunset PWD\nexec /runtime/payload\n").unwrap();
        std::fs::set_permissions(root.join("runtime/fixture.script"),Permissions::from_mode(0o444)).unwrap();
    }
    if scenario=="launch-exec-failure" {
        std::fs::write(root.join("runtime/broken"),b"\x7fELFnot-an-executable").unwrap();
        std::fs::set_permissions(root.join("runtime/broken"),Permissions::from_mode(0o555)).unwrap();
    }
    let mut descriptors=Vec::new();
    let mut identities=Vec::new();
    for (name,attrs) in [("home",6),("work",14),("config",15|0x0020_0000),
        ("runtime",7|0x0020_0000),("etc",15|0x0020_0000)] {
        let mount=detached(&root.join(name),attrs);
        identities.push(MountIdentity {mount:mount.expected_mount_id,fs_type:mount.expected_fs_type});
        descriptors.push(mount.fd);
    }
    let runtime=descriptors[3].as_raw_fd();
    let executable=open_program(runtime,if scripted {c"interpreter"} else if scenario=="launch-exec-failure" {c"broken"} else {c"payload"});
    let identity=fd::observe(executable.as_raw_fd()).unwrap().identity;
    let script=scripted.then(||open_program(runtime,c"fixture.script"));
    let script_identity=script.as_ref().map(|fd|fd::observe(fd.as_raw_fd()).unwrap().identity);
    let forwarder=if scenario=="launch-forwarder-failure" {
        Some(open_program(runtime,c"payload"))
    } else {None};
    descriptors.push(executable);
    if let Some(script)=script {descriptors.push(script);}
    descriptors.push(memfd(PROMPT));
    descriptors.push(memfd(ATTACHMENT));
    let (mcp,employee_mcp)=socketpair(libc::SOCK_STREAM);
    descriptors.push(employee_mcp);
    let (forwarder_program,egress)=if let Some(executable)=forwarder {
        let identity=fd::observe(executable.as_raw_fd()).unwrap().identity;
        let (host,employee)=socketpair(libc::SOCK_STREAM);
        descriptors.push(employee); descriptors.push(executable);
        (Some(program(identity,"forwarder",None)),Some(host))
    } else {(None,None)};
    let mode=match scenario {
        "launch-pty"=>"pty", "launch-teardown"=>"orphan", "launch-script"=>"script", _=>"normal",
    };
    let plan=TrustedRun {root:mounts::RootConfig {employee_uid:EMPLOYEE,employee_gid:EMPLOYEE,
        pty:scenario=="launch-pty",pty_limit:2,work_noexec:true,tmp_noexec:true,
        tmpfs_bytes:4*1024*1024,tmpfs_inodes:256},mounts:identities.try_into().ok().unwrap(),
        supervisor_uid:SUPERVISOR,supervisor_gid:SUPERVISOR,gateway_uid:998,gateway_mount:0,
        program:program(identity,mode,script_identity),forwarder:forwarder_program,
        prompt_bytes:PROMPT.len() as u64,attachments:vec![InputFd::SealedMemfd {bytes:ATTACHMENT.len() as u64}],
        mcp:true,proc_subset_pid:true};
    (plan,descriptors,mcp,egress)
}

// In the controlled test child, deny only the supervisor identity transition.
// This reaches the actual post-clone privilege failure without a production hook.
fn fail_supervisor_transition() {
    let mut instructions=[
        libc::sock_filter {code:0x20,jt:0,jf:0,k:0},
        libc::sock_filter {code:0x15,jt:0,jf:3,k:libc::SYS_setresuid as u32},
        libc::sock_filter {code:0x20,jt:0,jf:0,k:16},
        libc::sock_filter {code:0x15,jt:0,jf:1,k:SUPERVISOR},
        libc::sock_filter {code:0x06,jt:0,jf:0,k:seccomp::DENY},
        libc::sock_filter {code:0x06,jt:0,jf:0,k:seccomp::ALLOW},
    ];
    let filter=libc::sock_fprog {len:instructions.len() as u16,filter:instructions.as_mut_ptr()};
    assert_eq!(unsafe { libc::syscall(libc::SYS_seccomp,1,0,&filter) },0);
}

fn children(pid: i32) -> Vec<i32> {
    match std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
        Ok(text)=>text.split_whitespace().map(|value|value.parse().unwrap()).collect(),
        Err(error) if error.raw_os_error()==Some(libc::ENOENT)=>Vec::new(),
        Err(error)=>panic!("read controlled child list: {error}"),
    }
}
fn pidfd(pid: i32) -> OwnedFd {
    let descriptor=unsafe { libc::syscall(libc::SYS_pidfd_open,pid,0) };
    assert!(descriptor>=0,"open controlled child pidfd");
    unsafe {OwnedFd::from_raw_fd(descriptor as i32)}
}
fn signal(target: &OwnedFd, signal: i32) {
    assert_eq!(unsafe {libc::syscall(libc::SYS_pidfd_send_signal,target.as_raw_fd(),signal,
        std::ptr::null::<libc::siginfo_t>(),0)},0);
}
fn readable(fd: RawFd, milliseconds: i32) -> bool {
    let mut poll=libc::pollfd {fd,events:libc::POLLIN,revents:0};
    let result=unsafe {libc::poll(&mut poll,1,milliseconds)};
    assert!(result>=0,"fixture poll failed");
    result==1 && poll.revents&(libc::POLLIN|libc::POLLHUP)!=0
}
struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        fn stop_tree(pid: i32) {
            for child in children(pid) {stop_tree(child);}
            unsafe {libc::kill(pid,libc::SIGKILL);}
        }
        // This fixture is a subreaper; its descendants are exclusively test-owned.
        for child in children(unsafe {libc::getpid()}) {stop_tree(child);}
        loop {
            let pid=unsafe {libc::waitpid(-1,std::ptr::null_mut(),0)};
            if pid<0 {if errno()==libc::EINTR {continue;} break;}
        }
    }
}

pub(super) fn run(root: &Path, scenario: &str) {
    assert_eq!(std::env::args_os().count(),1,"serve fixture must have no arguments");
    assert_eq!(unsafe {libc::prctl(libc::PR_SET_CHILD_SUBREAPER,1,0,0,0)},0);
    mounts::enter_namespaces().expect("test coordinator private mount namespace");
    let (plan,descriptors,mcp,egress)=prepare(root,scenario);
    let (bootstrap,child_bootstrap)=socketpair(libc::SOCK_SEQPACKET);
    let (mut output,write_output)=pipe();
    let child=unsafe {libc::fork()};
    assert!(child>=0);
    if child==0 {
        // Simulate PrivateTmp without retaining any host path in the child root.
        mount_fixture_tmpfs(c"/tmp",c"size=8388608,nr_inodes=256,mode=1777");
        let channel=unsafe {libc::fcntl(child_bootstrap.as_raw_fd(),libc::F_DUPFD_CLOEXEC,100)};
        let stdout=unsafe {libc::fcntl(write_output.as_raw_fd(),libc::F_DUPFD_CLOEXEC,100)};
        assert!(channel>=100 && stdout>=100);
        assert_eq!(unsafe {libc::dup2(channel,0)},0);
        assert_eq!(unsafe {libc::dup2(stdout,1)},1);
        assert_eq!(unsafe {libc::dup2(stdout,2)},2);
        if scenario=="launch-supervisor-failure" {fail_supervisor_transition();}
        // All pre-fork descriptor owners are intentionally abandoned: serve takes
        // the complete descriptor table and never returns.
        unsafe {runner::serve(&mut Registry(Some(plan)))}
    }
    let cleanup=Cleanup;
    drop(child_bootstrap); drop(write_output);
    assert!(readable(bootstrap.as_raw_fd(),10_000),"bootstrap challenge timed out");
    let challenge=bootstrap::receive_challenge(bootstrap.as_raw_fd()).unwrap();
    let rights:Vec<_>=descriptors.iter().map(AsRawFd::as_raw_fd).collect();
    bootstrap::send_offer(bootstrap.as_raw_fd(),challenge,RUN,&rights).unwrap();
    drop(descriptors);
    let mut records=Vec::new();
    let mut pty=None;
    let mut ready=false;
    let mut seen_mcp=Vec::new();
    let mut init=None;
    let mut startup_cancelled=false;
    let mut descendant_handles=Vec::new();
    let deadline=Instant::now()+Duration::from_secs(15);
    let mut terminal=None;
    while terminal.is_none() {
        assert!(Instant::now()<deadline,"full launch timed out; status={records:?}");
        let mut polls=[libc::pollfd {fd:bootstrap.as_raw_fd(),events:libc::POLLIN,revents:0},
            libc::pollfd {fd:mcp.as_raw_fd(),events:libc::POLLIN,revents:0}];
        assert!(unsafe {libc::poll(polls.as_mut_ptr(),2,100)}>=0);
        if polls[0].revents&libc::POLLIN!=0 {
            let (packet,mut rights)=bootstrap::receive(bootstrap.as_raw_fd()).unwrap();
            duduclaw_run::status::StatusRecord::decode(&packet,rights.len()).unwrap();
            assert_eq!(&packet[..4],b"DCR1");
            let kind=packet[4]; let code=packet[5];
            let value=u32::from_be_bytes(packet[8..12].try_into().unwrap());
            records.push((kind,code,value));
            if kind==2 {
                assert_eq!(rights.len(),1); assert!(pty.is_none());
                let master=rights.pop().unwrap();
                let observed=fd::observe(master.as_raw_fd()).unwrap();
                assert_eq!(observed.fs_type,0x1cd1);
                assert_eq!((observed.rdev_major,observed.rdev_minor),(5,2));
                pty=Some(master);
            } else {assert!(rights.is_empty());}
            if kind==3 && scenario=="launch-cancel-startup" {
                let init_pid=children(child);
                assert_eq!(init_pid.len(),1);
                init=Some(pidfd(init_pid[0]));
                for pid in children(init_pid[0]) {descendant_handles.push(pidfd(pid));}
                assert!(!descendant_handles.is_empty());
                signal(init.as_ref().unwrap(),libc::SIGTERM);
                startup_cancelled=true;
            }
            if kind==4 || kind==5 {terminal=Some((kind,value));}
            if kind==0 && scenario=="launch-supervisor-failure" {break;}
        }
        if polls[1].revents&libc::POLLIN!=0 && !ready {
            let mut bytes=[0;64];
            let count=unsafe {libc::recv(mcp.as_raw_fd(),bytes.as_mut_ptr().cast(),bytes.len(),libc::MSG_DONTWAIT)};
            if count>0 {seen_mcp.extend_from_slice(&bytes[..count as usize]);}
            if seen_mcp==b"READY" {
                ready=true;
                if !startup_cancelled {
                let init_pid=children(child);
                assert_eq!(init_pid.len(),1,"one namespace init owned by supervisor");
                let init_pid=init_pid[0];
                init=Some(pidfd(init_pid));
                let runtime_children=children(init_pid);
                assert!(!runtime_children.is_empty());
                for pid in runtime_children {
                    descendant_handles.push(pidfd(pid));
                    for namespace in ["pid","mnt","net","ipc","uts"] {
                        assert_ne!(std::fs::read_link(format!("/proc/{pid}/ns/{namespace}")).unwrap(),
                            std::fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap());
                    }
                }
                if scenario=="launch-teardown" {assert!(descendant_handles.len()>=2,"orphan adopted by namespace init");}
                }
                if scenario=="launch-cancel-startup" {
                    // Cancellation is driven by the init receipt, before an
                    // application handshake. READY may race with that receipt.
                } else if scenario=="launch-cancel" {signal(init.as_ref().unwrap(),libc::SIGTERM);}
                else if scenario=="launch-forwarder-failure" {
                    let egress=egress.as_ref().unwrap();
                    assert_eq!(unsafe {libc::send(egress.as_raw_fd(),b"F".as_ptr().cast(),1,libc::MSG_NOSIGNAL)},1);
                } else {
                    assert_eq!(unsafe {libc::send(mcp.as_raw_fd(),b"G".as_ptr().cast(),1,libc::MSG_NOSIGNAL)},1);
                }
            } else {
                assert!(seen_mcp.len()<5 && b"READY".iter().zip(&seen_mcp).all(|(a,b)|a==b),
                    "unexpected fixture payload bytes");
            }
        }
        if polls[0].revents&libc::POLLHUP!=0 && polls[0].revents&libc::POLLIN==0 && terminal.is_none() {
            panic!("bootstrap EOF without terminal status: {records:?}");
        }
    }
    let mut wait_status=0;
    assert_eq!(unsafe {libc::waitpid(child,&mut wait_status,0)},child);
    assert!(libc::WIFEXITED(wait_status),"supervisor killed: {wait_status}; status={records:?}");
    if scenario=="launch-supervisor-failure" {
        assert!(!ready);
        assert!(!root.join("work/runtime-ran").exists(),"supervisor failure must never release the runtime");
        assert!(records.iter().any(|r|*r==(0,10,0)),"fixed privilege error: {records:?}");
        assert!(!records.iter().any(|r|matches!(r.0,1|3|6)));
        assert_eq!(libc::WEXITSTATUS(wait_status),125);
    } else {
        let supervisor=records.iter().position(|r|r.0==6).expect("supervisor readiness receipt");
        let employee=records.iter().position(|r|r.0==1).expect("employee privilege receipt");
        let forked=records.iter().position(|r|r.0==3).expect("children forked receipt");
        assert!(supervisor<employee && employee<forked,"launch receipt ordering");
        let expected=match scenario {"launch-exec-failure"=>126,"launch-forwarder-failure"=>125,
            "launch-cancel"|"launch-cancel-startup"=>143,_=>23};
        assert_eq!(terminal,Some((4,expected)));
        assert_eq!(libc::WEXITSTATUS(wait_status),expected as i32);
        if scenario=="launch-exec-failure" {
            assert!(!ready);
            assert!(records.iter().any(|r|*r==(0,16,0)),"late exec error must survive fd sweep");
        } else if scenario=="launch-forwarder-failure" {
            assert!(ready); assert!(records.iter().any(|r|*r==(0,14,0)));
        } else {
            assert!(ready || startup_cancelled);
            assert!(!records.iter().any(|r|r.0==0));
        }
    }
    for handle in init.iter().chain(descendant_handles.iter()) {
        assert!(readable(handle.as_raw_fd(),1000),"namespace descendant survived init exit");
    }
    if scenario=="launch-pty" {
        let master=pty.as_ref().expect("PTY master delivered");
        assert!(readable(master.as_raw_fd(),1000));
        let mut data=[0;64];
        let count=unsafe {libc::read(master.as_raw_fd(),data.as_mut_ptr().cast(),data.len())};
        assert!(count>0); assert_eq!(&data[..count as usize],b"PTY-OK\r\n");
    } else {assert!(pty.is_none());}
    let mut captured=Vec::new();
    output.read_to_end(&mut captured).unwrap();
    assert!(captured.is_empty(),"runtime/bootstrap leaked unexpected output: {}",String::from_utf8_lossy(&captured));
    drop(cleanup);
    assert!(children(unsafe {libc::getpid()}).is_empty(),"fixture descendants must be reaped");
}

fn exact_read(descriptor: RawFd, expected: &[u8]) {
    let mut actual=vec![0;expected.len()];
    let count=unsafe {libc::read(descriptor,actual.as_mut_ptr().cast(),actual.len())};
    assert_eq!(count,expected.len() as isize); assert_eq!(actual,expected);
}
pub(super) fn runtime_mode() {
    let Ok(mode)=std::env::var("DUDUCLAW_FIXTURE_PAYLOAD") else {return;};
    let result=std::panic::catch_unwind(|| {
        assert_eq!(unsafe {libc::getuid()},EMPLOYEE);
        assert_eq!(unsafe {libc::geteuid()},EMPLOYEE);
        assert_eq!(unsafe {libc::getgid()},EMPLOYEE);
        assert_eq!(unsafe {libc::getsid(0)},unsafe {libc::getpid()});
        assert_eq!(unsafe {libc::prctl(libc::PR_GET_NO_NEW_PRIVS,0,0,0,0)},1);
        let allowed=["DUDUCLAW_FIXTURE_PAYLOAD","HOME","XDG_RUNTIME_DIR"];
        assert_eq!(std::env::vars().count(),allowed.len());
        assert!(std::env::vars().all(|(name,_)|allowed.iter().any(|allowed|*allowed==name)),
            "environment constructed from scratch");
        let last=if mode=="forwarder" {4} else {5};
        for descriptor in 0..4096 {
            let flags=unsafe {libc::fcntl(descriptor,libc::F_GETFD)};
            if descriptor<=last {assert_eq!(flags,0);}
            else {assert_eq!(flags,-1); assert_eq!(errno(),libc::EBADF);}
        }
        if mode=="forwarder" {
            let null=fd::observe(4).unwrap();
            assert_eq!((null.rdev_major,null.rdev_minor),(1,3));
            exact_read(3,b"F"); unsafe {libc::_exit(9);}
        }
        exact_read(4,PROMPT); exact_read(5,ATTACHMENT);
        assert_eq!(unsafe {libc::fcntl(4,libc::F_GET_SEALS)},15);
        assert_eq!(unsafe {libc::fcntl(5,libc::F_GET_SEALS)},15);
        assert_eq!(unsafe {libc::write(4,b"x".as_ptr().cast(),1)},-1); assert_eq!(errno(),libc::EPERM);
        fd::socketpair_endpoint(3).unwrap();
        assert_eq!(std::env::current_dir().unwrap(),Path::new("/work"));
        std::fs::write("/work/runtime-ran",b"fixture").unwrap();
        assert!(!Path::new("/.old").exists());
        assert!(!Path::new("/proc/meminfo").exists());
        if mode=="pty" {
            assert_eq!(unsafe {libc::isatty(0)},1);
            assert_eq!(unsafe {libc::tcgetsid(0)},unsafe {libc::getpid()});
            assert_eq!(fd::observe(0).unwrap().identity,fd::observe(1).unwrap().identity);
            assert_eq!(fd::observe(0).unwrap().identity,fd::observe(2).unwrap().identity);
            assert_eq!(unsafe {libc::write(1,b"PTY-OK\n".as_ptr().cast(),7)},7);
        }
        if mode=="orphan" {
            let child=unsafe {libc::fork()}; assert!(child>=0);
            if child==0 {
                assert!(unsafe {libc::setsid()}>=0);
                let grandchild=unsafe {libc::fork()}; assert!(grandchild>=0);
                if grandchild>0 {unsafe {libc::_exit(0);}}
                loop {unsafe {libc::pause();}}
            }
            let mut status=0; assert_eq!(unsafe {libc::waitpid(child,&mut status,0)},child);
            assert_eq!(status,0);
        }
        assert_eq!(unsafe {libc::send(3,b"READY".as_ptr().cast(),5,libc::MSG_NOSIGNAL)},5);
        exact_read(3,b"G");
    });
    unsafe {libc::_exit(if result.is_ok() {23} else {101});}
}
