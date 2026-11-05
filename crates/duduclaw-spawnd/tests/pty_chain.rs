//! Installed A -> B1 -> systemd -> B2 PTY tests. No synthetic readiness or policy.
#![cfg(all(target_os = "linux", feature = "root-test-fixture"))]
use std::{fs, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd},
    unix::{fs::MetadataExt, net::UnixStream, process::CommandExt}}, path::Path,
    process::{Child, Command, Stdio}, time::{Duration, Instant}};
use duduclaw_execution::SpawndBackend;
use duduclaw_isolation::{bounded::Text, execution::*, ids::{EmployeeId, Sha256Digest}, ipc::RunStatus};
use duduclaw_spawnd::{installed::{ExecutableRecord, GatewayPublication}, systemd::linux::Systemd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
const CHILD_FD: i32 = 9;
const DEADLINE: Duration = Duration::from_secs(90);
fn kind() -> RuntimeKind { RuntimeKind::Registered(Text::new("iso-pty-fixture-v1".into()).unwrap()) }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup { employee: EmployeeId, uid: u32, generation: Sha256Digest, digest: Sha256Digest, nonce: String, mode: String }
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Event { Reserved { run: RunHandle }, Ready, Observed, Finished }
fn send<T: Serialize>(socket: &mut UnixStream, value: &T) {
    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let bytes = serde_json::to_vec(value).unwrap(); assert!(bytes.len() <= 4096);
    socket.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap(); socket.write_all(&bytes).unwrap();
}
fn receive<T: for<'de> Deserialize<'de>>(socket: &mut UnixStream) -> T {
    socket.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut size = [0;4]; socket.read_exact(&mut size).unwrap(); let size = u32::from_be_bytes(size) as usize;
    assert!(size > 0 && size <= 4096); let mut bytes = vec![0;size]; socket.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn sha(path: impl AsRef<Path>) -> Sha256Digest {
    let mut source = fs::File::open(path).unwrap(); let mut hash = Sha256::new(); let mut bytes = [0;65536];
    loop { let count = source.read(&mut bytes).unwrap(); if count == 0 { break; } hash.update(&bytes[..count]); }
    Sha256Digest(hash.finalize().into())
}
struct Cleanup { child: Child, run: Option<RunHandle>, uid: u32, finished: bool }
impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.finished { let _ = self.child.kill(); }
        if !self.finished {
            if let (Ok(systemd), Some(run)) = (Systemd::connect(), self.run) {
                let _ = systemd.kill_run(run.0);
                let deadline = Instant::now() + Duration::from_secs(15);
                loop {
                    let retired = match systemd.inspect_run(run.0) {
                        Ok(unit) => systemd.cleanup(&unit).is_ok_and(|proof| proof == (true, true)),
                        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
                    };
                    if retired && duduclaw_spawnd::accounts::unexpected_processes(self.uid, None).is_ok_and(|p| p.is_empty()) { break; }
                    if Instant::now() >= deadline { eprintln!("PTY test-owned run retirement could not be proved"); break; }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
fn acceptance(mode: &str) {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
    let (uid,gid) = duduclaw_spawnd::server::gateway_identity().unwrap();
    let publication = GatewayPublication::read(gid).expect("installed signed root publication");
    let policy = publication.policy().unwrap();
    let employee = publication.entitlements.iter().find(|entry| policy.derive(entry.employee, &kind(), true).is_ok())
        .expect("explicit signed PTY fixture entitlement on disposable VM").employee;
    let (runtime,_) = policy.derive(employee, &kind(), true).unwrap();
    assert!(!runtime.mcp && runtime.pty_allowed && runtime.fixed_args.is_empty() && runtime.network == NetworkPolicy::Offline);
    let idle = runtime.timeouts.pty_idle_ms.unwrap(); assert!((2000..=10000).contains(&idle));
    assert!(runtime.timeouts.runtime_ms > idle * 4);
    let ExecutableRecord::Elf { digest,.. } = publication.runtimes.iter().find(|entry| entry.profile == runtime.profile).unwrap().executable
        else { panic!("signed fixture ELF required"); };
    assert_eq!(digest, sha(env!("CARGO_BIN_EXE_duduclaw-spawnd-pty-fixture")));
    let registry_path = Path::new("/var/lib/duduclaw-spawnd/registry.json");
    let metadata = fs::symlink_metadata(registry_path).unwrap();
    assert!(metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0);
    let registry: duduclaw_spawnd::registry::Registry = serde_json::from_slice(&fs::read(registry_path).unwrap()).unwrap();
    let employee_uid = registry.lookup(employee).unwrap().uid;
    assert!(duduclaw_spawnd::accounts::unexpected_processes(employee_uid, None).unwrap().is_empty(), "test employee must be idle");
    let mut nonce = [0;16]; fs::File::open("/dev/urandom").unwrap().read_exact(&mut nonce).unwrap();
    let setup = Setup { employee, uid: employee_uid, generation: publication.generation, digest,
        nonce: nonce.iter().map(|byte| format!("{byte:02x}")).collect(), mode: mode.into() };
    let (mut observer,gateway) = UnixStream::pair().unwrap(); let fd = gateway.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--ignored","--exact","installed_pty_gateway_child","--test-threads=1","--nocapture"])
        .env_clear().env("DUDUCLAW_ISO_PTY_CHILD","1").stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    unsafe { command.pre_exec(move || {
        if libc::dup2(fd,CHILD_FD) < 0 || libc::fcntl(CHILD_FD,libc::F_SETFD,0) < 0
            || libc::setgroups(0,std::ptr::null()) != 0 || libc::setresgid(gid,gid,gid) != 0
            || libc::setresuid(uid,uid,uid) != 0 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    let mut cleanup = Cleanup { child: command.spawn().unwrap(), run: None, uid: setup.uid, finished: false }; drop(gateway);
    send(&mut observer,&setup);
    let Event::Reserved { run } = receive(&mut observer) else { panic!("missing run identity"); }; cleanup.run = Some(run);
    assert!(matches!(receive::<Event>(&mut observer),Event::Ready));
    let systemd = Systemd::connect().unwrap(); let unit = systemd.inspect_run(run.0).unwrap();
    let pids: Vec<u32> = fs::read_to_string(unit.cgroup.join("cgroup.procs")).unwrap().split_whitespace().map(|v|v.parse().unwrap()).collect();
    let pid = pids.into_iter().find(|pid| fs::metadata(format!("/proc/{pid}/exe")).is_ok()
        && sha(format!("/proc/{pid}/exe")) == setup.digest).expect("actual signed PTY fixture running in cgroup");
    for name in ["mnt","pid","net"] { assert_ne!(fs::read_link(format!("/proc/{pid}/ns/{name}")).unwrap(),fs::read_link(format!("/proc/self/ns/{name}")).unwrap()); }
    for fd in 0..=2 {
        let stat = fs::metadata(format!("/proc/{pid}/fd/{fd}")).unwrap();
        assert_eq!(stat.uid(),setup.uid); assert_eq!(stat.mode() & libc::S_IFMT,libc::S_IFCHR);
        assert_eq!(libc::major(stat.rdev()),136); assert_eq!(libc::minor(stat.rdev()),0);
    }
    send(&mut observer,&Event::Observed);
    assert!(matches!(receive::<Event>(&mut observer),Event::Finished));
    let deadline = Instant::now()+Duration::from_secs(15);
    loop {
        if systemd.cleanup(&unit).is_ok_and(|proof| proof == (true,true))
            && duduclaw_spawnd::accounts::unexpected_processes(setup.uid,None).unwrap().is_empty() { break; }
        assert!(Instant::now()<deadline,"unit, cgroup and employee UID must all retire");
        std::thread::sleep(Duration::from_millis(50));
    }
    while cleanup.child.try_wait().unwrap().is_none() { assert!(Instant::now()<deadline); std::thread::sleep(Duration::from_millis(20)); }
    assert!(cleanup.child.wait().unwrap().success()); cleanup.finished = true;
}
#[test]
#[ignore = "root disposable installed Linux VM; signed iso-pty-fixture-v1, idle confirmed employee, real B1/B2/F/H"]
fn iso_root_pty_allocation_streaming_and_resize() { acceptance("resize"); }
#[test]
#[ignore = "root disposable installed Linux VM; signed iso-pty-fixture-v1 with idle 2..10s; real B1/B2/F/H"]
fn iso_root_pty_idle_timeout_and_activity() { acceptance("idle"); }
#[test]
#[ignore = "root disposable installed Linux VM; signed iso-pty-fixture-v1; real B1/B2/F/H retirement"]
fn iso_root_pty_cancellation_and_retirement() { acceptance("cancel"); }

async fn line(io: &mut RunPty, pending: &mut Vec<u8>) -> String {
    loop {
        if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let mut bytes: Vec<_> = pending.drain(..=end).collect(); bytes.pop();
            return String::from_utf8(bytes).unwrap();
        }
        assert!(pending.len() < 1024); let mut buffer = [0;256];
        let count = io.output.read(&mut buffer).await.unwrap(); assert_ne!(count,0);
        pending.extend_from_slice(&buffer[..count]);
    }
}

#[test]
#[ignore = "internal gateway-UID helper, only run through root parent"]
fn installed_pty_gateway_child() {
    assert_eq!(std::env::var("DUDUCLAW_ISO_PTY_CHILD").as_deref(),Ok("1"));
    let (uid,gid)=duduclaw_spawnd::server::gateway_identity().unwrap();
    assert_eq!(unsafe{libc::getuid()},uid); assert_eq!(unsafe{libc::getgid()},gid);
    let mut observer=unsafe{UnixStream::from_raw_fd(CHILD_FD)};
    let mut peer:libc::ucred=unsafe{std::mem::zeroed()}; let mut len=std::mem::size_of_val(&peer) as libc::socklen_t;
    assert_eq!(unsafe{libc::getsockopt(observer.as_raw_fd(),libc::SOL_SOCKET,libc::SO_PEERCRED,(&mut peer as *mut libc::ucred).cast(),&mut len)},0);
    assert_eq!((peer.uid,peer.gid),(0,0)); assert_eq!(len as usize,std::mem::size_of_val(&peer));
    let setup:Setup=receive(&mut observer);
    let rt=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async{tokio::time::timeout(DEADLINE,async{
        let publication=GatewayPublication::read(gid).unwrap(); assert_eq!(publication.generation,setup.generation);
        let policy=publication.policy().unwrap(); let (runtime,_)=policy.derive(setup.employee,&kind(),true).unwrap();
        let backend=SpawndBackend::connect().await.unwrap(); let run=backend.reserve(setup.employee).await.unwrap();
        send(&mut observer,&Event::Reserved{run});
        backend.configure_pty(run,PtySize{rows:31,columns:113}).await.unwrap();
        backend.spawn(SpawnPolicy{run,employee:setup.employee,class:ExecutionClass::EmployeeMain,runtime:kind(),
            argv_allowlist:vec![],environment_allowlist:Default::default(),workspace:WorkspacePolicy{
                workspace:Text::new("employee-work".into()).unwrap(),relative_cwd:Text::new(String::new()).unwrap()},
            resources:runtime.resources.clone(),network:runtime.network.clone(),pty:true,
            capabilities:CapabilityHandover{mcp_bridge:false,egress_forwarder:false},timeouts:runtime.timeouts.clone(),
            inputs:RunInputs{prompt:RunInput{max_bytes:32,reader:Box::new(duduclaw_execution::io::Reader(std::io::Cursor::new(setup.nonce.as_bytes().to_vec())))},attachments:vec![]}
        }).await.unwrap();
        let mut io=loop{match backend.take_pty(run).await{Ok(io)=>break io,
            Err(BackendError::Unavailable)=>tokio::time::sleep(Duration::from_millis(20)).await,Err(error)=>panic!("PTY delivery: {error}")}};
        assert!(backend.take_pty(run).await.is_err());
        let mut pending = Vec::new();
        assert_eq!(line(&mut io, &mut pending).await,format!("ready:{}:31:113",setup.nonce));
        send(&mut observer,&Event::Ready); assert!(matches!(receive::<Event>(&mut observer),Event::Observed));
        match setup.mode.as_str(){
            "resize"=>{
                backend.resize(run,PtySize{rows:37,columns:93}).await.unwrap();
                assert_eq!(io.input.write(b"S").await.unwrap(),1); assert_eq!(line(&mut io, &mut pending).await,"size:37:93");
                assert_eq!(io.input.write(b"E").await.unwrap(),1);
                assert_eq!(backend.wait(run,WaitOrigin::Gateway).await.unwrap(),RunOutcome::Exited(0));
            }
            "idle"=>{
                let interval=Duration::from_millis(runtime.timeouts.pty_idle_ms.unwrap()/3);
                // Continue beyond the initial idle deadline using actual bytes.
                for _ in 0..4{tokio::time::sleep(interval).await; assert_eq!(io.input.write(b"P").await.unwrap(),1);
                    assert_eq!(line(&mut io, &mut pending).await,"pong"); assert_eq!(backend.status(run).await.unwrap(),RunStatus::Running);}
                assert_eq!(backend.wait(run,WaitOrigin::Gateway).await.unwrap(),RunOutcome::IdleTimeout);
            }
            "cancel"=>{backend.kill(run).await.unwrap(); assert_eq!(backend.wait(run,WaitOrigin::Gateway).await.unwrap(),RunOutcome::Cancelled);}
            _=>panic!("unknown acceptance mode"),
        }
        drop(io); backend.forget_finished(run).await.unwrap(); send(&mut observer,&Event::Finished);
    }).await.expect("native PTY chain deadline");});
}
