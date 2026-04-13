use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tracing::{debug, warn};

use duduclaw_core::error::Result;

/// Handler function type for routing tool calls.
///
/// Receives (tool_name, params) and returns the tool result as JSON value.
pub type ToolHandler = Arc<dyn Fn(String, Value) -> Result<Value> + Send + Sync>;

/// JSON-RPC server for PTC script-to-agent RPC.
///
/// On Unix, uses a Unix Domain Socket at `socket_path`.
/// On Windows, uses a TCP listener on localhost with a random port,
/// writing the port number to `socket_path` so the subprocess can connect.
pub struct PtcUdsServer {
    socket_path: PathBuf,
    allowed_tools: HashSet<String>,
    tool_handler: ToolHandler,
    call_count: Arc<AtomicU32>,
}

impl PtcUdsServer {
    pub fn new(
        socket_path: PathBuf,
        allowed_tools: HashSet<String>,
        tool_handler: ToolHandler,
    ) -> Self {
        Self {
            socket_path,
            allowed_tools,
            tool_handler,
            call_count: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Start listening. Returns a join handle and a shutdown sender.
    ///
    /// Send `()` on the `oneshot::Sender` to gracefully stop the server.
    pub async fn start(&self) -> Result<(tokio::task::JoinHandle<()>, oneshot::Sender<()>)> {
        // Clean up stale socket/port file if it exists
        let _ = std::fs::remove_file(&self.socket_path);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let allowed_tools = self.allowed_tools.clone();
        let tool_handler = Arc::clone(&self.tool_handler);
        let call_count = Arc::clone(&self.call_count);

        #[cfg(unix)]
        let handle = {
            let listener = tokio::net::UnixListener::bind(&self.socket_path)?;

            // SECURITY: Restrict socket permissions to owner-only
            duduclaw_core::platform::set_owner_only(&self.socket_path).ok();

            debug!(path = %self.socket_path.display(), "PTC RPC server listening (UDS)");

            tokio::spawn(accept_loop_unix(listener, shutdown_rx, allowed_tools, tool_handler, call_count))
        };

        #[cfg(windows)]
        let handle = {
            // Bind to localhost with port 0 (OS picks a free port)
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let port = listener.local_addr()?.port();

            // Write the port number to socket_path so the subprocess knows where to connect
            std::fs::write(&self.socket_path, port.to_string())?;

            debug!(port = port, path = %self.socket_path.display(), "PTC RPC server listening (TCP localhost)");

            tokio::spawn(accept_loop_tcp(listener, shutdown_rx, allowed_tools, tool_handler, call_count))
        };

        Ok((handle, shutdown_tx))
    }

    /// Number of tool calls handled so far.
    pub fn call_count(&self) -> u32 {
        self.call_count.load(Ordering::Relaxed)
    }

    /// Path to the socket file (or port file on Windows).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for PtcUdsServer {
    fn drop(&mut self) {
        // Best-effort cleanup of socket/port file
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

// ── Unix: UDS accept loop ────────────────────────────────────

#[cfg(unix)]
async fn accept_loop_unix(
    listener: tokio::net::UnixListener,
    mut shutdown_rx: oneshot::Receiver<()>,
    allowed_tools: HashSet<String>,
    tool_handler: ToolHandler,
    call_count: Arc<AtomicU32>,
) {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let allowed = allowed_tools.clone();
                        let handler = Arc::clone(&tool_handler);
                        let count = Arc::clone(&call_count);
                        let (reader, writer) = stream.into_split();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(reader, writer, &allowed, &handler, &count).await {
                                warn!(error = %e, "PTC RPC connection error");
                            }
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "PTC RPC accept error");
                    }
                }
            }
            _ = &mut shutdown_rx => {
                debug!("PTC RPC server shutting down");
                break;
            }
        }
    }
}

// ── Windows: TCP accept loop ─────────────────────────────────

#[cfg(windows)]
async fn accept_loop_tcp(
    listener: tokio::net::TcpListener,
    mut shutdown_rx: oneshot::Receiver<()>,
    allowed_tools: HashSet<String>,
    tool_handler: ToolHandler,
    call_count: Arc<AtomicU32>,
) {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, addr)) => {
                        // SECURITY: Only accept connections from localhost
                        if !addr.ip().is_loopback() {
                            warn!(remote = %addr, "PTC RPC: rejected non-loopback connection");
                            continue;
                        }
                        let allowed = allowed_tools.clone();
                        let handler = Arc::clone(&tool_handler);
                        let count = Arc::clone(&call_count);
                        let (reader, writer) = stream.into_split();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(reader, writer, &allowed, &handler, &count).await {
                                warn!(error = %e, "PTC RPC connection error");
                            }
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "PTC RPC accept error");
                    }
                }
            }
            _ = &mut shutdown_rx => {
                debug!("PTC RPC server shutting down");
                break;
            }
        }
    }
}

// ── Shared connection handler (generic over AsyncRead + AsyncWrite) ──

async fn handle_connection<R, W>(
    reader: R,
    mut writer: W,
    allowed_tools: &HashSet<String>,
    tool_handler: &ToolHandler,
    call_count: &AtomicU32,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = process_request(&line, allowed_tools, tool_handler, call_count);
        let mut response_bytes = serde_json::to_vec(&response)
            .unwrap_or_else(|_| br#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"internal serialization error"},"id":null}"#.to_vec());
        response_bytes.push(b'\n');

        writer.write_all(&response_bytes).await?;
        writer.flush().await?;
    }

    Ok(())
}

/// Parse and dispatch a single JSON-RPC request.
fn process_request(
    raw: &str,
    allowed_tools: &HashSet<String>,
    tool_handler: &ToolHandler,
    call_count: &AtomicU32,
) -> Value {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": -32700, "message": "parse error" },
                "id": null
            });
        }
    };

    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let method = parsed
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    if method.is_empty() {
        return serde_json::json!({
            "jsonrpc": "2.0",
            "error": { "code": -32600, "message": "invalid request: missing method" },
            "id": id
        });
    }

    // Check whitelist
    if !allowed_tools.contains(method) {
        warn!(tool = method, "PTC RPC: blocked tool call (not in whitelist)");
        return serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32601,
                "message": format!("tool '{}' is not allowed in this PTC session", method)
            },
            "id": id
        });
    }

    // Dispatch to tool handler
    match tool_handler(method.to_string(), params) {
        Ok(result) => {
            call_count.fetch_add(1, Ordering::Relaxed);
            debug!(tool = method, "PTC RPC: tool call succeeded");
            serde_json::json!({
                "jsonrpc": "2.0",
                "result": result,
                "id": id
            })
        }
        Err(e) => {
            warn!(tool = method, error = %e, "PTC RPC: tool call failed");
            serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": -32000, "message": e.to_string() },
                "id": id
            })
        }
    }
}
