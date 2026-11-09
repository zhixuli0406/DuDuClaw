//! Full A/B1/systemd/B2 D8 admission with a real double-forked descendant.
//! Requires an explicitly signed debug-only fixture runtime on a disposable VM.
//! Never manufactures signed policy, employee allocations, receipts or readiness.
#![cfg(all(target_os = "linux", feature = "root-test-fixture"))]

use std::{fs, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd, OwnedFd},
    unix::{fs::MetadataExt, net::UnixStream, process::CommandExt}}, path::Path,
    process::{Child, Command, Stdio}, sync::Arc, time::{Duration, Instant}};
use duduclaw_execution::SpawndBackend;
use duduclaw_isolation::{bounded::Text, execution::*, ids::{EmployeeId, Sha256Digest}, ipc::RunStatus};
use duduclaw_spawnd::{installed::{ExecutableRecord, GatewayPublication}, systemd::linux::{Systemd, StartedUnit}};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

const CHILD_FD: i32 = 9;
const FIXTURE: &str = "iso-d8-fixture-v1";
const DEADLINE: Duration = Duration::from_secs(120);
fn kind() -> RuntimeKind { RuntimeKind::Registered(Text::new(FIXTURE.into()).unwrap()) }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup { employee: EmployeeId, uid: u32, generation: Sha256Digest, executable: Sha256Digest, nonce: String }
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Event {
    Reserved { first: RunHandle }, FirstReady, SecondQueued { second: RunHandle }, InitStopped,
    MainExitRequested, CheckResidueQueue, ResidueStillQueued, InitReleased, SecondRunning, Observed, Finished,
}
fn remaining() -> Duration {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    DEADLINE.checked_sub(START.get_or_init(Instant::now).elapsed())
        .filter(|remaining| !remaining.is_zero()).expect("whole acceptance protocol deadline")
}
fn send<T: Serialize>(socket: &mut UnixStream, message: &T) {
    socket.set_write_timeout(Some(remaining().min(Duration::from_secs(10)))).unwrap();
    let bytes = serde_json::to_vec(message).unwrap(); assert!(bytes.len() <= 8192);
    socket.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap(); socket.write_all(&bytes).unwrap();
}
fn receive<T: for<'de> Deserialize<'de>>(socket: &mut UnixStream) -> T {
    socket.set_read_timeout(Some(remaining())).unwrap();
    let mut size = [0; 4]; socket.read_exact(&mut size).expect("D8 observer/child timeout or EOF");
    let size = u32::from_be_bytes(size) as usize; assert!(size > 0 && size <= 8192);
    let mut bytes = vec![0; size]; socket.read_exact(&mut bytes).unwrap(); serde_json::from_slice(&bytes).unwrap()
}
fn sha(path: impl AsRef<Path>) -> Sha256Digest {
    let mut source = fs::File::open(path).unwrap(); let mut hasher = Sha256::new(); let mut bytes = [0; 65536];
    loop { let count = source.read(&mut bytes).unwrap(); if count == 0 { break; } hasher.update(&bytes[..count]); }
    Sha256Digest(hasher.finalize().into())
}
fn setup(gid: u32) -> Setup {
    let publication = GatewayPublication::read(gid).expect("fresh installed root publication");
    let policy = publication.policy().unwrap();
    let employee = publication.entitlements.iter().find(|entry| policy.derive(entry.employee, &kind(), false).is_ok())
        .expect("explicit employee entitlement for signed iso-d8-fixture-v1").employee;
    let (runtime, _) = policy.derive(employee, &kind(), false).unwrap();
    assert!(!runtime.mcp && !runtime.pty_allowed && runtime.fixed_args.is_empty());
    assert_eq!(runtime.network, NetworkPolicy::Offline);
    assert!(runtime.timeouts.queue_ms >= 30_000 && runtime.timeouts.runtime_ms >= 60_000);
    let record = publication.runtimes.iter().find(|entry| entry.profile == runtime.profile).unwrap();
    let ExecutableRecord::Elf { digest, .. } = record.executable else { panic!("fixture must be signed ELF"); };
    assert_eq!(digest, sha(env!("CARGO_BIN_EXE_duduclaw-spawnd-d8-fixture")), "exact current fixture ELF must be signed/installed");
    let path = Path::new("/var/lib/duduclaw-spawnd/registry.json");
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0);
    let registry: duduclaw_spawnd::registry::Registry = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let uid = registry.lookup(employee).unwrap().uid;
    let mut nonce = [0; 16]; fs::File::open("/dev/urandom").unwrap().read_exact(&mut nonce).unwrap();
    Setup { employee, uid, generation: publication.generation, executable: digest,
        nonce: nonce.iter().map(|byte| format!("{byte:02x}")).collect() }
}
fn status(pid: u32) -> String { fs::read_to_string(format!("/proc/{pid}/status")).unwrap() }
fn field<'a>(status: &'a str, name: &str) -> &'a str {
    status.lines().find_map(|line| line.strip_prefix(name)).expect("required proc status field").trim()
}
fn state(pid: u32) -> char { field(&status(pid), "State:").chars().next().unwrap() }
fn pidfd(pid: u32) -> OwnedFd {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(fd >= 0, "bind observed process identity before signalling");
    unsafe { OwnedFd::from_raw_fd(fd as i32) }
}
fn signal(fd: &OwnedFd, value: i32) {
    assert_eq!(unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), value,
        std::ptr::null::<libc::siginfo_t>(), 0) }, 0, "signal exact observed process");
}
fn exited(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    assert!(unsafe { libc::poll(&mut poll, 1, 0) } >= 0);
    poll.revents & libc::POLLIN != 0
}
fn until(mut check: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() { assert!(Instant::now() < deadline, "{message}"); std::thread::sleep(Duration::from_millis(20)); }
}
fn unit_pids(systemd: &Systemd, run: RunHandle) -> (StartedUnit, Vec<u32>) {
    let unit = systemd.inspect_run(run.0).unwrap();
    assert_eq!(unit.name, duduclaw_spawnd::systemd::run_unit(run.0));
    assert!(unit.cgroup.starts_with("/sys/fs/cgroup"));
    let pids = fs::read_to_string(unit.cgroup.join("cgroup.procs")).unwrap()
        .split_whitespace().map(|value| value.parse().unwrap()).collect();
    (unit, pids)
}
fn empty(path: &Path) -> bool {
    match fs::read_to_string(path.join("cgroup.events")) {
        Ok(events) => duduclaw_spawnd::systemd::cgroup_empty(&events).unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => panic!("cgroup readback failed: {error}"),
    }
}
fn employee_process(pid: u32, setup: &Setup) {
    let status = status(pid);
    for key in ["Uid:", "Gid:"] {
        assert_eq!(field(&status, key).split_whitespace().map(|value| value.parse::<u32>().unwrap()).collect::<Vec<_>>(), vec![setup.uid; 4]);
    }
    for namespace in ["pid", "mnt", "net", "ipc", "uts"] {
        assert_ne!(fs::read_link(format!("/proc/{pid}/ns/{namespace}")).unwrap(), fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap());
    }
}
struct Cleanup { child: Child, runs: Vec<RunHandle>, held_init: Option<OwnedFd>, finished: bool }
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(init) = self.held_init.take() {
            unsafe { libc::syscall(libc::SYS_pidfd_send_signal, init.as_raw_fd(), libc::SIGCONT, std::ptr::null::<libc::siginfo_t>(), 0); }
        }
        if !self.finished { let _ = self.child.kill(); }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(25)); }
        if !self.finished {
            if let Ok(systemd) = Systemd::connect() {
                for run in &self.runs { let _ = systemd.kill_run(run.0); }
                for run in &self.runs {
                    if let Ok(unit) = systemd.inspect_run(run.0) {
                        let deadline = Instant::now() + Duration::from_secs(15);
                        loop {
                            match systemd.cleanup(&unit) {
                                Ok((true, true)) => break,
                                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                                _ => { eprintln!("D8 cleanup not proven for {}", unit.name); break; }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "iso_root_: installed disposable root Linux VM; signed debug D8 fixture, confirmed idle employee, real B1/F/H receipts; two isolated runs"]
fn iso_root_e2e_d8_double_fork_residue_blocks_second_run() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
    let (uid, gid) = duduclaw_spawnd::server::gateway_identity().unwrap();
    let setup = setup(gid);
    for entry in fs::read_dir("/proc").unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_str().and_then(|value| value.parse::<u32>().ok()).is_none() { continue; }
        match fs::read_to_string(entry.path().join("status")) {
            Ok(status) => assert_ne!(field(&status, "Uid:").split_whitespace().next().unwrap().parse::<u32>().unwrap(), setup.uid,
                "test employee must be idle; do not cancel another session's work"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => panic!("idle employee readback: {error}"),
        }
    }
    let (mut observer, gateway) = UnixStream::pair().unwrap();
    observer.set_read_timeout(Some(DEADLINE)).unwrap(); observer.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    let fd = gateway.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--ignored", "--exact", "installed_d8_gateway_child", "--nocapture", "--test-threads=1"])
        .env_clear().env("DUDUCLAW_ISO_D8_CHILD", "1").stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    // SAFETY: only async-signal-safe calls in the pre-exec child.
    unsafe { command.pre_exec(move || {
        if libc::dup2(fd, CHILD_FD) < 0 || libc::fcntl(CHILD_FD, libc::F_SETFD, 0) < 0
            || libc::setgroups(0, std::ptr::null()) != 0 || libc::setresgid(gid, gid, gid) != 0
            || libc::setresuid(uid, uid, uid) != 0 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    let child = command.spawn().unwrap(); drop(gateway);
    let mut cleanup = Cleanup { child, runs: vec![], held_init: None, finished: false };
    send(&mut observer, &setup);
    let Event::Reserved { first } = receive(&mut observer) else { panic!("first reservation"); };
    cleanup.runs.push(first);
    assert!(matches!(receive::<Event>(&mut observer), Event::FirstReady));
    let Event::SecondQueued { second } = receive(&mut observer) else { panic!("second run must queue"); };
    cleanup.runs.push(second);
    // Hold the systemd reference across both runs. Therefore absence is not
    // accepted as proof: first-unit ActiveState must actually be inactive.
    let systemd = Systemd::connect().unwrap();
    let (first_unit, pids) = unit_pids(&systemd, first);
    let first_cgroup = first_unit.cgroup.clone();
    let mut main = None; let mut orphan = None; let mut init = None;
    for pid in pids {
        let status = status(pid);
        if field(&status, "NSpid:").split_whitespace().last() == Some("1") { init = Some(pid); }
        match field(&status, "Name:") { "iso-d8-main" => main = Some(pid), "iso-d8-orphan" => orphan = Some(pid), _ => {} }
    }
    let (main, orphan, init) = (main.expect("real fixture main"), orphan.expect("real double-fork orphan"), init.expect("run namespace init"));
    for pid in [main, orphan, init] { employee_process(pid, &setup); }
    for pid in [main, orphan] { assert_eq!(sha(format!("/proc/{pid}/exe")), setup.executable); }
    assert_eq!(field(&status(orphan), "PPid:").parse::<u32>().unwrap(), init, "grandchild really orphaned/adopted by PID1");
    let main_fd = pidfd(main); let orphan_fd = pidfd(orphan); let init_fd = pidfd(init);
    signal(&init_fd, libc::SIGSTOP); cleanup.held_init = Some(init_fd);
    until(|| state(init) == 'T', "namespace init must be stopped before main exits");
    send(&mut observer, &Event::InitStopped);
    assert!(matches!(receive::<Event>(&mut observer), Event::MainExitRequested));
    until(|| exited(&main_fd), "fixture main must actually exit");
    assert_eq!(state(main), 'Z', "stopped init has not reaped the exited main");
    assert!(!exited(&orphan_fd), "double-fork residue remains alive after main exit");
    assert!(!empty(&first_cgroup));
    send(&mut observer, &Event::CheckResidueQueue);
    assert!(matches!(receive::<Event>(&mut observer), Event::ResidueStillQueued));
    assert!(!exited(&orphan_fd) && !empty(&first_cgroup), "residue survived the whole queue observation interval");
    signal(cleanup.held_init.as_ref().unwrap(), libc::SIGCONT); cleanup.held_init.take();
    send(&mut observer, &Event::InitReleased);
    assert!(matches!(receive::<Event>(&mut observer), Event::SecondRunning));
    assert!(exited(&orphan_fd), "first run orphan must be dead when the second process is observed");
    assert!(empty(&first_cgroup), "first cgroup must be empty when the second process is observed");
    assert_eq!(systemd.cleanup(&first_unit).expect("fresh first-unit inactive/cgroup readback"), (true, true),
        "all first-unit teardown evidence must hold when the second process is observed");
    let (second_unit, pids) = unit_pids(&systemd, second);
    let second_cgroup = second_unit.cgroup.clone();
    // The remaining UID processes must all belong to this second run, rather
    // than surviving elsewhere after the first cgroup was emptied.
    for entry in fs::read_dir("/proc").unwrap() {
        let entry = entry.unwrap();
        let Some(pid) = entry.file_name().to_str().and_then(|value| value.parse::<u32>().ok()) else { continue; };
        match fs::read_to_string(entry.path().join("status")) {
            Ok(status) => {
                let values: Vec<u32> = field(&status, "Uid:").split_whitespace().map(|value| value.parse().unwrap()).collect();
                assert_eq!(values.len(), 4);
                if values.contains(&setup.uid) { assert!(pids.contains(&pid), "employee UID residue outside second run: {pid}"); }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => panic!("complete employee UID readback failed: {error}"),
        }
    }
    let second_pid = pids.into_iter().find(|pid| field(&status(*pid), "Name:") == "iso-d8-second").expect("second isolated fixture alive");
    employee_process(second_pid, &setup); assert_eq!(sha(format!("/proc/{second_pid}/exe")), setup.executable);
    eprintln!("D8 proved first={} main={main} orphan={orphan} init={init}; second={} pid={second_pid}", first_cgroup.display(), second_cgroup.display());
    send(&mut observer, &Event::Observed);
    assert!(matches!(receive::<Event>(&mut observer), Event::Finished));
    until(|| cleanup.child.try_wait().unwrap().is_some(), "gateway helper exit");
    assert!(cleanup.child.try_wait().unwrap().unwrap().success());
    assert_eq!(systemd.cleanup(&second_unit).expect("second-unit retirement readback"), (true, true));
    assert!(empty(&second_cgroup)); cleanup.finished = true;
}

fn plan(setup: &Setup, gid: u32, run: RunHandle, role: &str) -> SpawnPolicy {
    let publication = GatewayPublication::read(gid).unwrap(); assert_eq!(publication.generation, setup.generation);
    let policy = publication.policy().unwrap(); let (runtime, _) = policy.derive(setup.employee, &kind(), false).unwrap();
    assert!(!runtime.mcp && runtime.fixed_args.is_empty() && runtime.network == NetworkPolicy::Offline);
    let prompt = serde_json::to_vec(&json!({"role":role,"nonce":setup.nonce})).unwrap();
    SpawnPolicy { workspace_delivery: None, run, employee: setup.employee, class: ExecutionClass::EmployeeMain, runtime: kind(),
        argv_allowlist: vec![], environment_allowlist: Default::default(),
        workspace: WorkspacePolicy { workspace: Text::new("employee-work".into()).unwrap(), relative_cwd: Text::new(String::new()).unwrap() },
        resources: runtime.resources.clone(), network: runtime.network.clone(), pty: false,
        capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: false }, timeouts: runtime.timeouts.clone(),
        inputs: RunInputs { prompt: RunInput { max_bytes: prompt.len() as u64,
            reader: Box::new(duduclaw_execution::io::Reader(std::io::Cursor::new(prompt))) }, attachments: vec![] } }
}
async fn io(backend: &SpawndBackend, run: RunHandle) -> RunStdio {
    loop { match backend.take_stdio(run).await { Ok(io) => return io,
        Err(BackendError::Unavailable) => tokio::time::sleep(Duration::from_millis(20)).await,
        Err(error) => panic!("D8 isolated I/O: {error}"), } }
}
async fn ready(io: &mut RunStdio, role: &str, nonce: &str) {
    let mut bytes = Vec::new(); let mut byte = [0; 1];
    loop { assert!(bytes.len() < 4096); assert_eq!(io.stdout.read(&mut byte).await.unwrap(), 1);
        if byte[0] == b'\n' { break; } bytes.push(byte[0]); }
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(), json!({"ready":role,"nonce":nonce}));
}
async fn queued(backend: &SpawndBackend, first: RunHandle, second: RunHandle) {
    let RunStatus::Queued(status) = backend.status(second).await.unwrap() else { panic!("same employee started before prior cgroup drained"); };
    assert!(status.deferred_until_current_run_ends);
    assert!(matches!(backend.take_stdio(second).await, Err(BackendError::Unavailable)));
    assert_eq!(backend.wait(second, WaitOrigin::Run(first)).await, Err(BackendError::WouldDeadlock));
}

// Not selected independently by the root runner's iso_root_ filter.
#[test]
#[ignore = "internal gateway-uid helper; only execute through the root parent"]
fn installed_d8_gateway_child() {
    assert_eq!(std::env::var("DUDUCLAW_ISO_D8_CHILD").as_deref(), Ok("1"));
    let (uid, gid) = duduclaw_spawnd::server::gateway_identity().unwrap();
    assert_eq!(unsafe { libc::getuid() }, uid); assert_eq!(unsafe { libc::getgid() }, gid);
    let mut observer = unsafe { UnixStream::from_raw_fd(CHILD_FD) };
    observer.set_read_timeout(Some(DEADLINE)).unwrap(); observer.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() }; let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    assert_eq!(unsafe { libc::getsockopt(observer.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut credentials as *mut libc::ucred).cast(), &mut length) }, 0);
    assert_eq!(length as usize, std::mem::size_of::<libc::ucred>()); assert_eq!((credentials.uid, credentials.gid), (0, 0));
    let setup: Setup = receive(&mut observer);
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    runtime.block_on(async { tokio::time::timeout(DEADLINE, async {
        let backend = Arc::new(SpawndBackend::connect().await.unwrap());
        let first = backend.reserve(setup.employee).await.unwrap(); send(&mut observer, &Event::Reserved { first });
        backend.spawn(plan(&setup, gid, first, "first")).await.unwrap();
        let mut first_io = io(&backend, first).await; ready(&mut first_io, "first", &setup.nonce).await;
        send(&mut observer, &Event::FirstReady);
        let second = backend.reserve(setup.employee).await.unwrap();
        backend.spawn(plan(&setup, gid, second, "second")).await.unwrap(); queued(&backend, first, second).await;
        send(&mut observer, &Event::SecondQueued { second });
        assert!(matches!(receive::<Event>(&mut observer), Event::InitStopped));
        assert_eq!(first_io.stdin.write(b"E").await.unwrap(), 1); first_io.stdin.close().await.unwrap();
        send(&mut observer, &Event::MainExitRequested);
        assert!(matches!(receive::<Event>(&mut observer), Event::CheckResidueQueue));
        for _ in 0..20 { queued(&backend, first, second).await; tokio::time::sleep(Duration::from_millis(50)).await; }
        send(&mut observer, &Event::ResidueStillQueued);
        assert!(matches!(receive::<Event>(&mut observer), Event::InitReleased));
        assert_eq!(backend.wait(first, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Exited(0));
        let mut second_io = io(&backend, second).await; ready(&mut second_io, "second", &setup.nonce).await;
        send(&mut observer, &Event::SecondRunning);
        assert!(matches!(receive::<Event>(&mut observer), Event::Observed));
        assert_eq!(second_io.stdin.write(b"E").await.unwrap(), 1); second_io.stdin.close().await.unwrap();
        assert_eq!(backend.wait(second, WaitOrigin::Gateway).await.unwrap(), RunOutcome::Exited(0));
        send(&mut observer, &Event::Finished);
    }).await.expect("D8 acceptance deadline"); });
}
