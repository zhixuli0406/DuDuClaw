//! JSON-RPC 2.0 dispatch: line-delimited frames over stdin/stdout.
//!
//! The pure part ([`respond_to`]) is separated from I/O so protocol behavior
//! is unit-testable; [`serve_stdio`] is the thin async loop `main` runs.

use serde_json::{json, Value};

use crate::api::DocusealApi;
use crate::tools::{plan_for, tool_definitions};

const JSONRPC: &str = "2.0";
/// MCP protocol revision this server accepts/echoes.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// What the loop should do with one parsed inbound frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Dispatch {
    /// Write this frame back.
    Reply(Value),
    /// Notification or unanswerable frame — write nothing.
    Silent,
    /// A `tools/call` to execute: (response id, tool name, arguments).
    Call(Value, String, Value),
}

fn reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC, "id": id, "result": result })
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": JSONRPC, "id": id, "error": { "code": code, "message": message } })
}

/// Pure protocol dispatch for one inbound frame.
pub fn respond_to(frame: &Value) -> Dispatch {
    let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
    let id = frame.get("id").cloned();

    // Notifications (no id) are consumed silently regardless of method.
    let Some(id) = id else { return Dispatch::Silent };

    match method {
        "initialize" => Dispatch::Reply(reply(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "duduclaw-docuseal-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )),
        "tools/list" => Dispatch::Reply(reply(id, json!({ "tools": tool_definitions() }))),
        "tools/call" => {
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if name.is_empty() {
                return Dispatch::Reply(error_reply(id, -32602, "tools/call missing params.name"));
            }
            Dispatch::Call(id, name, args)
        }
        "ping" => Dispatch::Reply(reply(id, json!({}))),
        other => Dispatch::Reply(error_reply(id, -32601, &format!("method not found: {other}"))),
    }
}

/// Wrap a tool outcome (or argument error) as a `tools/call` result frame.
/// Tool-level failures use `isError: true` — the model sees the cause and can
/// retry; JSON-RPC errors are reserved for protocol violations.
pub fn call_result_frame(id: Value, content: String, is_error: bool) -> Value {
    reply(
        id,
        json!({
            "content": [ { "type": "text", "text": content } ],
            "isError": is_error,
        }),
    )
}

/// Run the stdio server loop until stdin closes.
pub async fn serve_stdio(api: DocusealApi) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(trimmed) else {
            continue; // not JSON — ignore, matching the lenient client side
        };
        let out = match respond_to(&frame) {
            Dispatch::Silent => continue,
            Dispatch::Reply(v) => v,
            Dispatch::Call(id, name, args) => match plan_for(&name, &args) {
                Ok(plan) => {
                    let outcome = api.execute(&plan).await;
                    call_result_frame(id, outcome.content, outcome.is_error)
                }
                Err(e) => call_result_frame(id, e.to_string(), true),
            },
        };
        let mut buf = serde_json::to_string(&out).unwrap_or_default();
        buf.push('\n');
        stdout.write_all(buf.as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_replies_with_server_info() {
        let d = respond_to(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
        let Dispatch::Reply(v) = d else { panic!("expected reply") };
        assert_eq!(v["result"]["serverInfo"]["name"], json!("duduclaw-docuseal-mcp"));
        assert_eq!(v["result"]["protocolVersion"], json!(PROTOCOL_VERSION));
    }

    #[test]
    fn notifications_are_silent() {
        let d = respond_to(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        assert_eq!(d, Dispatch::Silent);
    }

    #[test]
    fn tools_list_advertises_all_tools() {
        let d = respond_to(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let Dispatch::Reply(v) = d else { panic!("expected reply") };
        let tools = v["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), tool_definitions().len());
        assert!(tools.iter().all(|t| t["name"].as_str().unwrap().starts_with("docuseal_")));
    }

    #[test]
    fn tools_call_routes_to_call() {
        let d = respond_to(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"docuseal_get_template","arguments":{"template_id":5}}
        }));
        let Dispatch::Call(id, name, args) = d else { panic!("expected call") };
        assert_eq!(id, json!(3));
        assert_eq!(name, "docuseal_get_template");
        assert_eq!(args["template_id"], json!(5));
    }

    #[test]
    fn unknown_method_is_rpc_error() {
        let d = respond_to(&json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}));
        let Dispatch::Reply(v) = d else { panic!("expected reply") };
        assert_eq!(v["error"]["code"], json!(-32601));
    }

    #[test]
    fn call_result_frame_shapes_mcp_content() {
        let v = call_result_frame(json!(9), "done".into(), false);
        assert_eq!(v["result"]["content"][0]["text"], json!("done"));
        assert_eq!(v["result"]["isError"], json!(false));
    }
}
