// Included separately below each credential owner. No raw-reply re-export.
use duduclaw_llm::{McpToolDef, ToolCallResult, OwnedScopedFailure, OwnedScopedReply};
use crate::google_identity::effect_authority::RemoteRequestBound;
use serde_json::Value;
#[async_trait::async_trait]
pub(super) trait LocalIo: Send {
    async fn post(
        &mut self,
        frame: &[u8],
        capture_session: bool,
    ) -> Result<(String, Vec<u8>), OwnedScopedFailure>;
    fn checkpoint(&mut self) -> Result<(), OwnedScopedFailure>;
    fn scrub(&self, value: &mut Value) -> Result<(), OwnedScopedFailure>;
    fn identifier(&self, value: &str) -> Result<(), OwnedScopedFailure>;
}
fn uncertain() -> OwnedScopedFailure {
    OwnedScopedFailure::Unconfirmed
}
fn decode(content_type: &str, bytes: &[u8], id: i64) -> Result<Value, OwnedScopedFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| uncertain())?;
    let value = if content_type
        .to_ascii_lowercase()
        .starts_with("text/event-stream")
    {
        text.replace("\r\n", "\n")
            .split("\n\n")
            .filter_map(|event| {
                let data = event
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|v| v.strip_prefix(' ').unwrap_or(v))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::from_str::<Value>(&data)
                    .ok()
                    .filter(|v| v.get("id").and_then(Value::as_i64) == Some(id))
            })
            .next()
            .ok_or_else(uncertain)?
    } else {
        serde_json::from_str(text).map_err(|_| uncertain())?
    };
    if value.get("id").and_then(Value::as_i64) != Some(id) || value.get("error").is_some() {
        return Err(uncertain());
    }
    Ok(value)
}
// Registry and loop redaction parse JSON-looking text again. Complete those
// transformations here before the last secret scrub. Excess quoted nesting
// refuses, rather than passing an unfinished representation to an outer hook.
fn scrub_nested<I: LocalIo>(
    io: &I,
    value: &mut Value,
    remaining: usize,
) -> Result<(), OwnedScopedFailure> {
    match value {
        Value::String(text) => {
            if let Ok(mut decoded) = serde_json::from_str::<Value>(text) {
                if remaining == 0 {
                    return Err(uncertain());
                }
                let original = decoded.clone();
                scrub_nested(io, &mut decoded, remaining - 1)?;
                if decoded != original {
                    *text = serde_json::to_string(&decoded).map_err(|_| uncertain())?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_nested(io, item, remaining)?;
            }
        }
        Value::Object(items) => {
            for key in items.keys() {
                io.identifier(key)?;
            }
            for item in items.values_mut() {
                scrub_nested(io, item, remaining)?;
            }
        }
        _ => {}
    }
    io.scrub(value)
}
fn text<I: LocalIo>(io: &I, value: String) -> Result<String, OwnedScopedFailure> {
    let mut value = Value::String(value);
    scrub_nested(io, &mut value, 64)?;
    match value {
        Value::String(s) => Ok(s),
        _ => Err(uncertain()),
    }
}
pub(super) async fn run<I: LocalIo>(
    io: &mut I,
    bound: &RemoteRequestBound,
    notice: Option<String>,
) -> Result<OwnedScopedReply, OwnedScopedFailure> {
    io.checkpoint()?;
    let (kind, bytes) = io.post(bound.initialize(), true).await?;
    let decoded = decode(&kind, &bytes, 1);
    io.checkpoint()?;
    let init = decoded?;
    if init.get("result").is_none() {
        return Err(uncertain());
    }
    io.checkpoint()?;
    // Notification bodies are consumed/bounded by the same private sender.
    let _ = io.post(bound.notification(), false).await?;
    io.checkpoint()?;
    let (kind, bytes) = io.post(bound.final_frame(), false).await?;
    let decoded = decode(&kind, &bytes, 2);
    io.checkpoint()?;
    let frame = decoded?;
    let result = frame.get("result").ok_or_else(uncertain)?;
    let reply = if bound.discovery() {
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(uncertain)?;
        let mut defs = Vec::new();
        for tool in tools {
            let Some(name) = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            io.identifier(name)?;
            let description = text(
                io,
                tool.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            )?;
            let mut input_schema = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type":"object"}));
            scrub_nested(io, &mut input_schema, 64)?;
            defs.push(McpToolDef {
                name: name.into(),
                description,
                input_schema,
            });
        }
        OwnedScopedReply::Discover(defs)
    } else {
        let mut parts = Vec::new();
        if let Some(blocks) = result.get("content").and_then(Value::as_array) {
            for block in blocks {
                let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                if kind == "text" {
                    if let Some(value) = block.get("text").and_then(Value::as_str) {
                        parts.push(value.to_owned());
                    }
                } else if !kind.is_empty() {
                    parts.push(format!("[{kind} content omitted]"));
                }
            }
        }
        let content = text(io, parts.join(""))?;
        let notice = notice.map(|notice| text(io, notice)).transpose()?;
        OwnedScopedReply::Tool {
            arguments: bound.arguments().clone(),
            result: ToolCallResult {
                content,
                is_error: result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            notice,
        }
    };
    io.checkpoint()?;
    Ok(reply)
}
