//! OpenAI **Responses API** provider (`POST /v1/responses`).
//!
//! Deliberately NOT Chat Completions — that endpoint is being sunset;
//! Responses is OpenAI's forward path (function calls and outputs are
//! top-level `input` items, system prompt goes in `instructions`).
//!
//! Streaming: **buffered in v1** — `complete()` then a single `Done` event
//! (Responses SSE is semantic-event based and heavier to parse; real
//! streaming is a later wave). Prompt caching is implicit on OpenAI's side;
//! `CacheHint` is ignored.

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde_json::{json, Value};

use crate::error::{classify_http, classify_transport, snippet, LlmError};
use crate::http::{http_client, retry_after_of};
use crate::provider::{buffered_stream, split_model_id, ApiAuth, ChatProvider};
use crate::types::{
    ChatRequest, ChatResponse, ContentPart, NormalizedUsage, ReasoningHint, Role, StopReason,
    StreamEvent, ToolChoice,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider {
    auth: ApiAuth,
}

impl OpenAiProvider {
    pub fn new(auth: ApiAuth) -> Self {
        Self { auth }
    }

    fn responses_url(&self) -> String {
        let base = self.auth.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
        format!("{}/responses", base.trim_end_matches('/'))
    }
}

// ---------------------------------------------------------------------------
// Request build (pure)
// ---------------------------------------------------------------------------

pub(crate) fn build_request_body(req: &ChatRequest) -> Value {
    let (_, bare_model) = split_model_id(&req.model);

    // System blocks → `instructions` (implicit caching; CacheHint ignored).
    let instructions = req
        .system
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    // Messages → input items. Text/image parts stay inside a message item;
    // tool calls / tool results become standalone function items.
    let mut input: Vec<Value> = Vec::new();
    for msg in &req.messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let content_type = match msg.role {
            Role::User => "input_text",
            Role::Assistant => "output_text",
        };
        let mut content: Vec<Value> = Vec::new();
        let flush = |content: &mut Vec<Value>, input: &mut Vec<Value>| {
            if !content.is_empty() {
                input.push(json!({
                    "type": "message", "role": role,
                    "content": Value::Array(std::mem::take(content)),
                }));
            }
        };
        for part in &msg.parts {
            match part {
                ContentPart::Text(t) => content.push(json!({"type": content_type, "text": t})),
                ContentPart::Image { media_type, data_base64 } => content.push(json!({
                    "type": "input_image",
                    "image_url": format!("data:{media_type};base64,{data_base64}"),
                })),
                ContentPart::ToolCall { id, name, args } => {
                    flush(&mut content, &mut input);
                    input.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        // Responses transports arguments as a JSON string.
                        "arguments": args.to_string(),
                    }));
                }
                ContentPart::ToolResult { call_id, content: result, .. } => {
                    flush(&mut content, &mut input);
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": result,
                    }));
                }
                // OpenAI reasoning replay requires encrypted reasoning items
                // we don't retain in v1 — skipped (server regenerates).
                ContentPart::Reasoning { .. } => {}
            }
        }
        flush(&mut content, &mut input);
    }

    let mut body = json!({
        "model": bare_model,
        "input": input,
        "max_output_tokens": req.max_tokens,
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if !req.tools.is_empty() {
        // Responses API uses a FLAT function tool shape (no nested `function`).
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    })
                })
                .collect(),
        );
        body["tool_choice"] = match &req.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Tool(name) => json!({"type": "function", "name": name}),
        };
    }
    if req.reasoning != ReasoningHint::Off {
        if let Some(effort) = req.reasoning.effort() {
            body["reasoning"] = json!({"effort": effort});
        }
    }
    if let Some(schema) = &req.response_format {
        body["text"] = json!({
            "format": {"type": "json_schema", "name": "response", "schema": schema}
        });
    }
    body
}

// ---------------------------------------------------------------------------
// Response parse (pure)
// ---------------------------------------------------------------------------

