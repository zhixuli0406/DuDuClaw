//! Anthropic Messages API provider.
//!
//! Absorbs the cache-placement behavior of the gateway's `direct_api.rs`:
//! - system blocks with `cache_control: ephemeral` on `CacheHint::Explicit`
//!   blocks, capped at 3 breakpoints (`MAX_SYSTEM_SEGMENTS`) so the 4th API
//!   breakpoint stays available for conversation history;
//! - "system_and_3" history strategy — a cache breakpoint on the
//!   3rd-to-last message once the conversation has ≥ 4 messages, so the
//!   last turns (which change every call) are re-sent while the stable
//!   prefix cache-hits.
//!
//! Streaming: real SSE (`content_block_delta` → Text/Reasoning/ToolCall
//! deltas, `message_delta` → stop reason + output usage).

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde_json::{json, Value};

use crate::error::{classify_http, classify_transport, snippet, LlmError};
use crate::http::{http_client, retry_after_of};
use crate::provider::{split_model_id, ApiAuth, ChatProvider};
use crate::sse::{drive_sse, sse_data, SseParser};
use crate::types::{
    CacheHint, ChatRequest, ChatResponse, ContentPart, NormalizedUsage, ReasoningHint, Role,
    StopReason, StreamEvent, ToolChoice,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Max system blocks that receive their own cache breakpoint (mirrors
/// `direct_api::MAX_SYSTEM_SEGMENTS`; the history breakpoint uses the 4th).
pub const MAX_SYSTEM_CACHE_BREAKPOINTS: usize = 3;

pub struct AnthropicProvider {
    auth: ApiAuth,
}

impl AnthropicProvider {
    pub fn new(auth: ApiAuth) -> Self {
        Self { auth }
    }

    fn messages_url(&self) -> String {
        let base = self.auth.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
        format!("{}/v1/messages", base.trim_end_matches('/'))
    }
}

// ---------------------------------------------------------------------------
// Request build (pure)
// ---------------------------------------------------------------------------

fn cache_control() -> Value {
    json!({"type": "ephemeral"})
}

fn part_to_block(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text(t) => json!({"type": "text", "text": t}),
        ContentPart::Image { media_type, data_base64 } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data_base64}
        }),
        ContentPart::ToolCall { id, name, args } => json!({
            "type": "tool_use", "id": id, "name": name, "input": args
        }),
        ContentPart::ToolResult { call_id, content, is_error } => json!({
            "type": "tool_result", "tool_use_id": call_id, "content": content, "is_error": is_error
        }),
        // Thinking replay: the signature must round-trip verbatim for
        // multi-turn tool use with extended thinking.
        ContentPart::Reasoning { text, signature } => json!({
            "type": "thinking", "thinking": text,
            "signature": signature.clone().unwrap_or_default()
        }),
    }
}

pub(crate) fn build_request_body(req: &ChatRequest, stream: bool) -> Value {
    let (_, bare_model) = split_model_id(&req.model);

    // System blocks — cache_control on Explicit blocks, capped at 3.
    let mut explicit_used = 0usize;
    let system: Vec<Value> = req
        .system
        .iter()
        .map(|b| {
            let mut block = json!({"type": "text", "text": b.text});
            if b.cache == CacheHint::Explicit && explicit_used < MAX_SYSTEM_CACHE_BREAKPOINTS {
                explicit_used += 1;
                block["cache_control"] = cache_control();
            }
            block
        })
        .collect();

    // Messages with the "system_and_3" history breakpoint: cache_control on
    // the last content block of the 3rd-to-last message when len ≥ 4.
    let breakpoint_idx = if req.messages.len() >= 4 {
        Some(req.messages.len() - 3)
    } else {
        None
    };
    let messages: Vec<Value> = req
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let mut blocks: Vec<Value> = m.parts.iter().map(part_to_block).collect();
            if breakpoint_idx == Some(i) {
                if let Some(last) = blocks.last_mut() {
                    last["cache_control"] = cache_control();
                }
            }
            json!({"role": role, "content": blocks})
        })
        .collect();

    let mut body = json!({
        "model": bare_model,
        "max_tokens": req.max_tokens,
        "system": system,
        "messages": messages,
    });

    if stream {
        body["stream"] = json!(true);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect(),
        );
        body["tool_choice"] = match &req.tool_choice {
            ToolChoice::Auto => json!({"type": "auto"}),
            ToolChoice::None => json!({"type": "none"}),
            ToolChoice::Required => json!({"type": "any"}),
            ToolChoice::Tool(name) => json!({"type": "tool", "name": name}),
        };
    }
    if req.reasoning != ReasoningHint::Off {
        if let Some(budget) = req.reasoning.budget_tokens() {
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        }
    }
    if let Some(schema) = &req.response_format {
        // Structured outputs (output_format, GA on the Messages API).
        body["output_format"] = json!({"type": "json_schema", "schema": schema});
    }
    body
}

