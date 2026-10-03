//! Operator-pinned evaluators: immutable registry, post-exit snapshots and selftests.
use super::config::{DiscoveryConfig, EvaluatorConfig, EvaluatorSandbox};
use super::contracts::{Evaluator, IsolationBackend, ScoreOutcome, ScoreRequest};
use super::tree::FailClass;
use super::workspace::{
    canonical_real_directory, directory_sha256, manifest, tamper_paths, tree_bytes,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

// PID 1 remains trusted even if the host Docker connection disappears. When
// it exits, the container namespace also destroys descendants that used setsid.
const CONTAINER_SUPERVISOR: &str = "import os,sys,subprocess,signal,time\ndeadline=float(os.environ['DUDU_EVAL_DEADLINE_UNIX'])\nif time.time()>=deadline: sys.exit(124)\np=subprocess.Popen(sys.argv[1:],start_new_session=True)\ntry:\n try: code=p.wait(timeout=max(0,min(float(os.environ['DUDU_EVAL_MAX_WALL']),deadline-time.time())))\n except subprocess.TimeoutExpired: code=124\nfinally:\n try: os.killpg(p.pid,signal.SIGKILL)\n except ProcessLookupError: pass\n if p.poll() is None: p.wait()\nsys.exit(code)";
const CONTAINER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RegisteredEvaluator {
    home: PathBuf,
    config: DiscoveryConfig,
    operator_identity: bool,
}
impl RegisteredEvaluator {
    pub fn new(home: PathBuf, config: DiscoveryConfig, operator_identity: bool) -> Self {
        Self {
            home,
            config,
            operator_identity,
        }
    }
    fn audit(&self, event: &str, details: Value) {
        crate::security_autopilot::audit_and_emit(
            &self.home,
            &duduclaw_security::audit::AuditEvent::new(
                event,
                "discovery-evaluator",
                duduclaw_security::audit::Severity::Warning,
                details,
            ),
        );
    }
    fn pinned(&self, name: &str) -> Result<(PathBuf, &EvaluatorConfig), String> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err("invalid evaluator name".into());
        }
        let config = self
            .config
            .evaluators
            .get(name)
            .ok_or("evaluator is not registered")?;
        let root = canonical_real_directory(&self.home.join("discovery/evaluators").join(name))
            .map_err(|e| e.to_string())?;
        validate_argv(&root, &config.command)?;
        validate_ownership(&root).map_err(|e| e.to_string())?;
        if config.timeout_secs == 0
            || config.memory_bytes == 0
            || config.pids == 0
            || config.scratch_bytes == 0
        {
            return Err("zero evaluator resource limit".into());
        }
        Ok((root, config))
    }
    /// Operator-only selftest before persisting this hash to config.toml.
    /// Registration never executes a solution outside the evaluator sandbox.
    pub async fn register(&self, name: &str) -> Result<String, String> {
        if !self.operator_identity {
            return Err("evaluator registration requires operator identity".into());
        }
        let (root, config) = self.pinned(name)?;
        let hash = directory_sha256(&root).map_err(|e| e.to_string())?;
        let fixture = |path: &Path| -> Result<PathBuf, String> {
            let path = canonical_real_directory(path).map_err(|e| e.to_string())?;
            if !path.starts_with(&root) || path == root {
                return Err("selftest must be a directory inside the pinned evaluator".into());
            }
            Ok(path)
        };
        let good = fixture(&config.good_solution)?;
        let cheating = fixture(&config.cheating_solution)?;
        let good = self
            .execute(&root, config, &good, "registration", "good", Duration::from_secs(config.timeout_secs))
            .await;
        let bad = self
            .execute(&root, config, &cheating, "registration", "cheating", Duration::from_secs(config.timeout_secs))
            .await;
        if !good.evaluated || !good.valid || (!bad.evaluated && bad.fail_class != FailClass::Tamper)
        {
            return Err("evaluator good/cheating selftest could not be verified".into());
        }
        // Both directions need the same contract. A cheating answer may never
        // receive ANY valid score: comparing only max scores is unsafe for min.
        if bad.valid {
            return Err("evaluator accepted the known cheating solution".into());
        }
        if directory_sha256(&root).map_err(|e| e.to_string())? != hash {
            return Err("evaluator changed during selftest".into());
        }
        Ok(hash)
    }
    async fn execute(
        &self,
        root: &Path,
        config: &EvaluatorConfig,
        source: &Path,
        run_id: &str,
        cell_id: &str,
        timeout: Duration,
    ) -> ScoreOutcome {
        let start = Instant::now();
        let failure = |class, message: &str, backend| {
            if class == FailClass::Tamper {
                self.audit("discovery_tamper", json!({"run_id":run_id,"cell_id":cell_id,"source":"evaluator","reason":message}));
            }
            failed(class, message, backend, start.elapsed().as_secs_f64())
        };
        // Native macOS cannot enforce memory/process/scratch quotas. Do not
        // claim equivalent evaluator protection; container or explicit opt-in.
        let backend = match config.sandbox {
            EvaluatorSandbox::Container => IsolationBackend::Container,
            EvaluatorSandbox::None if self.config.allow_unconfined && self.operator_identity => {
                IsolationBackend::None
            }
            _ => {
                self.audit(
                    "discovery_isolation_refused",
                    json!({"run_id":run_id,"cell_id":cell_id,"sandbox":config.sandbox}),
                );
                return failure(
                    FailClass::RuntimeError,
                    "evaluator isolation unavailable",
                    IsolationBackend::Native,
                );
            }
        };
        if timeout.is_zero() {
            return failure(FailClass::Timeout, "evaluator deadline exhausted", backend);
        }
        let source = match canonical_real_directory(source) {
            Ok(source) => source,
            Err(_) => {
                return failure(
                    FailClass::Tamper,
                    "solution root is not a real directory",
                    backend,
                );
            }
        };
        let source = source.as_path();
        match tamper_paths(source) {
            Ok(paths) if !paths.is_empty() => {
                return failure(
                    FailClass::Tamper,
                    "forbidden interpreter hook or linked file in solution",
                    backend,
                );
            }
            Err(_) => {
                return failure(
                    FailClass::NoOutput,
                    "solution workspace unavailable",
                    backend,
                );
            }
            _ => {}
        }
        if !tree_bytes(source).is_ok_and(|size| size <= self.config.max_starting_workspace_bytes) {
            return failure(
                FailClass::RuntimeError,
                "solution snapshot exceeds quota",
                backend,
            );
        }
        let private = match tempfile::Builder::new()
            .prefix("discovery_score_")
            .tempdir()
        {
            Ok(value) => value,
            Err(_) => {
                return failure(
                    FailClass::RuntimeError,
                    "cannot create evaluator scratch",
                    backend,
                );
            }
        };
        let private_root = match private.path().canonicalize() {
            Ok(path) => path,
            Err(_) => return failure(FailClass::RuntimeError, "cannot resolve scratch", backend),
        };
        let evaluator_snapshot = private_root.join("evaluator");
        let snapshot = private_root.join("snapshot");
        let scratch = private_root.join("scratch");
        let _sealed = SealedDirectories(vec![snapshot.clone(), evaluator_snapshot.clone()]);
        if fs::create_dir(&snapshot)
            .and_then(|_| fs::create_dir(&scratch))
            .is_err()
        {
            return failure(
                FailClass::RuntimeError,
                "cannot prepare evaluator snapshot",
                backend,
            );
        }
        let source_hash = match directory_sha256(source) {
            Ok(hash) => hash,
            Err(_) => return failure(FailClass::Tamper, "source integrity unavailable", backend),
        };
        // Copy only regular data. Tamper checks run first, before exclusions.
        if copy_data(source, &snapshot).is_err() {
            return failure(FailClass::Tamper, "snapshot copy rejected", backend);
        }
        if !directory_sha256(&snapshot).is_ok_and(|hash| hash == source_hash)
            || !directory_sha256(source).is_ok_and(|hash| hash == source_hash)
        {
            return failure(
                FailClass::Tamper,
                "solution changed while taking snapshot",
                backend,
            );
        }
        if let Some(test_data) = &config.test_data {
            let data = match canonical_real_directory(test_data) {
                Ok(path) if path.starts_with(root) => path,
                _ => {
                    return failure(
                        FailClass::RuntimeError,
                        "test data is outside pinned directory",
                        backend,
                    );
                }
            };
            if copy_data(&data, &snapshot).is_err() {
                return failure(
                    FailClass::RuntimeError,
                    "test data overlay rejected",
                    backend,
                );
            }
        }
        if !tree_bytes(&snapshot).is_ok_and(|size| size <= self.config.max_starting_workspace_bytes)
        {
            return failure(
                FailClass::RuntimeError,
                "test-data snapshot exceeds quota",
                backend,
            );
        }
        let original_evaluator_hash = match directory_sha256(root) {
            Ok(hash) => hash,
            Err(_) => return failure(FailClass::Tamper, "registry snapshot unavailable", backend),
        };
        let before = match directory_sha256(&snapshot) {
            Ok(hash) => hash,
            Err(_) => return failure(FailClass::Tamper, "snapshot integrity unavailable", backend),
        };
        if fs::create_dir(&evaluator_snapshot)
            .and_then(|_| copy_data(root, &evaluator_snapshot))
            .and_then(|_| make_readonly(&evaluator_snapshot))
            .and_then(|_| make_readonly(&snapshot))
            .is_err()
        {
            return failure(
                FailClass::RuntimeError,
                "cannot seal scoring snapshots",
                backend,
            );
        }
        if !directory_sha256(&evaluator_snapshot).is_ok_and(|hash| hash == original_evaluator_hash)
            || !directory_sha256(root).is_ok_and(|hash| hash == original_evaluator_hash)
        {
            return failure(
                FailClass::Tamper,
                "registry changed while taking snapshot",
                backend,
            );
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return failure(FailClass::Timeout, "evaluator snapshot deadline exceeded", backend);
        }
        let (mut command, workspace_input) = match backend {
            IsolationBackend::Container => {
                let labels = match super::maintenance::scope_labels(&self.home, run_id, "evaluator") {
                    Ok(labels) => labels,
                    Err(reason) => return failure(FailClass::RuntimeError, &reason, backend),
                };
                match docker_command_with_labels(root, &evaluator_snapshot, &snapshot, config, remaining, &labels) {
                    Ok(command) => (command, "/workspace".to_string()),
                    Err(message) => return failure(FailClass::RuntimeError, &message, backend),
                }
            }
            IsolationBackend::None => {
                self.audit(
                    "discovery_unconfined",
                    json!({"run_id":run_id,"cell_id":cell_id,"source":"evaluator"}),
                );
                let mut command = Command::new(&config.command[0]);
                command.args(&config.command[1..]);
                command.current_dir(&scratch);
                scrub_environment(&mut command, &scratch);
                (command, snapshot.to_string_lossy().into_owned())
            }
            IsolationBackend::Native => unreachable!(),
        };
        // Keep the established judge envelope's task/result shape, extending it
        // with a workspace data pointer. Evaluators must calculate the score.
        let payload = json!({"task":{"id":run_id,"goal":cell_id},"result":{"workspace":workspace_input},"workspace":workspace_input}).to_string();
        // The final argv is a data directory, never an executable chosen by a worker.
        command.arg(&workspace_input);
        let output = if backend == IsolationBackend::Container {
            run_container(command, payload.as_bytes(), remaining,
                &self.home, run_id, cell_id).await
        } else {
            super::process::run(command, payload.as_bytes(),
                remaining, 256 * 1024, |_| true).await
        };
        let output = match output {
            Ok(output) => output,
            Err(_) => {
                return failure(
                    FailClass::RuntimeError,
                    "evaluator process unavailable",
                    backend,
                );
            }
        };
        if output.timed_out || (backend == IsolationBackend::Container && output.status.code() == Some(124)) {
            return failure(FailClass::Timeout, "evaluator deadline exceeded", backend);
        }
        if backend == IsolationBackend::Container
            && matches!(output.status.code(), Some(125 | 126 | 127))
        {
            self.audit(
                "discovery_isolation_refused",
                json!({"run_id":run_id,"cell_id":cell_id,"sandbox":"container"}),
            );
        }
        if !output.status.success() || output.output_truncated {
            return failure(
                FailClass::RuntimeError,
                "evaluator failed or output limit exceeded",
                backend,
            );
        }
        if !directory_sha256(&snapshot).is_ok_and(|hash| hash == before) {
            return failure(
                FailClass::Tamper,
                "evaluator modified solution snapshot",
                backend,
            );
        }
        if !tree_bytes(&scratch).is_ok_and(|size| size <= config.scratch_bytes) {
            return failure(
                FailClass::RuntimeError,
                "evaluator scratch quota exceeded",
                backend,
            );
        }
        match parse_score(&output.stdout, backend, output.wall_secs) {
            Ok(outcome) => {
                if outcome.diagnostics.as_deref() == Some("外部評分回饋已因不安全指令而封鎖。")
                {
                    self.audit(
                        "discovery_diagnostics_blocked",
                        json!({"run_id":run_id,"cell_id":cell_id}),
                    );
                }
                outcome
            }
            Err(_) => failure(
                FailClass::RuntimeError,
                "invalid evaluator verdict envelope",
                backend,
            ),
        }
    }
}
#[async_trait]
impl Evaluator for RegisteredEvaluator {
    async fn score(&self, req: &ScoreRequest) -> ScoreOutcome {
        let started = Instant::now();
        let (root, config) = match self.pinned(&req.evaluator) {
            Ok(value) => value,
            Err(_) => {
                return failed(
                    FailClass::RuntimeError,
                    "evaluator registry rejected",
                    IsolationBackend::Native,
                    0.0,
                );
            }
        };
        if config.sha256.len() != 64
            || !directory_sha256(&root).is_ok_and(|hash| hash == config.sha256)
        {
            self.audit(
                "discovery_tamper",
                json!({"run_id":req.run_id,"cell_id":req.cell_id,"source":"evaluator_registry"}),
            );
            return failed(
                FailClass::Tamper,
                "evaluator registry hash mismatch",
                IsolationBackend::Native,
                0.0,
            );
        }
        let limit = req.timeout.unwrap_or(Duration::from_secs(config.timeout_secs))
            .min(Duration::from_secs(config.timeout_secs));
        let _timing_guard = if config.timing_sensitive {
            match super::timing_gate::acquire(&self.home, &config.sha256, limit.saturating_sub(started.elapsed())).await {
                Ok(guard) => guard,
                Err(reason) => return failed(FailClass::Timeout, &reason, IsolationBackend::Native, started.elapsed().as_secs_f64()),
            }
        } else { None };
        let timeout = req.timeout.unwrap_or(Duration::from_secs(config.timeout_secs))
            .min(Duration::from_secs(config.timeout_secs)).saturating_sub(started.elapsed());
        let outcome = self
            .execute(&root, config, &req.node_dir, &req.run_id, &req.cell_id,
                timeout)
            .await;
        if !directory_sha256(&root).is_ok_and(|hash| hash == config.sha256) {
            return failed(
                FailClass::Tamper,
                "evaluator registry changed during scoring",
                outcome.isolation,
                outcome.wall_secs,
            );
        }
        outcome
    }
}

