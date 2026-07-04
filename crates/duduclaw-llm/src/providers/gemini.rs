//! Google Gemini native `generateContent` provider.
//!
//! Key invariant: **`thoughtSignature` must be echoed back verbatim** —
//! Gemini rejects or degrades multi-turn function calling when the thought
//! signature from a previous response is dropped. Signatures are stored in
//! `ContentPart::Reasoning { signature }` (a signature attached to a
//! `functionCall` part is surfaced as a `Reasoning { text: "" }` part
//! immediately before the `ToolCall`) and re-attached on request build.
//!
//! Streaming: **buffered in v1** — `complete()` then a single `Done` event
//! (Gemini's `streamGenerateContent` uses chunked JSON arrays, not
//! line-based SSE; real streaming is a later wave).

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

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GeminiProvider {
    auth: ApiAuth,
}

impl GeminiProvider {
    pub fn new(auth: ApiAuth) -> Self {
        Self { auth }
    }

    fn generate_url(&self, model: &str) -> String {
        let base = self.auth.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
        format!("{}/models/{model}:generateContent", base.trim_end_matches('/'))
    }
}

// ---------------------------------------------------------------------------
// Request build (pure)
// ---------------------------------------------------------------------------

pub(crate) fn build_request_body(req: &ChatRequest) -> Value {
    // System blocks → systemInstruction (implicit caching; CacheHint ignored).
    let system_text = req
        .system
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let contents: Vec<Value> = req
        .messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "model",
            };
            let mut parts: Vec<Value> = Vec::new();
            // A Reasoning part's signature attaches to the FOLLOWING
            // functionCall part when its text is empty (round-trip of how
            // Gemini emits signatures on functionCall parts).
            let mut pending_signature: Option<String> = None;
            for part in &msg.parts {
                match part {
                    ContentPart::Text(t) => parts.push(json!({"text": t})),
                    ContentPart::Image { media_type, data_base64 } => parts.push(json!({
                        "inlineData": {"mimeType": media_type, "data": data_base64}
                    })),
                    ContentPart::ToolCall { name, args, .. } => {
                        let mut p = json!({"functionCall": {"name": name, "args": args}});
                        if let Some(sig) = pending_signature.take() {
                            p["thoughtSignature"] = json!(sig);
                        }
                        parts.push(p);
                    }
                    ContentPart::ToolResult { call_id, content, is_error } => {
                        // Gemini has no call ids — `call_id` carries the
                        // function NAME (set by our own parse_response).
                        let response = if *is_error {
                            json!({"error": content})
                        } else {
                            json!({"content": content})
                        };
                        parts.push(json!({
                            "functionResponse": {"name": call_id, "response": response}
                        }));
                    }
                    ContentPart::Reasoning { text, signature } => {
                        if text.is_empty() {
                            // Signature carrier for the next functionCall.
                            pending_signature = signature.clone();
                        } else {
                            let mut p = json!({"text": text, "thought": true});
                            if let Some(sig) = signature {
                                p["thoughtSignature"] = json!(sig);
                            }
                            parts.push(p);
                        }
                    }
                }
            }
            // Orphaned signature (no following functionCall): attach to a
            // thought part so it is still echoed verbatim, never dropped.
            if let Some(sig) = pending_signature.take() {
                parts.push(json!({"text": "", "thought": true, "thoughtSignature": sig}));
            }
            json!({"role": role, "parts": parts})
        })
        .collect();

    let mut generation_config = json!({"maxOutputTokens": req.max_tokens});
    if let Some(t) = req.temperature {
        generation_config["temperature"] = json!(t);
    }
    if req.reasoning != ReasoningHint::Off {
        if let Some(budget) = req.reasoning.budget_tokens() {
            generation_config["thinkingConfig"] = json!({"thinkingBudget": budget});
        }
    }
    if let Some(schema) = &req.response_format {
        generation_config["responseMimeType"] = json!("application/json");
        generation_config["responseSchema"] = schema.clone();
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": generation_config,
    });
    if !system_text.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system_text}]});
    }
    if !req.tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": req.tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            })).collect::<Vec<_>>()
        }]);
        let config = match &req.tool_choice {
            ToolChoice::Auto => json!({"mode": "AUTO"}),
            ToolChoice::None => json!({"mode": "NONE"}),
            ToolChoice::Required => json!({"mode": "ANY"}),
            ToolChoice::Tool(name) => json!({"mode": "ANY", "allowedFunctionNames": [name]}),
        };
        body["toolConfig"] = json!({"functionCallingConfig": config});
    }
    body
}

// ---------------------------------------------------------------------------
// Response parse (pure)
// ---------------------------------------------------------------------------

