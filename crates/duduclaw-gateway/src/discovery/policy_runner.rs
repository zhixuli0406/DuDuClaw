//! Confined Python policies. Only the trusted shim and source snapshot cross
//! the process boundary; questions expose the prefix-only protocol surface.
use std::io::{BufReader, Read, Write};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, Ordering}};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};

use super::contracts::{IsolationBackend, PolicyDegraded, PolicySource, PolicyVersion};
use super::eval::{ReplayConfig, run_point, score_point, plan_out_of_support};
use super::isolation::{ConfinementSpec, confine_command};
use super::policy::{BaselineParallelRefine, ExplorationPolicy, GridContext, GridPlan,
    PolicyConfig, PolicyError, Question, BASELINE_POLICY_ID};
use super::protocol::{ServeLimits, request_plan_grid, serve_question};
use super::score::{BETA_GRID, WorldScore, round9};
use super::tree::WorldTree;

pub const BASELINE_SOURCE: &str = include_str!("python/baseline.py");
const AST: &str = include_str!("python/policy_ast.py");
const SHIM: &str = include_str!("python/policy_shim.py");
const MAX_SOURCE: usize = 256 * 1024;
const EPISODE_TIMEOUT: Duration = Duration::from_secs(10);
// A trusted PID-1 supervisor enforces wall time independently of the Docker
// client/daemon connection. Policy code runs in a separate process group;
// normal exit also removes descendants before the container can exit.
const CONTAINER_SUPERVISOR: &str = "import os,sys,subprocess,signal,time,math\ndeadline=float(os.environ['DUDU_POLICY_DEADLINE_UNIX'])\nlimit=float(os.environ['DUDU_POLICY_MAX_WALL'])\nremaining=min(limit,deadline-time.time())\nif not math.isfinite(deadline) or not math.isfinite(limit) or remaining<=0: sys.exit(124)\np=subprocess.Popen(sys.argv[1:],start_new_session=True)\ntry:\n try: code=p.wait(timeout=max(0,min(limit,deadline-time.time())))\n except subprocess.TimeoutExpired: code=124\nfinally:\n try: os.killpg(p.pid,signal.SIGKILL)\n except ProcessLookupError: pass\n if p.poll() is None: p.wait()\nsys.exit(code)";

#[derive(Debug, Clone)]
pub struct PythonPolicyRuntime {
    python: PathBuf,
    runtime_root: PathBuf,
    container: Option<ContainerBackend>,
    scope: Option<PolicyScope>,
}

#[derive(Debug, Clone)]
struct PolicyScope { home: PathBuf, run_id: String, budget: super::budget::SharedBudget,
    quota: super::attempt_container::QuotaLimits }

const POLICY_IMAGE: &str = "python:3.12-alpine";
#[derive(Debug, Clone)]
struct DockerClient { executable: PathBuf, env: Vec<(OsString, OsString)> }
impl DockerClient {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear().envs(self.env.iter().cloned());
        command
    }
}
#[derive(Debug, Clone)]
struct ContainerBackend { client: DockerClient, image: String }
struct Launch { command: Command, create: Option<Command>, cleanup: Option<Arc<ContainerCleanup>>, budget: Option<super::budget::SharedBudget> }
impl From<Command> for Launch {
    fn from(command: Command) -> Self { Self { command, create: None, cleanup: None, budget: None } }
}

/// Removing the daemon-owned container is mandatory even when the attached
/// Docker client exits successfully. Cleanup itself is bounded and reaped.
struct ContainerCleanup {
    client: DockerClient,
    name: String,
    stopped: Mutex<Option<Result<(), PolicyDegraded>>>,
    scope: Option<PolicyScope>,
}
impl ContainerCleanup {
    fn stop(&self) -> Result<(), PolicyDegraded> {
        let mut stopped = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = &*stopped { return result.clone(); }
        let mut command = self.client.command();
        command.args(["rm", "-f", &self.name]);
        let result = control_output(command, Duration::from_secs(5)).map(|_| ())
            .or_else(|reason| {
                // --rm may already have removed a normally exited container.
                if reason.to_string().contains("No such container") { Ok(()) }
                else { Err(rejected(format!("container_cleanup:{reason}"))) }
            });
        *stopped = Some(result.clone());
        if let (Err(reason), Some(scope)) = (&result, &self.scope) {
            let _ = super::maintenance::record_cleanup_failure(&scope.home, &scope.run_id, "policy", &reason.to_string());
            scope.budget.cancel();
        }
        result
    }
}
fn control_output(command: Command, timeout: Duration) -> Result<Vec<u8>, PolicyDegraded> {
    control_launch_output(command.into(), timeout)
}
fn control_launch_output(launch: Launch, timeout: Duration) -> Result<Vec<u8>, PolicyDegraded> {
    let mut session = Session::spawn(launch, timeout)?;
    session.child.stdin.take();
    let mut out = Vec::new();
    let result = session.child.stdout.take().unwrap().take(64 * 1024)
        .read_to_end(&mut out).map_err(io_rejected);
    session.complete(result)?;
    Ok(out)
}

fn rejected(reason: impl Into<String>) -> PolicyDegraded {
    PolicyDegraded::Rejected(reason.into())
}
fn io_rejected(e: std::io::Error) -> PolicyDegraded { rejected(format!("io:{e}")) }