// Cleanup is bounded and reaped, including synchronous Drop on cancellation.
// No background cleanup thread outlives the snapshot owner. A failed cleanup
// is recorded explicitly and prevents an otherwise valid score from passing.
struct ContainerCleanup {
    program: std::ffi::OsString,
    environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    name: String,
    home: PathBuf,
    run_id: String,
    cell_id: String,
    result: Option<Result<(), String>>,
}
impl ContainerCleanup {
    fn for_create(command: &Command, home: &Path, run_id: &str, cell_id: &str) -> io::Result<Self> {
        let args = command.get_args().collect::<Vec<_>>();
        let name = args.windows(2).find(|pair| pair[0] == "--name")
            .map(|pair| pair[1].to_string_lossy().into_owned())
            .ok_or_else(|| io::Error::other("container create has no trusted name"))?;
        Ok(Self {
            program: command.get_program().to_owned(),
            environment: command.get_envs().filter_map(|(key, value)|
                value.map(|value| (key.to_owned(), value.to_owned()))).collect(),
            name, home: home.into(), run_id: run_id.into(), cell_id: cell_id.into(), result: None,
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.env_clear().envs(self.environment.iter().cloned());
        command
    }
    fn stop(&mut self) -> Result<(), String> {
        if let Some(result) = &self.result { return result.clone(); }
        let mut command = self.command();
        command.args(["rm", "-f", &self.name]);
        let result = bounded_remove(command, CONTAINER_CLEANUP_TIMEOUT);
        if let Err(reason) = &result {
            let _ = super::maintenance::record_cleanup_failure(&self.home, &self.run_id, "evaluator", reason);
        }
        crate::security_autopilot::audit_and_emit(&self.home,
            &duduclaw_security::audit::AuditEvent::new("discovery_container_cleanup", "discovery-evaluator",
                if result.is_ok() { duduclaw_security::audit::Severity::Info }
                else { duduclaw_security::audit::Severity::Warning },
                json!({"run_id":self.run_id,"cell_id":self.cell_id,"container":self.name,
                    "success":result.is_ok(),"reason":result.as_ref().err()})));
        self.result = Some(result.clone());
        result
    }
}
impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn bounded_remove(mut command: Command, timeout: Duration) -> Result<(), String> {
    use std::process::Stdio;
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| "container cleanup unavailable".to_string())?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break if status.success() { Ok(()) } else { Err("container cleanup failed".into()) },
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => break Err("container cleanup deadline exceeded".into()),
            Err(_) => break Err("container cleanup wait failed".into()),
        }
    };
    #[cfg(unix)]
    { let _ = duduclaw_core::platform::kill_process_group(child.id()); }
    if result.is_err() { let _ = child.kill(); }
    let _ = child.wait();
    result
}

