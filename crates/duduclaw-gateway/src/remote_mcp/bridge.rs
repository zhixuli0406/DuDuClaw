//! stdio ⇄ Streamable HTTP bridge (`duduclaw mcp-remote-bridge`).
//!
//! The Claude CLI starts the bridge as an ordinary stdio MCP server. Every
//! line it writes (requests, notifications, and responses to server-initiated
//! requests) is POSTed unchanged to the remote server with a fresh access
//! token; every JSON-RPC message the server answers with, as a JSON body or as
//! `text/event-stream` events, is written back as one line. The bridge does
//! not interpret MCP beyond three things the transport needs: it remembers the
//! `Mcp-Session-Id` the server assigns, sends the negotiated
//! `MCP-Protocol-Version` header after `initialize`, and on a `401` refreshes
//! the token once and retries before answering with an error.
//!
//! Ordering: `initialize`, notifications and responses are sent one at a time
//! in input order (the server must see `initialize` → `initialized` → the
//! rest); other requests run concurrently so a long tool call does not block
//! the next one.
//!
//! Server-initiated `GET` stream (2026-10-08, off by default, per-server
//! opt-in `server_stream` on the record): after `notifications/initialized`
//! the bridge opens `GET <url>` with `Accept: text/event-stream`, forwards
//! every message on it to stdout, remembers the last SSE `id:` and reconnects
//! with `Last-Event-ID` (backoff 1 s doubling to 30 s, reset by an event); a
//! `405` (the server offers no such stream) or `404` (session gone) stops it,
//! and it is aborted when stdin ends. Resuming a broken *POST* answer stream
//! is still not implemented. Legacy HTTP+SSE (2024-11-05) servers are not
//! supported by this bridge.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc};
use url::Url;

use super::connect::{AuthError, UpstreamAuth, upstream_auth};
use super::url_policy::{OutboundPolicy, pinned_client};

/// Protocol version announced by the connection probe.
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
/// Largest single JSON-RPC message accepted in either direction.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// A pinned HTTP client is rebuilt (DNS re-resolved and re-screened) this often.
const CLIENT_TTL: Duration = Duration::from_secs(300);
/// Upper bound for one upstream exchange (a tool call can take a while).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// JSON-RPC error code for bridge failures.
const BRIDGE_ERROR: i64 = -32000;

struct Session {
    session_id: Option<String>,
    protocol_version: Option<String>,
}

struct ClientCache {
    client: Option<(reqwest::Client, Instant, String)>,
}

struct Bridge {
    home: PathBuf,
    agent_id: String,
    server: String,
    session: Mutex<Session>,
    clients: Mutex<ClientCache>,
    /// Last `Authorization` value used, so a 401 refresh can tell whether
    /// another process already renewed the token.
    last_auth: Mutex<Option<UpstreamAuth>>,
    out: mpsc::UnboundedSender<String>,
    /// Effect classes / `action_rules` / explore lane for this server's
    /// tools (2026-10-08, [`crate::third_party_tools`]).
    gate: crate::third_party_tools::ThirdPartyGate,
    /// The record's `server_stream` opt-in.
    server_stream: bool,
}

fn error_frame(id: &Value, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": BRIDGE_ERROR, "message": message }
    })
    .to_string()
}

enum Kind {
    Request(Value),
    Notification,
    Response,
}

fn classify(frame: &Value) -> Kind {
    match (frame.get("method").is_some(), frame.get("id")) {
        (true, Some(id)) if !id.is_null() => Kind::Request(id.clone()),
        (true, _) => Kind::Notification,
        _ => Kind::Response,
    }
}

impl Bridge {
    fn emit(&self, line: String) {
        let _ = self.out.send(line);
    }

    async fn auth(&self, force: bool) -> Result<UpstreamAuth, AuthError> {
        let seen = if force {
            self.last_auth.lock().await.as_ref().and_then(|a| a.authorization.clone())
        } else {
            None
        };
        let a = upstream_auth(&self.home, &self.agent_id, &self.server, force, seen.as_deref()).await?;
        *self.last_auth.lock().await = Some(a.clone());
        Ok(a)
    }