/// Owns the child and every reader/watchdog thread. Drop kills the entire
/// group, reaps the leader, and joins threads even during host unwinding.
struct Session {
    child: Child,
    timed_out: Arc<AtomicBool>,
    finished: Arc<(Mutex<bool>, Condvar)>,
    watchdog: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    reaped: bool,
    cleanup: Option<Arc<ContainerCleanup>>,
    budget: Option<super::budget::SharedBudget>,
}
fn kill_group(pid: u32) {
    #[cfg(unix)]
    { let _ = duduclaw_core::platform::kill_process_group(pid); }
    #[cfg(windows)]
    { let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output(); }
}
impl Session {
    #[cfg(test)]
    fn spawn_scoped(mut launch: Launch, timeout: Duration, budget: super::budget::SharedBudget)
        -> Result<Self, PolicyDegraded> {
        launch.budget = Some(budget);
        Self::spawn(launch, timeout)
    }
    fn spawn(launch: Launch, timeout: Duration) -> Result<Self, PolicyDegraded> {
        if timeout.is_zero() { return Err(rejected("timeout")); }
        let started = Instant::now();
        let Launch { mut command, create, cleanup, budget } = launch;
        let timeout = budget.as_ref().map_or(timeout, |budget| timeout.min(budget.remaining_wall()));
        if timeout.is_zero() || budget.as_ref().is_some_and(|budget| budget.is_cancelled()) {
            return Err(rejected("cancelled_or_deadline"));
        }
        if let Some(create) = create {
            let absolute_deadline = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| rejected(e.to_string()))?.as_secs_f64() + timeout.as_secs_f64();
            // An unconfirmed create never executes the policy. This removes
            // the run/client-cancellation race where a late daemon response
            // could otherwise start code after the cleanup had found no name.
            let mut bounded_create = Command::new(create.get_program());
            bounded_create.env_clear();
            for (key, value) in create.get_envs() {
                if let Some(value) = value { bounded_create.env(key, value); }
            }
            for arg in create.get_args() {
                if arg == "DUDU_POLICY_MAX_WALL=10" {
                    bounded_create.arg(format!("DUDU_POLICY_MAX_WALL={}", timeout.as_secs_f64()));
                } else if arg == "DUDU_POLICY_DEADLINE_UNIX=0" {
                    bounded_create.arg(format!("DUDU_POLICY_DEADLINE_UNIX={absolute_deadline}"));
                } else { bounded_create.arg(arg); }
            }
            let mut launch: Launch = bounded_create.into();
            launch.budget = budget.clone();
            if let Err(reason) = control_launch_output(launch, timeout) {
                if let Some(cleanup) = &cleanup { let _ = cleanup.stop(); }
                return Err(reason);
            }
        }
        let timeout = timeout.saturating_sub(started.elapsed());
        if timeout.is_zero() {
            if let Some(cleanup) = &cleanup { let _ = cleanup.stop(); }
            return Err(rejected("timeout"));
        }
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        { use std::os::unix::process::CommandExt; command.process_group(0); }
        let mut child = command.spawn().map_err(|reason| {
            if let Some(cleanup) = &cleanup { let _ = cleanup.stop(); }
            io_rejected(reason)
        })?;
        let pid = child.id();
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let timed_out = Arc::new(AtomicBool::new(false));
        let signal = finished.clone();
        let expired = timed_out.clone();
        let deadline_cleanup = cleanup.clone();
        let watchdog_budget = budget.clone();
        let watchdog = std::thread::spawn(move || {
            let (lock, cv) = &*signal;
            let deadline = Instant::now() + timeout;
            let mut completed = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*completed && Instant::now() < deadline
                && !watchdog_budget.as_ref().is_some_and(|budget| budget.is_cancelled()) {
                let wait = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(20));
                completed = cv.wait_timeout(completed, wait).unwrap_or_else(|e|e.into_inner()).0;
            }
            if !*completed {
                expired.store(true, Ordering::SeqCst);
                kill_group(pid);
                if let Some(cleanup) = deadline_cleanup { let _ = cleanup.stop(); }
            }
        });
        let stderr = child.stderr.take().expect("piped stderr");
        let stderr = std::thread::spawn(move || {
            let mut stderr = stderr;
            let mut out = Vec::new();
            let mut buf = [0; 8192];
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 { break; }
                out.extend_from_slice(&buf[..n.min((64 * 1024usize).saturating_sub(out.len()))]);
            }
            out
        });
        Ok(Self { child, finished, timed_out, watchdog: Some(watchdog), stderr: Some(stderr), reaped: false, cleanup, budget })
    }
    fn complete<T>(&mut self, result: Result<T, PolicyDegraded>) -> Result<T, PolicyDegraded> {
        self.child.stdin.take();
        if result.is_err() {
            kill_group(self.child.id()); let _ = self.child.kill();
            if let Some(cleanup) = &self.cleanup { let _ = cleanup.stop(); }
        }
        let status = self.child.wait().map_err(io_rejected)?;
        self.reaped = true;
        // Kill descendants before joining pipe readers, including on success.
        kill_group(self.child.id());
        *self.finished.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.finished.1.notify_all();
        if let Some(cleanup) = &self.cleanup { cleanup.stop()?; }
        if self.budget.as_ref().is_some_and(|budget| budget.is_cancelled()) { return Err(rejected("cancelled")); }
        if self.timed_out.load(Ordering::SeqCst) { return Err(rejected("timeout")); }
        let ended_without_reply = result.as_ref().err().is_some_and(|e|
            e.to_string().contains("stream ended without"));
        if result.is_err() && !ended_without_reply { return result; }
        if !status.success() {
            let stderr = self.stderr.take().and_then(|h| h.join().ok()).unwrap_or_default();
            return Err(rejected(format!("policy_exception:{}", duduclaw_core::truncate_bytes(
                &String::from_utf8_lossy(&stderr), 500))));
        }
        result
    }
}

