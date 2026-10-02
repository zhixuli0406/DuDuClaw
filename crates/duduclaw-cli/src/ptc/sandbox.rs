//! PTC Sandbox — execute agent-submitted scripts (`execute_program`).
//!
//! [`PtcSandbox::run_program`] is the entry point. It runs the script in an
//! isolated container (the script sandbox: Docker / WSL2, read-only rootfs,
//! `--network=none`, non-root, memory / pids / CPU ceilings, a hard
//! deadline). When the container cannot run, it honours the script
//! sandbox's own switch, `config.toml [container.sandbox]
//! script_when_unavailable` (separate from the task sandbox's
//! `when_unavailable`, which has no effect here): `"fail"` (default)
//! refuses with the reason and the `docker pull <image>` remedy and audits
//! `script_sandbox_unavailable`; `"run_unsandboxed"` runs the script as a
//! host subprocess ([`PtcSandbox::execute`]) and audits
//! `script_sandbox_bypassed` every time.
//!
//! Scripts get no tool callbacks: nothing serves an RPC bridge, and only the
//! script itself is written to the scratch directory. `PtcRpcServer` is a
//! leftover descriptor whose only remaining use is the (always 0)
//! `tool_calls_count`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use duduclaw_core::error::{DuDuClawError, Result};

use super::types::{ScriptLanguage, ScriptRequest, ScriptResult};

// ── PTC RPC Server ─────────────────────────────────────────────

/// Descriptor of a JSON-RPC bridge for MCP tool calls from scripts that was
/// never served: it has no listener, nothing mounts or advertises its
/// socket, and `call_count` stays 0.
pub struct PtcRpcServer {
    socket_path: PathBuf,
    call_count: Arc<AtomicU64>,
}

impl PtcRpcServer {
    /// Create the descriptor. Nothing listens on `socket_path`.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            call_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Return the cumulative number of tool calls handled.
    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::Relaxed)
    }
}