    async fn client_for(&self, url: &Url) -> Result<reqwest::Client, String> {
        let key = url.origin().ascii_serialization();
        let mut cache = self.clients.lock().await;
        if let Some((c, at, k)) = &cache.client
            && *k == key
            && at.elapsed() < CLIENT_TTL
        {
            return Ok(c.clone());
        }
        let c = pinned_client(url, OutboundPolicy::for_mcp_url(url), REQUEST_TIMEOUT).await?;
        cache.client = Some((c.clone(), Instant::now(), key));
        Ok(c)
    }

    async fn post(&self, auth: &UpstreamAuth, body: &str) -> Result<reqwest::Response, String> {
        let client = self.client_for(&auth.url).await?;
        let mut req = client
            .post(auth.url.clone())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body.to_string());
        req = super::connect::apply_upstream_headers(req, auth);
        {
            let s = self.session.lock().await;
            if let Some(id) = &s.session_id {
                req = req.header("Mcp-Session-Id", id);
            }
            if let Some(v) = &s.protocol_version {
                req = req.header("MCP-Protocol-Version", v);
            }
        }
        req.send()
            .await
            .map_err(|e| format!("cannot reach {}: {e}", super::http::host_label(&auth.url)))
    }

    /// Forward one input line. Errors become JSON-RPC error responses for
    /// requests and are logged for everything else.
    async fn forward(&self, line: String) {
        let frame: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                self.emit(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}).to_string());
                return;
            }
        };
        let kind = classify(&frame);
        let method = frame.get("method").and_then(|m| m.as_str());
        let is_initialize = method == Some("initialize");
        // Third-party tool gate: a refused or unapproved call never leaves.
        if let (Kind::Request(id), Some("tools/call")) = (&kind, method) {
            let tool = frame
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            if let Err(msg) = self.gate.check_call(tool).await {
                self.emit(crate::third_party_tools::refusal_frame(id, &msg));
                return;
            }
        }
        let list_id = match (&kind, method) {
            (Kind::Request(id), Some("tools/list")) => Some(id.clone()),
            _ => None,
        };
        if let Err(msg) = self.exchange(&line, &kind, is_initialize, list_id.as_ref()).await {
            match &kind {
                Kind::Request(id) => self.emit(error_frame(id, &msg)),
                _ => tracing::warn!(server = %self.server, error = %msg, "remote MCP message not delivered"),
            }
        }
    }

    async fn exchange(
        &self,
        body: &str,
        kind: &Kind,
        is_initialize: bool,
        list_id: Option<&Value>,
    ) -> Result<(), String> {
        let mut auth = self.auth(false).await.map_err(|e| e.to_string())?;
        let mut resp = self.post(&auth, body).await?;
        if resp.status().as_u16() == 401 {
            auth = self.auth(true).await.map_err(|e| e.to_string())?;
            resp = self.post(&auth, body).await?;
            if resp.status().as_u16() == 401 {
                return Err(format!(
                    "remote MCP server '{}' still refuses the credential after a refresh (HTTP 401); \
                     an administrator must connect it again in the dashboard",
                    self.server
                ));
            }
        }
        let status = resp.status();
        if status.as_u16() == 404 {
            let had_session = self.session.lock().await.session_id.take().is_some();
            if had_session {
                return Err("the remote MCP session expired; restart the conversation to reconnect".into());
            }
        }
        if is_initialize && status.is_success() {
            if let Some(sid) = resp
                .headers()
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty() && s.len() <= 1024 && s.chars().all(|c| c.is_ascii_graphic()))
            {
                self.session.lock().await.session_id = Some(sid.to_string());
            }
        }
        if status.as_u16() == 202 || status.as_u16() == 204 {
            return Ok(());
        }
        if !status.is_success() {
            return Err(format!("remote MCP server answered HTTP {}", status.as_u16()));
        }
        let expect_id = match kind {
            Kind::Request(id) => Some(id.clone()),
            _ => None,
        };
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            self.relay_sse(resp, expect_id.as_ref(), is_initialize, list_id).await
        } else {
            let bytes = super::http::read_capped(resp, MAX_MESSAGE_BYTES).await?;
            if bytes.iter().all(|b| b.is_ascii_whitespace()) {
                return if expect_id.is_some() {
                    Err("remote MCP server sent an empty answer".into())
                } else {
                    Ok(())
                };
            }
            let v: Value = serde_json::from_slice(&bytes)
                .map_err(|_| "remote MCP server sent an answer that is not JSON".to_string())?;
            match v {
                Value::Array(items) => {
                    for item in items {
                        self.deliver(item, is_initialize, list_id).await;
                    }
                }
                other => self.deliver(other, is_initialize, list_id).await,
            }
            Ok(())
        }
    }

    async fn deliver(&self, mut msg: Value, is_initialize: bool, list_id: Option<&Value>) {
        // The answer to a `tools/list`: hide the tools this employee may not
        // call here and remember every tool's annotations.
        if let Some(id) = list_id
            && msg.get("id") == Some(id)
            && msg.get("method").is_none()
            && let Some(result) = msg.get_mut("result")
        {
            self.gate.filter_list_result(result);
        }
        if is_initialize
            && let Some(v) = msg
                .get("result")
                .and_then(|r| r.get("protocolVersion"))
                .and_then(|v| v.as_str())
            && !v.is_empty()
            && v.len() <= 64
            && v.chars().all(|c| c.is_ascii_graphic())
        {
            self.session.lock().await.protocol_version = Some(v.to_string());
        }
        self.emit(msg.to_string());
    }

    /// Relay SSE events until the response to `expect_id` arrives (or the
    /// stream ends).
    async fn relay_sse(
        &self,
        mut resp: reqwest::Response,
        expect_id: Option<&Value>,
        is_initialize: bool,
        list_id: Option<&Value>,
    ) -> Result<(), String> {
        let mut buf: Vec<u8> = Vec::new();
        let mut answered = expect_id.is_none();
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => return Err(format!("the remote event stream broke: {e}")),
            };
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_MESSAGE_BYTES {
                return Err("a remote event is larger than the bridge accepts".into());
            }
            while let Some((event, rest)) = split_event(&buf) {
                buf = rest;
                if let Some(data) = event_data(&event)
                    && let Ok(v) = serde_json::from_str::<Value>(&data)
                {
                    let is_answer = expect_id.is_some_and(|id| {
                        v.get("id") == Some(id) && v.get("method").is_none()
                    });
                    self.deliver(v, is_initialize, list_id).await;
                    if is_answer {
                        answered = true;
                    }
                }
                if answered && expect_id.is_some() {
                    return Ok(());
                }
            }
        }
        if !answered {
            return Err("the remote event stream ended without an answer".into());
        }
        Ok(())
    }

    async fn close_session(&self) {
        let sid = self.session.lock().await.session_id.clone();
        let Some(sid) = sid else { return };
        let Ok(auth) = self.auth(false).await else { return };
        let Ok(client) = self.client_for(&auth.url).await else { return };
        let req = super::connect::apply_upstream_headers(
            client.delete(auth.url.clone()).header("Mcp-Session-Id", sid),
            &auth,
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), req.send()).await;
    }

    /// The optional server-initiated stream (see the module docs). Runs
    /// until the server says it has none (405), the session is gone (404),
    /// the credential cannot be renewed, or the task is aborted.
    async fn server_stream_loop(self: Arc<Self>) {
        let mut last_event_id: Option<String> = None;
        let mut backoff = STREAM_BACKOFF_START;
        loop {
            match self.server_stream_once(&mut last_event_id).await {
                StreamEnd::Stop(why) => {
                    tracing::debug!(server = %self.server, why, "server-initiated stream stopped");
                    return;
                }
                StreamEnd::Progress => backoff = STREAM_BACKOFF_START,
                StreamEnd::Retry => {}
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(STREAM_BACKOFF_MAX);
        }
    }

    async fn server_stream_once(&self, last_event_id: &mut Option<String>) -> StreamEnd {
        let mut forced = false;
        let resp = loop {
            let auth = match self.auth(forced).await {
                Ok(a) => a,
                Err(_) => return StreamEnd::Stop("credential unavailable"),
            };
            let client = match self.client_for(&auth.url).await {
                Ok(c) => c,
                Err(_) => return StreamEnd::Retry,
            };
            // Long-lived: no overall timeout, only connect/idle limits of the
            // pinned client.
            let mut req = client.get(auth.url.clone()).header("Accept", "text/event-stream");
            req = super::connect::apply_upstream_headers(req, &auth);
            {
                let s = self.session.lock().await;
                if let Some(id) = &s.session_id {
                    req = req.header("Mcp-Session-Id", id);
                }
                if let Some(v) = &s.protocol_version {
                    req = req.header("MCP-Protocol-Version", v);
                }
            }
            if let Some(id) = last_event_id.as_deref() {
                req = req.header("Last-Event-ID", id);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(_) => return StreamEnd::Retry,
            };
            if resp.status().as_u16() == 401 && !forced {
                forced = true;
                continue;
            }
            break resp;
        };
        match resp.status().as_u16() {
            405 => return StreamEnd::Stop("server offers no GET stream"),
            404 => return StreamEnd::Stop("session gone"),
            401 | 403 => return StreamEnd::Stop("credential refused"),
            s if !(200..300).contains(&s) => return StreamEnd::Retry,
            _ => {}
        }
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.to_ascii_lowercase().starts_with("text/event-stream"));
        if !is_sse {
            return StreamEnd::Stop("not an event stream");
        }
        let mut resp = resp;
        let mut buf: Vec<u8> = Vec::new();
        let mut progressed = false;
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) | Err(_) => break,
            };
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_MESSAGE_BYTES {
                return StreamEnd::Retry;
            }
            while let Some((event, rest)) = split_event(&buf) {
                buf = rest;
                if let Some(id) = event_id(&event) {
                    *last_event_id = Some(id);
                }
                if let Some(data) = event_data(&event)
                    && let Ok(v) = serde_json::from_str::<Value>(&data)
                {
                    self.deliver(v, false, None).await;
                    progressed = true;
                }
            }
        }
        if progressed { StreamEnd::Progress } else { StreamEnd::Retry }
    }
}