#[cfg(all(test, unix))]
mod scoped_cancellation_tests {
    use super::*;
    #[test]
    fn policy_pid1_refuses_a_late_start_after_the_absolute_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("executed");
        let code = format!("from pathlib import Path; Path({}).write_text('bad')",
            serde_json::to_string(&marker.to_string_lossy()).unwrap());
        let result = Command::new("/usr/bin/python3").env_clear()
            .env("PATH", "/usr/bin:/bin").env("DUDU_POLICY_MAX_WALL", "1")
            .env("DUDU_POLICY_DEADLINE_UNIX", "0")
            .args(["-I", "-S", "-c", CONTAINER_SUPERVISOR, "/usr/bin/python3", "-I", "-S", "-c", &code])
            .output().unwrap();
        assert_eq!(result.status.code(), Some(124), "deadline must be checked before untrusted code starts");
        assert!(!marker.exists(), "a late daemon start must not execute policy code");
    }
    #[test]
    fn scoped_default_beta_cannot_stage_source_outside_the_run_quota() {
        let home = tempfile::tempdir().unwrap();
        let mut runtime = PythonPolicyRuntime::experimental_native_for_tests().unwrap();
        runtime.scope = Some(PolicyScope { home: home.path().canonicalize().unwrap(), run_id: "quota-run".into(),
            budget: super::super::budget::SharedBudget::new(super::super::contracts::RunBudget {
                max_agent_calls: 1, max_usd: 1.0, max_wall_secs: 10, max_rounds: 1 }).unwrap(),
            quota: super::super::attempt_container::QuotaLimits { max_run_bytes: 1, max_total_bytes: 1 } });
        assert!(runtime.default_beta(BASELINE_SOURCE, Duration::from_secs(1)).is_err(),
            "formal policy source bundles must be counted by the configured run quota");
    }
    #[test]
    fn cancellation_terminates_a_running_policy_without_waiting_for_episode_timeout() {
        let budget = super::super::budget::SharedBudget::new(super::super::contracts::RunBudget {
            max_agent_calls: 1, max_usd: 1.0, max_wall_secs: 10, max_rounds: 1,
        }).unwrap();
        let cancel = budget.clone();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 3; printf done"]);
        let started = Instant::now();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        let mut session = Session::spawn_scoped(command.into(), Duration::from_secs(2), budget).unwrap();
        session.child.stdin.take();
        let mut output = Vec::new();
        let read = session.child.stdout.take().unwrap().read_to_end(&mut output).map_err(io_rejected);
        assert!(session.complete(read).is_err());
        drop(session);
        thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_millis(700), "shared cancellation must interrupt policy I/O");
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Mark finished under the same lock the watchdog uses before reaping,
        // preventing it from signalling a PID that has already been reused.
        *self.finished.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.finished.1.notify_all();
        if !self.reaped {
            kill_group(self.child.id());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(cleanup) = &self.cleanup { let _ = cleanup.stop(); }
        if let Some(thread) = self.watchdog.take() { let _ = thread.join(); }
        if let Some(thread) = self.stderr.take() { let _ = thread.join(); }
    }
}

impl PythonPolicyRuntime {
    #[cfg(test)]
    pub(super) fn detect_container_for_tests(image: &str) -> Result<Self, PolicyDegraded> {
        Self::detect_container(image)
    }
    #[cfg(test)]
    pub(super) fn run_test_code(&self, code: &str) -> Result<String, PolicyDegraded> {
        let command = self.command(None, 1, ["-c", code])?;
        let mut session = Session::spawn(command, EPISODE_TIMEOUT)?;
        session.child.stdin.take();
        let mut out = Vec::new();
        let result = session.child.stdout.take().unwrap().take(64 * 1024)
            .read_to_end(&mut out).map(|_| ()).map_err(io_rejected);
        session.complete(result)?;
        String::from_utf8(out).map_err(|e| rejected(e.to_string()))
    }
    pub fn detect() -> Result<Self, PolicyDegraded> {
        Self::detect_container(POLICY_IMAGE)
    }

    pub fn detect_scoped(home: &Path, run_id: &str, budget: super::budget::SharedBudget)
        -> Result<Self, PolicyDegraded> {
        Self::detect_scoped_with_quota(home, run_id, budget, super::attempt_container::QuotaLimits::default())
    }

    pub fn detect_scoped_with_quota(home: &Path, run_id: &str, budget: super::budget::SharedBudget,
        quota: super::attempt_container::QuotaLimits) -> Result<Self, PolicyDegraded> {
        let home = super::workspace::canonical_real_directory(home).map_err(io_rejected)?;
        super::maintenance::scope_labels(&home, run_id, "policy").map_err(rejected)?;
        super::maintenance::check_clean(&home).map_err(rejected)?;
        Self::detect_container_scoped(POLICY_IMAGE, Some(PolicyScope { home, run_id: run_id.into(), budget, quota }))
    }

    fn detect_container(image: &str) -> Result<Self, PolicyDegraded> {
        Self::detect_container_scoped(image, None)
    }

