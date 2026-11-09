//! Installed A -> B1 -> B2 -> real Claude -> D transport acceptance.
//! The authority and the single echo callback are test-only. This deliberately
//! does NOT bypass or claim acceptance of production gateway boot/CLI factories.
#![cfg(target_os = "linux")]

use std::{ffi::OsString, fs, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd},
    unix::{fs::MetadataExt, net::UnixStream, process::CommandExt}}, path::{Path, PathBuf},
    process::{Child, Command, Stdio}, sync::{Arc, Mutex}, time::{Duration, Instant}};
use duduclaw_execution::{command::{NativeCommandSnapshot, ProtectedLaunch}, SpawndBackend};
use duduclaw_isolation::{bounded::{List, Text}, capability::{EmployeeBinding, Lane, Principal},
    execution::*, ids::{EmployeeId, Sha256Digest}, ipc::RunStatus};
use duduclaw_mcp_boundary::{Boundary, GrantPolicy, LiveAuthority, LiveSnapshot, RequestContext,
    VerificationError, protected_launch::ProtectedMcpLaunch, server::Dispatcher};
use duduclaw_spawnd::{gateway_mcp::InstalledRuntimeRegistry, installed::GatewayPublication};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CHILD_FD: i32 = 9;
const TOOL: &str = "acceptance_echo";
const DEADLINE: Duration = Duration::from_secs(180);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    employee: EmployeeId,
    uid: u32,
    generation: Sha256Digest,
    runtime_digest: Sha256Digest,
    bridge_digest: Sha256Digest,
    model: String,
    challenge: String,
    system_challenge: String,
    answer: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Event { Reserved { run: RunHandle }, ToolCalled { run: RunHandle }, Observed, Finished }

fn remaining() -> Duration {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    DEADLINE.checked_sub(START.get_or_init(Instant::now).elapsed())
        .filter(|remaining| !remaining.is_zero()).expect("whole acceptance protocol deadline")
}
fn send<T: Serialize>(socket: &mut UnixStream, value: &T) {
    socket.set_write_timeout(Some(remaining().min(Duration::from_secs(10)))).unwrap();
    let bytes = serde_json::to_vec(value).unwrap();
    assert!(bytes.len() <= 8192);
    socket.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
    socket.write_all(&bytes).unwrap();
}
fn receive<T: for<'de> Deserialize<'de>>(socket: &mut UnixStream) -> T {
    socket.set_read_timeout(Some(remaining())).unwrap();
    let mut size = [0; 4];
    socket.read_exact(&mut size).expect("acceptance child/observer timed out or disconnected");
    let size = u32::from_be_bytes(size) as usize;
    assert!(size > 0 && size <= 8192, "bounded acceptance protocol");
    let mut bytes = vec![0; size];
    socket.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    let mut source = fs::File::open("/dev/urandom").unwrap();
    source.read_exact(&mut bytes).unwrap();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn hash(path: impl AsRef<Path>) -> Sha256Digest {
    let mut file = fs::File::open(path).expect("read live executable by proc fd");
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
    }
    Sha256Digest(hasher.finalize().into())
}
fn setup(gid: u32) -> Setup {
    let publication = GatewayPublication::read(gid).expect("fresh installed signed root publication");
    let policy = publication.policy().unwrap();
    let employee = publication.entitlements.iter().find(|entry|
        policy.derive(entry.employee, &RuntimeKind::Claude, false).is_ok())
        .expect("confirmed employee entitled to one real MCP Claude runtime").employee;
    let (runtime, _) = policy.derive(employee, &RuntimeKind::Claude, false).unwrap();
    assert!(runtime.mcp);
    let runtime_digest = match runtime.executable {
        duduclaw_spawnd::policy::Executable::Elf { digest, .. } => digest,
        _ => panic!("real accepted Claude ELF is required"),
    };
    let record = publication.runtimes.iter().find(|entry| entry.profile == runtime.profile).unwrap();
    let acceptance = record.mcp_acceptance.as_ref().expect("signed real-runtime inheritance acceptance");
    let args: Vec<_> = record.fixed_args.iter().map(|value| value.as_str()).collect();
    let model = args.windows(2).find(|pair| pair[0] == "--model").map(|pair| pair[1]).unwrap_or("").to_owned();
    let registry_path = Path::new("/var/lib/duduclaw-spawnd/registry.json");
    let metadata = fs::symlink_metadata(registry_path).unwrap();
    assert!(metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0);
    let registry: duduclaw_spawnd::registry::Registry = serde_json::from_slice(&fs::read(registry_path).unwrap()).unwrap();
    let account = registry.lookup(employee).expect("ready permanent employee account");
    Setup { employee, uid: account.uid, generation: publication.generation, runtime_digest,
        bridge_digest: acceptance.bridge_digest, model, challenge: random_hex(), system_challenge: random_hex(), answer: random_hex() }
}