pub(crate) fn parse_response(body: &Value) -> Result<ChatResponse, LlmError> {
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::Parse(snippet(&format!("missing output array: {body}"))))?;

    let mut parts = Vec::new();
    let mut refused = false;
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for c in item.get("content").and_then(Value::as_array).unwrap_or(&Vec::new()) {
                    match c.get("type").and_then(Value::as_str) {
                        Some("output_text") => parts.push(ContentPart::Text(
                            c.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                        )),
                        Some("refusal") => refused = true,
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                let raw_args = item.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                parts.push(ContentPart::ToolCall {
                    id: item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: item.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    // STRING arguments → always parsed internally.
                    args: serde_json::from_str(raw_args)
                        .unwrap_or_else(|_| Value::String(raw_args.to_string())),
                });
            }
            Some("reasoning") => {
                let summary: String = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .map(|s| {
                        s.iter()
                            .filter_map(|e| e.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if !summary.is_empty() {
                    parts.push(ContentPart::Reasoning { text: summary, signature: None });
                }
            }
            _ => {}
        }
    }

    let stop = if refused {
        StopReason::Refusal
    } else if body.get("status").and_then(Value::as_str) == Some("incomplete") {
        match body.pointer("/incomplete_details/reason").and_then(Value::as_str) {
            Some("max_output_tokens") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::ContentFilter,
            Some(other) => StopReason::Other(other.to_string()),
            None => StopReason::Other("incomplete".to_string()),
        }
    } else if parts.iter().any(|p| matches!(p, ContentPart::ToolCall { .. })) {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    };

    let usage = body.get("usage").map(parse_usage).unwrap_or_default();

    Ok(ChatResponse {
        parts,
        stop,
        usage,
        model_used: body.get("model").and_then(Value::as_str).unwrap_or_default().to_string(),
        provider: "openai".to_string(),
    })
}

fn parse_usage(usage: &Value) -> NormalizedUsage {
    let g = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    NormalizedUsage {
        input_tokens: g("input_tokens"),
        output_tokens: g("output_tokens"),
        cache_read_tokens: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: 0, // OpenAI does not bill cache writes separately.
        // Reported for observability; already included in output_tokens, so
        // keep 0 here — cost math must not double-bill (see NormalizedUsage
        // docs: output + reasoning must equal total billable output).
        reasoning_tokens: 0,
    }
}

/// Reasoning tokens reported by the Responses API (observability only —
/// they are already counted inside `output_tokens`, so they are NOT added
/// to [`NormalizedUsage::reasoning_tokens`]; telemetry callers can read
/// them from the raw usage object with this helper).
pub fn reasoning_tokens_of(usage: &Value) -> u64 {
    usage
        .pointer("/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// HTTP execution
// ---------------------------------------------------------------------------

#[async_trait]
impl ChatProvider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let body = build_request_body(req);
        let response = http_client()
            .post(self.responses_url())
            .bearer_auth(&self.auth.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_transport(&e))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after_of(response.headers());
            let text = response.text().await.unwrap_or_default();
            return Err(classify_http(status.as_u16(), &text, retry_after));
        }
        let text = response.text().await.map_err(|e| classify_transport(&e))?;
        let parsed: Value =
            serde_json::from_str(&text).map_err(|e| LlmError::Parse(snippet(&e.to_string())))?;
        parse_response(&parsed)
    }

    /// v1: buffered — complete() then a single `Done` event.
    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let resp = self.complete(req).await?;
        Ok(buffered_stream(resp))
    }
}