pub(crate) fn parse_response(body: &Value) -> Result<ChatResponse, LlmError> {
    // Prompt-level block (no candidates at all).
    if let Some(reason) = body.pointer("/promptFeedback/blockReason").and_then(Value::as_str) {
        if body.get("candidates").and_then(Value::as_array).map_or(true, |c| c.is_empty()) {
            return Err(LlmError::ContentFilter).map_err(|e| {
                tracing::debug!(block_reason = reason, "gemini prompt blocked");
                e
            });
        }
    }

    let candidate = body
        .pointer("/candidates/0")
        .ok_or_else(|| LlmError::Parse(snippet(&format!("missing candidates: {body}"))))?;

    let mut parts = Vec::new();
    let mut has_tool_call = false;
    for part in candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let signature = part.get("thoughtSignature").and_then(Value::as_str).map(String::from);
        if let Some(fc) = part.get("functionCall") {
            has_tool_call = true;
            let name = fc.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            // Preserve a functionCall-attached signature as an empty
            // Reasoning carrier directly before the ToolCall (round-trips
            // through build_request_body — see module docs).
            if let Some(sig) = signature {
                parts.push(ContentPart::Reasoning { text: String::new(), signature: Some(sig) });
            }
            parts.push(ContentPart::ToolCall {
                // Gemini has no call ids; use the function name.
                id: name.clone(),
                name,
                args: fc.get("args").cloned().unwrap_or(json!({})),
            });
        } else if let Some(text) = part.get("text").and_then(Value::as_str) {
            if part.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                parts.push(ContentPart::Reasoning { text: text.to_string(), signature });
            } else {
                parts.push(ContentPart::Text(text.to_string()));
            }
        }
    }

    let stop = match candidate.get("finishReason").and_then(Value::as_str) {
        None | Some("STOP") => {
            if has_tool_call {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        }
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        Some("SAFETY") | Some("PROHIBITED_CONTENT") | Some("BLOCKLIST") | Some("SPII") => {
            StopReason::ContentFilter
        }
        Some(other) => StopReason::Other(other.to_string()),
    };

    let usage = body.get("usageMetadata").map(parse_usage).unwrap_or_default();

    Ok(ChatResponse {
        parts,
        stop,
        usage,
        model_used: body
            .get("modelVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        provider: "gemini".to_string(),
    })
}

fn parse_usage(u: &Value) -> NormalizedUsage {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    NormalizedUsage {
        input_tokens: g("promptTokenCount").saturating_sub(g("cachedContentTokenCount")),
        output_tokens: g("candidatesTokenCount"),
        cache_read_tokens: g("cachedContentTokenCount"),
        cache_write_tokens: 0,
        // Gemini reports thinking tokens OUTSIDE candidatesTokenCount →
        // billed as output on top (see NormalizedUsage docs).
        reasoning_tokens: g("thoughtsTokenCount"),
    }
}

// ---------------------------------------------------------------------------
// HTTP execution
// ---------------------------------------------------------------------------

#[async_trait]
impl ChatProvider for GeminiProvider {
    fn id(&self) -> &str {
        "gemini"
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let (_, bare_model) = split_model_id(&req.model);
        let body = build_request_body(req);
        let response = http_client()
            .post(self.generate_url(bare_model))
            .header("x-goog-api-key", &self.auth.api_key)
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
    fn build_system_instruction_and_roles() {
        let mut req = ChatRequest::new("gemini/gemini-3.1-pro");
        req.system = vec![SystemBlock::cached("rules"), SystemBlock::uncached("queue")];
        req.messages.push(ChatMessage::user("hi"));
        req.messages.push(ChatMessage::assistant("hello"));
        let body = build_request_body(&req);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "rules\n\nqueue");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 4096);
    }

    #[test]
    fn build_tools_and_function_calling_modes() {
        let mut req = ChatRequest::new("gemini/gemini-3.1-pro");
        req.tools.push(ToolDef {
            name: "lookup".into(),
            description: "kb lookup".into(),
            input_schema: json!({"type": "object"}),
        });
        for (choice, expected_mode) in [
            (ToolChoice::Auto, json!({"mode": "AUTO"})),
            (ToolChoice::None, json!({"mode": "NONE"})),
            (ToolChoice::Required, json!({"mode": "ANY"})),
            (
                ToolChoice::Tool("lookup".into()),
                json!({"mode": "ANY", "allowedFunctionNames": ["lookup"]}),
            ),
        ] {
            req.tool_choice = choice;
            let body = build_request_body(&req);
            assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "lookup");
            assert_eq!(body["toolConfig"]["functionCallingConfig"], expected_mode);
        }
    }

    #[test]
    fn build_thought_signature_echoed_verbatim_on_function_call() {
        // Round-trip: parse a response with a signature-bearing functionCall,
        // feed the parts back as an assistant message, and verify the
        // signature reappears verbatim on the functionCall part.
        let api_response = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "search", "args": {"q": "x"}},
                     "thoughtSignature": "OPAQUE_SIG_TOKEN=="}
                ]},
                "finishReason": "STOP"
            }]
        });
        let parsed = parse_response(&api_response).expect("parse");

        let mut req = ChatRequest::new("gemini/gemini-3.1-pro");
        req.messages.push(ChatMessage::user("find x"));
        req.messages.push(ChatMessage { role: Role::Assistant, parts: parsed.parts });
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![ContentPart::ToolResult {
                call_id: "search".into(),
                content: "found it".into(),
                is_error: false,
            }],
        });
        let body = build_request_body(&req);
        let model_parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(model_parts.len(), 1, "empty Reasoning carrier merges into functionCall part");
        assert_eq!(model_parts[0]["functionCall"]["name"], "search");
        assert_eq!(model_parts[0]["thoughtSignature"], "OPAQUE_SIG_TOKEN==");
        // functionResponse uses the function name (call_id carrier).
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "search"
        );
    }

    #[test]
    fn build_thought_text_part_keeps_signature() {
        let mut req = ChatRequest::new("gemini/gemini-3.1-pro");
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            parts: vec![ContentPart::Reasoning {
                text: "deliberation".into(),
                signature: Some("sigT".into()),
            }],
        });
        let body = build_request_body(&req);
        let p = &body["contents"][0]["parts"][0];
        assert_eq!(p["thought"], true);
        assert_eq!(p["text"], "deliberation");
        assert_eq!(p["thoughtSignature"], "sigT");
    }

    #[test]
    fn build_reasoning_hint_response_schema_and_image() {
        let mut req = ChatRequest::new("gemini/gemini-3.1-pro");
        req.reasoning = ReasoningHint::Low;
        req.response_format = Some(json!({"type": "object"}));
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![ContentPart::Image { media_type: "image/jpeg".into(), data_base64: "aGk=".into() }],
        });
        let body = build_request_body(&req);
        assert_eq!(body["generationConfig"]["thinkingConfig"]["thinkingBudget"], 2048);
        assert_eq!(body["generationConfig"]["responseMimeType"], "application/json");
        assert_eq!(body["generationConfig"]["responseSchema"], json!({"type": "object"}));
        let img = &body["contents"][0]["parts"][0]["inlineData"];
        assert_eq!(img["mimeType"], "image/jpeg");
        assert_eq!(img["data"], "aGk=");
    }

    #[test]
    fn parse_text_thought_and_usage() {
        let body = json!({
            "modelVersion": "gemini-3.1-pro",
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "planning", "thought": true},
                    {"text": "The answer is 4."}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 1000,
                "cachedContentTokenCount": 600,
                "candidatesTokenCount": 40,
                "thoughtsTokenCount": 25
            }
        });
        let resp = parse_response(&body).expect("parse");
        assert_eq!(resp.text(), "The answer is 4.");
        assert!(matches!(&resp.parts[0], ContentPart::Reasoning { text, .. } if text == "planning"));
        assert_eq!(resp.stop, StopReason::EndTurn);
        // promptTokenCount includes cached tokens → input = 1000 - 600.
        assert_eq!(resp.usage.input_tokens, 400);
        assert_eq!(resp.usage.cache_read_tokens, 600);
        assert_eq!(resp.usage.output_tokens, 40);
        assert_eq!(resp.usage.reasoning_tokens, 25);
        assert_eq!(resp.model_used, "gemini-3.1-pro");
    }

    #[test]
    fn parse_function_call_sets_tool_use_stop() {
        let body = json!({
            "candidates": [{
                "content": {"parts": [{"functionCall": {"name": "calc", "args": {"a": 1}}}]},
                "finishReason": "STOP"
            }]
        });
        let resp = parse_response(&body).expect("parse");
        assert_eq!(resp.stop, StopReason::ToolUse);
        let calls = resp.tool_calls();
        assert_eq!(calls[0].1, "calc");
        assert_eq!(calls[0].2, &json!({"a": 1}));
    }

    #[test]
    fn parse_finish_reasons() {
        for (raw, expected) in [
            ("MAX_TOKENS", StopReason::MaxTokens),
            ("SAFETY", StopReason::ContentFilter),
            ("PROHIBITED_CONTENT", StopReason::ContentFilter),
            ("RECITATION", StopReason::Other("RECITATION".into())),
        ] {
            let body = json!({"candidates": [{"content": {"parts": []}, "finishReason": raw}]});
            assert_eq!(parse_response(&body).unwrap().stop, expected);
        }
    }

    #[test]
    fn parse_prompt_block_is_content_filter_error() {
        let body = json!({"promptFeedback": {"blockReason": "SAFETY"}});
        assert_eq!(parse_response(&body), Err(LlmError::ContentFilter));
    }

    #[test]
    fn parse_missing_candidates_is_parse_error() {
        assert!(matches!(parse_response(&json!({})), Err(LlmError::Parse(_))));
    }
}