/// On panic, drop the gateway process/control connection before asking B1's
/// exact test-owned transient unit to terminate. Never kill another employee.
struct Cleanup { child: Child, run: Option<RunHandle>, cgroup: Option<PathBuf>, finished: bool }
impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.finished { let _ = self.child.kill(); }
        let child_deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < child_deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        if self.child.try_wait().ok().flatten().is_none() {
            eprintln!("gateway test child did not retire after SIGKILL");
        }
        if !self.finished {
            if let Some(run) = self.run {
                if let Ok(systemd) = duduclaw_spawnd::systemd::linux::Systemd::connect() {
                    let _ = systemd.kill_run(run.0);
                    if let Ok(unit) = systemd.inspect_run(run.0) {
                        if !unit.cgroup.as_os_str().is_empty() { self.cgroup = Some(unit.cgroup); }
                    }
                }
            }
        }
        if let Some(cgroup) = &self.cgroup {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                match fs::read_to_string(cgroup.join("cgroup.events")) {
                    Ok(events) if duduclaw_spawnd::systemd::cgroup_empty(&events).unwrap_or(false) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                    _ => { eprintln!("test-owned cgroup did not become empty after cleanup: {}", cgroup.display()); break; }
                }
            }
        }
    }
}

fn proc_status(pid: u32, uid: u32) -> Vec<PathBuf> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    for key in ["Uid:", "Gid:"] {
        let values: Vec<u32> = status.lines().find_map(|line| line.strip_prefix(key)).unwrap()
            .split_whitespace().map(|value| value.parse().unwrap()).collect();
        assert_eq!(values, vec![uid; 4], "runtime credential mismatch for {pid}");
    }
    for namespace in ["pid", "mnt", "net", "ipc", "uts"] {
        assert_ne!(fs::read_link(format!("/proc/{pid}/ns/{namespace}")).unwrap(),
            fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(), "shared host {namespace}");
    }
    // Enumerate every live fd as evidence; runtime-opened descriptors mean this
    // is not an assertion that a running vendor CLI retains only its exec set.
    let mut observed = Vec::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd")).unwrap() {
        let entry = entry.unwrap();
        if let Ok(target) = fs::read_link(entry.path()) {
            observed.push((entry.file_name(), target));
        }
    }
    observed.sort();
    eprintln!("observed employee pid={pid} uid={uid} descriptors={observed:?}");
    observed.into_iter().map(|(_, target)| target).collect()
}
fn observe(setup: &Setup, run: RunHandle) -> PathBuf {
    let systemd = duduclaw_spawnd::systemd::linux::Systemd::connect().unwrap();
    let unit = systemd.inspect_run(run.0).expect("actual transient unit exists");
    assert_eq!(unit.name, duduclaw_spawnd::systemd::run_unit(run.0));
    assert!(unit.cgroup.starts_with("/sys/fs/cgroup"));
    let pids: Vec<u32> = fs::read_to_string(unit.cgroup.join("cgroup.procs")).unwrap()
        .split_whitespace().map(|pid| pid.parse().unwrap()).collect();
    let mut claude = None;
    let mut bridge = None;
    for pid in pids {
        let path = format!("/proc/{pid}/exe");
        // Supervisors may disappear concurrently; the two held runtime
        // processes below are required and must remain observable.
        if fs::metadata(&path).is_err() { continue; }
        let digest = hash(&path);
        if digest == setup.runtime_digest { assert!(claude.replace(pid).is_none(), "ambiguous Claude process"); }
        if digest == setup.bridge_digest { assert!(bridge.replace(pid).is_none(), "ambiguous MCP bridge process"); }
    }
    let claude = claude.expect("the real signed Claude binary must be alive in this run cgroup");
    let bridge = bridge.expect("the signed MCP shim must be alive in this run cgroup");
    proc_status(claude, setup.uid);
    let bridge_fds = proc_status(bridge, setup.uid);
    let fd3 = fs::read_link(format!("/proc/{claude}/fd/3")).expect("Claude retains inherited MCP fd 3");
    assert!(fd3.to_str().unwrap().strip_prefix("socket:[").and_then(|value| value.strip_suffix(']'))
        .is_some_and(|value| value.parse::<u64>().is_ok()), "Claude fd 3 must be a socket");
    // The real shim duplicates fd 3 with CLOEXEC, closes the inherited slot,
    // then pumps two duplicates. Observe that same socket, not a hardcoded fd.
    assert!(bridge_fds.iter().filter(|target| **target == fd3).count() >= 2,
        "the live shim must retain duplicates of Claude's inherited socket");
    let prompt = fs::File::open(format!("/proc/{claude}/fd/4")).expect("sealed prompt fd 4");
    let seals = unsafe { libc::fcntl(prompt.as_raw_fd(), libc::F_GET_SEALS) };
    assert!(seals >= 0, "F_GET_SEALS must succeed");
    assert_eq!(seals & 15, 15, "prompt memfd remains sealed");
    let mut bytes = String::new();
    (&prompt).take(16384).read_to_string(&mut bytes).unwrap();
    assert!(bytes.contains(&setup.challenge));
    assert!(!bytes.contains(&setup.system_challenge), "system nonce must not leak into the user prompt");
    let mut system = fs::File::open(format!("/proc/{claude}/fd/5")).expect("system prompt attachment fd 5");
    let seals = unsafe { libc::fcntl(system.as_raw_fd(), libc::F_GET_SEALS) };
    assert!(seals >= 0 && seals & 15 == 15, "system prompt is sealed");
    let mut system_bytes = String::new();
    (&mut system).take(16384).read_to_string(&mut system_bytes).unwrap();
    assert!(system_bytes.contains(&setup.system_challenge));
    eprintln!("observed run={} cgroup={} Claude={claude} shim={bridge}",
        duduclaw_spawnd::systemd::run_unit(run.0), unit.cgroup.display());
    unit.cgroup
}