    fn detect_container_scoped(image: &str, scope: Option<PolicyScope>) -> Result<Self, PolicyDegraded> {
        let executable = std::env::var_os("PATH").into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|dir| dir.join("docker")).find(|path| path.is_file())
            .and_then(|path| path.canonicalize().ok()).ok_or(PolicyDegraded::NoIsolation)?;
        let mut env = vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin"))];
        // These configure the trusted Docker client only. They are never
        // forwarded as container environment variables or mounted resources.
        for key in ["HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH"] {
            if let Some(value) = std::env::var_os(key) { env.push((key.into(), value)); }
        }
        let client = DockerClient { executable, env };
        let mut inspect = client.command();
        inspect.args(["image", "inspect", "--format", "{{.Id}}", image]);
        let mut launch: Launch = inspect.into();
        launch.budget = scope.as_ref().map(|scope| scope.budget.clone());
        let bytes = control_launch_output(launch, EPISODE_TIMEOUT).map_err(|reason| {
            tracing::warn!(%reason, "Python policy isolation unavailable: image inspection failed");
            PolicyDegraded::NoIsolation
        })?;
        let image = String::from_utf8(bytes).map_err(|_| PolicyDegraded::NoIsolation)?.trim().to_owned();
        if !image.strip_prefix("sha256:").is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit())) {
            return Err(PolicyDegraded::NoIsolation);
        }
        let runtime = Self { python: "python3".into(), runtime_root: "/usr/local".into(),
            container: Some(ContainerBackend { client, image }), scope };
        // Check the actual kernel resource ceilings, rather than accepting
        // flags that an incompatible daemon might silently ignore.
        // Kernels can expose dormant tunnel devices (and bonding_masters,
        // which is not an interface) in a network-none namespace. Inspect
        // real interface flags: no non-loopback device may be UP, and there
        // must be no IPv4 route. cap-drop ALL prevents bringing devices UP.
        let probe = "import os,pathlib,socket,json\ncg=pathlib.Path('/sys/fs/cgroup')\nassert os.getuid()!=0\nfor _,name in socket.if_nameindex():\n if name!='lo': assert int((pathlib.Path('/sys/class/net')/name/'flags').read_text(),16)&1==0, 'non-loopback interface UP'\nassert len(pathlib.Path('/proc/net/route').read_text().splitlines())==1, 'IPv4 routes available'\nassert int((cg/'memory.max').read_text())==268435456\nassert int((cg/'pids.max').read_text())==64\nq,p=map(int,(cg/'cpu.max').read_text().split());assert q==p and q>0\ntry:\n open('/tmp/policy-write-canary','w').write('bad')\n raise RuntimeError('rootfs writable')\nexcept OSError: pass\ns=socket.socket();s.settimeout(0.2)\ntry:\n s.connect(('1.1.1.1',53))\n raise RuntimeError('network available')\nexcept OSError: pass\nassert 'ANTHROPIC_API_KEY' not in os.environ and 'OPENAI_API_KEY' not in os.environ\nprint('confined')";
        let launch = runtime.command(None, 1, ["-c", probe])?;
        let mut session = Session::spawn(launch, EPISODE_TIMEOUT).map_err(|reason| {
            tracing::warn!(%reason, "Python policy isolation unavailable: container launch failed");
            PolicyDegraded::NoIsolation
        })?;
        session.child.stdin.take();
        let mut bytes = Vec::new();
        let result = session.child.stdout.take().unwrap().take(4096).read_to_end(&mut bytes).map_err(io_rejected);
        session.complete(result).map_err(|reason| {
            tracing::warn!(%reason, "Python policy isolation unavailable: confinement probe failed");
            PolicyDegraded::NoIsolation
        })?;
        if bytes != b"confined\n" { return Err(PolicyDegraded::NoIsolation); }
        Ok(runtime)
    }

    /// Explicit test experiment covering native I/O/protocol only. This is not
    /// compiled into production and makes no memory/process-limit claim.
    #[cfg(test)]
    pub(super) fn experimental_native_for_tests() -> Result<Self, PolicyDegraded> {
        Self::detect_native_io()
    }

    #[cfg(test)]
    fn detect_native_io() -> Result<Self, PolicyDegraded> {
        let mut candidates = vec![PathBuf::from("/usr/bin/python3")];
        if let Some(path) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&path).map(|p| p.join("python3")));
        }
        let mut found_python = false;
        for python in candidates {
            if !python.is_file() { continue; }
            let mut command = Command::new(&python);
            command.env_clear().env("PATH", "/usr/bin:/bin").args(["-I", "-S", "-c",
                "import sys,json;print(json.dumps([sys.executable,sys.base_prefix]))"]);
            let Ok(mut session) = Session::spawn(command.into(), EPISODE_TIMEOUT) else { continue };
            session.child.stdin.take();
            let mut bytes = Vec::new();
            let output = session.child.stdout.take().unwrap().take(4096).read_to_end(&mut bytes);
            if session.complete(output.map_err(io_rejected)).is_err() { continue; }
            let Ok(paths) = serde_json::from_slice::<Vec<PathBuf>>(&bytes) else { continue };
            if paths.len() != 2 { continue; }
            let (Ok(python), Ok(runtime_root)) = (paths[0].canonicalize(), paths[1].canonicalize())
                else { continue };
            found_python = true;
            let runtime = Self { python, runtime_root, container: None, scope: None };
            // Actual confinement probe, not merely sandbox-exec existence.
            let Ok(stage) = tempfile::tempdir() else { continue };
            let secret = stage.path().join("canary");
            if std::fs::write(&secret, "private").is_err() { continue; }
            let script = format!("try:\n open({}).read()\n raise RuntimeError('sandbox leaked')\nexcept PermissionError:\n print('denied')",
                serde_json::to_string(&secret.to_string_lossy()).unwrap());
            let Ok(command) = runtime.command(None, 1, ["-c", &script]) else { continue };
            let Ok(mut session) = Session::spawn(command, EPISODE_TIMEOUT) else { continue };
            session.child.stdin.take();
            let mut out = Vec::new();
            let result = session.child.stdout.take().unwrap().take(4096).read_to_end(&mut out);
            if session.complete(result.map_err(io_rejected)).is_ok() && out == b"denied\n" {
                return Ok(runtime);
            }
        }
        Err(if found_python { PolicyDegraded::NoIsolation } else { PolicyDegraded::NoPython })
    }

    fn command<'a>(&self, stage: Option<&Path>, seed: u32,
        args: impl IntoIterator<Item = &'a str>) -> Result<Launch, PolicyDegraded> {
        if let Some(backend) = &self.container {
            let name = format!("dudu-policy-{}", uuid::Uuid::new_v4());
            let cleanup = Arc::new(ContainerCleanup { client: backend.client.clone(), name: name.clone(), stopped: Mutex::new(None), scope: self.scope.clone() });
            let mut create = backend.client.command();
            create.args(["create", "-i", "--pull", "never", "--name", &name,
                "--network", "none", "--read-only", "--user", "nobody",
                "--pids-limit", "64", "--memory", "256m", "--memory-swap", "256m",
                "--cpus", "1", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                "--ipc", "none", "--env", &format!("PYTHONHASHSEED={seed}"),
                "--env", "PYTHONDONTWRITEBYTECODE=1", "--env", "DUDU_POLICY_MAX_WALL=10",
                "--env", "DUDU_POLICY_DEADLINE_UNIX=0", "--entrypoint", "python3"]);
            if let Some(scope) = &self.scope {
                super::maintenance::check_clean(&scope.home).map_err(rejected)?;
                for label in super::maintenance::scope_labels(&scope.home, &scope.run_id, "policy").map_err(rejected)? {
                    create.args(["--label", &label]);
                }
            }
            if let Some(stage) = stage {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(stage, std::fs::Permissions::from_mode(0o755)).map_err(io_rejected)?;
                    for file in ["method.py", "shim.py"] {
                        std::fs::set_permissions(stage.join(file), std::fs::Permissions::from_mode(0o644)).map_err(io_rejected)?;
                    }
                }
                // Only the two trusted snapshot files exist in this private
                // bundle; no agent workspace, evaluator, history, or home.
                let stage = stage.to_str().filter(|s| !s.contains([',', '\n', '\r']))
                    .ok_or_else(|| rejected("container:invalid_bundle_path"))?;
                create.args(["--mount", &format!("type=bind,src={stage},dst=/policy,readonly"), "--workdir", "/policy"]);
            }
            create.arg(&backend.image).args(["-S", "-s", "-B", "-c", CONTAINER_SUPERVISOR,
                "python3", "-S", "-s", "-B"]);
            for arg in args {
                let arg = stage.and_then(|stage| Path::new(arg).strip_prefix(stage).ok())
                    .map(|relative| Path::new("/policy").join(relative).into_os_string())
                    .unwrap_or_else(|| OsString::from(arg));
                create.arg(arg);
            }
            let mut command = backend.client.command();
            command.args(["start", "--attach", "--interactive", &name]);
            return Ok(Launch { command, create: Some(create), cleanup: Some(cleanup), budget: self.scope.as_ref().map(|scope|scope.budget.clone()) });
        }
        let mut command = Command::new(&self.python);
        // -I ignores PYTHONHASHSEED; use an empty environment and -S/-s instead.
        command.env_clear().env("PATH", "/usr/bin:/bin")
            .env("PYTHONHASHSEED", seed.to_string()).args(["-S", "-s", "-B"]).args(args);
        let mut readonly = vec![self.python.clone(), self.runtime_root.clone()];
        if let Some(stage) = stage { readonly.push(stage.to_path_buf()); command.current_dir(stage); }
        let spec = ConfinementSpec { readonly, cpu_secs: 30, pids: 16,
            memory_bytes: 2 * 1024 * 1024 * 1024, ..Default::default() };
        match confine_command(&mut command, &spec) {
            Ok(IsolationBackend::Native | IsolationBackend::Container) => Ok(command.into()),
            _ => Err(PolicyDegraded::NoIsolation),
        }
    }

    pub fn check_source(&self, source: &str) -> Result<(), PolicyDegraded> {
        self.check_source_with_timeout(source, EPISODE_TIMEOUT)
    }

    fn stage_source(&self, source: &str, timeout: Duration) -> Result<tempfile::TempDir, PolicyDegraded> {
        if source.len() > MAX_SOURCE { return Err(rejected("ast:source_too_large")); }
        let shim = format!("{AST}\n{SHIM}");
        let build = |path: &Path| -> Result<(), super::contracts::AttemptInfraError> {
            std::fs::write(path.join("method.py"), source)
                .and_then(|_| std::fs::write(path.join("shim.py"), &shim))
                .map_err(|e| super::contracts::AttemptInfraError::Spawn(e.to_string()))
        };
        if let Some(scope) = &self.scope {
            let timeout = timeout.min(scope.budget.remaining_wall());
            if timeout.is_zero() { return Err(rejected("cancelled_or_deadline")); }
            let (stage, ()) = super::attempt_container::allocate_controlled_snapshot(
                &scope.home, &scope.run_id, scope.quota, Instant::now() + timeout,
                (source.len() + shim.len()) as u64, build).map_err(|e| rejected(e.to_string()))?;
            Ok(stage)
        } else {
            // Unscoped diagnostics and test runtimes do not belong to a run.
            let stage = tempfile::tempdir().map_err(io_rejected)?;
            build(stage.path()).map_err(|e| rejected(e.to_string()))?;
            Ok(stage)
        }
    }

    pub fn default_beta(&self, source: &str, timeout: Duration) -> Result<f64, PolicyDegraded> {
        let started = Instant::now();
        let timeout = timeout.min(EPISODE_TIMEOUT);
        let stage = self.stage_source(source, timeout)?;
        let root = stage.path().canonicalize().map_err(io_rejected)?;
        let method = root.join("method.py");
        let shim = root.join("shim.py");
        let launch = self.command(Some(&root), 1, [shim.to_str().unwrap(), method.to_str().unwrap()])?;
        let mut session = Session::spawn(launch, timeout.saturating_sub(started.elapsed()))?;
        session.child.stdin.as_mut().unwrap().write_all(b"{\"mode\":\"default_beta\",\"config\":{}}\n").map_err(io_rejected)?;
        session.child.stdin.take();
        let mut bytes = Vec::new();
        let result = session.child.stdout.take().unwrap().take(4096).read_to_end(&mut bytes).map_err(io_rejected);
        session.complete(result)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e|rejected(format!("default_beta:{e}")))?;
        value.get("default_beta").and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            .ok_or_else(||rejected("invalid_default_beta"))
    }
    fn check_source_with_timeout(&self, source: &str, timeout: Duration) -> Result<(), PolicyDegraded> {
        if source.len() > MAX_SOURCE { return Err(rejected("ast:source_too_large")); }
        let script = format!("{AST}\nimport json,sys\nprint(json.dumps(check_source(sys.stdin.read())))\n");
        let command = self.command(None, 1, ["-c", script.as_str()])?;
        let mut session = Session::spawn(command, EPISODE_TIMEOUT.min(timeout))?;
        let mut stdin = session.child.stdin.take().unwrap();
        let result = stdin.write_all(source.as_bytes()).map_err(io_rejected);
        drop(stdin);
        let mut out = Vec::new();
        let result = result.and_then(|_| session.child.stdout.take().unwrap().take(4096)
            .read_to_end(&mut out).map(|_| ()).map_err(io_rejected));
        session.complete(result)?;
        let reason: Option<String> = serde_json::from_slice(&out)
            .map_err(|e| rejected(format!("ast:invalid_response:{e}")))?;
        match reason { Some(reason) => Err(rejected(reason)), None => Ok(()) }
    }

    fn policy(&self, source: Arc<str>, beta: f64, seed: u32, timeout: Duration,
        state: Option<Arc<Mutex<SourceState>>>) -> PythonPolicy {
        let version = version(&source);
        PythonPolicy { runtime: self.clone(), source, config: PolicyConfig { beta },
            seed, timeout, version, state, started: Instant::now() }
    }
}

