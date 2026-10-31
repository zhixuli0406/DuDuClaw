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
//! Two transports behind one [`McpClient`]:
//!   * **stdio** ([`McpClient::connect`]): requests/responses are one JSON
//!     object per line. Reads skip any line that is not the awaited response
//!     (notifications, stray log lines that happen to be valid JSON without a
//!     matching id). The child is killed on drop (fail-closed — no orphaned
//!     server).
//!   * **Streamable HTTP** ([`McpClient::connect_http`]): every frame is
//!     POSTed to a single remote endpoint; the response arrives as a plain
//!     JSON body or as `text/event-stream` (the response frame inside SSE
//!     `data:` events). A server-issued `Mcp-Session-Id` is echoed on later
//!     requests; stateless servers (e.g. the Google Workspace remote MCP
//!     servers) simply never issue one. Auth is caller-supplied headers.
//!
//! Every request on either transport is bounded by a timeout.
//!
//! The wire-framing helpers ([`build_initialize_request`],
//! [`build_tools_list_request`], [`build_tools_call_request`],
//! [`parse_tools_list_response`], [`parse_tool_call_result`]) are pure and
//! unit-tested without spawning a process.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use duduclaw_core::process_tree::ManagedChild;
use tokio::sync::Mutex;
use tracing::warn;

use crate::ccr::{CcrScope, CcrSourceArtifact};
use crate::tool_loop::{
    ClientOperation, InterceptDecision, PreparedClientCall, SecretBearer, ToolCallLease,
    ToolExecutor, ToolInterceptor, ToolOutcome,
};
use crate::types::ToolDef;

/// JSON-RPC protocol version echoed in every frame.
const JSONRPC_VERSION: &str = "2.0";
/// MCP protocol revision advertised in the `initialize` handshake.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// Default per-request timeout.
pub const DEFAULT_MCP_TIMEOUT: Duration = Duration::from_secs(30);

/// A scoped operation includes queueing, preparation, handshake and its final
/// response. Hosts may choose a shorter deadline, never an unbounded lease.
const MAX_SCOPED_MCP_TIMEOUT: Duration = Duration::from_secs(300);

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

mod closed;
pub use closed::{ClosedClientTarget, OwnedScopedFailure, OwnedScopedOperation, OwnedScopedReply};
pub(crate) use closed::ClosedPostprocessPolicy;
#[cfg(test)]
#[path = "mcp_client/closed_tests.rs"]
pub(crate) mod closed_tests;

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

/// Immutable host-owned connection configuration. A scoped slot never caches a
/// bearer or MCP session in this recipe. It cannot be constructed from model
/// arguments or an MCP response. Other fixed headers remain host configuration;
/// the recipe deliberately does not implement Debug or serialization.
#[derive(Clone)]
pub struct ScopedHttpRecipe {
    url: String,
    headers: Vec<(String, String)>,
    operation_timeout: Duration,
}

impl ScopedHttpRecipe {
    pub fn new(
        url: String,
        headers: Vec<(String, String)>,
        operation_timeout: Duration,
    ) -> Result<Self, McpError> {
        let invalid = || McpError::Spawn("invalid scoped MCP HTTP recipe".into());
        let parsed = reqwest::Url::parse(&url).map_err(|_| invalid())?;
        if !http_endpoint_allowed(&url)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
            || operation_timeout.is_zero()
            || operation_timeout > MAX_SCOPED_MCP_TIMEOUT
        {
            return Err(invalid());
        }
        for (name, value) in &headers {
            if [
                "authorization",
                "proxy-authorization",
                "cookie",
                "cookie2",
                "mcp-session-id",
            ]
            .iter()
            .any(|denied| name.eq_ignore_ascii_case(denied))
                || reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                || reqwest::header::HeaderValue::from_str(value).is_err()
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            url,
            headers,
            operation_timeout,
        })
    }
}

struct ScopedHttpAuthority {
    bearer: SecretBearer,
    lease: Box<dyn ToolCallLease>,
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
    make_request(id, "tools/call", json!({ "name": name, "arguments": args }))
}

/// Extract a JSON-RPC `error` member into [`McpError::Rpc`], if present.
/// Whether a Streamable-HTTP MCP endpoint may be used: any `https://` URL,
/// or plain `http://` only when the parsed host is exactly `localhost`, an
/// IPv4 loopback address (127.0.0.0/8) or `::1`. The host is compared after
/// parsing, never by prefix: `http://localhost.evil.com` and
/// `http://localhost@evil.com` are remote hosts and need `https`.
fn http_endpoint_allowed(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    match parsed.scheme() {
        "https" => parsed.host_str().is_some(),
        "http" => match parsed.host_str() {
            Some(host) if host.eq_ignore_ascii_case("localhost") => true,
            // `host_str` brackets an IPv6 literal (`[::1]`).
            Some(host) => match host
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host)
                .parse::<std::net::IpAddr>()
            {
                Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback(),
                Ok(std::net::IpAddr::V6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
                Err(_) => false,
            },
            None => false,
        },
        _ => false,
    }
}

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
        out.push(McpToolDef {
            name,
            description,
            input_schema,
        });
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
// Client — stdio child process OR remote Streamable HTTP
// ---------------------------------------------------------------------------

/// The wire the client speaks over.
enum McpTransport {
    /// Line-delimited JSON-RPC over a spawned child's stdin/stdout.
    Stdio {
        child: ManagedChild,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    },
    /// MCP Streamable HTTP: every JSON-RPC frame is POSTed to one endpoint;
    /// the response is either a plain JSON body or a `text/event-stream`
    /// carrying the response frame as SSE `data:` events. Covers both
    /// stateless servers (e.g. Google Workspace MCP) and session-ful ones
    /// (the `Mcp-Session-Id` response header is echoed on later requests).
    Http {
        http: reqwest::Client,
        url: String,
        /// Extra request headers (e.g. `Authorization: Bearer …`).
        headers: Vec<(String, String)>,
        /// Session id issued by the server at `initialize`, if any.
        session_id: Option<String>,
        /// Present only for a single scoped operation. The transport owns
        /// both values through response parsing, including error/cancel paths.
        scoped: Option<ScopedHttpAuthority>,
    },
}