#[test]
#[ignore = "iso_root_: installed disposable Linux TEAM VM, signed real Claude and shim, live F/B1/H/egress receipts; consumes a model call; transport acceptance only"]
fn iso_root_e2e_claude_installed_shim_tool_transport() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
    let (uid, gid) = duduclaw_spawnd::server::gateway_identity().unwrap();
    let setup = setup(gid);
    // The harness owns this disposable employee for the entire test. Never
    // cancel a concurrent operator/other test run as incidental cleanup.
    for entry in fs::read_dir("/proc").unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()).is_none() { continue; }
        match fs::read_to_string(entry.path().join("status")) {
            Ok(status) => {
                let uid = status.lines().find_map(|line| line.strip_prefix("Uid:"))
                    .and_then(|values| values.split_whitespace().next()).and_then(|value| value.parse::<u32>().ok());
                assert_ne!(uid, Some(setup.uid), "acceptance employee must be idle before this test");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => panic!("cannot verify idle test employee: {error}"),
        }
    }
    let (mut observer, gateway) = UnixStream::pair().unwrap();
    observer.set_read_timeout(Some(DEADLINE)).unwrap();
    observer.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    let source = gateway.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--ignored", "--exact", "installed_claude_gateway_child", "--nocapture", "--test-threads=1"])
        .env_clear().env("DUDUCLAW_ISO_CLAUDE_CHILD", "1").env("DUDUCLAW_ISO_DISPOSABLE", "1")
        .stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    // SAFETY: child-only async-signal-safe descriptor/credential calls before
    // exec; no Rust/Tokio work runs between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(source, CHILD_FD) < 0 || libc::fcntl(CHILD_FD, libc::F_SETFD, 0) < 0
                || libc::setgroups(0, std::ptr::null()) != 0 || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(uid, uid, uid) != 0 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        });
    }
    let child = command.spawn().expect("execute transport harness as the real gateway uid");
    drop(gateway);
    let mut cleanup = Cleanup { child, run: None, cgroup: None, finished: false };
    send(&mut observer, &setup);
    let Event::Reserved { run } = receive(&mut observer) else { panic!("expected authenticated reservation"); };
    cleanup.run = Some(run);
    let Event::ToolCalled { run: actual } = receive(&mut observer) else { panic!("real Claude did not invoke D's test callback"); };
    assert_eq!(actual, run);
    cleanup.cgroup = Some(observe(&setup, run));
    send(&mut observer, &Event::Observed);
    assert!(matches!(receive::<Event>(&mut observer), Event::Finished));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = cleanup.child.try_wait().unwrap() {
            assert!(status.success(), "gateway transport child failed"); break;
        }
        assert!(Instant::now() < deadline, "gateway child did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
    let events = fs::read_to_string(cleanup.cgroup.as_ref().unwrap().join("cgroup.events"));
    assert!(match events { Ok(events) => duduclaw_spawnd::systemd::cgroup_empty(&events).unwrap(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound }, "Ended must mean empty run cgroup");
    cleanup.finished = true;
}

struct TestAuthority { backend: Arc<SpawndBackend>, employee: EmployeeId }
fn principal() -> Principal { Principal::Background { tenant_id: Text::new("transport-test-only".into()).unwrap(), generation: 1 } }
fn binding() -> EmployeeBinding { EmployeeBinding::Shared(Text::new("transport-test-only".into()).unwrap()) }
impl LiveAuthority for TestAuthority {
    fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError> {
        if employee != self.employee { return Err(VerificationError::BindingMismatch); }
        let (metadata, status) = self.backend.live_snapshot(run).map_err(|_| VerificationError::Unavailable)?;
        if metadata.employee != employee { return Err(VerificationError::BindingMismatch); }
        Ok(LiveSnapshot { employee, run, control: metadata.control, status,
            principal: principal(), binding: binding(), policy_revision: 1, agent_slug: "transport-test-only".into() })
    }
}
struct Echo { setup: Arc<Setup>, observer: Arc<Mutex<UnixStream>>, calls: Mutex<u32> }
impl Dispatcher for Echo {
    fn dispatch<'a>(&'a self, context: RequestContext, agent: &'a str, request: Value)
        -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            let record = context.record();
            if agent != "transport-test-only" || record.employee != self.setup.employee
                || record.principal != principal() || record.employee_binding != binding()
                || record.tools.as_slice().iter().map(|name| name.as_str()).collect::<Vec<_>>() != vec![TOOL] {
                return Err(VerificationError::BindingMismatch);
            }
            let method = request["method"].as_str().ok_or(VerificationError::Invalid)?;
            let result = match method {
                "initialize" => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},
                    "serverInfo":{"name":"isolation-transport-acceptance","version":"1"}}),
                "notifications/initialized" => return Ok(None),
                "ping" => json!({}),
                "tools/list" => json!({"tools":[{"name":TOOL,"description":"Return an acceptance nonce exactly once.",
                    "inputSchema":{"type":"object","properties":{"challenge":{"type":"string"},"system_challenge":{"type":"string"}},
                    "required":["challenge","system_challenge"],"additionalProperties":false}}]}),
                "tools/call" => {
                    if request["params"]["name"] != TOOL
                        || request["params"]["arguments"] != json!({"challenge":self.setup.challenge,"system_challenge":self.setup.system_challenge}) {
                        return Err(VerificationError::Invalid);
                    }
                    { let mut calls = self.calls.lock().unwrap();
                      if *calls != 0 { return Err(VerificationError::Invalid); } *calls += 1; }
                    let observer = self.observer.clone();
                    let run = RunHandle(record.run);
                    tokio::task::spawn_blocking(move || {
                        let mut socket = observer.lock().unwrap();
                        send(&mut socket, &Event::ToolCalled { run });
                        assert!(matches!(receive::<Event>(&mut socket), Event::Observed));
                    }).await.map_err(|_| VerificationError::Unavailable)?;
                    json!({"content":[{"type":"text","text":format!("{}:{}",self.setup.answer,self.setup.system_challenge)}],"isError":false})
                }
                _ => return Err(VerificationError::Invalid),
            };
            Ok(Some(json!({"jsonrpc":"2.0","id":request["id"],"result":result})))
        })
    }
}

