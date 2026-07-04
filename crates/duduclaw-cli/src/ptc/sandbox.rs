//! PTC Sandbox — execute user scripts with MCP tool access via Unix Domain Socket RPC.
//!
//! Two execution modes:
//! - **Subprocess** (`execute`): Direct child process, used when no container runtime is available.
//! - **Container** (`execute_in_container`): Isolated Docker/Apple Container with read-only rootfs,
//!   `--network=none`, tmpfs workspace. Falls back to subprocess on failure.
//!
//! The `PtcRpcServer` listens on a UDS socket so scripts can call MCP tools
//! via a lightweight JSON-RPC protocol.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;


use duduclaw_core::error::{DuDuClawError, Result};

use super::types::{ScriptLanguage, ScriptRequest, ScriptResult};

// ── Client stub generators ─────────────────────────────────────

/// Return the Python client stub that scripts import to call MCP tools via UDS.
pub fn python_client_stub() -> &'static str {
    r#""""PTC client — call MCP tools from sandbox scripts via Unix Domain Socket."""
import json, os, socket

_SOCKET_PATH = os.environ.get("DUDUCLAW_PTC_SOCKET")
if not _SOCKET_PATH:
    raise RuntimeError("DUDUCLAW_PTC_SOCKET not set — PTC client must run inside DuDuClaw sandbox")

def call_tool(name: str, params: dict | None = None) -> dict:
    """Send a JSON-RPC request to the PTC RPC server and return the result."""
    payload = json.dumps({"jsonrpc": "2.0", "method": name, "params": params or {}, "id": 1})
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.connect(_SOCKET_PATH)
        s.sendall(payload.encode() + b"\n")
        data = b""
        while True:
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
    resp = json.loads(data)
    if "error" in resp:
        raise RuntimeError(resp["error"].get("message", "RPC error"))
    return resp.get("result", {})
"#
}

/// Return the shell client stub for calling MCP tools from sandboxed scripts.
///
/// On Unix, uses `socat` over a Unix Domain Socket.
/// On Windows, uses PowerShell with TCP (the RPC bridge listens on TCP there).
pub fn bash_client_stub() -> &'static str {
    #[cfg(not(windows))]
    {
        r#"#!/usr/bin/env bash
# PTC client — call MCP tools from sandbox scripts via Unix Domain Socket.
PTC_SOCKET="${DUDUCLAW_PTC_SOCKET:?DUDUCLAW_PTC_SOCKET not set — PTC client must run inside DuDuClaw sandbox}"

ptc_call() {
    local method="$1"; shift
    local params="${1:-{}}"
    local payload="{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"params\":$params,\"id\":1}"
    echo "$payload" | socat - UNIX-CONNECT:"$PTC_SOCKET"
}
"#
    }
    #[cfg(windows)]
    {
        r#"@echo off
REM PTC client — call MCP tools from sandbox scripts via TCP.
REM Expects DUDUCLAW_PTC_SOCKET set to host:port (e.g. 127.0.0.1:9321)
if not defined DUDUCLAW_PTC_SOCKET (
    echo ERROR: DUDUCLAW_PTC_SOCKET not set — PTC client must run inside DuDuClaw sandbox >&2
    exit /b 1
)
"#
    }
}

// ── PTC RPC Server ─────────────────────────────────────────────

/// A lightweight JSON-RPC server listening on a Unix Domain Socket.
///
/// Scripts running inside the sandbox connect to this socket to invoke
/// MCP tools (e.g., file read, web fetch) through the host process.
pub struct PtcRpcServer {
    socket_path: PathBuf,
    call_count: Arc<AtomicU64>,
}