/// A live MCP client bound to a spawned child process or a remote
/// Streamable HTTP endpoint.
///
/// Request/response is serialized: each method awaits its own reply before the
/// next is issued (the tool loop dispatches sequentially), so a simple
/// read-until-matching-id loop suffices without a background reader task.
pub struct McpClient {
    transport: McpTransport,
    next_id: AtomicI64,
    timeout: Duration,
    /// Server name for diagnostics (the spawned command or the URL).
    label: String,
    workflow_version: Option<u32>,
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
        Self::connect_with_stderr(command, args, envs, timeout, std::process::Stdio::null()).await
    }

    /// [`connect`](Self::connect) with the child's stderr disposition chosen by
    /// the caller. Production keeps `Stdio::null()` (a server's stderr is not
    /// ours to relay); the CLI integration tests pass `Stdio::inherit()` so a
    /// child that exits before the handshake leaves its reason in the test
    /// output instead of a bare `Closed`.
    pub async fn connect_with_stderr(
        command: &str,
        args: &[String],
        envs: &[(String, String)],
        timeout: Duration,
        stderr: std::process::Stdio,
    ) -> Result<Self, McpError> {
        Self::spawn_and_handshake(command, args, envs, timeout, stderr, false).await
    }

    /// [`connect`](Self::connect) for a server that must not inherit this
    /// process's environment (a third-party MCP server behind `duduclaw
    /// mcp-proxy`, 2026-10-20): the child starts from the agent-CLI spawn
    /// allowlist (`duduclaw_core::spawn_env::agent_cli_spawn_env_pairs`:
    /// PATH, HOME, locale, proxy, temp dir; never a secret-shaped name) plus
    /// `envs`, the same base environment a server started by the Claude CLI
    /// sees. Without it every vendor key in the gateway's environment would
    /// reach the server.
    pub async fn connect_scrubbed(
        command: &str,
        args: &[String],
        envs: &[(String, String)],
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let mut all: Vec<(String, String)> = duduclaw_core::spawn_env::agent_cli_spawn_env_pairs()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        all.extend(envs.iter().cloned());
        Self::spawn_and_handshake(
            command,
            args,
            &all,
            timeout,
            std::process::Stdio::null(),
            true,
        )
        .await
    }

    async fn spawn_and_handshake(
        command: &str,
        args: &[String],
        envs: &[(String, String)],
        timeout: Duration,
        stderr: std::process::Stdio,
        clear_env: bool,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        if clear_env {
            cmd.env_clear();
        }
        let workflow_home = if envs
            .iter()
            .any(|(key, _)| key == "DUDUCLAW_WORKFLOW_SESSION_ID")
        {
            let home = envs
                .iter()
                .find(|(key, _)| key == "DUDUCLAW_HOME")
                .map(|(_, value)| value)
                .ok_or_else(|| McpError::Spawn("workflow child requires explicit home".into()))?;
            if !std::path::Path::new(home).is_absolute() {
                return Err(McpError::Spawn("workflow home must be absolute".into()));
            }
            cmd.env_clear();
            for key in ["PATH", "TMPDIR", "SystemRoot"] {
                if let Some(value) = std::env::var_os(key) {
                    cmd.env(key, value);
                }
            }
            // Windows: `std::env::temp_dir` reads `TMP` / `TEMP`, the profile
            // comes from `USERPROFILE`, and process start-up needs `SystemRoot`
            // / `windir` / `ComSpec`; with only the three keys above the child
            // `duduclaw mcp-server` exited before the MCP handshake on the CI
            // Windows runner (13 `workflow_stdio` cases, `McpError::Closed`).
            // Same system-only allow-list the agent CLI spawn uses; no
            // secret-shaped names (pinned by spawn_env's shape test).
            #[cfg(windows)]
            for key in duduclaw_core::spawn_env::AGENT_CLI_ENV_ALLOWLIST_WINDOWS {
                if let Some(value) = std::env::var_os(key) {
                    cmd.env(key, value);
                }
            }
            Some(home)
        } else {
            None
        };
        cmd.args(args)
            .envs(envs.iter().map(|(k, v)| (k.clone(), v.clone())))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true);
        if let Some(home) = workflow_home {
            cmd.env("HOME", home);
        }

        let mut child = ManagedChild::spawn(&mut cmd).map_err(|e| McpError::Spawn(e.to_string()))?;
        let stdin = child.child()
            .stdin
            .take()
            .ok_or_else(|| McpError::Spawn("child stdin unavailable".into()))?;
        let stdout = child.child()
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn("child stdout unavailable".into()))?;

        let mut client = Self {
            transport: McpTransport::Stdio {
                child,
                stdin,
                stdout: BufReader::new(stdout),
            },
            next_id: AtomicI64::new(1),
            timeout,
            label: command.to_string(),
            workflow_version: None,
        };

        if let Err(e) = client.handshake().await {
            // Best-effort teardown before surfacing the failure.
            if let McpTransport::Stdio { child, .. } = &mut client.transport {
                child.kill_and_reap().await;
            }
            return Err(e);
        }
        Ok(client)
    }

    /// Connect to a remote MCP server over Streamable HTTP and perform the
    /// `initialize` handshake. `headers` are sent on every request (put the
    /// `Authorization` bearer here). Redirects are refused — a redirect on a
    /// credential-bearing endpoint is treated as misconfiguration, not
    /// something to follow silently.
    pub async fn connect_http(
        url: &str,
        headers: &[(String, String)],
        timeout: Duration,
    ) -> Result<Self, McpError> {
        if !http_endpoint_allowed(url) {
            return Err(McpError::Spawn(format!(
                "MCP HTTP endpoint must be https:// (or localhost for dev): {url}"
            )));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| McpError::Spawn(e.to_string()))?;

        let mut client = Self {
            transport: McpTransport::Http {
                http,
                url: url.to_string(),
                headers: headers.to_vec(),
                session_id: None,
                scoped: None,
            },
            next_id: AtomicI64::new(1),
            timeout,
            label: url.to_string(),
            workflow_version: None,
        };
        client.handshake().await?;
        Ok(client)
    }

    // Only a RegistryClient scoped operation can create this client. Keeping
    // it private prevents callers from accidentally caching its live lease.
    async fn connect_http_with_call_lease(
        recipe: &ScopedHttpRecipe,
        bearer: SecretBearer,
        lease: Box<dyn ToolCallLease>,
    ) -> Result<Self, McpError> {
        let http = reqwest::Client::builder()
            .timeout(recipe.operation_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| McpError::Spawn("scoped MCP HTTP client unavailable".into()))?;
        let mut client = Self {
            transport: McpTransport::Http {
                http,
                url: recipe.url.clone(),
                headers: recipe.headers.clone(),
                session_id: None,
                scoped: Some(ScopedHttpAuthority { bearer, lease }),
            },
            next_id: AtomicI64::new(1),
            timeout: recipe.operation_timeout,
            label: "scoped-http".into(),
            workflow_version: None,
        };
        client.handshake().await?;
        Ok(client)
    }

    fn is_scoped_http(&self) -> bool {
        matches!(
            &self.transport,
            McpTransport::Http {
                scoped: Some(_),
                ..
            }
        )
    }

    async fn validate_scoped_authority(&mut self) -> Result<(), McpError> {
        if let McpTransport::Http {
            scoped: Some(authority),
            ..
        } = &mut self.transport
        {
            authority
                .lease
                .validate_before_send()
                .await
                .map_err(|_| McpError::Io("scoped MCP authority is no longer valid".into()))?;
        }
        Ok(())
    }

    fn redact_scoped_response(&self, value: &mut Value) {
        if let McpTransport::Http {
            scoped: Some(authority),
            ..
        } = &self.transport
        {
            authority.bearer.redact_value(value);
        }
    }

    async fn handshake(&mut self) -> Result<(), McpError> {
        let id = self.alloc_id();
        let frame = build_initialize_request(id);
        let is_http = matches!(self.transport, McpTransport::Http { .. });
        let resp = if is_http {
            // `initialize` is the one HTTP request that captures the
            // server-issued `Mcp-Session-Id` (if any) for later echo.
            self.http_request(frame, id, true).await?
        } else {
            self.request(frame, id).await?
        };
        if let Some(e) = rpc_error_of(&resp) {
            return Err(e);
        }
        self.workflow_version = resp
            .pointer("/result/capabilities/experimental/duduclaw_workflow/version")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        // Announce readiness; notifications get no reply.
        let note = build_initialized_notification();
        if is_http {
            // Persistent compatibility: stateless servers may reject this
            // notification. Scoped operations instead require acceptance.
            if let Err(e) = self.http_notify(&note).await {
                if self.is_scoped_http() {
                    // A scoped notification is part of the authorized
                    // operation. Never continue after a refused authority.
                    return Err(e);
                }
                warn!(server = %self.label, error = %e, "MCP initialized notification not accepted (continuing)");
            }
        } else {
            self.send_line(&note).await?;
        }
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
        self.call_tool_with_boundary(name, args, None).await
    }

    async fn call_tool_with_boundary(
        &mut self, name: &str, args: Value, lease: Option<&dyn ClientCallLease>,
    ) -> Result<ToolCallResult, McpError> {
        let id = self.alloc_id();
        if let Some(lease) = lease { lease.dispatched(); }
        let resp = self.request(build_tools_call_request(id, name, args), id).await?;
        // request() correlates the id. Parsing failures after a terminal frame
        // do not imply the remote operation is still running.
        if resp.get("method").is_none()
            && (resp.get("result").is_some() || resp.get("error").is_some())
        {
            if let Some(lease) = lease { lease.terminal_response(); }
        }
        parse_tool_call_result(&resp)
    }

    /// Service extension calls preserve typed results and never retry an effect.
    async fn workflow_request<T: serde::de::DeserializeOwned>(
        &mut self,
        call: Value,
        metadata: Value,
    ) -> Result<T, McpError> {
        if !matches!(self.transport, McpTransport::Stdio { .. })
            || self.workflow_version != Some(duduclaw_core::workflow_mcp::VERSION)
        {
            return Err(McpError::Parse(
                "workflow extension unavailable on this session".into(),
            ));
        }
        let name = call
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::Parse("missing workflow tool name".into()))?;
        let args = call
            .get("arguments")
            .cloned()
            .ok_or_else(|| McpError::Parse("missing workflow arguments".into()))?;
        let id = self.alloc_id();
        let mut frame = build_tools_call_request(id, name, args);
        frame["params"]["_meta"] = serde_json::json!({"duduclaw_workflow":metadata});
        duduclaw_core::workflow_mcp::canonical_bytes(&frame).map_err(McpError::Parse)?;
        let response = self.request(frame, id).await?;
        duduclaw_core::workflow_mcp::canonical_bytes(&response).map_err(McpError::Parse)?;
        if let Some(error) = rpc_error_of(&response) {
            return Err(error);
        }
        let version = response
            .pointer("/result/version")
            .or_else(|| response.pointer("/result/effective/version"))
            .and_then(Value::as_u64);
        if version != Some(u64::from(duduclaw_core::workflow_mcp::VERSION)) {
            return Err(McpError::Parse("unknown workflow response version".into()));
        }
        serde_json::from_value(
            response
                .get("result")
                .cloned()
                .ok_or_else(|| McpError::Parse("missing workflow result".into()))?,
        )
        .map_err(|_| McpError::Parse("invalid typed workflow response".into()))
    }
    pub async fn prepare_workflow_call(
        &mut self,
        call: Value,
        context: Value,
    ) -> Result<duduclaw_core::workflow_mcp::PrepareTicket, McpError> {
        self.workflow_request(
            call,
            serde_json::json!({"version":1,"phase":"prepare","context":context}),
        )
        .await
    }
    pub async fn read_workflow_call(
        &mut self,
        call: Value,
        context: Value,
    ) -> Result<duduclaw_core::workflow_mcp::ReadReply, McpError> {
        self.workflow_request(
            call,
            serde_json::json!({"version":1,"phase":"read","context":context}),
        )
        .await
    }
    pub async fn execute_workflow_call(
        &mut self,
        call: Value,
        ticket: duduclaw_core::workflow_mcp::ExecuteTicket,
    ) -> Result<duduclaw_core::workflow_mcp::OperationReply, McpError> {
        self.workflow_request(
            call,
            serde_json::json!({"version":1,"phase":"execute","ticket":ticket}),
        )
        .await
    }

    /// Server label (the spawned command), for diagnostics.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Write one JSON frame followed by a newline (stdio transport only).
    async fn send_line(&mut self, frame: &Value) -> Result<(), McpError> {
        let McpTransport::Stdio { stdin, .. } = &mut self.transport else {
            return Err(McpError::Io("send_line on non-stdio transport".into()));
        };
        let mut line = serde_json::to_string(frame).map_err(|e| McpError::Parse(e.to_string()))?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        Ok(())
    }

    /// Send a request and await the frame whose `id` matches, all under a
    /// single timeout. Stdio: read frames, skipping non-matching lines. HTTP:
    /// one POST whose response body carries the frame (JSON or SSE).
    async fn request(&mut self, frame: Value, expect_id: i64) -> Result<Value, McpError> {
        if matches!(self.transport, McpTransport::Http { .. }) {
            return self.http_request(frame, expect_id, false).await;
        }
        let timeout = self.timeout;
        let fut = async {
            self.send_line(&frame).await?;
            let McpTransport::Stdio { stdout, .. } = &mut self.transport else {
                return Err(McpError::Io("stdio transport vanished".into()));
            };
            loop {
                let mut line = String::new();
                let n = stdout
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

    /// POST one JSON-RPC request frame to the Streamable HTTP endpoint and
    /// extract the response frame with the matching `id` from either a plain
    /// JSON body or a `text/event-stream` body. `capture_session` stores a
    /// server-issued `Mcp-Session-Id` for echo on subsequent requests.
    async fn http_request(
        &mut self,
        frame: Value,
        expect_id: i64,
        capture_session: bool,
    ) -> Result<Value, McpError> {
        let scoped = self.is_scoped_http();
        let resp = self.http_post(&frame).await?;
        let status = resp.status();

        if capture_session {
            let sid = resp
                .headers()
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if let (Some(sid), McpTransport::Http { session_id, .. }) = (sid, &mut self.transport) {
                *session_id = Some(sid);
            }
        }

        // Refuse absurdly large bodies before buffering (protocol frames are
        // small; tool results are capped separately by the tool loop).
        if resp.content_length().unwrap_or(0) > MAX_HTTP_BODY_BYTES {
            return Err(McpError::Io("MCP HTTP response too large".into()));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = resp.text().await.map_err(|e| {
            if scoped {
                McpError::Io("scoped MCP response unavailable".into())
            } else {
                McpError::Io(e.to_string())
            }
        })?;

        if !status.is_success() {
            if scoped {
                // A provider may echo Authorization in its error body.
                return Err(McpError::Io(format!(
                    "scoped MCP HTTP response rejected: {status}"
                )));
            }
            let snippet: String = body.chars().take(300).collect();
            return Err(McpError::Io(format!("HTTP {status}: {snippet}")));
        }
        if body.len() as u64 > MAX_HTTP_BODY_BYTES {
            return Err(McpError::Io("MCP HTTP response too large".into()));
        }

        let mut value: Value = if content_type.starts_with("text/event-stream") {
            sse_extract_response(&body, expect_id)
                .ok_or_else(|| McpError::Parse("no matching response frame in SSE body".into()))?
        } else {
            serde_json::from_str(&body).map_err(|e| {
                if scoped {
                    McpError::Parse("invalid scoped MCP response".into())
                } else {
                    McpError::Parse(e.to_string())
                }
            })?
        };
        if value.get("id").and_then(Value::as_i64) != Some(expect_id) {
            return Err(McpError::Parse(format!(
                "HTTP response id mismatch (expected {expect_id})"
            )));
        }
        // Holding a lease alone does not freeze an independently updated ACL.
        // Recheck after body parsing before returning any response content.
        self.validate_scoped_authority().await?;
        self.redact_scoped_response(&mut value);
        Ok(value)
    }

    /// POST a notification frame (no reply expected). Any 2xx is success.
    async fn http_notify(&mut self, frame: &Value) -> Result<(), McpError> {
        let resp = self.http_post(frame).await?;
        let status = resp.status();
        if status.is_success() {
            self.validate_scoped_authority().await
        } else {
            Err(McpError::Io(format!("HTTP {status}")))
        }
    }

    /// Shared POST builder for the HTTP transport: standard MCP headers +
    /// caller headers + session echo.
    async fn http_post(&mut self, frame: &Value) -> Result<reqwest::Response, McpError> {
        let McpTransport::Http {
            http,
            url,
            headers,
            session_id,
            scoped,
        } = &mut self.transport
        else {
            return Err(McpError::Io("http_post on non-http transport".into()));
        };
        if let Some(authority) = scoped.as_mut() {
            authority
                .lease
                .validate_before_send()
                .await
                .map_err(|_| McpError::Io("scoped MCP authority is no longer valid".into()))?;
        }
        let mut req = http
            .post(url.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(authority) = scoped.as_ref() {
            let token = authority.bearer.as_bytes();
            if token.is_empty()
                || token.iter().any(|byte| {
                    !byte.is_ascii() || byte.is_ascii_control() || byte.is_ascii_whitespace()
                })
            {
                return Err(McpError::Io("invalid scoped MCP bearer".into()));
            }
            let mut bytes = b"Bearer ".to_vec();
            bytes.extend_from_slice(token);
            let header = reqwest::header::HeaderValue::from_bytes(&bytes);
            bytes.fill(0);
            let mut header =
                header.map_err(|_| McpError::Io("invalid scoped MCP bearer".into()))?;
            header.set_sensitive(true);
            req = req.header(reqwest::header::AUTHORIZATION, header);
        }
        if let Some(sid) = session_id {
            req = req.header("mcp-session-id", sid.as_str());
        }
        let is_scoped = scoped.is_some();
        req.json(frame).send().await.map_err(|e| {
            if is_scoped {
                McpError::Io("scoped MCP HTTP request failed".into())
            } else {
                McpError::Io(e.to_string())
            }
        })
    }
}

/// Upper bound for a buffered MCP HTTP response body (16 MB).
const MAX_HTTP_BODY_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
#[path = "mcp_client/scoped_http_tests.rs"]
mod scoped_http_tests;

/// Extract the JSON-RPC response frame with `expect_id` from an SSE body:
/// events are blank-line-separated; each event's `data:` lines join to one
/// JSON document. Pure for offline testing.
fn sse_extract_response(body: &str, expect_id: i64) -> Option<Value> {
    for event in body.split("\n\n") {
        let data: String = event
            .lines()
            .filter_map(|l| {
                let l = l.strip_prefix("data:")?;
                Some(l.strip_prefix(' ').unwrap_or(l))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            if v.get("id").and_then(Value::as_i64) == Some(expect_id) {
                return Some(v);
            }
        }
    }
    None
}

// The stdio transport owns a ManagedChild. Dropping a client, including while
// its handshake or tool call is suspended, kills its whole process tree and
// schedules leader reaping through the same helper used by gateway runtimes.

// ---------------------------------------------------------------------------
// Registry — aggregates N clients behind the ToolExecutor trait
// ---------------------------------------------------------------------------

/// Host-created registry entries. Existing constructors wrap their clients in
/// Persistent. Scoped HTTP entries contain only an immutable recipe and guard;
/// discovery and every call create and drop their own authenticated client.
/// Cross-call stateful MCP sessions are not preserved. Dropping a call releases
/// local authority only; an already-sent remote effect remains unconfirmed.
pub enum RegistryClient {
    Persistent(McpClient),
    ScopedHttp(ScopedHttpSlot),
}

impl RegistryClient {
    pub fn scoped_http(recipe: ScopedHttpRecipe, guard: Arc<dyn ToolInterceptor>) -> Self {
        Self::ScopedHttp(ScopedHttpSlot {
            recipe,
            guard,
            mode: ScopedMode::Generic,
            host_binding: String::new(),
            closed_filter: ToolFilter::default(),
        })
    }
}

/// Opaque host configuration; no bearer or held authority lives in this slot.
pub struct ScopedHttpSlot {
    recipe: ScopedHttpRecipe,
    guard: Arc<dyn ToolInterceptor>,
    mode: ScopedMode,
    host_binding: String,
    closed_filter: ToolFilter,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopedMode {
    Generic,
    Closed,
}

enum ScopedCallError {
    Denied,
    Unconfirmed,
}

impl ScopedCallError {
    fn message(&self) -> &'static str {
        match self {
            Self::Denied => "Scoped MCP authorization unavailable; operation refused",
            Self::Unconfirmed => {
                "Scoped MCP result unconfirmed; remote execution may have occurred"
            }
        }
    }

    fn into_mcp_error(self) -> McpError {
        McpError::Io(self.message().into())
    }
}

impl ScopedHttpSlot {
    async fn prepare(
        &self,
        server: &str,
        operation: ClientOperation<'_>,
        args: Value,
    ) -> Result<(Value, SecretBearer, Box<dyn ToolCallLease>), ScopedCallError> {
        // Do not surface a host implementation's arbitrary error string. It
        // might include credentials obtained while preparing this call.
        let PreparedClientCall {
            arguments,
            lease,
            fresh_http_bearer,
        } = self
            .guard
            .prepare_client_call(server, operation, args)
            .await
            .map_err(|_| ScopedCallError::Denied)?;
        let lease = lease.ok_or(ScopedCallError::Denied)?;
        let bearer = fresh_http_bearer.ok_or(ScopedCallError::Denied)?;
        Ok((arguments, bearer, lease))
    }

    async fn discover(&self, server: &str) -> Result<Vec<McpToolDef>, McpError> {
        let operation = async {
            let (_, bearer, lease) = self
                .prepare(server, ClientOperation::Discover, Value::Null)
                .await?;
            let mut client = McpClient::connect_http_with_call_lease(&self.recipe, bearer, lease)
                .await
                .map_err(|_| ScopedCallError::Unconfirmed)?;
            let tools = client.list_tools().await;
            drop(client);
            tools.map_err(|_| ScopedCallError::Unconfirmed)
        };
        match tokio::time::timeout(self.recipe.operation_timeout, operation).await {
            Ok(result) => result.map_err(ScopedCallError::into_mcp_error),
            Err(_) => Err(ScopedCallError::Unconfirmed.into_mcp_error()),
        }
    }

    async fn call(
        &self,
        server: &str,
        name: &str,
        args: Value,
        boundary: Option<&Arc<dyn ClientCallBoundary>>,
    ) -> Result<(Value, ToolCallResult, Option<String>, Option<Box<dyn ClientCallLease>>), ScopedCallError> {
        let (args, bearer, lease) = self
            .prepare(server, ClientOperation::Tool { name }, args)
            .await?;
        let (reservation, boundary_lease) = if let Some(boundary) = boundary {
            boundary.begin_scoped_call(name, &args).await.map_err(|_| ScopedCallError::Denied)?
        } else { (None, None) };
        let mut client = McpClient::connect_http_with_call_lease(&self.recipe, bearer, lease)
            .await
            .map_err(|_| ScopedCallError::Unconfirmed)?;
        let result = client.call_tool_with_boundary(name, args.clone(), boundary_lease.as_deref()).await;
        // Drop the authority before releasing the registry's exact-client
        // mutex. Error, timeout and cancellation drop the same owned value.
        drop(client);
        result
            .map(|result| (args, result, reservation, boundary_lease))
            .map_err(|_| ScopedCallError::Unconfirmed)
    }
}

fn model_visible_tool_reply(result: &Result<ToolCallResult, McpError>) -> bool {
    // Io includes HTTP error bodies; Parse can include upstream diagnostics.
    // Only fixed, payload-less terminal messages can release a reservation.
    !matches!(result, Err(McpError::Closed | McpError::Timeout))
}

/// Transport-owned egress fence, separate from credential authority. Mark
/// before a potentially partial write; only a correlated terminal response (or
/// confirmed upstream cancellation) permits release after dispatch.
pub trait ClientCallLease: Send + Sync {
    fn dispatched(&self);
    fn terminal_response(&self);
}

/// An additive host boundary, independent of credential/transport authority.
/// Admission sees final arguments; completion is based on model-visible payload.
#[async_trait]
pub trait ClientCallBoundary: Send + Sync {
    async fn before_call(&self, tool: &str, args: &Value) -> Result<(), String>;
    async fn after_success(&self, tool: &str) -> Result<(), String>;
    async fn begin_call(&self, tool: &str, args: &Value) -> Result<Option<String>, String> {
        self.before_call(tool, args).await?;
        Ok(None)
    }
    /// The transport owns this guard through completion, including cancellation
    /// and timeout. It must not be kept in a process-local correlation map.
    async fn begin_scoped_call(
        &self, tool: &str, args: &Value,
    ) -> Result<(Option<String>, Option<Box<dyn ClientCallLease>>), String> {
        Ok((self.begin_call(tool, args).await?, None))
    }
    async fn finish_call(&self, tool: &str, _reservation: Option<&str>, payload: bool) -> Result<(), String> {
        if payload { self.after_success(tool).await } else { Ok(()) }
    }
}

/// Aggregates tools from multiple [`McpClient`]s and routes calls by tool
/// name. Implements [`ToolExecutor`], so a registry is what
/// [`run_tool_loop`](crate::run_tool_loop) drives.
///
/// **Collision policy:** first-wins. If two servers advertise the same tool
/// name, the earlier client owns it and a warning is logged; the later one's
/// tool is dropped from both the routing table and `tool_defs()`.
pub struct ToolRegistry {
    clients: Vec<Mutex<RegistryClient>>,
    /// tool name → index into `clients`.
    routes: HashMap<String, usize>,
    defs: Vec<ToolDef>,
    /// Logical server name per client index, parallel to `clients`. Empty
    /// (or a shorter vec) ⇒ the registry was built without names and
    /// [`ToolExecutor::server_of`] answers `None`. Only used to give a
    /// [`crate::ToolInterceptor`] the RFC-23 `<server>.<tool>` namespace.
    server_names: Vec<String>,
    /// Explicitly registered connector authorities, keyed by the resolved
    /// first-wins route. Tool output, including MCP result metadata, cannot
    /// register a verifier or supply an ACL/version by itself.
    source_verifiers: HashMap<String, VerifiedMcpRoute>,
    source_attestation_required: HashSet<String>,
    ccr_disabled_tools: HashSet<String>,
    /// Host-owned guards bound to the exact connected client, never a name
    /// supplied by a model or a colliding logical server name.
    client_interceptors: HashMap<usize, Arc<dyn ToolInterceptor>>,
    client_boundaries: HashMap<usize, Arc<dyn ClientCallBoundary>>,
    client_notices: HashMap<usize, String>,
    /// Host-only deadlines parallel to scoped slots. Keeping this outside the
    /// mutex lets one timeout also bound waiting for that exact client lock.
    scoped_operation_timeouts: HashMap<usize, Duration>,
    closed_client_indices: HashSet<usize>,
}

struct VerifiedMcpRoute {
    scope: CcrScope,
    verifier: Arc<dyn McpSourceVerifier>,
}

/// Independently checked source identity and the latest permitted retention
/// deadline. Neither field is read from MCP output metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedMcpSource {
    pub artifact: CcrSourceArtifact,
    pub retention_at: i64,
}

/// Application-owned authority for one exact MCP route. Implementations
/// must read a source independently of the MCP response, check the caller's
/// scope and compare the entire content that CCR would save. A source ID in
/// model arguments is only a selector, never proof of access or provenance.
#[async_trait]
pub trait McpSourceVerifier: Send + Sync {
    async fn verify(
        &self,
        scope: &CcrScope,
        args: &Value,
        content: &str,
    ) -> Result<VerifiedMcpSource, String>;

    /// True when a call with these arguments is not the source this verifier
    /// guards (for example the same tool pointed at a different store), so
    /// its result is delivered as an ordinary tool result with CCR off
    /// instead of being attested. Defaults to false: every call on the route
    /// must verify or be withheld.
    fn passes_through(&self, _args: &Value) -> bool {
        false
    }
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
        let clients = clients
            .into_iter()
            .map(|client| Mutex::new(RegistryClient::Persistent(client)))
            .collect();
        Ok(Self {
            clients,
            routes,
            defs,
            server_names: Vec::new(),
            source_verifiers: HashMap::new(),
            source_attestation_required: HashSet::new(),
            ccr_disabled_tools: HashSet::new(),
            client_interceptors: HashMap::new(),
            client_boundaries: HashMap::new(),
            client_notices: HashMap::new(),
            scoped_operation_timeouts: HashMap::new(),
            closed_client_indices: HashSet::new(),
        })
    }

    /// Like [`from_clients_filtered`](Self::from_clients_filtered) but records
    /// each client's logical `.mcp.json` / `agent.toml` server name, so
    /// [`ToolExecutor::server_of`] can answer and a
    /// [`crate::ToolInterceptor`] sees the RFC-23 `<server>.<tool>` namespace
    /// (§13.6). Names are positional; a short list simply leaves the trailing
    /// clients unnamed.
    pub async fn from_clients_named(
        clients: Vec<(String, McpClient)>,
        filters: Vec<ToolFilter>,
    ) -> Result<Self, McpError> {
        let (server_names, clients): (Vec<String>, Vec<McpClient>) = clients.into_iter().unzip();
        let mut registry = Self::from_clients_filtered(clients, filters).await?;
        registry.server_names = server_names;
        Ok(registry)
    }

    /// Build mixed persistent/scoped slots. A scoped guard is fixed to its
    /// actual index before any credential-bearing discovery request; neither
    /// remote tool metadata nor a colliding logical name can replace it.
    pub async fn from_client_slots_named(
        mut clients: Vec<(String, RegistryClient)>,
        filters: Vec<ToolFilter>,
    ) -> Result<Self, McpError> {
        let mut client_interceptors = HashMap::new();
        let mut scoped_operation_timeouts = HashMap::new();
        let mut closed_client_indices = HashSet::new();
        for (index, (name, client)) in clients.iter_mut().enumerate() {
            if let RegistryClient::ScopedHttp(slot) = client {
                if name.trim().is_empty() {
                    return Err(McpError::Spawn(
                        "scoped MCP server must have a host name".into(),
                    ));
                }
                if slot.mode == ScopedMode::Closed {
                    slot.closed_filter = filters.get(index).cloned().unwrap_or_default();
                    closed_client_indices.insert(index);
                } else {
                    client_interceptors.insert(index, slot.guard.clone());
                }
                scoped_operation_timeouts.insert(index, slot.recipe.operation_timeout);
            }
        }
        let mut per_client = Vec::with_capacity(clients.len());
        let mut closed_discovery_deadlines = Vec::new();
        for (name, client) in &mut clients {
            let index = per_client.len();
            per_client.push(match client {
                RegistryClient::Persistent(client) => client.list_tools().await?,
                RegistryClient::ScopedHttp(slot) if slot.mode == ScopedMode::Closed => {
                    let deadline = tokio::time::Instant::now() + slot.recipe.operation_timeout;
                    closed_discovery_deadlines.push((index, deadline));
                    slot.discover_closed(name, deadline).await?
                }
                RegistryClient::ScopedHttp(slot) => slot.discover(name).await?,
            });
        }
        for (index, deadline) in &closed_discovery_deadlines {
            if tokio::time::Instant::now() >= *deadline {
                per_client[*index].clear();
            }
        }
        let (routes, defs) = build_routes_filtered(&per_client, &filters);
        let (server_names, clients): (Vec<_>, Vec<_>) = clients.into_iter().unzip();
        let registry = Self {
            clients: clients.into_iter().map(Mutex::new).collect(),
            routes,
            defs,
            server_names,
            source_verifiers: HashMap::new(),
            source_attestation_required: HashSet::new(),
            ccr_disabled_tools: HashSet::new(),
            client_interceptors,
            client_boundaries: HashMap::new(),
            client_notices: HashMap::new(),
            scoped_operation_timeouts,
            closed_client_indices,
        };
        // Cloning/filtering schemas and dropping discovery inputs are part
        // of discovery admission, before the final original-deadline check.
        drop(per_client);
        drop(filters);
        #[cfg(test)]
        if !closed_discovery_deadlines.is_empty() {
            closed_tests::r2_admission_checkpoint("routes", registry.routes.len());
        }
        let now = tokio::time::Instant::now();
        if closed_discovery_deadlines
            .iter()
            .any(|(_, deadline)| now >= *deadline)
        {
            return Err(McpError::Io(
                OwnedScopedFailure::Unconfirmed.message().into(),
            ));
        }
        Ok(registry)
    }

    /// Logical server name owning `tool`, when this registry was built with
    /// names. Exposed for callers that want the namespace outside the loop.
    pub fn server_for_tool(&self, tool: &str) -> Option<&str> {
        let idx = *self.routes.get(tool)?;
        self.server_names.get(idx).map(String::as_str)
    }

    /// Apply a host guard on every call routed to this exact client. It runs
    /// after acquiring the client's lock so a queued call sees live authority.
    /// This composes with, and never replaces, the outer redaction interceptor.
    pub fn register_client_interceptor(
        &mut self,
        client: usize,
        interceptor: Arc<dyn ToolInterceptor>,
    ) -> Result<(), String> {
        if client >= self.clients.len() {
            return Err("MCP guard client is not registered".into());
        }
        if self.client_interceptors.contains_key(&client) {
            return Err("MCP guard already registered for this client".into());
        }
        self.client_interceptors.insert(client, interceptor);
        Ok(())
    }

    pub fn register_client_boundary(
        &mut self,
        client: usize,
        boundary: Arc<dyn ClientCallBoundary>,
    ) -> Result<(), String> {
        if client >= self.clients.len() || self.client_boundaries.contains_key(&client) {
            return Err("MCP call boundary slot unavailable".into());
        }
        self.client_boundaries.insert(client, boundary);
        Ok(())
    }

    /// Add a trusted account notice to schemas routed to this exact client.
    /// The original tool-result JSON remains unchanged for redaction/CCR.
    pub fn annotate_client_tools(&mut self, client: usize, notice: &str) -> Result<(), String> {
        if client >= self.clients.len() {
            return Err("MCP notice client is not registered".into());
        }
        for def in &mut self.defs {
            if self.routes.get(&def.name) == Some(&client) {
                def.description.push_str("\n\n");
                def.description
                    .push_str(&duduclaw_core::truncate_chars(notice, 500));
            }
        }
        Ok(())
    }

    /// Notice for calls actually sent through this client. A refused host
    /// guard returns before this is attached; provider output cannot set it.
    pub fn set_client_notice(&mut self, client: usize, notice: &str) -> Result<(), String> {
        if client >= self.clients.len() {
            return Err("MCP notice client is not registered".into());
        }
        self.client_notices
            .insert(client, duduclaw_core::truncate_chars(notice, 500));
        Ok(())
    }

    /// Require independent source attestation on one exact registry-owned
    /// server/tool route. A failed attestation withholds that result from the
    /// model and retires any prior CCR handle for the call. Unregistered MCP routes retain
    /// their existing scoped, unbound CCR behavior. The model cannot invoke
    /// this registration method.
    pub fn register_source_verifier(
        &mut self,
        scope: CcrScope,
        server: &str,
        tool: &str,
        verifier: Arc<dyn McpSourceVerifier>,
    ) -> Result<(), McpError> {
        if server.trim().is_empty()
            || tool.trim().is_empty()
            || self.server_for_tool(tool) != Some(server)
            || self.source_verifiers.contains_key(tool)
        {
            return Err(McpError::Parse(
                "verified source route does not match a unique registered MCP route".into(),
            ));
        }
        self.source_verifiers
            .insert(tool.into(), VerifiedMcpRoute { scope, verifier });
        self.source_attestation_required.insert(tool.into());
        Ok(())
    }

    /// A declared trusted route must not deliver raw MCP bytes if its
    /// verifier cannot be registered. This is separate from disabling CCR on
    /// an ordinary route whose source authority was never declared.
    pub fn require_source_attestation_for_tool(&mut self, tool: &str) {
        if self.routes.contains_key(tool) {
            self.source_attestation_required.insert(tool.to_owned());
        }
    }

    /// A malformed trusted-source declaration may not reveal which tool was
    /// intended. In that case every mounted route needs a verifier before it
    /// can deliver bytes to the model.
    pub fn require_source_attestation_for_all_tools(&mut self) {
        self.source_attestation_required
            .extend(self.routes.keys().cloned());
    }

    /// Keep normal MCP tool execution while preventing a misconfigured
    /// trusted-source route from silently falling back to unbound CCR.
    pub fn disable_ccr_for_tool(&mut self, tool: &str) {
        if self.routes.contains_key(tool) {
            self.ccr_disabled_tools.insert(tool.to_owned());
        }
    }

    pub fn disable_ccr_for_all_tools(&mut self) {
        self.ccr_disabled_tools.extend(self.routes.keys().cloned());
    }

    async fn outcome_from_result(
        &self,
        name: &str,
        args: &Value,
        result: ToolCallResult,
    ) -> ToolOutcome {
        if let Some(route) = self.source_verifiers.get(name) {
            if route.verifier.passes_through(args) {
                return if result.is_error {
                    ToolOutcome::error(result.content)
                } else {
                    ToolOutcome::ok(result.content).without_ccr()
                };
            }
            // A trusted route must not pass unverified bytes through the
            // ordinary tool-result path, including upstream error bodies.
            if result.is_error {
                return ToolOutcome::error("Trusted MCP source failed; result withheld")
                    .without_ccr();
            }
            return match route
                .verifier
                .verify(&route.scope, args, &result.content)
                .await
            {
                Ok(source) => {
                    let outcome = ToolOutcome::ok(result.content)
                        .with_source_artifact(source.artifact)
                        .with_source_retention_at(source.retention_at);
                    if self.ccr_disabled_tools.contains(name) {
                        outcome.without_ccr()
                    } else {
                        outcome
                    }
                }
                Err(reason) => {
                    warn!(tool = %name, reason = %reason, "MCP source attestation refused");
                    ToolOutcome::error("Trusted MCP source could not be verified; result withheld")
                        .without_ccr()
                }
            };
        }
        if self.source_attestation_required.contains(name) {
            return ToolOutcome::error("Trusted MCP source verifier unavailable; result withheld")
                .without_ccr();
        }
        if result.is_error {
            return ToolOutcome::error(result.content);
        }
        if self.ccr_disabled_tools.contains(name) {
            return ToolOutcome::ok(result.content).without_ccr();
        }
        ToolOutcome::ok(result.content)
    }

    fn outcome_from_error(&self, name: &str, error: McpError) -> Result<ToolOutcome, String> {
        if self.source_attestation_required.contains(name)
            || self.source_verifiers.contains_key(name)
        {
            // JSON-RPC error messages are controlled by the source server
            // and may echo source bytes. A trusted route has no verified
            // content on this path, so retire its old call and reveal no
            // server diagnostic to the model.
            warn!(tool = %name, "trusted MCP route failed");
            return Ok(
                ToolOutcome::error("Trusted MCP source failed; result withheld").without_ccr(),
            );
        }
        Err(error.to_string())
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

    fn server_of(&self, tool: &str) -> Option<String> {
        self.server_for_tool(tool).map(str::to_string)
    }

    async fn verify_ccr_source(
        &self,
        name: &str,
        args: &Value,
        content: &str,
        artifact: &CcrSourceArtifact,
        retention_at: Option<i64>,
    ) -> bool {
        let Some(route) = self.source_verifiers.get(name) else {
            return false;
        };
        route
            .verifier
            .verify(&route.scope, args, content)
            .await
            .is_ok_and(|current| {
                current.artifact == *artifact && retention_at == Some(current.retention_at)
            })
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutcome, String> {
        // Fail-closed: an unrouted name is a dispatch error the loop turns
        // into an is_error tool result.
        let idx = *self
            .routes
            .get(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        if self.closed_client_indices.contains(&idx) {
            let timeout = *self
                .scoped_operation_timeouts
                .get(&idx)
                .ok_or_else(|| "Closed MCP route unavailable".to_string())?;
            return self
                .call_closed_on_client(idx, name, args, tokio::time::Instant::now() + timeout)
                .await;
        }
        if self.source_attestation_required.contains(name)
            && !self.source_verifiers.contains_key(name)
        {
            return Ok(ToolOutcome::error(
                "Trusted MCP source verifier unavailable; result withheld",
            )
            .without_ccr());
        }
        // One deadline owns the complete future, including waiting for this
        // exact slot. Cancellation only drops local authority; remote effects
        // already sent are unconfirmed and must not be automatically replayed.
        if let Some(timeout) = self.scoped_operation_timeouts.get(&idx) {
            match tokio::time::timeout(*timeout, self.call_on_client(idx, name, args)).await {
                Ok(result) => result,
                Err(_) => {
                    // A timeout can happen before the lock is acquired; do
                    // not imply this account actually received the call.
                    Ok(ToolOutcome::error(ScopedCallError::Unconfirmed.message()))
                }
            }
        } else {
            self.call_on_client(idx, name, args).await
        }
    }
}

impl ToolRegistry {
    async fn call_on_client(
        &self,
        idx: usize,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome, String> {
        let mut client = self.clients[idx].lock().await;
        let server = self.server_names.get(idx).map(String::as_str).unwrap_or("");
        let interceptor = self.client_interceptors.get(&idx);
        let boundary = self.client_boundaries.get(&idx);
        let (args, result, reservation, boundary_lease) = match &mut *client {
            RegistryClient::Persistent(client) => {
                let args = if let Some(interceptor) = interceptor {
                    match interceptor.before_call_async(server, name, args).await {
                        InterceptDecision::Allow(args) => args,
                        InterceptDecision::Deny(reason) => return Ok(ToolOutcome::error(reason)),
                    }
                } else {
                    args
                };
                let (reservation, boundary_lease) = if let Some(boundary) = boundary {
                    match boundary.begin_scoped_call(name, &args).await {
                        Ok(admission) => admission,
                        Err(reason) => return Ok(ToolOutcome::error(reason)),
                    }
                } else { (None, None) };
                let result = client.call_tool_with_boundary(name, args.clone(), boundary_lease.as_deref()).await;
                (args, result, reservation, boundary_lease)
            }
            RegistryClient::ScopedHttp(slot) => {
                // prepare_client_call replaces the old before hook only for
                // this branch; calling both would authorize twice.
                match slot.call(server, name, args.clone(), boundary).await {
                    Ok((args, result, reservation, lease)) => (args, Ok(result), reservation, lease),
                    Err(ScopedCallError::Denied) => {
                        return Ok(ToolOutcome::error(ScopedCallError::Denied.message()));
                    }
                    Err(error) => (args, Err(error.into_mcp_error()), None, None),
                }
            }
        };
        drop(client);
        // RPC error messages are upstream-controlled payload too. Scoped
        // failures/cancellation retain the reservation when completion is unknown.
        let payload = model_visible_tool_reply(&result);
        if let Some(boundary) = boundary {
            if let Err(reason) = boundary.finish_call(name, reservation.as_deref(), payload).await {
                return Ok(ToolOutcome::error(reason));
            }
        }
        // The private-read completion is recorded before a later read can enter.
        drop(boundary_lease);
        let outcome = match result {
            Ok(mut result) => {
                if let Some(interceptor) = interceptor {
                    let mut content = serde_json::from_str::<Value>(&result.content)
                        .unwrap_or_else(|_| Value::String(result.content.clone()));
                    let original = content.clone();
                    interceptor.after_call(server, name, &args, &mut content);
                    if content != original {
                        result.content = match content {
                            Value::String(text) => text,
                            value => value.to_string(),
                        };
                    }
                }
                // A transformed result still goes through the same source
                // attestation; a hook cannot attest its own rewritten bytes.
                Ok(self.outcome_from_result(name, &args, result).await)
            }
            Err(e) => self.outcome_from_error(name, e),
        };
        match (outcome, self.client_notices.get(&idx)) {
            (Ok(outcome), Some(notice)) => Ok(outcome.with_host_notice(notice)),
            (Err(error), Some(notice)) => {
                Ok(ToolOutcome::error(format!("tool dispatch failed: {error}"))
                    .with_host_notice(notice))
            }
            (outcome, None) => outcome,
        }
    }
}

#[cfg(test)]
#[path = "mcp_client/boundary_tests.rs"]
mod boundary_tests;

// ---------------------------------------------------------------------------
// Tests — pure framing + routing, no child processes.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `connect_scrubbed` starts the child from the allowlist, not this
    /// process's environment (cargo sets `CARGO_MANIFEST_DIR` on the test
    /// process; it is not on the allowlist). The stub dumps its env and exits
    /// before the handshake, so the connect itself fails.
    #[cfg(unix)]
    #[tokio::test]
    async fn connect_scrubbed_does_not_inherit_the_parent_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("env.txt");
        let script = format!("env > '{}'", out.display());
        let res = McpClient::connect_scrubbed(
            "/bin/sh",
            &["-c".to_string(), script],
            &[("EXPLICIT_VAR".to_string(), "given".to_string())],
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_err(), "the stub never answers the handshake");
        let env = std::fs::read_to_string(&out).unwrap();
        assert!(env.lines().any(|l| l == "EXPLICIT_VAR=given"), "{env}");
        assert!(env.lines().any(|l| l.starts_with("PATH=")) || std::env::var_os("PATH").is_none());
        if std::env::var_os("CARGO_MANIFEST_DIR").is_some() {
            assert!(!env.contains("CARGO_MANIFEST_DIR="), "{env}");
        }
    }

    #[test]
    fn http_endpoint_host_is_compared_exactly_not_by_prefix() {
        // Remote hosts that merely start with a local-looking string.
        for url in [
            "http://localhost.evil.com",
            "http://localhost.evil.com/mcp",
            "http://localhost@evil.com",
            "http://localhost:3000@evil.com/mcp",
            "http://127.0.0.1.evil.com",
            "http://127.0.0.1.evil.com:8080/mcp",
            "http://remote.example",
            "http://[::2]:8080",
            "ftp://localhost/mcp",
            "localhost:3000",
            "not a url",
        ] {
            assert!(!http_endpoint_allowed(url), "{url} must need https");
        }
        // Loopback over plain http, and any https endpoint.
        for url in [
            "http://[::1]:8080",
            "http://[::1]/mcp",
            "http://localhost:3000",
            "http://LOCALHOST:3000/mcp",
            "http://127.0.0.1:8765/mcp",
            "http://127.1.2.3/mcp",
            "https://remote.example",
            "https://remote.example/mcp",
        ] {
            assert!(http_endpoint_allowed(url), "{url} must be accepted");
        }
    }

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
    fn model_or_mcp_metadata_cannot_become_verified_source_identity() {
        let frame = json!({
            "result": {
                "content": [{
                    "type": "text",
                    "text": "unverified body",
                    "_meta": {"connector": "causal", "version": "v1"}
                }],
                "_meta": {
                    "sourceArtifact": {
                        "connector": "causal",
                        "artifact_id": "source-1",
                        "version": "v1",
                        "acl_revision": "admin"
                    }
                }
            }
        });
        let parsed = parse_tool_call_result(&frame).unwrap();
        assert_eq!(parsed.content, "unverified body");
        assert!(!parsed.is_error);
        // ToolCallResult has no artifact field. A bound outcome requires an
        // application-registered verifier; arbitrary MCP _meta is ignored.
    }

    struct ExactVerifier;

    #[async_trait]
    impl McpSourceVerifier for ExactVerifier {
        async fn verify(
            &self,
            scope: &CcrScope,
            args: &Value,
            content: &str,
        ) -> Result<VerifiedMcpSource, String> {
            if scope.source_acl != "principal"
                || args["artifact_id"] != "source-1"
                || content != "exact"
            {
                return Err("unverified".into());
            }
            Ok(VerifiedMcpSource {
                artifact: CcrSourceArtifact {
                    connector: "causal".into(),
                    artifact_id: "source-1".into(),
                    version: "v1".into(),
                    acl_revision: "acl".into(),
                },
                retention_at: 42,
            })
        }
    }

    struct SplitVerifier;

    #[async_trait]
    impl McpSourceVerifier for SplitVerifier {
        async fn verify(
            &self,
            _: &CcrScope,
            _: &Value,
            _: &str,
        ) -> Result<VerifiedMcpSource, String> {
            Err("never verifies".into())
        }
        fn passes_through(&self, args: &Value) -> bool {
            args["store"] == "other"
        }
    }

    /// A call the verifier declares as another store is delivered as an
    /// ordinary result with CCR off; every other call still has to verify.
    #[tokio::test]
    async fn pass_through_calls_skip_attestation_and_ccr() {
        let mut registry = ToolRegistry {
            clients: Vec::new(),
            routes: HashMap::from([("get_source".into(), 0)]),
            defs: Vec::new(),
            server_names: vec!["causal-mcp".into()],
            source_verifiers: HashMap::new(),
            source_attestation_required: HashSet::new(),
            ccr_disabled_tools: HashSet::new(),
            client_interceptors: HashMap::new(),
            client_boundaries: HashMap::new(),
            client_notices: HashMap::new(),
            scoped_operation_timeouts: HashMap::new(),
            closed_client_indices: HashSet::new(),
        };
        let scope = CcrScope {
            tenant_id: "local".into(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            source_acl: "principal".into(),
        };
        registry
            .register_source_verifier(scope, "causal-mcp", "get_source", Arc::new(SplitVerifier))
            .unwrap();
        let ok = |content: &str| ToolCallResult {
            content: content.into(),
            is_error: false,
        };
        let other = registry
            .outcome_from_result("get_source", &json!({"store": "other"}), ok("raw bytes"))
            .await;
        assert_eq!(other.content, "raw bytes");
        assert!(!other.is_error && !other.ccr_eligible && other.source_artifact.is_none());
        let bound = registry
            .outcome_from_result("get_source", &json!({"store": "own"}), ok("raw bytes"))
            .await;
        assert!(
            bound.is_error,
            "an unverifiable call on the route is still withheld"
        );
    }

    #[tokio::test]
    async fn verified_route_requires_exact_registry_owner_and_rechecks_bytes() {
        // A pure registry shell is enough to exercise route registration
        // without starting an MCP process; actual network calls use the same
        // route table built by from_clients_named.
        let mut registry = ToolRegistry {
            clients: Vec::new(),
            routes: HashMap::from([("get_source".into(), 0)]),
            defs: Vec::new(),
            server_names: vec!["causal-mcp".into()],
            source_verifiers: HashMap::new(),
            source_attestation_required: HashSet::new(),
            ccr_disabled_tools: HashSet::new(),
            client_interceptors: HashMap::new(),
            client_boundaries: HashMap::new(),
            client_notices: HashMap::new(),
            scoped_operation_timeouts: HashMap::new(),
            closed_client_indices: HashSet::new(),
        };
        let scope = CcrScope {
            tenant_id: "local".into(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            source_acl: "principal".into(),
        };
        assert!(
            registry
                .register_source_verifier(
                    scope.clone(),
                    "spoofed-server",
                    "get_source",
                    Arc::new(ExactVerifier),
                )
                .is_err()
        );
        registry
            .register_source_verifier(scope, "causal-mcp", "get_source", Arc::new(ExactVerifier))
            .unwrap();
        assert!(
            registry
                .register_source_verifier(
                    CcrScope {
                        tenant_id: "local".into(),
                        agent_id: "agent".into(),
                        session_id: "session".into(),
                        source_acl: "principal".into(),
                    },
                    "causal-mcp",
                    "get_source",
                    Arc::new(ExactVerifier),
                )
                .is_err()
        );
        let source = CcrSourceArtifact {
            connector: "causal".into(),
            artifact_id: "source-1".into(),
            version: "v1".into(),
            acl_revision: "acl".into(),
        };
        let args = json!({"artifact_id": "source-1"});
        assert!(
            registry
                .verify_ccr_source("get_source", &args, "exact", &source, Some(42))
                .await
        );
        assert!(
            !registry
                .verify_ccr_source("get_source", &args, "different", &source, Some(42))
                .await
        );
        assert!(
            !registry
                .verify_ccr_source("get_source", &args, "exact", &source, Some(43))
                .await
        );
        let verified = registry
            .outcome_from_result(
                "get_source",
                &args,
                ToolCallResult {
                    content: "exact".into(),
                    is_error: false,
                },
            )
            .await;
        assert_eq!(verified.source_artifact, Some(source.clone()));
        assert_eq!(verified.source_retention_at, Some(42));
        assert!(verified.ccr_eligible);
        let mismatched = registry
            .outcome_from_result(
                "get_source",
                &args,
                ToolCallResult {
                    content: "different".into(),
                    is_error: false,
                },
            )
            .await;
        assert_eq!(
            mismatched.content,
            "Trusted MCP source could not be verified; result withheld"
        );
        assert!(mismatched.is_error);
        assert!(mismatched.source_artifact.is_none());
        assert!(!mismatched.ccr_eligible);
        assert!(mismatched.ccr_revoke_call);
        let upstream_error = registry
            .outcome_from_result(
                "get_source",
                &args,
                ToolCallResult {
                    content: "sensitive source text in upstream error".into(),
                    is_error: true,
                },
            )
            .await;
        assert_eq!(
            upstream_error.content,
            "Trusted MCP source failed; result withheld"
        );
        assert!(upstream_error.is_error);
        assert!(upstream_error.ccr_revoke_call);
        registry.disable_ccr_for_tool("get_source");
        let disabled = registry
            .outcome_from_result(
                "get_source",
                &args,
                ToolCallResult {
                    content: "exact".into(),
                    is_error: false,
                },
            )
            .await;
        assert!(!disabled.ccr_eligible);
        assert_eq!(disabled.source_artifact, Some(source));
        assert!(disabled.ccr_revoke_call);
        let disabled_mismatch = registry
            .outcome_from_result(
                "get_source",
                &args,
                ToolCallResult {
                    content: "different".into(),
                    is_error: false,
                },
            )
            .await;
        assert!(disabled_mismatch.is_error);
        assert!(!disabled_mismatch.content.contains("different"));
        let rpc_failure = registry
            .outcome_from_error(
                "get_source",
                McpError::Rpc {
                    code: -32000,
                    message: "sensitive source text in RPC error".into(),
                },
            )
            .unwrap();
        assert_eq!(
            rpc_failure.content,
            "Trusted MCP source failed; result withheld"
        );
        assert!(rpc_failure.is_error);
        assert!(rpc_failure.ccr_revoke_call);
        assert!(!rpc_failure.ccr_eligible);
        registry.ccr_disabled_tools.clear();
        registry.source_verifiers.clear();
        let missing_verifier = registry
            .outcome_from_result(
                "get_source",
                &args,
                ToolCallResult {
                    content: "sensitive source without a verifier".into(),
                    is_error: false,
                },
            )
            .await;
        assert!(missing_verifier.is_error);
        assert!(missing_verifier.ccr_revoke_call);
        assert!(!missing_verifier.content.contains("sensitive source"));
        registry.source_attestation_required.clear();
        assert!(
            registry
                .outcome_from_error(
                    "get_source",
                    McpError::Rpc {
                        code: -32000,
                        message: "ordinary MCP diagnostic".into(),
                    },
                )
                .unwrap_err()
                .contains("ordinary MCP diagnostic")
        );
        let generic = registry
            .outcome_from_result(
                "get_source",
                &args,
                ToolCallResult {
                    content: "generic result".into(),
                    is_error: false,
                },
            )
            .await;
        assert!(generic.ccr_eligible);
        assert!(generic.source_artifact.is_none());
    }

    #[test]
    fn parse_tools_list_rpc_error_propagates() {
        let frame = json!({ "id": 1, "error": { "code": -32601, "message": "method not found" } });
        let err = parse_tools_list_response(&frame).unwrap_err();
        assert_eq!(
            err,
            McpError::Rpc {
                code: -32601,
                message: "method not found".into()
            }
        );
    }

    #[test]
    fn parse_tools_list_missing_array_is_parse_error() {
        let frame = json!({ "result": {} });
        assert!(matches!(
            parse_tools_list_response(&frame),
            Err(McpError::Parse(_))
        ));
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
        assert_eq!(
            err,
            McpError::Rpc {
                code: -32000,
                message: "server error".into()
            }
        );
    }

    #[test]
    fn build_routes_first_wins_on_collision() {
        let client_a = vec![
            McpToolDef {
                name: "search".into(),
                description: "A search".into(),
                input_schema: json!({}),
            },
            McpToolDef {
                name: "fetch".into(),
                description: "A fetch".into(),
                input_schema: json!({}),
            },
        ];
        let client_b = vec![
            // Collides with client_a's "search" — must be ignored.
            McpToolDef {
                name: "search".into(),
                description: "B search".into(),
                input_schema: json!({}),
            },
            McpToolDef {
                name: "write".into(),
                description: "B write".into(),
                input_schema: json!({}),
            },
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
        let f2 = ToolFilter {
            allowed: vec![],
            denied: vec!["danger".into()],
        };
        assert!(f2.permits("anything"));
        assert!(!f2.permits("danger"));
    }

    #[test]
    fn build_routes_filtered_applies_per_client_filter() {
        let internal = vec![McpToolDef {
            name: "memory_search".into(),
            description: "".into(),
            input_schema: json!({}),
        }];
        let external = vec![
            McpToolDef {
                name: "crm_list".into(),
                description: "".into(),
                input_schema: json!({}),
            },
            McpToolDef {
                name: "crm_delete".into(),
                description: "".into(),
                input_schema: json!({}),
            },
        ];
        // Internal server: permissive. External server: allowlist crm_list only.
        let filters = vec![
            ToolFilter::default(),
            ToolFilter {
                allowed: vec!["crm_list".into()],
                denied: vec![],
            },
        ];
        let (routes, defs) = build_routes_filtered(&[internal, external], &filters);
        assert!(routes.contains_key("memory_search"));
        assert!(routes.contains_key("crm_list"));
        assert!(
            !routes.contains_key("crm_delete"),
            "filtered external tool absent"
        );
        assert_eq!(defs.len(), 2);
    }
}

#[cfg(test)]
mod http_transport_tests {
    use super::*;

    #[test]
    fn sse_extract_matching_frame() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n";
        let v = sse_extract_response(body, 7).expect("frame found");
        assert_eq!(v["result"]["ok"], serde_json::Value::Bool(true));
    }

    #[test]
    fn sse_extract_skips_other_events_and_multiline_data() {
        // A notification (no id), then the awaited response split over two
        // data: lines within one event.
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\n",
            "data: \"id\":3,\"result\":{}}\n\n",
        );
        assert!(sse_extract_response(body, 3).is_some());
        assert!(sse_extract_response(body, 4).is_none());
    }

    #[tokio::test]
    async fn connect_http_rejects_plain_http_non_localhost() {
        let err = McpClient::connect_http("http://example.com/mcp", &[], Duration::from_secs(1))
            .await
            .err()
            .expect("plain-http non-localhost must be rejected");
        assert!(matches!(err, McpError::Spawn(_)));
    }

    /// Minimal stateless Streamable-HTTP MCP server on a local TCP socket:
    /// answers initialize / tools/list / tools/call with canned JSON bodies.
    /// Exercises the full connect_http → list_tools → call_tool path.
    #[tokio::test]
    async fn http_client_end_to_end_against_local_mock() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = calls.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let calls = server_calls.clone();
                tokio::spawn(async move {
                    // Read until headers end, then honor content-length.
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    let body_start;
                    loop {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            body_start = pos + 4;
                            break;
                        }
                    }
                    let headers = String::from_utf8_lossy(&buf[..body_start]).to_string();
                    let content_length: usize = headers
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + content_length {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body = String::from_utf8_lossy(&buf[body_start..]).to_string();
                    let frame: Value = serde_json::from_str(body.trim()).unwrap_or(Value::Null);
                    let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
                    let id = frame.get("id").and_then(Value::as_i64);

                    let (status, payload) = match (method, id) {
                        ("initialize", Some(id)) => (
                            "200 OK",
                            Some(json!({"jsonrpc":"2.0","id":id,"result":{
                                "protocolVersion":"2025-06-18",
                                "serverInfo":{"name":"MockStateless"},
                                "capabilities":{"tools":{}}}})),
                        ),
                        ("notifications/initialized", None) => ("202 Accepted", None),
                        ("tools/list", Some(id)) => (
                            "200 OK",
                            Some(json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                                {"name":"echo","description":"echo back","inputSchema":{"type":"object"}}
                            ]}})),
                        ),
                        ("tools/call", Some(id)) => {
                            calls.fetch_add(1, Ordering::SeqCst);
                            (
                                "200 OK",
                                Some(json!({"jsonrpc":"2.0","id":id,"result":{
                                "content":[{"type":"text","text":"echoed!"}],
                                "isError":frame["params"]["arguments"]["fail"] == true}})),
                            )
                        }
                        _ => ("400 Bad Request", None),
                    };
                    let body = payload.map(|p| p.to_string()).unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        let url = format!("http://127.0.0.1:{}/mcp", addr.port());
        let mut client = McpClient::connect_http(
            &url,
            &[("authorization".into(), "Bearer test-token".into())],
            Duration::from_secs(5),
        )
        .await
        .expect("handshake against mock");

        let tools = client.list_tools().await.expect("tools/list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let result = client
            .call_tool("echo", json!({"msg": "hi"}))
            .await
            .expect("tools/call");
        assert_eq!(result.content, "echoed!");
        assert!(!result.is_error);

        struct LiveGuard(Arc<AtomicBool>);
        impl ToolInterceptor for LiveGuard {
            fn before_call(&self, _: &str, _: &str, args: Value) -> InterceptDecision {
                if self.0.load(Ordering::SeqCst) {
                    InterceptDecision::Deny("revoked".into())
                } else {
                    InterceptDecision::Allow(args)
                }
            }
            fn after_call(&self, _: &str, _: &str, _: &Value, _: &mut Value) {}
        }
        let second = McpClient::connect_http(&url, &[], Duration::from_secs(5))
            .await
            .unwrap();
        let mut registry = ToolRegistry::from_clients_named(
            vec![("same-name".into(), client), ("same-name".into(), second)],
            vec![],
        )
        .await
        .unwrap();
        // Both advertise echo; first-wins routing must not borrow the other
        // client's authority just because their logical names collide.
        registry
            .register_client_interceptor(1, Arc::new(LiveGuard(Arc::new(AtomicBool::new(true)))))
            .unwrap();
        registry.annotate_client_tools(1, "wrong-account").unwrap();
        registry
            .set_client_notice(1, "wrong-account-fallback")
            .unwrap();
        assert!(!registry.defs()[0].description.contains("wrong-account"));
        let original = registry.call("echo", json!({})).await.unwrap();
        assert!(!original.is_error);
        assert!(
            original.host_notice.is_none(),
            "other client's fallback must not contaminate a successful delegate"
        );
        let revoked = Arc::new(AtomicBool::new(false));
        registry
            .register_client_interceptor(0, Arc::new(LiveGuard(revoked.clone())))
            .unwrap();
        registry
            .annotate_client_tools(0, "company-account fallback")
            .unwrap();
        registry
            .set_client_notice(0, "company-account fallback")
            .unwrap();
        assert!(
            registry.defs()[0]
                .description
                .contains("company-account fallback")
        );
        assert!(
            registry
                .register_client_interceptor(0, Arc::new(LiveGuard(revoked.clone())))
                .is_err()
        );
        assert!(registry.annotate_client_tools(2, "invalid").is_err());
        let fallback = registry.call("echo", json!({})).await.unwrap();
        assert!(!fallback.is_error);
        assert_eq!(
            fallback.content, "echoed!",
            "provider payload remains unchanged"
        );
        assert_eq!(
            fallback.host_notice.as_deref(),
            Some("company-account fallback")
        );
        let boundary = Arc::new(boundary_tests::Probe::default());
        registry.register_client_boundary(0, boundary.clone()).unwrap();
        assert!(!registry.call("echo", json!({"final": true})).await.unwrap().is_error);
        assert_eq!(boundary.after.load(Ordering::SeqCst), 1);
        assert_eq!(boundary.released.load(Ordering::SeqCst), 1);
        assert_eq!(*boundary.args.lock().unwrap(), json!({"final": true}));
        assert!(registry.call("echo", json!({"fail": true})).await.unwrap().is_error);
        assert_eq!(
            boundary.after.load(Ordering::SeqCst),
            2,
            "MCP error responses with payload mark a read"
        );
        assert_eq!(boundary.released.load(Ordering::SeqCst), 2, "error completion releases the sink lease");
        let count = calls.load(Ordering::SeqCst);
        boundary.deny.store(true, Ordering::SeqCst);
        assert!(registry.call("echo", json!({})).await.unwrap().is_error);
        assert_eq!(calls.load(Ordering::SeqCst), count);
        assert_eq!(
            boundary.after.load(Ordering::SeqCst),
            2,
            "pre-call denial does not mark another read"
        );
        boundary.deny.store(false, Ordering::SeqCst);
        let before = calls.load(Ordering::SeqCst);
        let held = registry.clients[0].lock().await;
        let queued = registry.call("echo", json!({}));
        tokio::pin!(queued);
        tokio::select! {
            result = &mut queued => panic!("call escaped the held client lock: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {},
        }
        revoked.store(true, Ordering::SeqCst);
        drop(held);
        let denied = queued.await.unwrap();
        assert!(denied.is_error);
        assert_eq!(denied.content, "revoked");
        assert!(
            denied.host_notice.is_none(),
            "a call never dispatched did not use the fallback account"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            before,
            "denied queued call never reaches provider"
        );
    }
}

#[cfg(all(test, unix))]
mod process_tree_cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn mcp_stdio_cancel_during_handshake_kills_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let target = pidfile.to_string_lossy().into_owned();
        let task = tokio::spawn(async move {
            McpClient::connect("/bin/sh", &[
                "-c".into(), "/bin/sleep 300 & echo $! > \"$PIDFILE\"; wait".into(),
            ], &[("PIDFILE".into(), target)], Duration::from_secs(60)).await
        });
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(pid) = std::fs::read_to_string(&pidfile)
                    .ok().and_then(|text| text.trim().parse::<u32>().ok()) {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("fixture must start a grandchild");
        assert!(duduclaw_core::platform::process_alive(pid));
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(Duration::from_secs(5), async {
            while duduclaw_core::platform::process_alive(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("cancel must kill the MCP server's grandchild");
    }
}