fn version(source: &str) -> PolicyVersion {
    let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
    PolicyVersion { policy_id: format!("llm-{}", duduclaw_core::truncate_bytes(&hash, 16)), source_sha256: Some(hash) }
}
fn baseline_version() -> PolicyVersion {
    PolicyVersion { policy_id: BASELINE_POLICY_ID.into(), source_sha256: None }
}
struct SourceState { source: Option<Arc<str>>, degraded: Option<PolicyDegraded>, timeout: Duration,
    beta: Option<f64>, origin_task_ids: Vec<String> }
pub struct ManagedPolicySource {
    runtime: Option<PythonPolicyRuntime>,
    state: Arc<Mutex<SourceState>>,
}
impl ManagedPolicySource {
    pub fn new(runtime: Result<PythonPolicyRuntime, PolicyDegraded>) -> Self {
        let (runtime, degraded) = match runtime { Ok(r) => (Some(r), None), Err(e) => (None, Some(e)) };
        Self { runtime, state: Arc::new(Mutex::new(SourceState {
            source: None, degraded, timeout: EPISODE_TIMEOUT, beta: None, origin_task_ids: Vec::new() })) }
    }
    pub fn runtime(&self) -> Option<PythonPolicyRuntime> { self.runtime.clone() }
    pub fn current_source(&self) -> Option<String> {
        self.state.lock().unwrap().source.as_ref().map(|s| s.to_string())
    }
    pub fn origin_task_ids(&self) -> Vec<String> { self.state.lock().unwrap().origin_task_ids.clone() }
    pub fn install_deployment(&self, source: &str, beta: f64, origins: &[String])
        -> Result<PolicyVersion, PolicyDegraded> {
        if !beta.is_finite() || !(0.0..=1.0).contains(&beta) { return Err(rejected("invalid_beta")); }
        self.install(source)?;
        Ok(self.install_validated_deployment(source, beta, origins))
    }
    pub(super) fn install_validated_deployment(&self, source: &str, beta: f64, origins: &[String]) -> PolicyVersion {
        let mut state = self.state.lock().unwrap();
        state.source = Some(Arc::from(source));
        state.beta = Some(beta);
        state.origin_task_ids = origins.to_vec();
        state.degraded = None;
        version(source)
    }
    pub fn install(&self, source: &str) -> Result<PolicyVersion, PolicyDegraded> {
        let runtime = self.runtime.as_ref().ok_or_else(|| self.degraded().unwrap_or(PolicyDegraded::NoIsolation))?;
        runtime.check_source(source)?;
        Ok(self.install_validated(source))
    }
    /// Only the dream selector calls this after successful isolated replay.
    /// Re-running the AST subprocess at commit could exceed the run deadline.
    pub(super) fn install_validated(&self, source: &str) -> PolicyVersion {
        let mut state = self.state.lock().unwrap();
        state.source = Some(Arc::from(source));
        state.beta = None;
        state.origin_task_ids.clear();
        state.degraded = None;
        version(source)
    }
    pub fn degrade(&self, reason: PolicyDegraded) {
        let mut state = self.state.lock().unwrap();
        state.source = None;
        state.beta = None;
        state.origin_task_ids.clear();
        state.degraded = Some(reason);
    }
    pub fn set_online_timeout(&self, timeout: Duration) { self.state.lock().unwrap().timeout = timeout; }
}
impl PolicySource for ManagedPolicySource {
    fn live_beta(&self, configured: f64) -> f64 {
        let state = self.state.lock().unwrap();
        state.source.as_ref().and(state.beta).unwrap_or(configured)
    }
    fn current(&self) -> PolicyVersion {
        self.state.lock().unwrap().source.as_deref().map(version).unwrap_or_else(baseline_version)
    }
    fn degraded(&self) -> Option<PolicyDegraded> { self.state.lock().unwrap().degraded.clone() }
    fn instantiate(&self, beta: f64) -> Result<Box<dyn ExplorationPolicy + Send>, PolicyDegraded> {
        if !beta.is_finite() || !(0.0..=1.0).contains(&beta) { return Err(rejected("invalid_beta")); }
        let state = self.state.lock().unwrap();
        match (&self.runtime, &state.source) {
            (Some(runtime), Some(source)) => Ok(Box::new(runtime.policy(source.clone(), beta, 1,
                state.timeout, Some(self.state.clone())))),
            _ => Ok(Box::new(BaselineParallelRefine::new(&PolicyConfig { beta }))),
        }
    }
}