// ---------------------------------------------------------------------------
// Response parse (pure)
// ---------------------------------------------------------------------------

fn parse_stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refusal,
        Some(other) => StopReason::Other(other.to_string()),
        None => StopReason::Other("missing".to_string()),
    }
}

fn parse_usage(usage: &Value) -> NormalizedUsage {
    let g = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    NormalizedUsage {
        input_tokens: g("input_tokens"),
        output_tokens: g("output_tokens"),
        cache_read_tokens: g("cache_read_input_tokens"),
        cache_write_tokens: g("cache_creation_input_tokens"),
        // Anthropic thinking tokens are included in output_tokens.
        reasoning_tokens: 0,
    }
}

pub(crate) fn parse_response(body: &Value) -> Result<ChatResponse, LlmError> {
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::Parse(snippet(&format!("missing content array: {body}"))))?;

    let mut parts = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
                parts.push(ContentPart::Text(text.to_string()));
            }
            Some("tool_use") => parts.push(ContentPart::ToolCall {
                id: block.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                args: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            Some("thinking") => parts.push(ContentPart::Reasoning {
                text: block.get("thinking").and_then(Value::as_str).unwrap_or_default().to_string(),
                signature: block.get("signature").and_then(Value::as_str).map(String::from),
            }),
            // redacted_thinking and unknown block types are skipped.
            _ => {}
        }
    }

    Ok(ChatResponse {
        parts,
        stop: parse_stop_reason(body.get("stop_reason").and_then(Value::as_str)),
        usage: body.get("usage").map(parse_usage).unwrap_or_default(),
        model_used: body.get("model").and_then(Value::as_str).unwrap_or_default().to_string(),
        provider: "anthropic".to_string(),
    })
}

// ---------------------------------------------------------------------------
// SSE streaming parser (pure state machine)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct AnthropicSse {
    parts: Vec<ContentPart>,
    /// content-block index → position in `parts` (tool args accumulate).
    tool_args_buf: std::collections::HashMap<usize, (usize, String)>,
    usage: NormalizedUsage,
    stop: Option<StopReason>,
    model_used: String,
    finished: bool,
    error: Option<LlmError>,
}

impl AnthropicSse {
    fn handle_event(&mut self, ev: &Value, out: &mut Vec<StreamEvent>) {
        match ev.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(msg) = ev.get("message") {
                    if let Some(u) = msg.get("usage") {
                        self.usage = parse_usage(u);
                    }
                    if let Some(m) = msg.get("model").and_then(Value::as_str) {
                        self.model_used = m.to_string();
                    }
                }
            }
            Some("content_block_start") => {
                let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = ev.get("content_block").cloned().unwrap_or(Value::Null);
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                        let name = block.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                        self.parts.push(ContentPart::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            args: Value::Null,
                        });
                        self.tool_args_buf.insert(index, (self.parts.len() - 1, String::new()));
                        out.push(StreamEvent::ToolCallStart { index, id, name });
                    }
                    Some("text") => self.parts.push(ContentPart::Text(String::new())),
                    Some("thinking") => {
                        self.parts.push(ContentPart::Reasoning { text: String::new(), signature: None })
                    }
                    _ => {}
                }
            }
            Some("content_block_delta") => {
                let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = ev.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let t = delta.get("text").and_then(Value::as_str).unwrap_or_default();
                        if let Some(ContentPart::Text(buf)) = self.parts.last_mut() {
                            buf.push_str(t);
                        }
                        out.push(StreamEvent::TextDelta(t.to_string()));
                    }
                    Some("thinking_delta") => {
                        let t = delta.get("thinking").and_then(Value::as_str).unwrap_or_default();
                        if let Some(ContentPart::Reasoning { text, .. }) = self.parts.last_mut() {
                            text.push_str(t);
                        }
                        out.push(StreamEvent::ReasoningDelta(t.to_string()));
                    }
                    Some("input_json_delta") => {
                        let frag = delta.get("partial_json").and_then(Value::as_str).unwrap_or_default();
                        if let Some((_, buf)) = self.tool_args_buf.get_mut(&index) {
                            buf.push_str(frag);
                        }
                        out.push(StreamEvent::ToolCallDelta { index, args_fragment: frag.to_string() });
                    }
                    Some("signature_delta") => {
                        let sig = delta.get("signature").and_then(Value::as_str).unwrap_or_default();
                        if let Some(ContentPart::Reasoning { signature, .. }) = self.parts.last_mut() {
                            match signature {
                                Some(s) => s.push_str(sig),
                                None => *signature = Some(sig.to_string()),
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some((part_idx, buf)) = self.tool_args_buf.remove(&index) {
                    if let Some(ContentPart::ToolCall { args, .. }) = self.parts.get_mut(part_idx) {
                        *args = serde_json::from_str(&buf)
                            .unwrap_or_else(|_| if buf.is_empty() { json!({}) } else { Value::String(buf) });
                    }
                }
            }
            Some("message_delta") => {
                if let Some(sr) = ev.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(parse_stop_reason(Some(sr)));
                }
                if let Some(u) = ev.get("usage") {
                    let out_tokens = u.get("output_tokens").and_then(Value::as_u64);
                    if let Some(t) = out_tokens {
                        self.usage.output_tokens = t;
                    }
                }
            }
            Some("message_stop") => self.finished = true,
            Some("error") => {
                let msg = ev.pointer("/error/message").and_then(Value::as_str).unwrap_or("stream error");
                self.error = Some(LlmError::Http { status: 0, body_snippet: snippet(msg) });
                self.finished = true;
            }
            _ => {}
        }
    }
}

