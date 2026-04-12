//! XML-RPC and JSON-RPC dual-protocol client for Odoo.
//!
//! [O-1a] Supports both protocols; JSON-RPC is preferred for performance.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use tracing::debug;

/// Monotonically increasing JSON-RPC request ID for correct response pairing (MW-M7).
static RPC_ID: AtomicU64 = AtomicU64::new(1);

/// RPC protocol to use for communicating with Odoo.
#[derive(Debug, Clone, PartialEq)]
pub enum Protocol {
    JsonRpc,
    XmlRpc,
}

impl Protocol {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "xmlrpc" | "xml-rpc" | "xml_rpc" => Self::XmlRpc,
            _ => Self::JsonRpc,
        }
    }
}

/// Low-level JSON-RPC call to Odoo.
pub async fn jsonrpc_call(
    http: &reqwest::Client,
    url: &str,
    service: &str,
    method: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    let payload = json!({
        "jsonrpc": "2.0",
        "method": "call",
        "params": {
            "service": service,
            "method": method,
            "args": args,
        },
        "id": RPC_ID.fetch_add(1, Ordering::Relaxed),
    });

    debug!(url, service, method, "Odoo JSON-RPC call");

    let resp = http
        .post(format!("{url}/jsonrpc"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Odoo returned HTTP {}", resp.status()));
    }

    let body: Value = resp.json().await.map_err(|e| format!("JSON parse: {e}"))?;

    if let Some(error) = body.get("error") {
        let msg = error
            .get("data")
            .and_then(|d| d.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| error.get("message").and_then(|m| m.as_str()))
            .unwrap_or("Unknown Odoo error");
        return Err(format!("Odoo error: {msg}"));
    }

    Ok(body.get("result").cloned().unwrap_or(Value::Null))
}

/// Authenticate with Odoo and return the user ID.
pub async fn authenticate(
    http: &reqwest::Client,
    url: &str,
    db: &str,
    username: &str,
    credential: &str,
) -> Result<i64, String> {
    let result = jsonrpc_call(
        http,
        url,
        "common",
        "authenticate",
        vec![json!(db), json!(username), json!(credential), json!({})],
    )
    .await?;

    result
        .as_i64()
        .filter(|uid| *uid > 0)
        .ok_or_else(|| "Authentication failed: invalid credentials or database".to_string())
}

/// Get Odoo server version info.
pub async fn version(http: &reqwest::Client, url: &str) -> Result<Value, String> {
    jsonrpc_call(http, url, "common", "version", vec![]).await
}

/// Execute an ORM method via `execute_kw`.
#[allow(clippy::too_many_arguments)]
pub async fn execute_kw(
    http: &reqwest::Client,
    url: &str,
    db: &str,
    uid: i64,
    credential: &str,
    model: &str,
    method: &str,
    args: Vec<Value>,
    kwargs: Value,
) -> Result<Value, String> {
    jsonrpc_call(
        http,
        url,
        "object",
        "execute_kw",
        vec![
            json!(db),
            json!(uid),
            json!(credential),
            json!(model),
            json!(method),
            json!(args),
            kwargs,
        ],
    )
    .await
}