async fn drain(mut source: Box<dyn RunReader>) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = source.read(&mut buffer).await.unwrap();
        if count == 0 { return result; }
        assert!(result.len() + count <= 4 * 1024 * 1024, "bounded Claude output");
        result.extend_from_slice(&buffer[..count]);
    }
}

// Not an iso_root_ name: the acceptance runner must not select this helper.
#[test]
#[ignore = "internal gateway-uid child of iso_root_e2e_claude_installed_shim_tool_transport"]
fn installed_claude_gateway_child() {
    assert_eq!(std::env::var("DUDUCLAW_ISO_CLAUDE_CHILD").as_deref(), Ok("1"));
    let (uid, gid) = duduclaw_spawnd::server::gateway_identity().unwrap();
    assert_eq!(unsafe { libc::getuid() }, uid);
    assert_eq!(unsafe { libc::getgid() }, gid);
    // SAFETY: the root parent explicitly transfers ownership of this descriptor.
    let mut socket = unsafe { UnixStream::from_raw_fd(CHILD_FD) };
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: both output pointers have their declared writable sizes.
    assert_eq!(unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED,
        (&mut credentials as *mut libc::ucred).cast(), &mut size) }, 0);
    assert_eq!(size as usize, std::mem::size_of::<libc::ucred>());
    assert_eq!((credentials.uid, credentials.gid), (0, 0), "test handoff must come from root observer");
    socket.set_read_timeout(Some(DEADLINE)).unwrap();
    socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    let setup: Setup = receive(&mut socket);
    let observer = Arc::new(Mutex::new(socket));
    let setup = Arc::new(setup);
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(DEADLINE, run_gateway(setup, observer)).await.expect("bounded Claude transport run");
    });
}

