//! A minimal MCP client for `events/subscribe` / `events/unsubscribe`
//! against a remote server connected through [`crate::remote_mcp`].
//!
//! One short session per operation: `initialize` (the result must declare
//! `capabilities.events`), `notifications/initialized`, the request, then a
//! `DELETE` of the session. Same transport rules as the bridge: the
//! credential comes from `remote_mcp::connect::upstream_auth` (refreshed once
//! on a 401), the HTTP client is pinned to screened public addresses, JSON
//! or `text/event-stream` answers are read with the 8 MiB cap.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::remote_mcp::bridge::{MAX_MESSAGE_BYTES, event_data, split_event};
use crate::remote_mcp::connect::{UpstreamAuth, upstream_auth};
use crate::remote_mcp::url_policy::{OutboundPolicy, pinned_client};

/// Protocol version announced in `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const TIMEOUT: Duration = Duration::from_secs(30);

/// An open session with the server.
pub struct Session {
    home: std::path::PathBuf,
    agent_id: String,
    server: String,
    client: reqwest::Client,
    auth: UpstreamAuth,
    session_id: Option<String>,
    protocol: Option<String>,
    next_id: i64,
}

/// Why an operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// The server does not declare `capabilities.events`.
    NoEventsCapability,
    /// A JSON-RPC error answer (code, message cut at 200 chars).
    Rpc(i64, String),
    /// Transport, auth or protocol failure.
    Other(String),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEventsCapability => f.write_str("the server does not offer MCP Events"),
            Self::Rpc(code, msg) => write!(f, "the server refused ({code}): {msg}"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl Session {
    /// Connect and initialize. Fails with [`UpstreamError::NoEventsCapability`]
    /// when the server does not advertise events.
    pub async fn open(home: &Path, agent_id: &str, server: &str) -> Result<Self, UpstreamError> {
        let auth = upstream_auth(home, agent_id, server, false, None)
            .await
            .map_err(|e| UpstreamError::Other(e.to_string()))?;
        let client = pinned_client(&auth.url, OutboundPolicy::for_mcp_url(&auth.url), TIMEOUT)
            .await
            .map_err(UpstreamError::Other)?;
        let mut s = Self {
            home: home.to_path_buf(),
            agent_id: agent_id.to_string(),
            server: server.to_string(),
            client,
            auth,
            session_id: None,
            protocol: None,
            next_id: 1,
        };
        let init = s
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "DuDuClaw", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        if let Some(v) = init.get("protocolVersion").and_then(|v| v.as_str())
            && !v.is_empty()
            && v.len() <= 64
            && v.chars().all(|c| c.is_ascii_graphic())
        {
            s.protocol = Some(v.to_string());
        }
        if init.get("capabilities").and_then(|c| c.get("events")).is_none() {
            s.close().await;
            return Err(UpstreamError::NoEventsCapability);
        }
        s.notify("notifications/initialized").await?;
        Ok(s)
    }

    async fn post(&self, body: &str) -> Result<reqwest::Response, UpstreamError> {
        let mut req = self
            .client
            .post(self.auth.url.clone())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body.to_string());
        req = crate::remote_mcp::connect::apply_upstream_headers(req, &self.auth);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        if let Some(v) = &self.protocol {
            req = req.header("MCP-Protocol-Version", v);
        }
        req.send().await.map_err(|e| {
            UpstreamError::Other(format!(
                "cannot reach {}: {e}",
                crate::remote_mcp::http::host_label(&self.auth.url)
            ))
        })
    }

    async fn post_with_refresh(&mut self, body: &str) -> Result<reqwest::Response, UpstreamError> {
        let resp = self.post(body).await?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        let seen = self.auth.authorization.clone();
        self.auth = upstream_auth(&self.home, &self.agent_id, &self.server, true, seen.as_deref())
            .await
            .map_err(|e| UpstreamError::Other(e.to_string()))?;
        self.post(body).await
    }

    async fn notify(&mut self, method: &str) -> Result<(), UpstreamError> {
        let body = json!({ "jsonrpc": "2.0", "method": method }).to_string();
        let resp = self.post_with_refresh(&body).await?;
        if !resp.status().is_success() {
            return Err(UpstreamError::Other(format!("HTTP {} for {method}", resp.status().as_u16())));
        }
        Ok(())
    }

    /// Send one request and return its `result`.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, UpstreamError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        let resp = self.post_with_refresh(&body).await?;
        let status = resp.status();
        if method == "initialize"
            && let Some(sid) = resp
                .headers()
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty() && s.len() <= 1024 && s.chars().all(|c| c.is_ascii_graphic()))
        {
            self.session_id = Some(sid.to_string());
        }
        if !status.is_success() {
            return Err(UpstreamError::Other(format!("HTTP {} for {method}", status.as_u16())));
        }
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let answer = if ctype.starts_with("text/event-stream") {
            read_sse_answer(resp, id).await?
        } else {
            let bytes = crate::remote_mcp::http::read_capped(resp, MAX_MESSAGE_BYTES)
                .await
                .map_err(UpstreamError::Other)?;
            let v: Value = serde_json::from_slice(&bytes)
                .map_err(|_| UpstreamError::Other(format!("{method}: the answer is not JSON")))?;
            match v {
                Value::Array(items) => items
                    .into_iter()
                    .find(|m| m.get("id") == Some(&json!(id)))
                    .ok_or_else(|| UpstreamError::Other(format!("{method}: no answer")))?,
                other => other,
            }
        };
        if let Some(err) = answer.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            let mut msg = duduclaw_core::truncate_chars(err.get("message").and_then(|m| m.as_str()).unwrap_or(""), 200).to_string();
            // `-32015 CallbackEndpointError`: name the draft's fixed reason
            // category (never a raw string from the server).
            if code == -32015
                && let Some(reason) = err.get("data").and_then(|d| d.get("reason")).and_then(|r| r.as_str())
                && super::store::DELIVERY_ERROR_CATEGORIES.contains(&reason)
            {
                msg = format!("{msg} [{reason}]");
            }
            return Err(UpstreamError::Rpc(code, msg));
        }
        answer
            .get("result")
            .cloned()
            .ok_or_else(|| UpstreamError::Other(format!("{method}: the answer has no result")))
    }

    /// End the session (best effort).
    pub async fn close(self) {
        let Some(sid) = self.session_id.clone() else { return };
        let mut req = self.client.delete(self.auth.url.clone()).header("Mcp-Session-Id", sid);
        req = crate::remote_mcp::connect::apply_upstream_headers(req, &self.auth);
        let _ = tokio::time::timeout(Duration::from_secs(5), req.send()).await;
    }
}