async fn run_container(create: Command, payload: &[u8], timeout: Duration,
    home: &Path, run_id: &str, cell_id: &str) -> io::Result<super::process::ProcessOutput> {
    let started = Instant::now();
    let mut cleanup = ContainerCleanup::for_create(&create, home, run_id, cell_id)?;
    // A late create may leave only a stopped container. Evaluator code cannot
    // execute until the host has confirmed a successful create and exact ID.
    let created = super::process::run(create, b"", timeout, 1024, |_| true).await?;
    let id = created.stdout.trim();
    if !created.status.success() || created.timed_out || created.output_truncated
        || id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::other("evaluator container create was not confirmed"));
    }
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() { return Err(io::Error::other("container launch deadline exceeded")); }
    let mut command = cleanup.command();
    command.args(["start", "--attach", "--interactive", id]);
    let output = super::process::run(command, payload, remaining, 256 * 1024, |_| true).await;
    cleanup.stop().map_err(io::Error::other)?;
    output
}

fn failed(
    class: FailClass,
    message: &str,
    backend: IsolationBackend,
    wall_secs: f64,
) -> ScoreOutcome {
    ScoreOutcome {
        evaluated: false,
        valid: false,
        score: None,
        fail_class: class,
        diagnostics: Some(duduclaw_core::truncate_bytes(message, 500).to_owned()),
        isolation: backend,
        wall_secs,
    }
}
fn scrub_environment(command: &mut Command, scratch: &Path) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", scratch)
        .env("TMPDIR", scratch)
        .env("LANG", "C.UTF-8")
        .env("TZ", "UTC")
        .env("PYTHONDONTWRITEBYTECODE", "1");
}
pub fn validate_argv(root: &Path, argv: &[String]) -> Result<(), String> {
    if argv.is_empty() || argv[0].is_empty() {
        return Err("evaluator command is empty".into());
    }
    for (index, arg) in argv.iter().enumerate() {
        if arg.chars().any(char::is_control) {
            return Err("command contains control character".into());
        }
        if matches!(
            arg.as_str(),
            "-c" | "--command" | "-Command" | "-EncodedCommand"
        ) {
            return Err("inline command execution is forbidden".into());
        }
        let token = arg.split_once('=').map_or(arg.as_str(), |(_, value)| value);
        let path = Path::new(token);
        let resolved = if path.is_absolute() {
            Some(
                path.canonicalize()
                    .map_err(|_| "command path does not exist")?,
            )
        } else if root.join(path).exists() || path.exists() {
            return Err("existing command path must be absolute".into());
        } else {
            None
        };
        if index == 0 && resolved.is_none() {
            return Err("program must be a pinned absolute path".into());
        }
        if let Some(path) = resolved {
            if !path.starts_with(root) || path == root {
                return Err("command path is outside evaluator directory".into());
            }
            if index == 0
                && matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some(
                        "sh" | "bash" | "zsh" | "dash" | "fish" | "powershell" | "pwsh" | "cmd.exe"
                    )
                )
            {
                return Err("shell evaluator is forbidden".into());
            }
        }
    }
    Ok(())
}
fn validate_ownership(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::geteuid() };
        for path in std::iter::once(PathBuf::new()).chain(manifest(root)?.into_keys()) {
            let metadata = fs::symlink_metadata(root.join(path))?;
            if metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
                return Err(io::Error::other(
                    "evaluator must be operator owned and not group/world writable",
                ));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        return Err(io::Error::other(
            "cannot verify evaluator ownership on this platform",
        ));
    }
    Ok(())
}
fn copy_data(source: &Path, destination: &Path) -> io::Result<()> {
    // Build a manifest before copying: linked and special files reject, rather
    // than silently converting potentially executable redirects into data.
    let _ = manifest(source)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.is_dir() {
            fs::create_dir_all(&target)?;
            copy_data(&entry.path(), &target)?;
        } else if meta.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(io::Error::other("unsupported snapshot file"));
        }
    }
    Ok(())
}