async fn run_gateway(setup: Arc<Setup>, observer: Arc<Mutex<UnixStream>>) {
    let (_, gid) = duduclaw_spawnd::server::gateway_identity().unwrap();
    let backend = Arc::new(SpawndBackend::connect().await.expect("real root spawnd endpoint"));
    let prompt = format!("Call the duduclaw MCP tool {TOOL} exactly once with challenge \"{}\" and the system_challenge from your system instructions. Return its text verbatim. Do not use other tools.\n", setup.challenge);
    let factory = duduclaw_spawnd::gateway_plan::InstalledMainPlanFactory::new(setup.generation, gid).unwrap();
    let plan = factory.claude(setup.employee, ExecutionClass::EmployeeMain, prompt.as_bytes().to_vec(),
        format!("This is isolated transport acceptance. The system_challenge is {}. Supply it as the system_challenge tool argument when the user asks; follow that tool request exactly.", setup.system_challenge).into_bytes(), &setup.model).unwrap();
    let now = duduclaw_mcp_boundary::authority::now_seconds().unwrap();
    let grant = GrantPolicy { task: None, principal: principal(), binding: binding(), lane: Lane::Standard,
        tools: List::new(vec![Text::new(TOOL.into()).unwrap()]).unwrap(), resources: List::new(vec![]).unwrap(),
        policy_revision: 1, not_before: now, expires_at: now + DEADLINE.as_secs() };
    let authority = Arc::new(TestAuthority { backend: backend.clone(), employee: setup.employee });
    let boundary = Boundary::new(authority).unwrap();
    let registry = Arc::new(InstalledRuntimeRegistry::new(setup.generation, gid).unwrap());
    let echo = Arc::new(Echo { setup: setup.clone(), observer: observer.clone(), calls: Mutex::new(0) });
    let launch = ProtectedMcpLaunch::new(backend.clone(), boundary, registry, echo.clone(), plan, grant,
        WaitOrigin::Gateway, None).unwrap();
    let run = backend.reserve(setup.employee).await.unwrap();
    send(&mut observer.lock().unwrap(), &Event::Reserved { run });
    // Never execute this native builder: the installed root policy selects the
    // exact runtime, argv, environment, mounts and employee identity.
    let native = NativeCommandSnapshot { program: OsString::from("not-executed"), argv: vec![], env: vec![], cwd: None };
    let operation = async {
        let policy = launch.prepare(run, ExecutionClass::EmployeeMain, RuntimeKind::Claude, &native, false).await.unwrap();
        backend.spawn(policy).await.unwrap();
        let mut io = loop {
            match backend.take_stdio(run).await {
                Ok(io) => break io,
                Err(BackendError::Unavailable) => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(error) => panic!("isolated run I/O: {error}"),
            }
        };
        launch.activate(run, Some(backend.take_mcp(run).await.unwrap())).await.unwrap();
        let stdout = tokio::spawn(drain(io.stdout));
        let stderr = tokio::spawn(drain(io.stderr));
        let mut remaining = prompt.as_bytes();
        while !remaining.is_empty() {
            let count = io.stdin.write(remaining).await.unwrap();
            assert!(count > 0); remaining = &remaining[count..];
        }
        io.stdin.close().await.unwrap();
        let outcome = backend.wait(run, WaitOrigin::Gateway).await.unwrap();
        assert_eq!(outcome, RunOutcome::Exited(0), "real Claude must exit successfully");
        let output = stdout.await.unwrap();
        let errors = stderr.await.unwrap();
        assert_eq!(*echo.calls.lock().unwrap(), 1, "one authenticated D tool call");
        assert!(String::from_utf8_lossy(&output).contains(&format!("{}:{}", setup.answer, setup.system_challenge)),
            "Claude output must contain the tool's unpredictable answer (stderr bytes: {})", errors.len());
        assert!(matches!(backend.status(run).await.unwrap(), RunStatus::Ended(_)));
    };
    let result = tokio::time::timeout(Duration::from_secs(165), operation).await;
    // Normal completion and explicit timeout both revoke before teardown. Panic
    // unwinding drops A's control sockets; root parent additionally kills this
    // exact unit and verifies its cgroup becomes empty.
    let _ = launch.revoke(run).await;
    let _ = backend.kill(run).await;
    let retired = tokio::time::timeout(Duration::from_secs(10), backend.wait(run, WaitOrigin::Gateway)).await;
    retired.expect("run retirement deadline").expect("root-confirmed terminal outcome");
    result.expect("Claude transport deadline");
    send(&mut observer.lock().unwrap(), &Event::Finished);
}