impl Drop for PtcRpcServer {
    fn drop(&mut self) {
        // Best-effort cleanup on drop
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

// ── Helpers ────────────────────────────────────────────────────

/// Truncate a `String` safely at a UTF-8 char boundary.
///
/// Returns `true` if the string was actually truncated.
/// Plain `String::truncate(n)` panics when `n` falls inside a multi-byte
/// character (common with CJK text). This finds the largest valid boundary
/// at or below `max_bytes`.
fn safe_truncate_string(s: &mut String, max_bytes: usize) -> bool {
    if s.len() <= max_bytes {
        return false;
    }
    let boundary = (0..=max_bytes)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    s.truncate(boundary);
    s.push_str("\n...[truncated]");
    true
}

// ── PTC Sandbox ────────────────────────────────────────────────

/// Python interpreter inside the script-sandbox container, looked up through
/// the image's `PATH` (`/usr/local/bin/python3` in the platform image,
/// `/usr/bin/python3` in a distro image). Shared with the secaudit PoC step.
pub(crate) const CONTAINER_PYTHON: &str = "python3";

/// Sandbox executor for PTC scripts.
pub struct PtcSandbox;

impl PtcSandbox {
    /// Execute a script as a direct child process (no container isolation).
    /// Reached only through `script_when_unavailable = "run_unsandboxed"`.
    pub async fn execute(req: &ScriptRequest, rpc_server: &PtcRpcServer) -> Result<ScriptResult> {
        let start = std::time::Instant::now();

        // Private (owner-only), randomly named, created exclusively; removed
        // when `scratch` drops, on every path.
        let scratch = private_script_dir("duduclaw_ptc_")
            .map_err(|e| DuDuClawError::Agent(format!("Failed to create the script directory: {e}")))?;
        let tmp_dir = scratch.path().to_path_buf();

        let (script_path, program, args) = match req.language {
            ScriptLanguage::Python => {
                let path = tmp_dir.join("script.py");
                std::fs::write(&path, &req.script)
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                (
                    path.clone(),
                    duduclaw_core::platform::python3_command().to_string(),
                    vec![path.to_string_lossy().to_string()],
                )
            }
            ScriptLanguage::Bash => {
                #[cfg(not(windows))]
                {
                    let path = tmp_dir.join("script.sh");
                    std::fs::write(&path, &req.script).map_err(|e| {
                        DuDuClawError::Agent(format!("Failed to write script: {e}"))
                    })?;
                    (
                        path.clone(),
                        "bash".to_string(),
                        vec![path.to_string_lossy().to_string()],
                    )
                }
                #[cfg(windows)]
                {
                    let path = tmp_dir.join("script.cmd");
                    std::fs::write(&path, &req.script).map_err(|e| {
                        DuDuClawError::Agent(format!("Failed to write script: {e}"))
                    })?;
                    (
                        path.clone(),
                        "cmd".to_string(),
                        vec!["/C".to_string(), path.to_string_lossy().to_string()],
                    )
                }
            }
        };

        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        let mut child = tokio::process::Command::new(&program)
            .args(&args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(
                "HOME",
                std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_default(),
            )
            .env(
                "USERPROFILE",
                std::env::var("USERPROFILE")
                    .or_else(|_| std::env::var("HOME"))
                    .unwrap_or_default(),
            )
            .env("LANG", std::env::var("LANG").unwrap_or_default())
            .env("PYTHONUNBUFFERED", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| DuDuClawError::Agent(format!("Failed to spawn {program}: {e}")))?;

        // Take stdout/stderr handles before waiting so we retain ownership of `child` for kill()
        let mut child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();

        // HC4: drain stdout+stderr CONCURRENTLY with the wait. Reading only after
        // `child.wait()` deadlocks once the child writes more than the OS pipe
        // buffer (~64KB): the child blocks on a full pipe waiting for us to read,
        // while we block in wait() waiting for the child to exit. tokio::join!
        // on (wait, drain_stdout, drain_stderr) keeps the pipes draining.
        let read_limit = req.max_output_bytes as u64 + 1024;
        let drain = |handle: Option<tokio::process::ChildStdout>| async move {
            let mut buf = Vec::with_capacity(65536);
            if let Some(out) = handle {
                use tokio::io::AsyncReadExt as _;
                let mut limited = out.take(read_limit);
                let _ = limited.read_to_end(&mut buf).await;
            }
            buf
        };
        let drain_err = |handle: Option<tokio::process::ChildStderr>| async move {
            let mut buf = Vec::with_capacity(4096);
            if let Some(err) = handle {
                use tokio::io::AsyncReadExt as _;
                let mut limited = err.take(read_limit);
                let _ = limited.read_to_end(&mut buf).await;
            }
            buf
        };

        let result = tokio::time::timeout(timeout, async {
            let (status, raw_stdout, raw_stderr) = tokio::join!(
                child.wait(),
                drain(child_stdout.take()),
                drain_err(child_stderr.take()),
            );
            status.map(|s| (s, raw_stdout, raw_stderr))
        })
        .await;

        // Cleanup temp files
        let _ = std::fs::remove_file(&script_path);
        drop(scratch);

        let execution_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok((status, raw_stdout, raw_stderr))) => {
                let mut stdout = String::from_utf8_lossy(&raw_stdout).to_string();
                let stderr = String::from_utf8_lossy(&raw_stderr).to_string();
                let exit_code = status.code().unwrap_or(-1);

                let truncated = safe_truncate_string(&mut stdout, req.max_output_bytes);

                Ok(ScriptResult {
                    stdout,
                    stderr,
                    exit_code,
                    tool_calls_count: rpc_server.call_count(),
                    execution_ms,
                    truncated,
                })
            }
            Ok(Err(e)) => Err(DuDuClawError::Agent(format!(
                "Script execution failed: {e}"
            ))),
            Err(_) => {
                // CRITICAL: Kill child process on timeout to prevent orphaned processes
                let _ = child.kill().await;
                let _ = child.wait().await; // Reap zombie process
                Ok(ScriptResult {
                    stdout: String::new(),
                    stderr: "Script execution timed out".to_string(),
                    exit_code: 124,
                    tool_calls_count: rpc_server.call_count(),
                    execution_ms,
                    truncated: false,
                })
            }
        }
    }

    /// Run `req`: in the script sandbox when it can run, otherwise as
    /// `config.toml [container.sandbox] script_when_unavailable` says (see
    /// the module doc). `agent_id` names the caller in the audit events.
    pub async fn run_program(
        req: &ScriptRequest,
        rpc_server: &PtcRpcServer,
        home: &Path,
        agent_id: &str,
    ) -> Result<ScriptResult> {
        use duduclaw_gateway::task_sandbox::settings::{self, WhenUnavailable};
        // The script sandbox's own escape hatch, read on its own: it keeps
        // working while another key of the section is invalid. The task
        // sandbox's `when_unavailable` never applies here.
        let (loaded, when_unavailable) = settings::load_for_scripts(home);
        let reason = match loaded {
            Err(why) => ScriptSandboxUnavailable::InvalidConfig(why),
            Ok(settings) => match Self::execute_in_container(req, rpc_server, &settings.image).await {
                Ok(result) => return Ok(result),
                Err(ContainerOutcome::Failed(e)) => return Err(e),
                Err(ContainerOutcome::Unavailable(reason)) => reason,
            },
        };
        let language = match req.language {
            ScriptLanguage::Python => "python",
            ScriptLanguage::Bash => "bash",
        };
        script_sandbox_audit(home, "script_sandbox_unavailable", agent_id, serde_json::json!({
            "reason": reason.code(), "language": language, "script_when_unavailable": when_unavailable.as_str(),
        }));
        match when_unavailable {
            WhenUnavailable::Fail => Err(DuDuClawError::Agent(reason.message())),
            WhenUnavailable::RunUnsandboxed => {
                tracing::warn!(agent = %agent_id, reason = reason.code(),
                    "script sandbox unavailable; running the script UNSANDBOXED on the host (script_when_unavailable = run_unsandboxed)");
                script_sandbox_audit(home, "script_sandbox_bypassed", agent_id, serde_json::json!({
                    "reason": reason.code(), "language": language,
                }));
                Self::execute(req, rpc_server).await
            }
        }
    }

    /// Execute a script inside an isolated container running `image`.
    /// Never falls back to the host: every way the container cannot run is
    /// an [`ContainerOutcome::Unavailable`] for [`Self::run_program`] to
    /// decide on.
    async fn execute_in_container(
        req: &ScriptRequest,
        rpc_server: &PtcRpcServer,
        image: &str,
    ) -> std::result::Result<ScriptResult, ContainerOutcome> {
        use duduclaw_core::traits::ContainerRuntime;
        use duduclaw_core::types::{ContainerConfig, MountConfig};
        let unavailable = |reason| Err(ContainerOutcome::Unavailable(reason));

        let runtime = match duduclaw_container::RuntimeBackend::detect_with_image(image) {
            Ok(rt) => rt,
            Err(e) => return unavailable(ScriptSandboxUnavailable::NoRuntime(e.to_string(), image.to_string())),
        };
        match runtime.health_check().await {
            Ok(h) if h.healthy => {}
            Ok(h) => return unavailable(ScriptSandboxUnavailable::RuntimeUnhealthy(h.message, image.to_string())),
            Err(e) => return unavailable(ScriptSandboxUnavailable::RuntimeUnhealthy(e.to_string(), image.to_string())),
        }
        match runtime.image_present().await {
            Ok(true) => {}
            Ok(false) => return unavailable(ScriptSandboxUnavailable::ImageMissing(image.to_string())),
            Err(e) => return unavailable(ScriptSandboxUnavailable::RuntimeUnhealthy(e.to_string(), image.to_string())),
        }

        // The script in a private, randomly named directory handed to the
        // container user, removed when `scratch` drops on every path.
        let failed = |e: String| Err(ContainerOutcome::Failed(DuDuClawError::Agent(e)));
        let scratch = match container_script_dir("duduclaw_ptc_container_") {
            Ok(dir) => dir,
            Err(e) => return failed(format!("Failed to create the script directory: {e}")),
        };
        let tmp_dir = scratch.path();

        // The in-container path where the script is mounted, and the
        // program/args that run it.
        const CONTAINER_WORKSPACE: &str = "/workspace";
        let (script_name, container_cmd) = match req.language {
            ScriptLanguage::Python => (
                "script.py",
                // The container is Linux whatever the host is, so the host's
                // interpreter name (`python` on Windows) is wrong here;
                // resolved through the image PATH.
                vec![CONTAINER_PYTHON.to_string(), format!("{CONTAINER_WORKSPACE}/script.py")],
            ),
            ScriptLanguage::Bash => (
                "script.sh",
                vec!["bash".to_string(), format!("{CONTAINER_WORKSPACE}/script.sh")],
            ),
        };
        if let Err(e) = write_container_script(tmp_dir, script_name, &req.script) {
            return failed(format!("Failed to write script: {e}"));
        }

        // Container sandbox configuration:
        // - the private script directory read-only at /workspace;
        // - no PTC socket: nothing serves it (`PtcRpcServer` has no listener),
        //   and its directory would be the host's shared temp directory;
        // - --network=none, read-only rootfs.
        let container_config = ContainerConfig {
            timeout_ms: req.timeout_ms,
            max_concurrent: 1,
            readonly_project: true,
            additional_mounts: vec![MountConfig {
                host: tmp_dir.to_string_lossy().to_string(),
                container: CONTAINER_WORKSPACE.to_string(),
                readonly: true,
            }],
            sandbox_enabled: true,
            network_access: false, // --network=none
            cmd: container_cmd,
            env: vec![],
        };

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        let outcome = runtime.run_once(container_config, timeout).await;
        let execution_ms = start.elapsed().as_millis() as u64;
        drop(scratch);

        match outcome {
            Ok(exit) => {
                let mut stdout = exit.logs;
                let truncated = safe_truncate_string(&mut stdout, req.max_output_bytes);
                Ok(ScriptResult {
                    stdout,
                    stderr: String::new(),
                    exit_code: exit.exit_code as i32,
                    tool_calls_count: rpc_server.call_count(),
                    execution_ms,
                    truncated,
                })
            }
            Err(duduclaw_container::RunFailure::Create(e)) => {
                unavailable(ScriptSandboxUnavailable::CreateFailed(e.to_string(), image.to_string()))
            }
            Err(duduclaw_container::RunFailure::Start(e)) => {
                unavailable(ScriptSandboxUnavailable::StartFailed(e.to_string(), image.to_string()))
            }
            Err(duduclaw_container::RunFailure::Wait(e)) => {
                failed(format!("PTC container execution failed: {e}"))
            }
            Err(duduclaw_container::RunFailure::TimedOut) => Ok(ScriptResult {
                stdout: String::new(),
                stderr: "PTC container execution timed out".to_string(),
                exit_code: 124,
                tool_calls_count: rpc_server.call_count(),
                execution_ms,
                truncated: false,
            }),
        }
    }
}

/// Why the container path produced no result.
enum ContainerOutcome {
    /// The sandbox cannot run here (fail closed or escape hatch).
    Unavailable(ScriptSandboxUnavailable),
    /// The sandbox ran and the run failed.
    Failed(DuDuClawError),
}

/// Why the script sandbox cannot run. `code()` is the closed set used in
/// the `script_sandbox_unavailable` / `script_sandbox_bypassed` audit events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScriptSandboxUnavailable {
    /// `config.toml` or its `[container.sandbox]` section is invalid.
    InvalidConfig(String),
    /// No container runtime could be set up: `(why, image)`.
    NoRuntime(String, String),
    /// The runtime did not answer its health check: `(why, image)`.
    RuntimeUnhealthy(String, String),
    /// The image is not present locally (never pulled).
    ImageMissing(String),
    /// `create` failed: `(why, image)`.
    CreateFailed(String, String),
    /// `start` failed: `(why, image)`.
    StartFailed(String, String),
}