async fn read_sse_answer(mut resp: reqwest::Response, id: i64) -> Result<Value, UpstreamError> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return Err(UpstreamError::Other(format!("the event stream broke: {e}"))),
        };
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_MESSAGE_BYTES {
            return Err(UpstreamError::Other("the answer is too large".into()));
        }
        while let Some((event, rest)) = split_event(&buf) {
            buf = rest;
            if let Some(data) = event_data(&event)
                && let Ok(v) = serde_json::from_str::<Value>(&data)
                && v.get("id") == Some(&json!(id))
                && v.get("method").is_none()
            {
                return Ok(v);
            }
        }
    }
    Err(UpstreamError::Other("the event stream ended without an answer".into()))
}

/// The server's answer to `events/subscribe`.
#[derive(Debug, Clone, Default)]
pub struct SubscribeReply {
    pub id: Option<String>,
    pub refresh_before: Option<String>,
    pub cursor: Option<String>,
    pub truncated: bool,
    pub delivery_status: Option<super::store::DeliveryStatus>,
}

fn clean_token(v: Option<&Value>, max: usize) -> Option<String> {
    v.and_then(|v| v.as_str())
        .filter(|v| !v.is_empty() && v.len() <= max && v.chars().all(|c| c.is_ascii_graphic()))
        .map(str::to_string)
}

/// A cursor is an opaque server token: kept only when it is a bounded
/// printable string (it is sent back verbatim).
fn clean_cursor(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .filter(|v| !v.is_empty() && v.len() <= 1024 && v.chars().all(|c| !c.is_control()))
        .map(str::to_string)
}

fn parse_delivery_status(v: &Value) -> Option<super::store::DeliveryStatus> {
    let o = v.as_object()?;
    let ts = |k: &str| {
        o.get(k)
            .and_then(|v| v.as_str())
            .filter(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
            .map(str::to_string)
    };
    Some(super::store::DeliveryStatus {
        active: o.get("active").and_then(|v| v.as_bool()).unwrap_or(true),
        last_delivery_at: ts("lastDeliveryAt"),
        last_error: o
            .get("lastError")
            .and_then(|v| v.as_str())
            .filter(|e| super::store::DELIVERY_ERROR_CATEGORIES.contains(e))
            .map(str::to_string),
        failed_since: ts("failedSince"),
        throttled: o.get("throttled").and_then(|v| v.as_bool()).unwrap_or(false),
        retry_after_ms: o.get("retryAfterMs").and_then(|v| v.as_u64()).map(|n| n.min(86_400_000)),
    })
}

/// `events/subscribe` for one event name (also the refresh: same key).
pub async fn subscribe(
    s: &mut Session,
    name: &str,
    arguments: &Value,
    url: &str,
    secret: &str,
    cursor: Option<&str>,
    max_age_ms: Option<u64>,
    ttl_ms: u64,
) -> Result<SubscribeReply, UpstreamError> {
    let mut params = json!({
        "name": name,
        "arguments": arguments,
        "delivery": { "mode": "webhook", "url": url, "secret": secret },
        "cursor": cursor,
        "ttlMs": ttl_ms,
    });
    if cursor.is_some()
        && let Some(ms) = max_age_ms
    {
        params["maxAgeMs"] = json!(ms);
    }
    let r = s.request("events/subscribe", params).await?;
    Ok(SubscribeReply {
        id: clean_token(r.get("id"), 256),
        refresh_before: r
            .get("refreshBefore")
            .and_then(|v| v.as_str())
            .filter(|v| chrono::DateTime::parse_from_rfc3339(v).is_ok())
            .map(str::to_string),
        cursor: clean_cursor(r.get("cursor")),
        truncated: r.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false),
        delivery_status: r.get("deliveryStatus").and_then(parse_delivery_status),
    })
}