struct PythonPolicy {
    runtime: PythonPolicyRuntime, source: Arc<str>, config: PolicyConfig, seed: u32,
    timeout: Duration, version: PolicyVersion, state: Option<Arc<Mutex<SourceState>>>,
    started: Instant,
}
impl PythonPolicy {
    fn retire(&self, reason: PolicyDegraded) -> PolicyError {
        if let Some(state) = &self.state {
            let mut state = state.lock().unwrap();
            if state.source.as_deref().map(version).as_ref() == Some(&self.version) {
                state.source = None;
                state.beta = None;
                state.origin_task_ids.clear();
                state.degraded = Some(reason.clone());
            }
        }
        PolicyError::Failed(reason.to_string())
    }
    fn exchange<T>(&self, serve: impl FnOnce(&mut BufReader<std::process::ChildStdout>,
        &mut std::process::ChildStdin) -> Result<T, super::protocol::ServeError>) -> Result<T, PolicyError> {
        let result = (|| {
            let stage = self.runtime.stage_source(&self.source, self.timeout.saturating_sub(self.started.elapsed()))?;
            let stage_path = stage.path().canonicalize().map_err(io_rejected)?;
            let method = stage_path.join("method.py");
            let shim = stage_path.join("shim.py");
            let command = self.runtime.command(Some(&stage_path), self.seed,
                [shim.to_str().unwrap(), method.to_str().unwrap()])?;
            let mut session = Session::spawn(command, self.timeout.saturating_sub(self.started.elapsed()))?;
            let mut reader = BufReader::new(session.child.stdout.take().unwrap());
            let mut writer = session.child.stdin.take().unwrap();
            let result = serve(&mut reader, &mut writer).map_err(|e| rejected(e.to_string()));
            drop(writer);
            // Release stdout before cleanup; the protocol has bounded each line.
            let result = session.complete(result);
            drop(reader);
            result
        })();
        result.map_err(|reason| self.retire(reason))
    }
}
impl ExplorationPolicy for PythonPolicy {
    fn id(&self) -> &str { &self.version.policy_id }
    fn plan_grid(&mut self, ctx: &GridContext) -> Result<GridPlan, PolicyError> {
        let plan = self.exchange(|r, w| request_plan_grid(&self.config, ctx, r, w, &ServeLimits::default()))?;
        if plan.branch_count == 0 || (ctx.trace_branch_count.is_none()
            && (plan.branch_count > ctx.hard_max_branch_count || plan.refine_count > ctx.hard_max_refine_count)) {
            return Err(self.retire(rejected("invalid_plan:outside_hard_caps")));
        }
        Ok(plan)
    }
    fn solve(&mut self, question: &mut dyn Question) -> Result<(), PolicyError> {
        self.exchange(|r, w| serve_question(question, &self.config, r, w, &ServeLimits::default())).map(|_| ())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub valid: bool,
    pub violation: Option<String>,
    pub value: Option<f64>,
    pub worlds: Vec<WorldScore>,
    pub context_mismatch_rate: Option<f64>,
}
fn evaluate_seed(runtime: &PythonPolicyRuntime, source: Arc<str>, tree: &WorldTree, seed: u32, deadline: Instant)
    -> Result<(WorldScore, Vec<GridPlan>), String> {
    let cfg = ReplayConfig::for_world(tree);
    let mut points = Vec::new();
    let mut raws = Vec::new();
    let mut plans = Vec::new();
    let mut out_of_support = false;
    for beta in BETA_GRID {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return Err("timeout:run_wall_budget".into()); }
        let mut policy = runtime.policy(source.clone(), beta, seed, EPISODE_TIMEOUT.min(remaining), None);
        let run = run_point(&mut policy, tree, &cfg, beta).map_err(|e| e.to_string())?;
        out_of_support |= plan_out_of_support(&run.plan, tree);
        let (point, raw) = score_point(tree, &run);
        points.push(point); raws.push(raw); plans.push(run.plan);
    }
    Ok((WorldScore::aggregate(tree.world().run_id.clone(), tree.world().round,
        points, &raws, out_of_support), plans))
}
pub fn evaluate_candidate(runtime: &PythonPolicyRuntime, source: &str, worlds: &[WorldTree]) -> CandidateEvaluation {
    evaluate_candidate_with_timeout(runtime, source, worlds, Duration::from_secs(3600))
}
pub fn evaluate_candidate_with_timeout(runtime: &PythonPolicyRuntime, source: &str,
    worlds: &[WorldTree], timeout: Duration) -> CandidateEvaluation {
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return CandidateEvaluation { valid: false, violation: Some("invalid_timeout".into()),
            value: None, worlds: vec![], context_mismatch_rate: None };
    };
    let result = (|| -> Result<Vec<WorldScore>, String> {
        if worlds.is_empty() { return Err("no_completed_worlds".into()); }
        runtime.check_source_with_timeout(source, deadline.saturating_duration_since(Instant::now()))
            .map_err(|e| e.to_string())?;
        let source: Arc<str> = Arc::from(source);
        let mut scores = Vec::new();
        for tree in worlds {
            let first = evaluate_seed(runtime, source.clone(), tree, 1, deadline)?;
            let second = evaluate_seed(runtime, source.clone(), tree, 2, deadline)?;
            if first != second { return Err(format!("nondeterministic:round:{}", tree.world().round)); }
            scores.push(first.0);
        }
        Ok(scores)
    })();
    match result {
        Err(violation) => CandidateEvaluation { valid: false, violation: Some(violation),
            value: None, worlds: vec![], context_mismatch_rate: None },
        Ok(worlds) => {
            let value = round9(worlds.iter().map(|w| w.pareto_reward).sum::<f64>() / worlds.len() as f64);
            let mismatch = round9(worlds.iter().flat_map(|w| &w.points)
                .map(|p| p.context_mismatch_rate).sum::<f64>() / (worlds.len() * BETA_GRID.len()) as f64);
            CandidateEvaluation { valid: true, violation: None, value: Some(value), worlds,
                context_mismatch_rate: Some(mismatch) }
        }
    }
}

