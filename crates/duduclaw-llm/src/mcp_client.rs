//! Minimal MCP (Model Context Protocol) client over a stdio child process.
//!
//! This is the API-path counterpart to the CLI backends' MCP wiring: it lets
//! the direct-API / local-inference providers reach the same MCP servers by
//! speaking JSON-RPC 2.0 over a spawned child's stdin/stdout (line-delimited
//! JSON), then feeds the discovered tools into [`run_tool_loop`] via the
//! [`ToolExecutor`] trait.
//!
//! ## Scope (deliberately partial)
//!
//! Only the three methods the tool loop needs are implemented:
//!   * `initialize` handshake (+ the `notifications/initialized` follow-up),
//!   * `tools/list`,
//!   * `tools/call`.
//!
//! **Not implemented** (out of scope for this client): resources
//! (`resources/*`), prompts (`prompts/*`), sampling, roots, logging,
//! completion, progress notifications, server-initiated requests,
//! cancellation, and pagination cursors on `tools/list`. A server that
//! *requires* any of those to serve tools is unsupported.
//!
//! ## Transport shape
//!
//! Requests/responses are one JSON object per line. Reads skip any line that
//! is not the awaited response (notifications, stray log lines that happen to
//! be valid JSON without a matching id). Every request is bounded by a
//! timeout; the child is killed on drop (fail-closed — no orphaned server).
//!
//! The wire-framing helpers ([`build_initialize_request`],
//! [`build_tools_list_request`], [`build_tools_call_request`],
//! [`parse_tools_list_response`], [`parse_tool_call_result`]) are pure and
//! unit-tested without spawning a process.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::warn;

use crate::tool_loop::{ToolExecutor, ToolOutcome};
use crate::types::ToolDef;

/// JSON-RPC protocol version echoed in every frame.
const JSONRPC_VERSION: &str = "2.0";
/// MCP protocol revision advertised in the `initialize` handshake.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// Default per-request timeout.
pub const DEFAULT_MCP_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failure of an MCP client operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("mcp spawn failed: {0}")]
    Spawn(String),

    #[error("mcp io error: {0}")]
    Io(String),

    #[error("mcp request timed out")]
    Timeout,

    #[error("mcp server closed the stream")]
    Closed,

    /// A JSON-RPC protocol error (the `error` member of a response frame).
    #[error("mcp rpc error {code}: {message}")]
    Rpc { code: i64, message: String },

    #[error("mcp parse error: {0}")]
    Parse(String),
}

// ---------------------------------------------------------------------------
// Data shapes
// ---------------------------------------------------------------------------

/// A tool advertised by an MCP server (`tools/list` entry).
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool input (`inputSchema` on the wire).
    pub input_schema: Value,
}

impl McpToolDef {
    /// Map to the crate's provider-agnostic [`ToolDef`].
    pub fn to_tool_def(&self) -> ToolDef {
        ToolDef {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

/// The result of a `tools/call`: concatenated text content plus the server's
/// `isError` flag (a tool that ran but failed, not a protocol error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallResult {
    pub content: String,
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Pure JSON-RPC framing (offline-testable)
// ---------------------------------------------------------------------------

/// Build a JSON-RPC 2.0 request frame.
fn make_request(id: i64, method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "method": method,
        "params": params,
    })
}

/// The `initialize` handshake frame.
pub fn build_initialize_request(id: i64) -> Value {
    make_request(
        id,
        "initialize",
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "clientInfo": { "name": "duduclaw-llm", "version": env!("CARGO_PKG_VERSION") },
        }),
    )
}

/// The `notifications/initialized` frame sent after a successful handshake.
/// Notifications carry no `id` and receive no response.
pub fn build_initialized_notification() -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "method": "notifications/initialized",
        "params": {},
    })
}

/// The `tools/list` request frame.
pub fn build_tools_list_request(id: i64) -> Value {
    make_request(id, "tools/list", json!({}))
}