/// `events/unsubscribe` for one event name.
pub async fn unsubscribe(s: &mut Session, name: &str, arguments: &Value, url: &str) -> Result<(), UpstreamError> {
    s.request(
        "events/unsubscribe",
        json!({ "name": name, "arguments": arguments, "delivery": { "url": url } }),
    )
    .await
    .map(|_| ())
}

/// One event type from `events/list`.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct EventDescriptor {
    pub name: String,
    pub description: String,
    /// Subset of `poll`, `push`, `webhook` the server advertises.
    pub delivery: Vec<String>,
    /// JSON Schema of the subscription arguments (`null` when over 8 KiB).
    pub input_schema: Value,
    pub payload_schema: Value,
}

const MAX_SCHEMA_BYTES: usize = 8 * 1024;
/// Pages and descriptors read from `events/list`.
const MAX_LIST_PAGES: usize = 10;
pub const MAX_DESCRIPTORS: usize = 200;

fn bounded_schema(v: Option<&Value>) -> Value {
    match v {
        Some(v) if v.is_object() && v.to_string().len() <= MAX_SCHEMA_BYTES => v.clone(),
        _ => Value::Null,
    }
}

/// `events/list`, following `nextCursor` (≤10 pages, ≤200 descriptors).
/// Server text is data: descriptions are cut and stripped of control
/// characters; names must be valid event names.
pub async fn list_events(s: &mut Session) -> Result<Vec<EventDescriptor>, UpstreamError> {
    let mut out: Vec<EventDescriptor> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_LIST_PAGES {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let r = s.request("events/list", params).await?;
        for e in r.get("events").and_then(|e| e.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
            let name = e.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if !super::store::is_valid_event_name(name) || out.len() >= MAX_DESCRIPTORS {
                continue;
            }
            let delivery: Vec<String> = e
                .get("delivery")
                .and_then(|d| d.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.as_str())
                        .filter(|m| matches!(*m, "poll" | "push" | "webhook"))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let desc: String = e
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            out.push(EventDescriptor {
                name: name.to_string(),
                description: duduclaw_core::truncate_chars(&desc, 300).to_string(),
                delivery,
                input_schema: bounded_schema(e.get("inputSchema")),
                payload_schema: bounded_schema(e.get("payloadSchema")),
            });
        }
        cursor = clean_cursor(r.get("nextCursor"));
        if cursor.is_none() || out.len() >= MAX_DESCRIPTORS {
            break;
        }
    }
    Ok(out)
}

/// The answer to one `events/poll`.
#[derive(Debug, Clone, Default)]
pub struct PollReply {
    /// Raw `EventOccurrence` objects (at most `maxEvents`).
    pub events: Vec<Value>,
    pub cursor: Option<String>,
    pub truncated: bool,
    pub has_more: bool,
    pub next_poll_ms: Option<u64>,
}

/// Most events taken from one poll answer.
pub const POLL_MAX_EVENTS: usize = 50;

/// `events/poll` for one subscription. `cursor: None` = start from now.
pub async fn poll(
    s: &mut Session,
    name: &str,
    arguments: &Value,
    cursor: Option<&str>,
    max_age_ms: Option<u64>,
) -> Result<PollReply, UpstreamError> {
    let mut params = json!({ "name": name, "arguments": arguments, "cursor": cursor, "maxEvents": POLL_MAX_EVENTS });
    if cursor.is_some()
        && let Some(ms) = max_age_ms
    {
        params["maxAgeMs"] = json!(ms);
    }
    let r = s.request("events/poll", params).await?;
    let events: Vec<Value> = r
        .get("events")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter(|e| e.is_object()).take(POLL_MAX_EVENTS).cloned().collect())
        .unwrap_or_default();
    Ok(PollReply {
        events,
        cursor: clean_cursor(r.get("cursor")),
        truncated: r.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false),
        has_more: r.get("hasMore").and_then(|v| v.as_bool()).unwrap_or(false),
        next_poll_ms: r.get("nextPollMs").and_then(|v| v.as_u64()),
    })
}