impl SseParser for AnthropicSse {
    fn on_line(&mut self, line: &str, out: &mut Vec<StreamEvent>) {
        let Some(data) = sse_data(line) else { return };
        if data.is_empty() {
            return;
        }
        if let Ok(ev) = serde_json::from_str::<Value>(data) {
            self.handle_event(&ev, out);
        }
    }

    fn finished(&self) -> bool {
        self.finished
    }

    fn finalize(&mut self) -> Result<StreamEvent, LlmError> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        Ok(StreamEvent::Done(ChatResponse {
            parts: std::mem::take(&mut self.parts),
            stop: self.stop.take().unwrap_or(StopReason::EndTurn),
            usage: self.usage,
            model_used: std::mem::take(&mut self.model_used),
            provider: "anthropic".to_string(),
        }))
    }
}

// ---------------------------------------------------------------------------
// HTTP execution
// ---------------------------------------------------------------------------

impl AnthropicProvider {
    async fn send(&self, req: &ChatRequest, stream: bool) -> Result<reqwest::Response, LlmError> {
        let body = build_request_body(req, stream);
        let response = http_client()
            .post(self.messages_url())
            .header("x-api-key", &self.auth.api_key)
            .header("anthropic-version", API_VERSION)
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
        Ok(response)
    }
}

#[async_trait]
impl ChatProvider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let response = self.send(req, false).await?;
        let text = response.text().await.map_err(|e| classify_transport(&e))?;
        let body: Value =
            serde_json::from_str(&text).map_err(|e| LlmError::Parse(snippet(&e.to_string())))?;
        parse_response(&body)
    }

    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let response = self.send(req, true).await?;
        Ok(drive_sse(response, AnthropicSse::default()))
    }
}