/// The `tools/call` request frame.
pub fn build_tools_call_request(id: i64, name: &str, args: Value) -> Value {
    make_request(
        id,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
}

/// Extract a JSON-RPC `error` member into [`McpError::Rpc`], if present.
fn rpc_error_of(frame: &Value) -> Option<McpError> {
    let err = frame.get("error")?;
    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    Some(McpError::Rpc { code, message })
}

/// Parse a `tools/list` response frame into [`McpToolDef`]s.
///
/// A tool missing `name` is skipped (fail-closed: an unnamed tool is
/// unroutable). Absent `description` / `inputSchema` default to empty.
pub fn parse_tools_list_response(frame: &Value) -> Result<Vec<McpToolDef>, McpError> {
    if let Some(e) = rpc_error_of(frame) {
        return Err(e);
    }
    let tools = frame
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| McpError::Parse("missing result.tools array".into()))?;

    let mut out = Vec::with_capacity(tools.len());
    for t in tools {
        let name = match t.get("name").and_then(Value::as_str) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => {
                warn!("mcp tools/list entry without a name — skipping");
                continue;
            }
        };
        let description = t
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let input_schema = t
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        out.push(McpToolDef { name, description, input_schema });
    }
    Ok(out)
}

/// Parse a `tools/call` response frame into a [`ToolCallResult`].
///
/// MCP puts *tool-execution* failures in `result.isError = true` (with the
/// error text in the content blocks) and reserves the JSON-RPC `error` member
/// for *protocol* failures. Only `text` content blocks are concatenated;
/// other block kinds (image, resource) are noted as a placeholder so the
/// model still sees that non-text content was returned.
pub fn parse_tool_call_result(frame: &Value) -> Result<ToolCallResult, McpError> {
    if let Some(e) = rpc_error_of(frame) {
        return Err(e);
    }
    let result = frame
        .get("result")
        .ok_or_else(|| McpError::Parse("missing result".into()))?;

    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let content = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| concat_content_blocks(blocks))
        .unwrap_or_default();

    Ok(ToolCallResult { content, is_error })
}

/// Concatenate text content blocks; summarize non-text blocks by type.
fn concat_content_blocks(blocks: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        let kind = b.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "text" {
            if let Some(t) = b.get("text").and_then(Value::as_str) {
                parts.push(t.to_string());
            }
        } else if !kind.is_empty() {
            parts.push(format!("[{kind} content omitted]"));
        }
    }
    parts.join("")
}

// ---------------------------------------------------------------------------
// Stdio client
// ---------------------------------------------------------------------------

/// A live MCP client bound to a spawned child process.
///
/// Request/response is serialized: each method awaits its own reply before the
/// next is issued (the tool loop dispatches sequentially), so a simple
/// read-until-matching-id loop suffices without a background reader task.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: AtomicI64,
    timeout: Duration,
    /// Server name for diagnostics (the spawned command).
    label: String,
}

