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

use tokio::sync::oneshot;

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
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl PtcRpcServer {
    /// Create a new RPC server bound to the given socket path.
    ///
    /// The server does not start listening until `start()` is called.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            call_count: Arc::new(AtomicU64::new(0)),
            shutdown_tx: None,
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

    /// Explicitly stop the RPC server and clean up the socket file.
    ///
    /// Note: The server also stops automatically via the oneshot shutdown channel
    /// and the Drop trait, but this method provides explicit control.
    pub fn stop(&mut self) {
        // Send shutdown signal if the channel is still open
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Remove the socket file to prevent new connections
        let _ = std::fs::remove_file(&self.socket_path);
        tracing::debug!(path = %self.socket_path.display(), "PTC RPC server stopped");
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

        let result = tokio::time::timeout(timeout, child.wait()).await;

        // Cleanup temp files
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_dir_all(&tmp_dir);

        let execution_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(status)) => {
                // Read captured output with bounded limits to prevent OOM
                let read_limit = req.max_output_bytes as u64 + 1024;
                let mut raw_stdout = Vec::with_capacity(65536);
                let mut raw_stderr = Vec::with_capacity(4096);
                if let Some(ref mut out) = child_stdout {
                    use tokio::io::AsyncReadExt as _;
                    let mut limited = out.take(read_limit);
                    let _ = limited.read_to_end(&mut raw_stdout).await;
                }
                if let Some(ref mut err) = child_stderr {
                    use tokio::io::AsyncReadExt as _;
                    let mut limited = err.take(read_limit);
                    let _ = limited.read_to_end(&mut raw_stderr).await;
                }
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

        let _script_file = match req.language {
            ScriptLanguage::Python => {
                let path = tmp_dir.join("script.py");
                std::fs::write(&path, &req.script)
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                let client_path = tmp_dir.join("ptc_client.py");
                std::fs::write(&client_path, python_client_stub())
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write client stub: {e}")))?;
                path
            }
            ScriptLanguage::Bash => {
                let path = tmp_dir.join("script.sh");
                std::fs::write(&path, &req.script)
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write script: {e}")))?;
                let client_path = tmp_dir.join("ptc_client.sh");
                std::fs::write(&client_path, bash_client_stub())
                    .map_err(|e| DuDuClawError::Agent(format!("Failed to write client stub: {e}")))?;
                path
            }
        };

        // Container sandbox configuration:
        // - Mount workspace directory read-only
        // - --network=none (scripts must use MCP tools for network access)
        // - Read-only rootfs
        let container_config = ContainerConfig {
            timeout_ms: req.timeout_ms,
            max_concurrent: 1,
            readonly_project: true,
            additional_mounts: vec![MountConfig {
                host: tmp_dir.to_string_lossy().to_string(),
                container: "/workspace".to_string(),
                readonly: true,
            }],
            sandbox_enabled: true,
            network_access: false, // --network=none
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

        // Wait for completion with timeout
        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        let logs = tokio::time::timeout(timeout, runtime.logs(&container_id)).await;

        let (stdout, exit_code) = match logs {
            Ok(Ok(output)) => (output, 0),
            Ok(Err(e)) => (format!("Container error: {e}"), 1),
            Err(_) => ("Container execution timed out".to_string(), 124),
        };

        // Cleanup: stop, remove container, delete temp files
        let _ = runtime
            .stop(&container_id, std::time::Duration::from_secs(5))
            .await;
        let _ = runtime.remove(&container_id).await;
        let _ = std::fs::remove_dir_all(&tmp_dir);

        let execution_ms = start.elapsed().as_millis() as u64;

        let mut stdout = stdout;
        let truncated = safe_truncate_string(&mut stdout, req.max_output_bytes);

        Ok(ScriptResult {
            stdout,
            stderr: String::new(),
            exit_code,
            tool_calls_count: rpc_server.call_count(),
            execution_ms,
            truncated,
        })
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
    fn test_bash_client_stub_is_valid() {
        let stub = bash_client_stub();
        assert!(stub.contains("ptc_call"));
        assert!(stub.contains("DUDUCLAW_PTC_SOCKET"));
        assert!(stub.contains("UNIX-CONNECT"));
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
    fn test_rpc_server_stop_cleans_socket() {
        let tmp = std::env::temp_dir().join("ptc_test_stop.sock");
        // Create a dummy file to simulate the socket
        let _ = std::fs::write(&tmp, b"");
        assert!(tmp.exists());

        let mut server = PtcRpcServer::new(tmp.clone());
        server.stop();

        // Socket file should be removed
        assert!(!tmp.exists());
    }

    #[test]
    fn test_rpc_server_stop_sends_shutdown() {
        let tmp = std::env::temp_dir().join("ptc_test_shutdown.sock");
        let (tx, mut rx) = oneshot::channel();
        let mut server = PtcRpcServer::new(tmp);
        server.shutdown_tx = Some(tx);

        server.stop();

        // The receiver should have gotten the signal (channel closed with value)
        assert!(rx.try_recv().is_ok());
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