// ---------------------------------------------------------------------------
// Tests (offline — pure request build / response parse / SSE state machine)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, SystemBlock, ToolDef};

    fn req_with_history(n: usize) -> ChatRequest {
        let mut req = ChatRequest::new("anthropic/claude-sonnet-5");
        for i in 0..n {
            if i % 2 == 0 {
                req.messages.push(ChatMessage::user(format!("q{i}")));
            } else {
                req.messages.push(ChatMessage::assistant(format!("a{i}")));
            }
        }
        req
    }

    #[test]
    fn build_strips_provider_prefix_and_sets_max_tokens() {
        let body = build_request_body(&ChatRequest::new("anthropic/claude-sonnet-5"), false);
        assert_eq!(body["model"], "claude-sonnet-5");
        assert_eq!(body["max_tokens"], 4096);
        assert!(body.get("stream").is_none());
        let body = build_request_body(&ChatRequest::new("claude-haiku-4-5"), true);
        assert_eq!(body["model"], "claude-haiku-4-5");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn build_system_cache_control_only_on_explicit_capped_at_3() {
        let mut req = ChatRequest::new("anthropic/claude-sonnet-5");
        req.system = vec![
            SystemBlock::cached("soul"),
            SystemBlock { text: "auto".into(), cache: CacheHint::Auto },
            SystemBlock::cached("wiki"),
            SystemBlock::cached("skills"),
            SystemBlock::cached("extra-beyond-budget"),
            SystemBlock::uncached("task queue"),
        ];
        let body = build_request_body(&req, false);
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 6);
        let has_cc: Vec<bool> = system.iter().map(|b| b.get("cache_control").is_some()).collect();
        // Explicit #1, #3, #4 get breakpoints; Auto ignored; 4th Explicit
        // exceeds MAX_SYSTEM_CACHE_BREAKPOINTS; uncached suffix stays uncached.
        assert_eq!(has_cc, vec![true, false, true, true, false, false]);
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn build_history_breakpoint_on_third_to_last_when_len_ge_4() {
        let body = build_request_body(&req_with_history(6), false);
        let messages = body["messages"].as_array().unwrap();
        let with_cc: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m["content"].as_array().unwrap().iter().any(|b| b.get("cache_control").is_some())
            })
            .map(|(i, _)| i)
            .collect();
        // 6 messages → 3rd-to-last is index 3.
        assert_eq!(with_cc, vec![3]);

        // Short history: no breakpoint.
        let body = build_request_body(&req_with_history(3), false);
        for m in body["messages"].as_array().unwrap() {
            for b in m["content"].as_array().unwrap() {
                assert!(b.get("cache_control").is_none());
            }
        }
    }

    #[test]
    fn build_tools_tool_choice_and_image() {
        let mut req = ChatRequest::new("anthropic/claude-sonnet-5");
        req.tools.push(ToolDef {
            name: "search".into(),
            description: "web search".into(),
            input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        });
        req.tool_choice = ToolChoice::Tool("search".into());
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![
                ContentPart::Text("what is this?".into()),
                ContentPart::Image { media_type: "image/png".into(), data_base64: "aGk=".into() },
            ],
        });
        let body = build_request_body(&req, false);
        assert_eq!(body["tools"][0]["name"], "search");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["tool_choice"], json!({"type": "tool", "name": "search"}));
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "aGk=");
    }

    #[test]
    fn build_tool_choice_variants() {
        let mut req = ChatRequest::new("anthropic/m");
        req.tools.push(ToolDef { name: "t".into(), description: String::new(), input_schema: json!({}) });
        req.tool_choice = ToolChoice::Required;
        assert_eq!(build_request_body(&req, false)["tool_choice"], json!({"type": "any"}));
        req.tool_choice = ToolChoice::None;
        assert_eq!(build_request_body(&req, false)["tool_choice"], json!({"type": "none"}));
        req.tool_choice = ToolChoice::Auto;
        assert_eq!(build_request_body(&req, false)["tool_choice"], json!({"type": "auto"}));
    }

    #[test]
    fn build_thinking_replay_with_signature_and_tool_cycle() {
        let mut req = ChatRequest::new("anthropic/claude-sonnet-5");
        req.messages.push(ChatMessage::user("do the thing"));
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            parts: vec![
                ContentPart::Reasoning { text: "let me think".into(), signature: Some("sig123".into()) },
                ContentPart::ToolCall { id: "tu_1".into(), name: "run".into(), args: json!({"x": 1}) },
            ],
        });
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![ContentPart::ToolResult { call_id: "tu_1".into(), content: "done".into(), is_error: false }],
        });
        let body = build_request_body(&req, false);
        let asst = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(asst[0]["type"], "thinking");
        assert_eq!(asst[0]["thinking"], "let me think");
        assert_eq!(asst[0]["signature"], "sig123");
        assert_eq!(asst[1]["type"], "tool_use");
        assert_eq!(asst[1]["input"], json!({"x": 1}));
        let result = &body["messages"][2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "tu_1");
        assert_eq!(result["is_error"], false);
    }

    #[test]
    fn build_reasoning_hint_and_response_format() {
        let mut req = ChatRequest::new("anthropic/claude-sonnet-5");
        req.reasoning = ReasoningHint::Medium;
        req.response_format = Some(json!({"type": "object"}));
        let body = build_request_body(&req, false);
        assert_eq!(body["thinking"], json!({"type": "enabled", "budget_tokens": 8192}));
        assert_eq!(body["output_format"]["type"], "json_schema");
        // Off → field absent.
        let body = build_request_body(&ChatRequest::new("anthropic/m"), false);
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn parse_text_tool_use_and_usage() {
        let body = json!({
            "model": "claude-sonnet-5",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "tu_1", "name": "search", "input": {"q": "rust"}},
                {"type": "thinking", "thinking": "hmm", "signature": "s1"}
            ],
            "usage": {
                "input_tokens": 10, "output_tokens": 20,
                "cache_read_input_tokens": 3000, "cache_creation_input_tokens": 40
            }
        });
        let resp = parse_response(&body).expect("parse");
        assert_eq!(resp.stop, StopReason::ToolUse);
        assert_eq!(resp.text(), "checking");
        assert_eq!(resp.tool_calls()[0].2, &json!({"q": "rust"}));
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.cache_read_tokens, 3000);
        assert_eq!(resp.usage.cache_write_tokens, 40);
        assert!(resp.parts.iter().any(|p| matches!(p,
            ContentPart::Reasoning { text, signature } if text == "hmm" && signature.as_deref() == Some("s1"))));
        assert_eq!(resp.model_used, "claude-sonnet-5");
        assert_eq!(resp.provider, "anthropic");
    }

    #[test]
    fn parse_stop_reasons() {
        for (raw, expected) in [
            ("end_turn", StopReason::EndTurn),
            ("max_tokens", StopReason::MaxTokens),
            ("refusal", StopReason::Refusal),
            ("stop_sequence", StopReason::Other("stop_sequence".into())),
        ] {
            let body = json!({"content": [], "stop_reason": raw});
            assert_eq!(parse_response(&body).unwrap().stop, expected);
        }
    }

    #[test]
    fn parse_missing_content_is_parse_error() {
        assert!(matches!(parse_response(&json!({"id": "x"})), Err(LlmError::Parse(_))));
    }

    #[test]
    fn sse_full_stream_accumulates_response() {
        let mut p = AnthropicSse::default();
        let mut out = Vec::new();
        let lines = [
            r#"data: {"type":"message_start","message":{"model":"claude-sonnet-5","usage":{"input_tokens":25,"output_tokens":1,"cache_read_input_tokens":100}}}"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_9","name":"calc"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"1}"}}"#,
            r#"data: {"type":"content_block_stop","index":1}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
            r#"data: {"type":"message_stop"}"#,
        ];
        for line in lines {
            p.on_line(line, &mut out);
        }
        assert!(p.finished());
        // Delta events observed in order.
        assert_eq!(out[0], StreamEvent::TextDelta("Hel".into()));
        assert_eq!(out[1], StreamEvent::TextDelta("lo".into()));
        assert!(matches!(&out[2], StreamEvent::ToolCallStart { id, name, .. } if id == "tu_9" && name == "calc"));
        assert!(matches!(&out[3], StreamEvent::ToolCallDelta { args_fragment, .. } if args_fragment == "{\"a\":"));

        let done = p.finalize().expect("done");
        let StreamEvent::Done(resp) = done else { panic!("expected Done") };
        assert_eq!(resp.text(), "Hello");
        assert_eq!(resp.stop, StopReason::ToolUse);
        assert_eq!(resp.tool_calls()[0].2, &json!({"a": 1}));
        assert_eq!(resp.usage.input_tokens, 25);
        assert_eq!(resp.usage.output_tokens, 42);
        assert_eq!(resp.usage.cache_read_tokens, 100);
        assert_eq!(resp.model_used, "claude-sonnet-5");
    }

    #[test]
    fn sse_thinking_deltas_and_signature() {
        let mut p = AnthropicSse::default();
        let mut out = Vec::new();
        for line in [
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"step 1"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sigX"}}"#,
            r#"data: {"type":"message_stop"}"#,
        ] {
            p.on_line(line, &mut out);
        }
        assert_eq!(out[0], StreamEvent::ReasoningDelta("step 1".into()));
        let StreamEvent::Done(resp) = p.finalize().unwrap() else { panic!() };
        assert!(matches!(&resp.parts[0],
            ContentPart::Reasoning { text, signature } if text == "step 1" && signature.as_deref() == Some("sigX")));
    }

    #[test]
    fn sse_error_event_surfaces_as_error() {
        let mut p = AnthropicSse::default();
        let mut out = Vec::new();
        p.on_line(r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#, &mut out);
        assert!(p.finished());
        assert!(p.finalize().is_err());
    }
}