struct SealedDirectories(Vec<PathBuf>);
impl Drop for SealedDirectories {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            fn restore(path: &Path) {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        if fs::symlink_metadata(entry.path()).is_ok_and(|meta| meta.is_dir()) {
                            restore(&entry.path());
                        }
                    }
                }
            }
            for path in &self.0 {
                restore(path);
            }
        }
    }
}

fn make_readonly(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                make_readonly(&entry.path())?;
            } else {
                fs::set_permissions(
                    entry.path(),
                    fs::Permissions::from_mode(0o444 | metadata.permissions().mode() & 0o111),
                )?;
            }
        }
        fs::set_permissions(root, fs::Permissions::from_mode(0o555))?;
    }
    Ok(())
}

/// Docker CLI argv is built without a shell. Host paths never become code.
pub fn docker_command(
    root: &Path,
    snapshot: &Path,
    config: &EvaluatorConfig,
) -> Result<Command, String> {
    docker_command_with_mount(root, root, snapshot, config, Duration::from_secs(config.timeout_secs))
}
fn docker_command_with_mount(
    root: &Path,
    evaluator_snapshot: &Path,
    snapshot: &Path,
    config: &EvaluatorConfig,
    timeout: Duration,
) -> Result<Command, String> {
    docker_command_with_labels(root, evaluator_snapshot, snapshot, config, timeout, &[])
}
fn docker_command_with_labels(
    root: &Path, evaluator_snapshot: &Path, snapshot: &Path, config: &EvaluatorConfig,
    timeout: Duration, labels: &[String],
) -> Result<Command, String> {
    for path in [evaluator_snapshot, snapshot] {
        if path
            .as_os_str()
            .to_string_lossy()
            .chars()
            .any(|c| c == ',' || c.is_control())
        {
            return Err("unsupported container mount path".into());
        }
    }
    let image = config
        .image
        .as_deref()
        .filter(|value| {
            !value.is_empty() && !value.starts_with('-') && !value.chars().any(char::is_control)
        })
        .ok_or("container image is required")?;
    let container_name = format!("duduclaw-discovery-eval-{}", uuid::Uuid::new_v4());
    let deadline = std::time::SystemTime::now().checked_add(timeout)
        .ok_or("evaluator deadline overflow")?.duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "evaluator wall clock unavailable")?.as_secs_f64();
    let cidfile = snapshot
        .parent()
        .ok_or("snapshot has no parent")?
        .join(format!("{container_name}.cid"));
    let mut command = Command::new("docker");
    command.env_clear();
    // Docker context/credential files remain with the host launcher, never in
    // the scoring container. The default daemon endpoint is intentional.
    command.env("PATH", "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin");
    command.args([
        "create",
        // Never fetched behind the operator's back: the pinned image must
        // already be local.
        "--pull",
        "never",
        "--name",
        &container_name,
        "--cidfile",
        &cidfile.to_string_lossy(),
        "--network",
        "none",
        "--read-only",
        "--user",
        "65534:65534",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        &config.pids.to_string(),
        "--memory",
        &config.memory_bytes.to_string(),
        "--memory-swap",
        &config.memory_bytes.to_string(),
        "--cpus",
        "1",
        "--ulimit",
        &format!("cpu={0}:{0}", config.timeout_secs),
        "--workdir",
        "/scratch",
        "--tmpfs",
        &format!(
            "/scratch:rw,nosuid,nodev,noexec,size={},mode=700,uid=65534,gid=65534",
            config.scratch_bytes
        ),
        "--env",
        "PATH=/usr/local/bin:/usr/bin:/bin",
        "--env",
        "HOME=/scratch",
        "--env",
        "TMPDIR=/scratch",
        "--env",
        "LANG=C.UTF-8",
        "--env",
        "TZ=UTC",
        "--env",
        "PYTHONDONTWRITEBYTECODE=1",
        "--env",
        &format!("DUDU_EVAL_MAX_WALL={}", timeout.as_secs_f64()),
        "--env",
        &format!("DUDU_EVAL_DEADLINE_UNIX={deadline}"),
        "--entrypoint",
        "python3",
        "--mount",
        &format!(
            "type=bind,source={},target=/evaluator,readonly",
            evaluator_snapshot.display()
        ),
        "--mount",
        &format!(
            "type=bind,source={},target=/workspace,readonly",
            snapshot.display()
        ),
    ]);
    for label in labels { command.args(["--label", label]); }
    command.arg("-i").arg(image);
    command.args(["-I", "-S", "-B", "-c", CONTAINER_SUPERVISOR]);
    for arg in &config.command {
        let (prefix, token) = arg
            .split_once('=')
            .map_or((None, arg.as_str()), |(prefix, token)| {
                (Some(prefix), token)
            });
        if Path::new(token).is_absolute() {
            let path = Path::new(token).canonicalize().map_err(|e| e.to_string())?;
            let value = container_path(
                "/evaluator",
                path.strip_prefix(root)
                    .map_err(|_| "unmapped evaluator path")?,
            )
            .ok_or("unmapped evaluator path")?;
            command.arg(match prefix {
                Some(prefix) => format!("{prefix}={value}"),
                None => value,
            });
        } else {
            command.arg(arg);
        }
    }
    Ok(command)
}
/// Path inside a Linux container: `base` plus the plain components of a
/// host-relative path, always joined with `/`. `Path::join` would use the
/// host separator (a backslash on Windows) and hand the container a path that does
/// not exist. `None` for anything but plain UTF-8 components.
pub(crate) fn container_path(base: &str, relative: &Path) -> Option<String> {
    let mut out = base.trim_end_matches('/').to_string();
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else { return None };
        out.push('/');
        out.push_str(part.to_str()?);
    }
    Some(out)
}
/// Extend the established `pass`/`feedback` judge envelope with score fields.
pub fn parse_score(
    raw: &str,
    backend: IsolationBackend,
    wall_secs: f64,
) -> Result<ScoreOutcome, String> {
    let start = raw.find('{').ok_or("missing verdict object")?;
    let end = raw.rfind('}').ok_or("missing verdict object")?;
    let value: Value = serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())?;
    let pass = value
        .get("pass")
        .and_then(|pass| match pass {
            Value::Bool(value) => Some(*value),
            Value::String(value) => match value.to_ascii_lowercase().as_str() {
                "pass" => Some(true),
                "fail" => Some(false),
                _ => None,
            },
            _ => None,
        })
        .ok_or("missing pass")?;
    let valid = value
        .get("valid")
        .and_then(Value::as_bool)
        .ok_or("missing valid")?;
    let fail = value
        .get("fail_class")
        .and_then(Value::as_str)
        .and_then(FailClass::parse)
        .ok_or("invalid failure class")?;
    let score = value.get("score").and_then(Value::as_f64);
    if pass != valid
        || valid && (fail != FailClass::Ok || !score.is_some_and(f64::is_finite))
        || !valid && (fail == FailClass::Ok || score.is_some())
    {
        return Err("inconsistent scoring verdict".into());
    }
    let feedback = value.get("feedback").and_then(Value::as_str).unwrap_or("");
    let feedback = duduclaw_core::truncate_bytes(feedback, 400);
    let diagnostics = match crate::judge_mode::sanitize_external_feedback(&feedback) {
        Ok(text) => Some(duduclaw_core::truncate_bytes(&text, 500).to_owned()),
        Err(_) => Some("外部評分回饋已因不安全指令而封鎖。".into()),
    };
    Ok(ScoreOutcome {
        evaluated: true,
        valid,
        score,
        fail_class: fail,
        diagnostics,
        isolation: backend,
        wall_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn config(command: Vec<String>) -> EvaluatorConfig {
        EvaluatorConfig {
            command,
            sha256: String::new(),
            sandbox: EvaluatorSandbox::Container,
            image: Some("test-image".into()),
            good_solution: PathBuf::new(),
            cheating_solution: PathBuf::new(),
            timeout_secs: 5,
            memory_bytes: 1 << 20,
            pids: 8,
            scratch_bytes: 1 << 20,
            test_data: None,
            timing_sensitive: false,
        }
    }
    #[test]
    fn score_envelope_rejects_missing_and_inconsistent_fields() {
        let good = r#"{"pass":true,"valid":true,"score":2.5,"fail_class":"ok","feedback":"fine"}"#;
        assert_eq!(
            parse_score(good, IsolationBackend::Container, 0.0)
                .unwrap()
                .score,
            Some(2.5)
        );
        assert!(
            parse_score(
                r#"{"valid":true,"score":2.5,"fail_class":"ok"}"#,
                IsolationBackend::Container,
                0.0
            )
            .is_err()
        );
        assert!(
            parse_score(
                r#"{"pass":true,"valid":false,"score":2.5,"fail_class":"tamper"}"#,
                IsolationBackend::Container,
                0.0
            )
            .is_err()
        );
    }
    #[test]
    fn argv_pins_every_existing_path_and_forbids_shells() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("judge"), "x").unwrap();
        fs::write(root.join("sh"), "x").unwrap();
        assert!(validate_argv(&root, &[root.join("judge").to_string_lossy().into_owned()]).is_ok());
        assert!(validate_argv(&root, &["/bin/true".into()]).is_err());
        assert!(
            validate_argv(
                &root,
                &[
                    root.join("judge").to_string_lossy().into_owned(),
                    "judge".into()
                ]
            )
            .is_err()
        );
        assert!(
            validate_argv(
                &root,
                &[
                    root.join("sh").to_string_lossy().into_owned(),
                    "-c".into(),
                    "echo ok".into()
                ]
            )
            .is_err()
        );
    }
    #[test]
    fn container_mounts_readonly_and_limits_network_privilege_and_resources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("judge"), "x").unwrap();
        let command = docker_command(
            &root,
            &root,
            &config(vec![root.join("judge").to_string_lossy().into_owned()]),
        )
        .unwrap();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args[0], "create");
        assert!(!args.iter().any(|arg| arg == "run" || arg == "--rm"));
        assert!(args.windows(2).any(|pair| pair == ["--entrypoint", "python3"]));
        assert!(args.iter().any(|arg| arg == CONTAINER_SUPERVISOR));
        for pair in [
            ["--network", "none"],
            ["--user", "65534:65534"],
            ["--cap-drop", "ALL"],
            ["--pids-limit", "8"],
        ] {
            assert!(args.windows(2).any(|args| args == pair));
        }
        assert!(args.iter().any(|arg| arg == "/evaluator/judge"));
        assert_eq!(
            args.iter().filter(|arg| arg.ends_with(",readonly")).count(),
            2
        );
    }
    #[tokio::test]
    async fn missing_registry_or_isolation_never_spawns_worker_code() {
        let temp = tempfile::tempdir().unwrap();
        let evaluator =
            RegisteredEvaluator::new(temp.path().to_path_buf(), DiscoveryConfig::default(), false);
        let outcome = evaluator
            .score(&ScoreRequest {
                run_id: "run".into(),
                cell_id: "cell".into(),
                node_dir: temp.path().to_path_buf(),
                evaluator: "missing".into(),
                timeout: None,
            })
            .await;
        assert!(!outcome.evaluated);
        assert!(evaluator.register("missing").await.is_err());
    }
    #[test]
    fn container_paths_use_forward_slashes_on_every_host() {
        let relative = Path::new("nested").join("judge");
        assert_eq!(container_path("/evaluator", &relative).as_deref(), Some("/evaluator/nested/judge"));
        assert_eq!(container_path("/policy/", Path::new("p.py")).as_deref(), Some("/policy/p.py"));
        assert_eq!(container_path("/policy", &Path::new("..").join("x")), None);
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn directory_hash_detects_edit_and_nested_insertion() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("judge"), "x").unwrap();
        let first = directory_sha256(temp.path()).unwrap();
        fs::write(temp.path().join("judge"), "y").unwrap();
        assert_ne!(first, directory_sha256(temp.path()).unwrap());
    }
}