#[cfg(all(test, unix))]
mod container_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_runtime(mode: &str) -> (tempfile::TempDir, PythonPolicyRuntime, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let docker = dir.path().join("docker");
        let log = dir.path().join("cleanup.log");
        // An external fake client gives the same pipe/process-group lifecycle
        // without consulting Docker, global environment, or an API account.
        std::fs::write(&docker, "#!/bin/sh\ncase \"$1\" in\ncreate) printf 'created\\n';;\nstart) if [ \"$MODE\" = hang ]; then sleep 30; else printf 'reply\\n'; fi;;\nrm) printf '%s\\n' \"$*\" >> \"$CLEANUP_LOG\";;\n*) exit 2;;\nesac\n").unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let client = DockerClient { executable: docker, env: vec![
            ("PATH".into(), "/usr/bin:/bin".into()), ("MODE".into(), mode.into()),
            ("CLEANUP_LOG".into(), log.clone().into_os_string())] };
        let runtime = PythonPolicyRuntime { python: "python3".into(), runtime_root: "/usr/local".into(),
            container: Some(ContainerBackend { client, image: format!("sha256:{}", "a".repeat(64)) }), scope: None };
        (dir, runtime, log)
    }

    #[test]
    fn container_launch_has_hard_limits_and_only_readonly_bundle_mount() {
        let (_dir, runtime, _log) = fake_runtime("reply");
        let bundle = tempfile::tempdir().unwrap();
        std::fs::write(bundle.path().join("method.py"), BASELINE_SOURCE).unwrap();
        std::fs::write(bundle.path().join("shim.py"), SHIM).unwrap();
        let method = bundle.path().join("method.py");
        let launch = runtime.command(Some(bundle.path()), 2, [method.to_str().unwrap()]).unwrap();
        let args: Vec<_> = launch.create.unwrap().get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        for (flag, value) in [("--network", "none"), ("--user", "nobody"),
            ("--pids-limit", "64"), ("--memory", "256m"), ("--memory-swap", "256m"),
            ("--cpus", "1"), ("--cap-drop", "ALL"), ("--security-opt", "no-new-privileges")] {
            assert!(args.windows(2).any(|a| a == [flag, value]), "{flag}");
        }
        assert!(args.iter().any(|a| a == "--read-only"));
        assert!(args.iter().any(|a| a == "PYTHONHASHSEED=2"));
        assert_eq!(args.iter().filter(|a| a.as_str() == "--mount").count(), 1);
        assert!(args.iter().any(|a| a.ends_with("dst=/policy,readonly")));
        assert_eq!(args.last().unwrap(), "/policy/method.py");
        assert_eq!(std::fs::metadata(bundle.path()).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn container_removed_on_normal_exit_protocol_error_timeout_and_drop() {
        for mode in ["normal", "error", "timeout", "drop"] {
            let (_dir, runtime, log) = fake_runtime(if mode == "timeout" || mode == "drop" { "hang" } else { "reply" });
            let launch = runtime.command(None, 1, ["-c", "pass"]).unwrap();
            let started = Instant::now();
            let mut session = Session::spawn(launch, Duration::from_secs(1)).unwrap();
            if mode == "drop" { drop(session); }
            else if mode == "error" { assert!(session.complete::<()>(Err(rejected("protocol_error"))).is_err()); drop(session); }
            else {
                session.child.stdin.take();
                let mut out = Vec::new();
                let result = session.child.stdout.take().unwrap().read_to_end(&mut out).map_err(io_rejected);
                let result = session.complete(result);
                if mode == "timeout" { assert!(result.unwrap_err().to_string().contains("timeout")); }
                else { assert!(result.is_ok()); assert_eq!(out, b"reply\n"); }
                drop(session);
            }
            assert!(started.elapsed() < Duration::from_secs(3));
            let log = std::fs::read_to_string(log).unwrap();
            assert_eq!(log.lines().count(), 1, "cleanup occurs exactly once: {mode}");
            assert!(log.starts_with("rm -f dudu-policy-"));
        }
    }
}