// ---------------------------------------------------------------------------
// Tests (offline)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, SystemBlock, ToolDef};

    #[test]
    fn build_uses_instructions_and_input_items() {
        let mut req = ChatRequest::new("openai/gpt-5.4");
        req.system = vec![SystemBlock::cached("be helpful"), SystemBlock::uncached("queue")];
        req.messages.push(ChatMessage::user("hi"));
        req.messages.push(ChatMessage::assistant("hello"));
        let body = build_request_body(&req);
        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["instructions"], "be helpful\n\nqueue");
        assert_eq!(body["max_output_tokens"], 4096);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["content"][0]["type"], "output_text");
    }

    #[test]
    fn build_function_call_cycle_items() {
        let mut req = ChatRequest::new("openai/gpt-5.4");
        req.messages.push(ChatMessage::user("calc 1+1"));
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            parts: vec![ContentPart::ToolCall {
                id: "call_1".into(),
                name: "calc".into(),
                args: json!({"expr": "1+1"}),
            }],
        });
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![ContentPart::ToolResult {
                call_id: "call_1".into(),
                content: "2".into(),
                is_error: false,
            }],
        });
        let body = build_request_body(&req);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_1");
        // Arguments serialized back to a string for the wire.
        assert_eq!(input[1]["arguments"], r#"{"expr":"1+1"}"#);
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["output"], "2");
    }

    #[test]
    fn build_flat_tools_tool_choice_reasoning_and_format() {
        let mut req = ChatRequest::new("openai/gpt-5.5");
        req.tools.push(ToolDef {
            name: "search".into(),
            description: "web".into(),
            input_schema: json!({"type": "object"}),
        });
        req.tool_choice = ToolChoice::Required;
        req.reasoning = ReasoningHint::High;
        req.response_format = Some(json!({"type": "object"}));
        let body = build_request_body(&req);
        // Flat shape: name at top level, no nested "function".
        assert_eq!(body["tools"][0]["name"], "search");
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["text"]["format"]["type"], "json_schema");

        req.tool_choice = ToolChoice::Tool("search".into());
        let body = build_request_body(&req);
        assert_eq!(body["tool_choice"], json!({"type": "function", "name": "search"}));
    }

    #[test]
    fn build_image_becomes_data_url() {
        let mut req = ChatRequest::new("openai/gpt-5.4");
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![ContentPart::Image { media_type: "image/png".into(), data_base64: "aGk=".into() }],
        });
        let body = build_request_body(&req);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_image");
        assert_eq!(body["input"][0]["content"][0]["image_url"], "data:image/png;base64,aGk=");
    }

    #[test]
    fn parse_text_response_and_usage_mapping() {
        let body = json!({
            "model": "gpt-5.4",
            "status": "completed",
            "output": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "thought"}]},
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "Answer."}
                ]}
            ],
            "usage": {
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 80},
                "output_tokens": 50,
                "output_tokens_details": {"reasoning_tokens": 30}
            }
        });
        let resp = parse_response(&body).expect("parse");
        assert_eq!(resp.text(), "Answer.");
        assert_eq!(resp.stop, StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.cache_read_tokens, 80);
        assert_eq!(resp.usage.output_tokens, 50);
        // Reasoning tokens are inside output_tokens → not double-counted.
        assert_eq!(resp.usage.reasoning_tokens, 0);
        assert_eq!(reasoning_tokens_of(&body["usage"]), 30);
        assert!(matches!(&resp.parts[0], ContentPart::Reasoning { text, .. } if text == "thought"));
    }

    #[test]
    fn parse_function_call_with_string_args() {
        let body = json!({
            "model": "gpt-5.4",
            "output": [
                {"type": "function_call", "call_id": "call_9", "name": "calc",
                 "arguments": "{\"expr\":\"2*3\"}"}
            ]
        });
        let resp = parse_response(&body).expect("parse");
        assert_eq!(resp.stop, StopReason::ToolUse);
        let calls = resp.tool_calls();
        assert_eq!(calls[0].0, "call_9");
        assert_eq!(calls[0].2, &json!({"expr": "2*3"}));
    }

    #[test]
    fn parse_malformed_string_args_fall_back_to_raw_string() {
        let body = json!({
            "output": [
                {"type": "function_call", "call_id": "c", "name": "f", "arguments": "not json"}
            ]
        });
        let resp = parse_response(&body).expect("parse");
        assert_eq!(resp.tool_calls()[0].2, &Value::String("not json".into()));
    }

    #[test]
    fn parse_incomplete_and_refusal_stop_reasons() {
        let body = json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": []
        });
        assert_eq!(parse_response(&body).unwrap().stop, StopReason::MaxTokens);

        let body = json!({
            "status": "completed",
            "output": [{"type": "message", "role": "assistant",
                        "content": [{"type": "refusal", "refusal": "no"}]}]
        });
        assert_eq!(parse_response(&body).unwrap().stop, StopReason::Refusal);
    }

    #[test]
    fn parse_missing_output_is_parse_error() {
        assert!(matches!(parse_response(&json!({"id": "resp_1"})), Err(LlmError::Parse(_))));
    }
}
