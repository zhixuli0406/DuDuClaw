use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tracing::{debug, warn};

use duduclaw_core::error::Result;

/// Handler function type for routing tool calls.
///
/// Receives (tool_name, params) and returns the tool result as JSON value.
pub type ToolHandler = Arc<dyn Fn(String, Value) -> Result<Value> + Send + Sync>;

/// JSON-RPC server over Unix Domain Socket for PTC script-to-agent RPC.
///
/// During script execution, this server listens on a UDS and forwards
/// tool calls from the subprocess to the host MCP tool handler, enforcing
/// a whitelist of allowed tool names.
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

    /// Start listening on the UDS. Returns a join handle and a shutdown sender.
    ///
    /// Send `()` on the `oneshot::Sender` to gracefully stop the server.
    pub async fn start(&self) -> Result<(tokio::task::JoinHandle<()>, oneshot::Sender<()>)> {
        // Clean up stale socket file if it exists
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)?;

        // SECURITY: Restrict socket permissions to owner-only (prevent other users from connecting)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o600),
            );
        }

        debug!(path = %self.socket_path.display(), "PTC RPC server listening");

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let allowed_tools = self.allowed_tools.clone();
        let tool_handler = Arc::clone(&self.tool_handler);
        let call_count = Arc::clone(&self.call_count);

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((stream, _addr)) => {
                                let allowed = allowed_tools.clone();
                                let handler = Arc::clone(&tool_handler);
                                let count = Arc::clone(&call_count);

                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, &allowed, &handler, &count).await {
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
        });

        Ok((handle, shutdown_tx))
    }

    /// Number of tool calls handled so far.
    pub fn call_count(&self) -> u32 {
        self.call_count.load(Ordering::Relaxed)
    }

    /// Path to the Unix Domain Socket.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for PtcUdsServer {
    fn drop(&mut self) {
        // Best-effort cleanup of socket file
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Handle a single UDS connection: read line-delimited JSON-RPC requests,
/// dispatch to the tool handler, and write responses.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    allowed_tools: &HashSet<String>,
    tool_handler: &ToolHandler,
    call_count: &AtomicU32,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
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