impl ScriptSandboxUnavailable {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::NoRuntime(..) => "no_runtime",
            Self::RuntimeUnhealthy(..) => "runtime_unhealthy",
            Self::ImageMissing(_) => "image_missing",
            Self::CreateFailed(..) => "create_failed",
            Self::StartFailed(..) => "start_failed",
        }
    }

    /// The tool error: the reason and the remedy. Detail strings are
    /// bounded; they come from the container runtime, never from the script.
    pub(crate) fn message(&self) -> String {
        let bounded = |s: &str| duduclaw_core::truncate_chars(s.trim(), 300).to_string();
        let remedy = |image: &str| {
            format!(
                "The script sandbox needs a running Docker and the image {image} present locally; \
                 run `docker pull {image}` (it is never pulled automatically)."
            )
        };
        let (why, remedy) = match self {
            Self::InvalidConfig(why) => (
                format!("config.toml [container.sandbox] is invalid: {}", bounded(why)),
                "Fix the [container.sandbox] section of config.toml.".to_string(),
            ),
            Self::NoRuntime(why, image) => (format!("no container runtime: {}", bounded(why)), remedy(image)),
            Self::RuntimeUnhealthy(why, image) => (format!("the container runtime is not reachable: {}", bounded(why)), remedy(image)),
            Self::ImageMissing(image) => (format!("the sandbox image {image} is not present locally"), remedy(image)),
            Self::CreateFailed(why, image) => (format!("the container could not be created: {}", bounded(why)), remedy(image)),
            Self::StartFailed(why, image) => (format!("the container could not be started: {}", bounded(why)), remedy(image)),
        };
        format!(
            "Script sandbox unavailable ({}): {why}. The script was not run. {remedy}",
            self.code()
        )
    }
}