impl PtcRpcServer {
    /// Create a new RPC server bound to the given socket path.
    ///
    /// The server does not start listening until `start()` is called.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            call_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Return the socket path this server listens on.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
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

/// Sandbox executor for PTC scripts.
pub struct PtcSandbox;

impl PtcSandbox {
    /// Execute a script as a direct child process (no container isolation).
    ///
    /// The script has access to the PTC RPC socket for MCP tool calls.
    pub async fn execute(
        req: &ScriptRequest,
        rpc_server: &PtcRpcServer,
    ) -> Result<ScriptResult> {
        let start = std::time::Instant::now();

        // Write script to a temporary file
        // Use PID + timestamp to make temp dir unpredictable (prevents symlink attacks)
        let tmp_dir = std::env::temp_dir().join(format!(
            "duduclaw_ptc_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&tmp_dir);

        let (script_path, program, args) = match req.language {
            ScriptLanguage::Python => {
                let path = tmp_dir.join("script.py");
                std::fs::write(&path, &req.script)
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                // Also write the PTC client stub alongside
                let client_path = tmp_dir.join("ptc_client.py");
                std::fs::write(&client_path, python_client_stub())
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write client stub: {e}")))?;
                (path.clone(), duduclaw_core::platform::python3_command().to_string(), vec![path.to_string_lossy().to_string()])
            }
            ScriptLanguage::Bash => {
                #[cfg(not(windows))]
                {
                    let path = tmp_dir.join("script.sh");
                    std::fs::write(&path, &req.script)
                        .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                    (path.clone(), "bash".to_string(), vec![path.to_string_lossy().to_string()])
                }
                #[cfg(windows)]
                {
                    let path = tmp_dir.join("script.cmd");
                    std::fs::write(&path, &req.script)
                        .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                    (path.clone(), "cmd".to_string(), vec!["/C".to_string(), path.to_string_lossy().to_string()])
                }
            }
        };

        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        let mut child = tokio::process::Command::new(&program)
            .args(&args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default())
            .env("USERPROFILE", std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default())
            .env("LANG", std::env::var("LANG").unwrap_or_default())
            .env("PYTHONUNBUFFERED", "1")
            .env("DUDUCLAW_PTC_SOCKET", rpc_server.socket_path().to_string_lossy().as_ref())
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
        let _ = std::fs::remove_dir_all(&tmp_dir);

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
            Ok(Err(e)) => Err(DuDuClawError::Agent(format!("Script execution failed: {e}"))),
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

    /// Execute a script inside an isolated container sandbox.
    ///
    /// Falls back to subprocess execution if no container runtime is available
    /// or if container creation/start fails.
    pub async fn execute_in_container(
        req: &ScriptRequest,
        rpc_server: &PtcRpcServer,
    ) -> Result<ScriptResult> {
        use duduclaw_core::traits::ContainerRuntime;
        use duduclaw_core::types::{ContainerConfig, MountConfig};

        // Try to detect available container runtime
        let runtime = match duduclaw_container::RuntimeBackend::detect() {
            Ok(rt) => rt,
            Err(_) => {
                tracing::debug!("No container runtime available, falling back to subprocess");
                return Self::execute(req, rpc_server).await;
            }
        };

        // Write script and client stubs to a temp directory for mounting
        // Use PID + timestamp to make temp dir unpredictable (prevents symlink attacks)
        let tmp_dir = std::env::temp_dir().join(format!(
            "duduclaw_ptc_container_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&tmp_dir);

        // The in-container path where the script + client stub are mounted, and
        // the program/args that run it.
        const CONTAINER_WORKSPACE: &str = "/workspace";
        let (container_script, container_cmd) = match req.language {
            ScriptLanguage::Python => {
                let path = tmp_dir.join("script.py");
                std::fs::write(&path, &req.script)
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                let client_path = tmp_dir.join("ptc_client.py");
                std::fs::write(&client_path, python_client_stub())
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write client stub: {e}")))?;
                (
                    format!("{CONTAINER_WORKSPACE}/script.py"),
                    vec![
                        duduclaw_core::platform::python3_command().to_string(),
                        format!("{CONTAINER_WORKSPACE}/script.py"),
                    ],
                )
            }
            ScriptLanguage::Bash => {
                let path = tmp_dir.join("script.sh");
                std::fs::write(&path, &req.script)
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                let client_path = tmp_dir.join("ptc_client.sh");
                std::fs::write(&client_path, bash_client_stub())
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write client stub: {e}")))?;
                (
                    format!("{CONTAINER_WORKSPACE}/script.sh"),
                    vec!["bash".to_string(), format!("{CONTAINER_WORKSPACE}/script.sh")],
                )
            }
        };
        let _ = &container_script;

        // Container sandbox configuration (HC5):
        // - Mount workspace directory read-only (/workspace holds the script).
        // - Bind-mount the directory containing the PTC UDS socket read-write so
        //   in-container scripts can reach the host RPC bridge.
        // - Set DUDUCLAW_PTC_SOCKET to the in-container socket path.
        // - Set `cmd` to actually run the mounted user script.
        // - --network=none (scripts must use MCP tools for network access),
        //   read-only rootfs.
        const CONTAINER_PTC_DIR: &str = "/run/duduclaw";
        let socket_host = rpc_server.socket_path();
        let socket_parent = socket_host
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/tmp".to_string());
        // Preserve the host socket file name so the in-container path is correct
        // even if the bridge does not use the canonical "ptc.sock" name.
        let socket_file = socket_host
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "ptc.sock".to_string());
        let container_socket = format!("{CONTAINER_PTC_DIR}/{socket_file}");
        let container_config = ContainerConfig {
            timeout_ms: req.timeout_ms,
            max_concurrent: 1,
            readonly_project: true,
            additional_mounts: vec![
                MountConfig {
                    host: tmp_dir.to_string_lossy().to_string(),
                    container: CONTAINER_WORKSPACE.to_string(),
                    readonly: true,
                },
                // Bind-mount the directory containing the UDS socket read-write
                // (a socket needs rw to connect) at a fixed in-container path.
                MountConfig {
                    host: socket_parent,
                    container: CONTAINER_PTC_DIR.to_string(),
                    readonly: false,
                },
            ],
            sandbox_enabled: true,
            network_access: false, // --network=none
            worktree_enabled: false,
            worktree_auto_merge: true,
            worktree_cleanup_on_exit: true,
            worktree_copy_files: vec![],
            cmd: container_cmd,
            env: vec![("DUDUCLAW_PTC_SOCKET".to_string(), container_socket)],
        };

        let start = std::time::Instant::now();

        // Create container (fall back to subprocess on failure)
        let container_id = match runtime.create(container_config).await {
            Ok(id) => id,
            Err(e) => {
                tracing::debug!("Container creation failed: {e}, falling back to subprocess");
                let _ = std::fs::remove_dir_all(&tmp_dir);
                return Self::execute(req, rpc_server).await;
            }
        };

        tracing::info!(
            container = %container_id.0,
            socket = %rpc_server.socket_path().display(),
            network = "none",
            "PTC container sandbox created"
        );

        // Start container (fall back on failure)
        if let Err(e) = runtime.start(&container_id).await {
            tracing::warn!("Container start failed: {e}, falling back to subprocess");
            let _ = runtime.remove(&container_id).await;
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Self::execute(req, rpc_server).await;
        }

        // HC5: wait for the script to actually finish, then collect the real exit
        // code + logs. The container now runs the mounted user script (via
        // `config.cmd`) with `DUDUCLAW_PTC_SOCKET` pointing at the bind-mounted
        // UDS socket, so MCP tool calls work and the result is genuine.
        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        let wait_result = tokio::time::timeout(timeout, runtime.wait(&container_id)).await;

        let execution_ms = start.elapsed().as_millis() as u64;

        // Cleanup: stop, remove container, delete temp files
        let _ = runtime
            .stop(&container_id, std::time::Duration::from_secs(5))
            .await;
        let _ = runtime.remove(&container_id).await;
        let _ = std::fs::remove_dir_all(&tmp_dir);

        match wait_result {
            Ok(Ok(exit)) => {
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
            Ok(Err(e)) => Err(DuDuClawError::Agent(format!(
                "PTC container execution failed: {e}"
            ))),
            Err(_) => Ok(ScriptResult {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_client_stub_is_valid() {
        let stub = python_client_stub();
        assert!(stub.contains("call_tool"));
        assert!(stub.contains("DUDUCLAW_PTC_SOCKET"));
        assert!(stub.contains("jsonrpc"));
        // Must NOT have a fallback path
        assert!(!stub.contains("/tmp/ptc.sock"));
    }

    #[test]
    #[cfg(not(windows))]
    fn test_bash_client_stub_is_valid() {
        let stub = bash_client_stub();
        assert!(stub.contains("ptc_call"));
        assert!(stub.contains("DUDUCLAW_PTC_SOCKET"));
        assert!(stub.contains("UNIX-CONNECT"));
        // Must NOT have a fallback path
        assert!(!stub.contains("/tmp/ptc.sock"));
    }

    #[test]
    #[cfg(windows)]
    fn test_bash_client_stub_is_valid() {
        let stub = bash_client_stub();
        assert!(stub.contains("DUDUCLAW_PTC_SOCKET"));
        // Must NOT have a fallback path
        assert!(!stub.contains("/tmp/ptc.sock"));
    }

    #[test]
    fn test_rpc_server_new() {
        let path = std::path::PathBuf::from("/tmp/test_ptc.sock");
        let server = PtcRpcServer::new(path.clone());
        assert_eq!(server.socket_path(), path.as_path());
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
}