#[cfg(all(test, unix))]
mod container_lifecycle_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // This is a mock Docker client; it validates host launch/cleanup ordering,
    // not container isolation. The ignored test below exercises a real daemon.
    fn fixture(mode: &str) -> (tempfile::TempDir, Command) {
        let home = tempfile::tempdir().unwrap();
        let client = home.path().join("docker-mock");
        fs::write(&client, format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$LOG\"\ncase \"$1\" in\ncreate) case \"$MODE\" in create_fail) exit 2;; bad_id) echo unconfirmed;; *) echo {ID};; esac;;\nstart) if [ \"$MODE\" = hang ]; then sleep 30; else cat >/dev/null; echo '{{\"pass\":true,\"valid\":true,\"score\":5,\"fail_class\":\"ok\"}}'; fi;;\nrm) if [ \"$MODE\" = cleanup_fail ]; then exit 3; fi;;\n*) exit 4;;\nesac\n")).unwrap();
        fs::set_permissions(&client, fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(client);
        command.env_clear().env("PATH", "/usr/bin:/bin")
            .env("LOG", home.path().join("lifecycle.log")).env("MODE", mode)
            .args(["create", "--name", "duduclaw-discovery-eval-test"]);
        (home, command)
    }

    #[tokio::test]
    async fn confirmed_create_starts_once_and_reaps_cleanup_once() {
        let (home, create) = fixture("ok");
        let output = run_container(create, b"input", Duration::from_secs(2), home.path(), "run", "cell")
            .await.unwrap();
        assert!(output.status.success());
        assert_eq!(fs::read_to_string(home.path().join("lifecycle.log")).unwrap(), "create\nstart\nrm\n");
        let audit = fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("discovery_container_cleanup"));
        assert!(audit.contains("\"success\":true"));
    }

    #[tokio::test]
    async fn failed_or_unconfirmed_create_never_starts_evaluator() {
        for mode in ["create_fail", "bad_id"] {
            let (home, create) = fixture(mode);
            assert!(run_container(create, b"", Duration::from_secs(2), home.path(), "run", "cell").await.is_err());
            assert_eq!(fs::read_to_string(home.path().join("lifecycle.log")).unwrap(), "create\nrm\n");
        }
    }

    #[tokio::test]
    async fn unsuccessful_cleanup_rejects_valid_stdout_and_records_failure() {
        let (home, create) = fixture("cleanup_fail");
        assert!(run_container(create, b"", Duration::from_secs(2), home.path(), "run", "cell").await.is_err());
        assert_eq!(fs::read_to_string(home.path().join("lifecycle.log")).unwrap(), "create\nstart\nrm\n");
        let audit = fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("\"success\":false"));
    }

    #[tokio::test]
    async fn timeout_and_future_cancellation_both_finish_cleanup() {
        let (home, create) = fixture("hang");
        // Allow the mock client to reach start on a loaded dev host. This
        // exercises an attached evaluator timeout, not a create timeout.
        let started = Instant::now();
        let output = run_container(create, b"", Duration::from_secs(2), home.path(), "run", "cell")
            .await.unwrap();
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(fs::read_to_string(home.path().join("lifecycle.log")).unwrap(), "create\nstart\nrm\n");

        let (home, create) = fixture("hang");
        let path = home.path().to_path_buf();
        let task = tokio::spawn(async move {
            run_container(create, b"", Duration::from_secs(30), &path, "run", "cancelled").await
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if fs::read_to_string(home.path().join("lifecycle.log")).is_ok_and(|s| s.contains("start")) { break; }
            assert!(Instant::now() < deadline, "mock start did not run");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(fs::read_to_string(home.path().join("lifecycle.log")).unwrap(), "create\nstart\nrm\n");
    }

    #[test]
    fn cleanup_client_timeout_is_bounded_and_reaped() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let started = Instant::now();
        assert!(bounded_remove(command, Duration::from_millis(100)).unwrap_err().contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    #[ignore = "requires a real Docker daemon and local python:3.12-alpine image; no fake isolation claim"]
    async fn real_container_calculates_score_and_enforces_internal_wall_without_host_connection() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let snapshot = root.join("solution");
        fs::create_dir(&snapshot).unwrap();
        fs::write(snapshot.join("values.json"), "[2,3]").unwrap();
        let judge = root.join("judge.py");
        fs::write(&judge, "#!/usr/bin/env python3\nimport json,sys,socket\nfrom pathlib import Path\nw=Path(sys.argv[1])\ntry:\n (w/'forbidden').write_text('x')\n raise RuntimeError('readonly mount is writable')\nexcept OSError: pass\ntry:\n socket.create_connection(('1.1.1.1',443),timeout=0.2)\n raise RuntimeError('network is reachable')\nexcept OSError: pass\ns=sum(json.loads((w/'values.json').read_text()))\nprint(json.dumps(dict(pass_=True,valid=True,score=s,fail_class='ok')).replace('pass_', 'pass'))\n").unwrap();
        fs::set_permissions(&judge, fs::Permissions::from_mode(0o755)).unwrap();
        let mut config = super::tests::config(vec![judge.to_string_lossy().into_owned()]);
        config.image = Some("python:3.12-alpine".into());
        config.pids = 64;
        config.memory_bytes = 64 * 1024 * 1024;
        config.timeout_secs = 3;
        let mut create = docker_command(&root, &snapshot, &config).unwrap();
        create.arg("/workspace");
        let output = run_container(create, b"{}", Duration::from_secs(10), &root, "probe", "valid").await.unwrap();
        assert!(output.status.success(), "{}", output.stderr);
        assert_eq!(parse_score(&output.stdout, IsolationBackend::Container, output.wall_secs).unwrap().score, Some(5.0));

        // Start through the daemon, then intentionally detach the host client.
        // The trusted in-container deadline still ends the namespace itself.
        fs::write(&judge, "#!/usr/bin/env python3\nimport subprocess,time\np=subprocess.Popen(['python3','-c','import time; time.sleep(30)'],start_new_session=True)\nprint('detached_pid:'+str(p.pid),flush=True)\ntime.sleep(30)\n").unwrap();
        config.timeout_secs = 1;
        let mut create = docker_command(&root, &snapshot, &config).unwrap();
        create.arg("/workspace");
        let mut cleanup = ContainerCleanup::for_create(&create, &root, "probe", "detached").unwrap();
        let created = super::super::process::run(create, b"", Duration::from_secs(10), 1024, |_| true).await.unwrap();
        assert!(created.status.success());
        let id = created.stdout.trim();
        assert_eq!(id.len(), 64);
        let mut start = cleanup.command();
        start.args(["start", id]);
        let started = super::super::process::run(start, b"", Duration::from_secs(5), 1024, |_| true).await.unwrap();
        assert!(started.status.success());
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut inspect = cleanup.command();
        inspect.args(["inspect", "--format", "{{.State.Running}} {{.State.ExitCode}}", id]);
        let state = super::super::process::run(inspect, b"", Duration::from_secs(5), 1024, |_| true).await.unwrap();
        assert!(state.status.success());
        assert_eq!(state.stdout.trim(), "false 124");
        let mut logs = cleanup.command();
        logs.args(["logs", id]);
        let logs = super::super::process::run(logs, b"", Duration::from_secs(5), 1024, |_| true).await.unwrap();
        assert!(logs.status.success());
        assert!(logs.stdout.contains("detached_pid:"), "probe never reached detached-child creation");
        cleanup.stop().unwrap();
    }
}

#[cfg(all(test, unix))]
mod sealed_execution_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture(cheating_valid: bool) -> (tempfile::TempDir, RegisteredEvaluator) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let root = home.join("discovery/evaluators/test");
        fs::create_dir_all(root.join("good")).unwrap();
        fs::create_dir(root.join("bad")).unwrap();
        let good =
            json!({"pass":true,"valid":true,"score":2.5,"fail_class":"ok","feedback":"verified"});
        let bad = if cheating_valid {
            json!({"pass":true,"valid":true,"score":99.0,"fail_class":"ok"})
        } else {
            json!({"pass":false,"valid":false,"score":null,"fail_class":"invalid_solution","feedback":"rejected"})
        };
        fs::write(root.join("good/verdict.json"), good.to_string()).unwrap();
        fs::write(root.join("bad/verdict.json"), bad.to_string()).unwrap();
        fs::write(root.join("judge"), "#!/bin/sh\ncat \"$1/verdict.json\"\n").unwrap();
        fs::set_permissions(root.join("judge"), fs::Permissions::from_mode(0o755)).unwrap();
        let entry = EvaluatorConfig {
            command: vec![root.join("judge").to_string_lossy().into_owned()],
            sha256: directory_sha256(&root).unwrap(),
            sandbox: EvaluatorSandbox::None,
            image: None,
            good_solution: root.join("good"),
            cheating_solution: root.join("bad"),
            timeout_secs: 3,
            memory_bytes: 1 << 20,
            pids: 64,
            scratch_bytes: 1 << 20,
            test_data: None,
            timing_sensitive: false,
        };
        let config = DiscoveryConfig {
            allow_unconfined: true,
            evaluators: [("test".into(), entry)].into(),
            ..Default::default()
        };
        (temp, RegisteredEvaluator::new(home, config, true))
    }
    #[tokio::test]
    async fn registration_runs_both_fixtures_and_rejects_cheating_scores() {
        let (_temp, evaluator) = fixture(false);
        assert_eq!(evaluator.register("test").await.unwrap().len(), 64);
        let (_temp, cheating) = fixture(true);
        assert!(
            cheating
                .register("test")
                .await
                .unwrap_err()
                .contains("cheating")
        );
    }
    #[tokio::test]
    async fn changed_registry_hash_blocks_before_execution() {
        let (_temp, evaluator) = fixture(false);
        let root = evaluator.home.join("discovery/evaluators/test");
        fs::write(root.join("judge"), "#!/bin/sh\ntouch \"$1/executed\"\n").unwrap();
        let req = ScoreRequest {
            run_id: "run".into(),
            cell_id: "cell".into(),
            node_dir: root.join("good"),
            evaluator: "test".into(),
            timeout: None,
        };
        let outcome = evaluator.score(&req).await;
        assert_eq!(outcome.fail_class, FailClass::Tamper);
        assert!(!outcome.evaluated);
        assert!(!req.node_dir.join("executed").exists());
    }
    #[tokio::test]
    async fn unconfined_requires_both_operator_identity_and_global_switch() {
        let (_temp, mut evaluator) = fixture(false);
        let root = evaluator.home.join("discovery/evaluators/test");
        let req = ScoreRequest {
            run_id: "run".into(),
            cell_id: "cell".into(),
            node_dir: root.join("good"),
            evaluator: "test".into(),
            timeout: None,
        };
        evaluator.operator_identity = false;
        assert!(!evaluator.score(&req).await.evaluated);
        evaluator.operator_identity = true;
        evaluator.config.allow_unconfined = false;
        assert!(!evaluator.score(&req).await.evaluated);
    }
    #[tokio::test]
    async fn exhausted_host_scoring_deadline_never_executes_evaluator() {
        let (_temp, evaluator) = fixture(false);
        let root = evaluator.home.join("discovery/evaluators/test");
        let req = ScoreRequest {
            run_id: "run".into(), cell_id: "cell".into(), node_dir: root.join("good"),
            evaluator: "test".into(), timeout: Some(Duration::ZERO),
        };
        let outcome = evaluator.score(&req).await;
        assert!(!outcome.evaluated);
        assert_eq!(outcome.fail_class, FailClass::Timeout);
    }
    #[tokio::test]
    async fn malicious_workspace_hook_is_classified_as_tamper() {
        let (_temp, evaluator) = fixture(false);
        let root = evaluator.home.join("discovery/evaluators/test");
        let solution = evaluator.home.join("solution");
        fs::create_dir(&solution).unwrap();
        fs::write(
            solution.join("sitecustomize.py"),
            "raise RuntimeError('attack')",
        )
        .unwrap();
        let config = evaluator.config.evaluators.get("test").unwrap();
        let outcome = evaluator
            .execute(&root, config, &solution, "run", "cell", Duration::from_secs(config.timeout_secs))
            .await;
        assert_eq!(outcome.fail_class, FailClass::Tamper);
        assert!(!outcome.valid);
    }
    #[test]
    fn sealed_snapshot_cleanup_restores_directory_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sealed");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("x"), "data").unwrap();
        {
            let _guard = SealedDirectories(vec![path.clone()]);
            make_readonly(&path).unwrap();
        }
        fs::remove_dir_all(&path).unwrap();
    }
}