fn script_sandbox_audit(home: &Path, kind: &str, agent_id: &str, details: serde_json::Value) {
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(kind, agent_id, duduclaw_security::audit::Severity::Warning, details),
    );
}

/// A private scratch directory for one script under the system temp
/// directory: a random name created exclusively (never an existing path),
/// mode 0700 and owned by this process, so other local users can neither
/// read nor swap the script. Removed when the guard drops. For a script the
/// host runs itself; a container mount uses [`container_script_dir`].
pub(crate) fn private_script_dir(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

/// The image's unprivileged user that a script-sandbox container runs as
/// when this process is root ([`duduclaw_core::sandbox_image::IMAGE_DEFAULT_USER`]).
#[cfg(unix)]
fn container_user_ids() -> Option<(u32, u32)> {
    let (uid, gid) = duduclaw_core::sandbox_image::IMAGE_DEFAULT_USER.split_once(':')?;
    Some((uid.parse().ok()?, gid.parse().ok()?))
}

#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// [`private_script_dir`] for a directory bind-mounted into a script-sandbox
/// container. Not root: the container runs as this process's uid, so 0700
/// is enough. Root: the container runs as the image's user (1000:1000), so
/// the directory is handed to that user and stays 0700; only if that chown
/// fails does it fall back to 0711 (enter, not list), with a warning.
pub(crate) fn container_script_dir(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    let dir = private_script_dir(prefix)?;
    #[cfg(unix)]
    if running_as_root() {
        use std::os::unix::fs::PermissionsExt;
        let chowned = container_user_ids()
            .ok_or_else(|| std::io::Error::other("invalid container user"))
            .and_then(|(uid, gid)| std::os::unix::fs::chown(dir.path(), Some(uid), Some(gid)));
        if let Err(error) = chowned {
            tracing::warn!(%error, "script sandbox: could not hand the script directory to the container user; using mode 0711");
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o711))?;
        }
    }
    Ok(dir)
}