/// How one `GET` stream attempt ended.
enum StreamEnd {
    /// Do not reconnect.
    Stop(&'static str),
    /// At least one message arrived; reconnect from the start of the backoff.
    Progress,
    /// Reconnect after the current backoff.
    Retry,
}

const STREAM_BACKOFF_START: Duration = Duration::from_secs(1);
const STREAM_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The `id:` of one SSE event (no NUL, at most 256 bytes, as sent back in
/// `Last-Event-ID`).
pub(crate) fn event_id(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    let mut out = None;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("id:") {
            let v = rest.strip_prefix(' ').unwrap_or(rest);
            if !v.contains('\0') && v.len() <= 256 && v.chars().all(|c| c.is_ascii_graphic()) {
                out = Some(v.to_string());
            }
        }
    }
    out
}

/// Split the first complete SSE event (terminated by a blank line) off `buf`.
pub(crate) fn split_event(buf: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            // "\n\n" or "\n\r\n"
            if buf.get(i + 1) == Some(&b'\n') {
                return Some((buf[..i].to_vec(), buf[i + 2..].to_vec()));
            }
            if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
                return Some((buf[..i].to_vec(), buf[i + 3..].to_vec()));
            }
        }
        i += 1;
    }
    None
}

/// The joined `data:` lines of one SSE event.
pub(crate) fn event_data(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    let mut lines: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if lines.is_empty() { None } else { Some(lines.join("\n")) }
}