impl McpClient {
    /// Spawn `command args...` and perform the `initialize` handshake.
    ///
    /// `envs` are added to the child environment. On any handshake failure the
    /// child is killed before returning (no orphaned server).
    pub async fn connect(
        command: &str,
        args: &[String],
        envs: &[(String, String)],
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .envs(envs.iter().map(|(k, v)| (k.clone(), v.clone())))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| McpError::Spawn(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Spawn("child stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn("child stdout unavailable".into()))?;

        let mut client = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: AtomicI64::new(1),
            timeout,
            label: command.to_string(),
        };

        if let Err(e) = client.handshake().await {
            // Best-effort teardown before surfacing the failure.
            let _ = client.child.start_kill();
            return Err(e);
        }
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<(), McpError> {
        let id = self.alloc_id();
        let frame = build_initialize_request(id);
        let resp = self.request(frame, id).await?;
        if let Some(e) = rpc_error_of(&resp) {
            return Err(e);
        }
        // Announce readiness; notifications get no reply.
        self.send_line(&build_initialized_notification()).await?;
        Ok(())
    }

    fn alloc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// List the server's tools.
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>, McpError> {
        let id = self.alloc_id();
        let resp = self.request(build_tools_list_request(id), id).await?;
        parse_tools_list_response(&resp)
    }

    /// Call one tool by name with parsed JSON arguments.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<ToolCallResult, McpError> {
        let id = self.alloc_id();
        let resp = self
            .request(build_tools_call_request(id, name, args), id)
            .await?;
        parse_tool_call_result(&resp)
    }

    /// Server label (the spawned command), for diagnostics.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Write one JSON frame followed by a newline.
    async fn send_line(&mut self, frame: &Value) -> Result<(), McpError> {
        let mut line = serde_json::to_string(frame).map_err(|e| McpError::Parse(e.to_string()))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        Ok(())
    }

    /// Send a request and read frames until the one whose `id` matches, all
    /// under a single timeout. Non-matching frames (notifications, other ids,
    /// non-JSON log lines) are skipped.
    async fn request(&mut self, frame: Value, expect_id: i64) -> Result<Value, McpError> {
        let timeout = self.timeout;
        let fut = async {
            self.send_line(&frame).await?;
            loop {
                let mut line = String::new();
                let n = self
                    .stdout
                    .read_line(&mut line)
                    .await
                    .map_err(|e| McpError::Io(e.to_string()))?;
                if n == 0 {
                    return Err(McpError::Closed);
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    // Not JSON (stray log line) — ignore and keep reading.
                    Err(_) => continue,
                };
                if value.get("id").and_then(Value::as_i64) == Some(expect_id) {
                    return Ok(value);
                }
                // Different id or a notification — not ours; skip.
            }
        };

        match tokio::time::timeout(timeout, fut).await {
            Ok(res) => res,
            Err(_) => Err(McpError::Timeout),
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Fail-closed: never leave the server running. `kill_on_drop(true)`
        // covers the tokio Child too, but start_kill is explicit + immediate.
        let _ = self.child.start_kill();
    }
}

// ---------------------------------------------------------------------------
// Registry — aggregates N clients behind the ToolExecutor trait
// ---------------------------------------------------------------------------

/// Aggregates tools from multiple [`McpClient`]s and routes calls by tool
/// name. Implements [`ToolExecutor`], so a registry is what
/// [`run_tool_loop`](crate::run_tool_loop) drives.
///
/// **Collision policy:** first-wins. If two servers advertise the same tool
/// name, the earlier client owns it and a warning is logged; the later one's
/// tool is dropped from both the routing table and `tool_defs()`.
pub struct ToolRegistry {
    clients: Vec<Mutex<McpClient>>,
    /// tool name → index into `clients`.
    routes: HashMap<String, usize>,
    defs: Vec<ToolDef>,
}

/// Per-server tool visibility filter for mounted MCP servers.
///
/// `allowed` non-empty ⇒ allowlist (deny-by-default: only listed tools are
/// exposed). `denied` always removes a tool even if allowlisted. Used by the
/// MCP Bridge to constrain the tool surface an external third-party server
/// contributes to an agent.
#[derive(Debug, Clone, Default)]
pub struct ToolFilter {
    pub allowed: Vec<String>,
    pub denied: Vec<String>,
}

impl ToolFilter {
    /// Whether `name` may be exposed under this filter.
    pub fn permits(&self, name: &str) -> bool {
        if self.denied.iter().any(|d| d == name) {
            return false;
        }
        if !self.allowed.is_empty() {
            return self.allowed.iter().any(|a| a == name);
        }
        true
    }
}

impl ToolRegistry {
    /// Build a registry from already-connected clients, discovering each
    /// server's tools via `tools/list` and resolving name collisions.
    pub async fn from_clients(clients: Vec<McpClient>) -> Result<Self, McpError> {
        Self::from_clients_filtered(clients, Vec::new()).await
    }

    /// Like [`from_clients`](Self::from_clients) but applies a per-server
    /// [`ToolFilter`] (parallel to `clients`; a missing entry ⇒ permissive)
    /// before the first-wins collision pass. This is the MCP Bridge entry point
    /// for mounting external servers with a constrained tool surface.
    pub async fn from_clients_filtered(
        mut clients: Vec<McpClient>,
        filters: Vec<ToolFilter>,
    ) -> Result<Self, McpError> {
        let mut per_client: Vec<Vec<McpToolDef>> = Vec::with_capacity(clients.len());
        for c in clients.iter_mut() {
            per_client.push(c.list_tools().await?);
        }
        let (routes, defs) = build_routes_filtered(&per_client, &filters);
        let clients = clients.into_iter().map(Mutex::new).collect();
        Ok(Self { clients, routes, defs })
    }

    /// Tool definitions to seed [`ChatRequest::tools`](crate::ChatRequest).
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.defs.clone()
    }

    /// Number of routable tools.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Whether the registry exposes no tools.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// Pure routing builder: name → client index with first-wins collision
/// handling. Split out so the collision policy is unit-testable without
/// spawning any process.
/// Routing builder with per-client [`ToolFilter`]s applied before the first-wins
/// collision pass. `filters[idx]` gates client `idx`; a missing entry is
/// permissive. Pure (no process spawn) so both filtering and collision policy
/// are unit-testable.
fn build_routes_filtered(
    per_client: &[Vec<McpToolDef>],
    filters: &[ToolFilter],
) -> (HashMap<String, usize>, Vec<ToolDef>) {
    let mut routes = HashMap::new();
    let mut defs = Vec::new();
    for (idx, tools) in per_client.iter().enumerate() {
        let filter = filters.get(idx);
        for t in tools {
            if let Some(f) = filter {
                if !f.permits(&t.name) {
                    continue; // filtered out by this server's allow/deny list
                }
            }
            if routes.contains_key(&t.name) {
                warn!(
                    tool = %t.name,
                    client = idx,
                    "duplicate MCP tool name across servers — first-wins, ignoring later server"
                );
                continue;
            }
            routes.insert(t.name.clone(), idx);
            defs.push(t.to_tool_def());
        }
    }
    (routes, defs)
}

#[async_trait]
impl ToolExecutor for ToolRegistry {
    fn defs(&self) -> Vec<ToolDef> {
        self.tool_defs()
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutcome, String> {
        // Fail-closed: an unrouted name is a dispatch error the loop turns
        // into an is_error tool result.
        let idx = *self
            .routes
            .get(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let mut client = self.clients[idx].lock().await;
        match client.call_tool(name, args).await {
            Ok(r) => Ok(ToolOutcome { content: r.content, is_error: r.is_error }),
            Err(e) => Err(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — pure framing + routing, no child processes.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_request_shape() {
        let f = build_initialize_request(1);
        assert_eq!(f["jsonrpc"], "2.0");
        assert_eq!(f["id"], 1);
        assert_eq!(f["method"], "initialize");
        assert_eq!(f["params"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(f["params"]["clientInfo"]["name"], "duduclaw-llm");
    }

    #[test]
    fn initialized_notification_has_no_id() {
        let f = build_initialized_notification();
        assert_eq!(f["method"], "notifications/initialized");
        assert!(f.get("id").is_none());
    }

    #[test]
    fn tools_call_request_shape() {
        let f = build_tools_call_request(7, "search", json!({"q": "rust"}));
        assert_eq!(f["id"], 7);
        assert_eq!(f["method"], "tools/call");
        assert_eq!(f["params"]["name"], "search");
        assert_eq!(f["params"]["arguments"]["q"], "rust");
    }

    #[test]
    fn parse_tools_list_maps_and_defaults() {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [
                    { "name": "search", "description": "web search",
                      "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}} },
                    { "name": "noschema" }
                ]
            }
        });
        let tools = parse_tools_list_response(&frame).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].description, "web search");
        assert_eq!(tools[0].input_schema["properties"]["q"]["type"], "string");
        // Missing description/inputSchema default gracefully.
        assert_eq!(tools[1].description, "");
        assert_eq!(tools[1].input_schema["type"], "object");
        // McpToolDef → ToolDef mapping preserves the schema.
        let td = tools[0].to_tool_def();
        assert_eq!(td.name, "search");
        assert_eq!(td.input_schema, tools[0].input_schema);
    }

    #[test]
    fn parse_tools_list_skips_unnamed() {
        let frame = json!({ "result": { "tools": [ {"description": "x"}, {"name": "ok"} ] } });
        let tools = parse_tools_list_response(&frame).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "ok");
    }

    #[test]
    fn parse_tools_list_rpc_error_propagates() {
        let frame = json!({ "id": 1, "error": { "code": -32601, "message": "method not found" } });
        let err = parse_tools_list_response(&frame).unwrap_err();
        assert_eq!(err, McpError::Rpc { code: -32601, message: "method not found".into() });
    }

    #[test]
    fn parse_tools_list_missing_array_is_parse_error() {
        let frame = json!({ "result": {} });
        assert!(matches!(parse_tools_list_response(&frame), Err(McpError::Parse(_))));
    }

    #[test]
    fn parse_tool_call_concatenates_text_blocks() {
        let frame = json!({
            "id": 3,
            "result": {
                "content": [
                    { "type": "text", "text": "hello " },
                    { "type": "text", "text": "world" }
                ]
            }
        });
        let r = parse_tool_call_result(&frame).unwrap();
        assert_eq!(r.content, "hello world");
        assert!(!r.is_error);
    }

    #[test]
    fn parse_tool_call_honours_is_error_flag() {
        let frame = json!({
            "id": 4,
            "result": {
                "isError": true,
                "content": [ { "type": "text", "text": "boom" } ]
            }
        });
        let r = parse_tool_call_result(&frame).unwrap();
        assert!(r.is_error);
        assert_eq!(r.content, "boom");
    }

    #[test]
    fn parse_tool_call_summarizes_non_text_blocks() {
        let frame = json!({
            "id": 5,
            "result": { "content": [
                { "type": "text", "text": "see: " },
                { "type": "image", "data": "..." }
            ] }
        });
        let r = parse_tool_call_result(&frame).unwrap();
        assert_eq!(r.content, "see: [image content omitted]");
    }

    #[test]
    fn parse_tool_call_rpc_error_propagates() {
        let frame = json!({ "id": 6, "error": { "code": -32000, "message": "server error" } });
        let err = parse_tool_call_result(&frame).unwrap_err();
        assert_eq!(err, McpError::Rpc { code: -32000, message: "server error".into() });
    }

    #[test]
    fn build_routes_first_wins_on_collision() {
        let client_a = vec![
            McpToolDef { name: "search".into(), description: "A search".into(), input_schema: json!({}) },
            McpToolDef { name: "fetch".into(), description: "A fetch".into(), input_schema: json!({}) },
        ];
        let client_b = vec![
            // Collides with client_a's "search" — must be ignored.
            McpToolDef { name: "search".into(), description: "B search".into(), input_schema: json!({}) },
            McpToolDef { name: "write".into(), description: "B write".into(), input_schema: json!({}) },
        ];
        let (routes, defs) = build_routes_filtered(&[client_a, client_b], &[]);

        assert_eq!(routes.len(), 3);
        assert_eq!(routes["search"], 0); // first client wins
        assert_eq!(routes["fetch"], 0);
        assert_eq!(routes["write"], 1);

        // Defs carry the first-wins description, not the collided one.
        let search = defs.iter().find(|d| d.name == "search").unwrap();
        assert_eq!(search.description, "A search");
        assert_eq!(defs.len(), 3);
    }

    #[test]
    fn tool_filter_allowlist_is_deny_by_default() {
        let f = ToolFilter {
            allowed: vec!["read".into(), "list".into()],
            denied: vec![],
        };
        assert!(f.permits("read"));
        assert!(f.permits("list"));
        assert!(!f.permits("delete"), "unlisted tool denied under allowlist");
    }

    #[test]
    fn tool_filter_denylist_overrides_allow() {
        let f = ToolFilter {
            allowed: vec!["read".into(), "write".into()],
            denied: vec!["write".into()],
        };
        assert!(f.permits("read"));
        assert!(!f.permits("write"), "explicit deny beats allow");
        // Empty allowlist + only denylist: permissive except denied.
        let f2 = ToolFilter { allowed: vec![], denied: vec!["danger".into()] };
        assert!(f2.permits("anything"));
        assert!(!f2.permits("danger"));
    }

    #[test]
    fn build_routes_filtered_applies_per_client_filter() {
        let internal = vec![
            McpToolDef { name: "memory_search".into(), description: "".into(), input_schema: json!({}) },
        ];
        let external = vec![
            McpToolDef { name: "crm_list".into(), description: "".into(), input_schema: json!({}) },
            McpToolDef { name: "crm_delete".into(), description: "".into(), input_schema: json!({}) },
        ];
        // Internal server: permissive. External server: allowlist crm_list only.
        let filters = vec![
            ToolFilter::default(),
            ToolFilter { allowed: vec!["crm_list".into()], denied: vec![] },
        ];
        let (routes, defs) = build_routes_filtered(&[internal, external], &filters);
        assert!(routes.contains_key("memory_search"));
        assert!(routes.contains_key("crm_list"));
        assert!(!routes.contains_key("crm_delete"), "filtered external tool absent");
        assert_eq!(defs.len(), 2);
    }
}