/// Write a script into a [`container_script_dir`] directory. Not root: a
/// plain write (default mode). Root: created exclusively with mode 0600 and
/// handed to the container user; only if that chown fails is it made 0644
/// (readable by the container user), with a warning.
pub(crate) fn write_container_script(dir: &Path, name: &str, contents: &str) -> std::io::Result<()> {
    let path = dir.join(name);
    #[cfg(unix)]
    if running_as_root() {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        file.write_all(contents.as_bytes())?;
        let chowned = container_user_ids()
            .ok_or_else(|| std::io::Error::other("invalid container user"))
            .and_then(|(uid, gid)| std::os::unix::fs::fchown(&file, Some(uid), Some(gid)));
        if let Err(error) = chowned {
            tracing::warn!(%error, "script sandbox: could not hand the script to the container user; using mode 0644");
            file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
        return Ok(());
    }
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rpc_server_new() {
        let path = std::path::PathBuf::from("/tmp/test_ptc.sock");
        let server = PtcRpcServer::new(path.clone());
        assert_eq!(server.socket_path, path);
        assert_eq!(server.call_count(), 0);
    }

    #[test]
    fn test_script_request_serialization() {
        let req = ScriptRequest {
            script: "print('hello')".to_string(),
            language: ScriptLanguage::Python,
            timeout_ms: 30_000,
            max_output_bytes: 1024,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"language\":\"python\""));

        let deserialized: ScriptRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.language, ScriptLanguage::Python);
    }

    // ── B1 / B6 (2026-10-01) ─────────────────────────────────────

    fn bash_req(script: &str) -> ScriptRequest {
        ScriptRequest {
            script: script.to_string(),
            language: ScriptLanguage::Bash,
            timeout_ms: 10_000,
            max_output_bytes: 4096,
        }
    }

    fn audit_lines(home: &Path) -> String {
        std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default()
    }

    /// Default `script_when_unavailable = "fail"`: an unavailable sandbox refuses,
    /// names the reason, never runs the script on the host, and is audited.
    #[tokio::test]
    #[cfg(unix)]
    async fn unavailable_sandbox_refuses_by_default_and_is_audited() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[container.sandbox]\npids = 0\n").unwrap();
        let marker = home.path().join("ran-on-host");
        let server = PtcRpcServer::new(home.path().join("unused.sock"));
        let err = PtcSandbox::run_program(&bash_req(&format!("touch '{}'", marker.display())), &server, home.path(), "worker")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Script sandbox unavailable (invalid_config)"), "{err}");
        assert!(err.contains("not run"), "{err}");
        assert!(!marker.exists(), "the script must not run on the host");
        let audit = audit_lines(home.path());
        assert!(audit.contains("script_sandbox_unavailable") && audit.contains("invalid_config"), "{audit}");
        assert!(audit.contains("\"script_when_unavailable\":\"fail\""), "{audit}");
        assert!(!audit.contains("script_sandbox_bypassed"), "{audit}");
    }

    /// The task sandbox's `when_unavailable = "run_unsandboxed"` alone does
    /// NOT let a script run on the host: PTC reads only
    /// `script_when_unavailable`.
    #[tokio::test]
    #[cfg(unix)]
    async fn task_escape_hatch_does_not_open_the_script_sandbox() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[container.sandbox]\nwhen_unavailable = \"run_unsandboxed\"\npids = 0\n",
        )
        .unwrap();
        let marker = home.path().join("ran-on-host");
        let server = PtcRpcServer::new(home.path().join("unused.sock"));
        let err = PtcSandbox::run_program(&bash_req(&format!("touch '{}'", marker.display())), &server, home.path(), "worker")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Script sandbox unavailable (invalid_config)"), "{err}");
        assert!(!marker.exists(), "the script must not run on the host");
        let audit = audit_lines(home.path());
        assert!(audit.contains("\"script_when_unavailable\":\"fail\""), "{audit}");
        assert!(!audit.contains("script_sandbox_bypassed"), "{audit}");
    }

    /// `script_when_unavailable = "run_unsandboxed"` is honoured even while another
    /// key of the section is invalid: the host run happens and every bypass
    /// is audited.
    #[tokio::test]
    #[cfg(unix)]
    async fn escape_hatch_runs_on_the_host_and_audits_every_time() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[container.sandbox]\nscript_when_unavailable = \"run_unsandboxed\"\npids = 0\n",
        )
        .unwrap();
        let server = PtcRpcServer::new(home.path().join("unused.sock"));
        for _ in 0..2 {
            let result = PtcSandbox::run_program(&bash_req("echo host-run"), &server, home.path(), "worker")
                .await
                .unwrap();
            assert_eq!(result.exit_code, 0, "{result:?}");
            assert!(result.stdout.contains("host-run"), "{result:?}");
        }
        let audit = audit_lines(home.path());
        assert_eq!(audit.matches("script_sandbox_bypassed").count(), 2, "{audit}");
        assert_eq!(audit.matches("script_sandbox_unavailable").count(), 2, "{audit}");
        assert!(audit.contains("\"agent_id\":\"worker\""), "{audit}");
        assert!(audit.contains("\"script_when_unavailable\":\"run_unsandboxed\""), "{audit}");
    }

    #[test]
    fn unavailable_messages_name_the_docker_pull_remedy() {
        let image = "ghcr.io/zhixuli0406/duduclaw:v9.9.9";
        for reason in [
            ScriptSandboxUnavailable::ImageMissing(image.into()),
            ScriptSandboxUnavailable::NoRuntime("x".into(), image.into()),
            ScriptSandboxUnavailable::RuntimeUnhealthy("daemon down".into(), image.into()),
            ScriptSandboxUnavailable::CreateFailed("x".into(), image.into()),
            ScriptSandboxUnavailable::StartFailed("x".into(), image.into()),
        ] {
            let m = reason.message();
            assert!(m.contains(&format!("docker pull {image}")), "{m}");
            assert!(m.contains(&format!("({})", reason.code())), "{m}");
        }
        let m = ScriptSandboxUnavailable::InvalidConfig("bad".into()).message();
        assert!(m.contains("[container.sandbox]"), "{m}");
    }

    #[test]
    #[cfg(unix)]
    fn script_directory_is_private_random_and_removed() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let a = private_script_dir("duduclaw_ptc_").unwrap();
        let b = private_script_dir("duduclaw_ptc_").unwrap();
        assert_ne!(a.path(), b.path());
        let mode = std::fs::metadata(a.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists());

        // The container variant: still 0700 (handed to 1000:1000 as root),
        // and the script is never readable by other users when root.
        let c = container_script_dir("duduclaw_ptc_container_").unwrap();
        write_container_script(c.path(), "script.sh", "echo hi").unwrap();
        let dir_meta = std::fs::metadata(c.path()).unwrap();
        let file_meta = std::fs::metadata(c.path().join("script.sh")).unwrap();
        assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::read_to_string(c.path().join("script.sh")).unwrap(), "echo hi");
        if running_as_root() {
            assert_eq!((dir_meta.uid(), dir_meta.gid()), (1000, 1000));
            assert_eq!((file_meta.uid(), file_meta.gid()), (1000, 1000));
            assert_eq!(file_meta.permissions().mode() & 0o777, 0o600);
        } else {
            // SAFETY: geteuid has no preconditions and cannot fail.
            assert_eq!(dir_meta.uid(), unsafe { libc::geteuid() });
        }
        // The file is created exclusively: an existing name is an error.
        if running_as_root() {
            assert!(write_container_script(c.path(), "script.sh", "x").is_err());
        }
    }

    /// Real Docker: `execute_program`'s path runs the script in the
    /// container (not on the host) and, with the image missing, refuses with
    /// the `docker pull` remedy instead of falling back.
    /// `DUDU_TASK_SANDBOX_IMAGE=<image> cargo test -p duduclaw-cli --lib
    ///   --features app-compat -- --ignored script_sandbox_docker`
    #[tokio::test]
    #[ignore = "requires a Docker daemon + the sandbox image present locally"]
    async fn script_sandbox_docker_ptc_runs_in_the_container_or_refuses() {
        let image = std::env::var("DUDU_TASK_SANDBOX_IMAGE").expect("set DUDU_TASK_SANDBOX_IMAGE");
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), format!("[container.sandbox]\nimage = \"{image}\"\n")).unwrap();
        let server = PtcRpcServer::new(home.path().join("unused.sock"));
        let script = "echo in-container; echo \"socket=${DUDUCLAW_PTC_SOCKET:-none}\"; ls /run/duduclaw 2>/dev/null && echo run-dir-mounted; ls /workspace; uname -s";
        let result = PtcSandbox::run_program(&bash_req(script), &server, home.path(), "worker").await.unwrap();
        assert_eq!(result.exit_code, 0, "{result:?}");
        assert!(result.stdout.contains("in-container") && result.stdout.contains("Linux"), "{result:?}");
        assert!(result.stdout.contains("socket=none") && !result.stdout.contains("run-dir-mounted"), "{result:?}");
        assert!(result.stdout.contains("script.sh") && !result.stdout.contains("ptc_client"), "{result:?}");
        assert!(!audit_lines(home.path()).contains("script_sandbox"), "no unavailable/bypass event");

        let missing = "duduclaw-test-image-that-does-not-exist:v0.0.0";
        std::fs::write(home.path().join("config.toml"), format!("[container.sandbox]\nimage = \"{missing}\"\n")).unwrap();
        let err = PtcSandbox::run_program(&bash_req("echo should-not-run"), &server, home.path(), "worker")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("(image_missing)") && err.contains(&format!("docker pull {missing}")), "{err}");
        assert!(audit_lines(home.path()).contains("image_missing"));
    }
}