/// Run the bridge until `input` ends. Returns the process exit code.
pub async fn run_bridge<R, W>(home: &Path, agent_id: &str, server: &str, input: R, mut output: W) -> i32
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    if let Err(e) = super::store::validate_ids(agent_id, server) {
        tracing::error!(error = %e, "mcp-remote-bridge: invalid arguments");
        return 2;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if output.write_all(line.as_bytes()).await.is_err()
                || output.write_all(b"\n").await.is_err()
                || output.flush().await.is_err()
            {
                break;
            }
        }
    });
    let bridge = Arc::new(Bridge {
        home: home.to_path_buf(),
        agent_id: agent_id.to_string(),
        server: server.to_string(),
        session: Mutex::new(Session { session_id: None, protocol_version: None }),
        clients: Mutex::new(ClientCache { client: None }),
        last_auth: Mutex::new(None),
        out: tx,
        gate: crate::third_party_tools::ThirdPartyGate::new(home, agent_id, server, "bridge"),
        server_stream: super::store::get(home, agent_id, server)
            .ok()
            .flatten()
            .is_some_and(|r| r.server_stream),
    });
    let mut stream_task: Option<tokio::task::JoinHandle<()>> = None;

    let mut lines = input.lines();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "mcp-remote-bridge: stdin read failed");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_MESSAGE_BYTES {
            bridge.emit(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"message too large"}}).to_string());
            continue;
        }
        let parsed = serde_json::from_str::<Value>(&line).ok();
        let ordered = match &parsed {
            Some(v) => {
                !matches!(classify(v), Kind::Request(_))
                    || v.get("method").and_then(|m| m.as_str()) == Some("initialize")
            }
            None => true,
        };
        let is_initialized = parsed
            .as_ref()
            .and_then(|v| v.get("method"))
            .and_then(|m| m.as_str())
            == Some("notifications/initialized");
        if ordered {
            bridge.forward(line).await;
            if is_initialized && bridge.server_stream && stream_task.is_none() {
                stream_task = Some(tokio::spawn(bridge.clone().server_stream_loop()));
            }
        } else {
            let b = bridge.clone();
            tasks.spawn(async move { b.forward(line).await });
        }
        while tasks.try_join_next().is_some() {}
    }
    while tasks.join_next().await.is_some() {}
    if let Some(t) = stream_task.take() {
        t.abort();
        let _ = t.await;
    }
    bridge.close_session().await;
    drop(bridge);
    let _ = writer.await;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_are_split_and_data_joined() {
        let buf = b"event: message\ndata: {\"a\":1}\n\ndata: {\"b\":\r\ndata: 2}\r\n\r\npartial";
        let (e1, rest) = split_event(buf).unwrap();
        assert_eq!(event_data(&e1).unwrap(), "{\"a\":1}");
        let (e2, rest) = split_event(&rest).unwrap();
        assert_eq!(event_data(&e2).unwrap(), "{\"b\":\n2}");
        assert!(split_event(&rest).is_none());
        assert!(event_data(b": comment only").is_none());
        assert_eq!(event_id(b"id: 42\ndata: {}").as_deref(), Some("42"));
        assert_eq!(event_id(b"data: {}"), None);
    }

    #[test]
    fn frames_are_classified() {
        assert!(matches!(classify(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})), Kind::Request(_)));
        assert!(matches!(classify(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})), Kind::Notification));
        assert!(matches!(classify(&json!({"jsonrpc":"2.0","id":7,"result":{}})), Kind::Response));
    }

    #[tokio::test]
    async fn a_server_that_is_not_connected_answers_every_request_with_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n";
        let (client, server) = tokio::io::duplex(64 * 1024);
        let code = run_bridge(dir.path(), "a1", "remote", &input[..], server).await;
        assert_eq!(code, 0);
        let mut out = String::new();
        let mut r = tokio::io::BufReader::new(client);
        use tokio::io::AsyncReadExt;
        r.read_to_string(&mut out).await.unwrap();
        let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert_eq!(lines[0]["id"], 1);
        assert!(lines[0]["error"]["message"].as_str().unwrap().contains("dashboard"));
        assert_eq!(lines[1]["id"], 2);
    }
}
