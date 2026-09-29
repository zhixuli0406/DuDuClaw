//! Legacy `chat/completions` provider for OpenAI-compatible endpoints —
//! DeepSeek / Qwen / xAI / Groq / Together / Mistral / MiniMax / OpenRouter
//! and local servers (llamafile / vLLM / SGLang / Ollama).
//!
//! Subsumes the two near-duplicate gateway/inference compat clients
//! (`duduclaw-gateway/src/runtime/openai_compat.rs`,
//! `duduclaw-inference/src/openai_compat.rs`); the provider presets and
//! env-var names below are ported from the gateway table.
//!
//! Quirk handling:
//! - `tool_calls[].function.arguments` is a STRING → parsed to
//!   `serde_json::Value` at the boundary (raw string fallback on bad JSON);
//! - DeepSeek `reasoning_content` / Qwen (and OpenRouter) `reasoning`
//!   fields → `Reasoning` part / `ReasoningDelta` events;
//! - real SSE streaming (delta chunks), including streamed tool calls.

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde_json::{Value, json};

use crate::error::{LlmError, classify_http, classify_transport, snippet};
use crate::http::{http_client, retry_after_of};
use crate::provider::{ApiAuth, ChatProvider, split_model_id};
use crate::sse::{SseParser, drive_sse, sse_data};
use crate::types::{
    ChatRequest, ChatResponse, ContentPart, NormalizedUsage, Role, StopReason, StreamEvent,
    ToolChoice,
};

// ---------------------------------------------------------------------------
// Provider presets (ported from gateway runtime/openai_compat.rs)
// ---------------------------------------------------------------------------

/// A known OpenAI-compatible provider preset.
#[derive(Debug, Clone, Copy)]
pub struct CompatPreset {
    pub name: &'static str,
    pub base_url: &'static str,
    /// Standard env var carrying the API key.
    pub env_key: &'static str,
    pub default_model: &'static str,
}

/// Built-in presets. Env-var names match the gateway's provider table plus
/// `resolve_env_key` in `provider.rs`.
pub const COMPAT_PRESETS: &[CompatPreset] = &[
    CompatPreset {
        name: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        env_key: "DEEPSEEK_API_KEY",
        default_model: "deepseek-chat",
    },
    CompatPreset {
        name: "minimax",
        base_url: "https://api.minimax.io/v1",
        env_key: "MINIMAX_API_KEY",
        default_model: "MiniMax-M2.7",
    },
    CompatPreset {
        name: "groq",
        base_url: "https://api.groq.com/openai/v1",
        env_key: "GROQ_API_KEY",
        default_model: "llama-3.3-70b-versatile",
    },
    CompatPreset {
        name: "together",
        base_url: "https://api.together.xyz/v1",
        env_key: "TOGETHER_API_KEY",
        default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    },
    CompatPreset {
        name: "mistral",
        base_url: "https://api.mistral.ai/v1",
        env_key: "MISTRAL_API_KEY",
        default_model: "mistral-small-3",
    },
    CompatPreset {
        name: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        env_key: "OPENROUTER_API_KEY",
        default_model: "anthropic/claude-sonnet-5",
    },
    CompatPreset {
        name: "xai",
        base_url: "https://api.x.ai/v1",
        env_key: "XAI_API_KEY",
        default_model: "grok-4.1-fast",
    },
    CompatPreset {
        name: "qwen",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        env_key: "DASHSCOPE_API_KEY",
        default_model: "qwen3.7-max",
    },
];

/// Look up a preset by provider name.
pub fn preset(name: &str) -> Option<&'static CompatPreset> {
    COMPAT_PRESETS.iter().find(|p| p.name == name)
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct OpenAiCompatProvider {
    /// Provider id (`"deepseek"`, `"openrouter"`, `"local"`, ...).
    id: String,
    auth: ApiAuth,
    base_url: String,
    /// Extra headers (e.g. OpenRouter `HTTP-Referer` attribution).
    extra_headers: Vec<(String, String)>,
    /// O11: send `ChatRequest::model` verbatim instead of stripping a
    /// `provider/` qualifier. Off by default (hosted presets qualify their
    /// ids); see [`OpenAiCompatProvider::with_verbatim_model_id`].
    verbatim_model_id: bool,
    /// O11: an owned client replacing the shared 120s singleton. `None` ⇒ the
    /// singleton, i.e. byte-identical behavior for every pre-O11 caller.
    client: Option<reqwest::Client>,
}

impl OpenAiCompatProvider {
    /// The base URL requests go to (e.g. `http://127.0.0.1:8080/v1`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Construct against an explicit base URL (local servers, proxies).
    /// `auth.base_url` (if set) takes precedence over `default_base_url`.
    pub fn new(id: impl Into<String>, auth: ApiAuth, default_base_url: impl Into<String>) -> Self {
        let base_url = auth
            .base_url
            .clone()
            .unwrap_or_else(|| default_base_url.into())
            .trim_end_matches('/')
            .to_string();
        Self {
            id: id.into(),
            auth,
            base_url,
            extra_headers: Vec::new(),
            verbatim_model_id: false,
            client: None,
        }
    }

    /// Construct from a built-in preset. `None` for unknown preset names
    /// (fail closed — no guessed base URL).
    pub fn from_preset(name: &str, auth: ApiAuth) -> Option<Self> {
        let p = preset(name)?;
        Some(Self::new(p.name, auth, p.base_url))
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }

    /// Send [`ChatRequest::model`] to the server **verbatim**, without
    /// stripping a leading `provider/` qualifier.
    ///
    /// O11: the hosted presets qualify their ids (`"deepseek/deepseek-v3.2"`)
    /// and must be stripped, which is why [`split_model_id`] is the default.
    /// A **local** server is the opposite case: llama.cpp / Ollama / vLLM
    /// routinely serve HuggingFace repo ids that legitimately contain a slash
    /// (`qwen/qwen3-4b`), and stripping the first segment would name a model
    /// the server does not have. Local callers opt in here.
    ///
    /// [`split_model_id`]: crate::split_model_id
    pub fn with_verbatim_model_id(mut self) -> Self {
        self.verbatim_model_id = true;
        self
    }

    /// Use an owned HTTP client with this request timeout instead of the
    /// crate-shared 120s singleton.
    ///
    /// O11: local inference on CPU regularly runs a single completion past two
    /// minutes, so the local-inference backend carried its own 300s client.
    /// Callers that do not set this keep the singleton — unchanged behavior.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.client = reqwest::Client::builder().timeout(timeout).build().ok();
        self
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

// ---------------------------------------------------------------------------
// Request build (pure)
// ---------------------------------------------------------------------------

/// Build the chat/completions body for `req` (pure — no I/O).
///
/// `pub` so callers that embed this provider can assert on the exact wire
/// body offline; `build_request_body_with` is the variant that also controls
/// model-id qualifier stripping.
pub fn build_request_body(req: &ChatRequest, stream: bool) -> Value {
    build_request_body_with(req, stream, false)
}

/// `verbatim_model_id` — see [`OpenAiCompatProvider::with_verbatim_model_id`].
pub fn build_request_body_with(
    req: &ChatRequest,
    stream: bool,
    verbatim_model_id: bool,
) -> Value {
    let bare_model = if verbatim_model_id {
        req.model.as_str()
    } else {
        split_model_id(&req.model).1
    };

    let mut messages: Vec<Value> = Vec::new();
    let system_text = req
        .system
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !system_text.is_empty() {
        messages.push(json!({"role": "system", "content": system_text}));
    }

    for msg in &req.messages {
        match msg.role {
            Role::User => {
                // Tool results become standalone `tool` role messages; the
                // remaining parts form the user message.
                let mut content_parts: Vec<Value> = Vec::new();
                let mut has_image = false;
                let mut plain_text = String::new();
                for part in &msg.parts {
                    match part {
                        ContentPart::Text(t) => {
                            plain_text.push_str(t);
                            content_parts.push(json!({"type": "text", "text": t}));
                        }
                        ContentPart::Image {
                            media_type,
                            data_base64,
                        } => {
                            has_image = true;
                            content_parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{media_type};base64,{data_base64}")}
                            }));
                        }
                        ContentPart::ToolResult {
                            call_id, content, ..
                        } => {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": call_id,
                                "content": content,
                            }));
                        }
                        // Tool calls never appear in user messages; reasoning
                        // is never replayed to compat providers (DeepSeek
                        // rejects echoed reasoning_content).
                        ContentPart::ToolCall { .. } | ContentPart::Reasoning { .. } => {}
                    }
                }
                if has_image {
                    messages.push(json!({"role": "user", "content": content_parts}));
                } else if !plain_text.is_empty() {
                    // Plain string content — maximum server compatibility.
                    messages.push(json!({"role": "user", "content": plain_text}));
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                for part in &msg.parts {
                    match part {
                        ContentPart::Text(t) => text.push_str(t),
                        ContentPart::ToolCall { id, name, args } => tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            // Arguments back to STRING for the wire format.
                            "function": {"name": name, "arguments": args.to_string()},
                        })),
                        _ => {}
                    }
                }
                let mut m = json!({"role": "assistant"});
                m["content"] = if text.is_empty() {
                    Value::Null
                } else {
                    json!(text)
                };
                if !tool_calls.is_empty() {
                    m["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(m);
            }
        }
    }

    let mut body = json!({
        "model": bare_model,
        "messages": messages,
        "max_tokens": req.max_tokens,
        "stream": stream,
    });
    if stream {
        // Ask for a usage chunk on the final SSE event (OpenAI extension,
        // honored by vLLM/DeepSeek/Groq; harmlessly ignored elsewhere).
        body["stream_options"] = json!({"include_usage": true});
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    // O11: sampling / stop / logprob knobs the local-inference backend used to
    // send from its own client. Every one of them is absent when unset, so a
    // pre-O11 request body is byte-identical.
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if !req.stop.is_empty() {
        body["stop"] = json!(req.stop);
    }
    if let Some(l) = req.logprobs {
        body["logprobs"] = json!(l);
    }
    if let Some(n) = req.top_logprobs {
        body["top_logprobs"] = json!(n);
    }
    // P1/WP-3: `reasoning_effort` is the OpenAI **chat/completions** spelling
    // (top-level scalar), which is what this compat surface speaks — unlike the
    // Responses API's nested `reasoning: {effort}` in `openai.rs`. Presets that
    // don't understand it ignore an unknown top-level key; `None` ⇒ absent.
    if let Some(effort) = req.reasoning_effort.as_deref() {
        body["reasoning_effort"] = json!(effort);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect(),
        );
        body["tool_choice"] = match &req.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Tool(name) => json!({"type": "function", "function": {"name": name}}),
        };
    }
    if let Some(schema) = &req.response_format {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {"name": "response", "schema": schema}
        });
    }
    // ReasoningHint has no portable chat/completions mapping → omitted.
    body
}

// ---------------------------------------------------------------------------
// Response parse (pure)
// ---------------------------------------------------------------------------

fn parse_string_args(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| {
        if raw.is_empty() {
            json!({})
        } else {
            Value::String(raw.to_string())
        }
    })
}

fn parse_finish_reason(raw: Option<&str>) -> StopReason {
    match raw {
        None | Some("stop") => StopReason::EndTurn,
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::ContentFilter,
        Some(other) => StopReason::Other(other.to_string()),
    }
}

fn parse_usage(u: &Value) -> NormalizedUsage {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    // Cache reads: OpenAI-style prompt_tokens_details.cached_tokens or
    // DeepSeek's prompt_cache_hit_tokens.
    let cache_read = u
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| g("prompt_cache_hit_tokens"));
    NormalizedUsage {
        input_tokens: g("prompt_tokens").saturating_sub(cache_read),
        output_tokens: g("completion_tokens"),
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        // Included in completion_tokens by every compat provider → 0 here.
        reasoning_tokens: 0,
    }
}

// ---------------------------------------------------------------------------
// Logprobs (O11 — the capability local inference had its own client for)
// ---------------------------------------------------------------------------

/// One generated token's logprob, plus the alternatives the server reported.
///
/// Deliberately a thin, provider-shaped record rather than a scored signal:
/// the *interpretation* (mean logprob, UCCI top-2 margin) belongs to the
/// caller that owns the calibration model, not to the transport.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenLogprob {
    /// Logprob of the token that was actually emitted.
    pub logprob: f64,
    /// Logprobs of the top-N candidates at this position, when the request
    /// asked for them ([`ChatRequest::top_logprobs`]) and the server obliged.
    pub top_logprobs: Option<Vec<f64>>,
}

/// A choice's token-level logprobs, as returned for one completion.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceLogprobs {
    pub tokens: Vec<TokenLogprob>,
    /// The choice's `finish_reason` verbatim — UCCI needs to know whether the
    /// last token was a reported stop token so it can exclude it.
    pub finish_reason: Option<String>,
}

/// Extract `choices[0].logprobs` from a chat/completions body.
///
/// `None` when the server returned no logprobs (or an empty token list) —
/// the fail-safe path, identical to a server without logprob support.
pub fn parse_logprobs(body: &Value) -> Option<ChoiceLogprobs> {
    let choice = body.pointer("/choices/0")?;
    let content = choice.pointer("/logprobs/content")?.as_array()?;
    if content.is_empty() {
        return None;
    }
    let tokens = content
        .iter()
        .map(|t| TokenLogprob {
            logprob: t.get("logprob").and_then(Value::as_f64).unwrap_or(0.0),
            top_logprobs: t.get("top_logprobs").and_then(Value::as_array).map(|a| {
                a.iter()
                    .map(|c| c.get("logprob").and_then(Value::as_f64).unwrap_or(0.0))
                    .collect()
            }),
        })
        .collect();
    Some(ChoiceLogprobs {
        tokens,
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub(crate) fn parse_response(body: &Value, provider_id: &str) -> Result<ChatResponse, LlmError> {
    let choice = body
        .pointer("/choices/0")
        .ok_or_else(|| LlmError::Parse(snippet(&format!("missing choices: {body}"))))?;
    let message = choice
        .get("message")
        .ok_or_else(|| LlmError::Parse(snippet(&format!("missing message: {choice}"))))?;

    let mut parts = Vec::new();
    // DeepSeek `reasoning_content` / Qwen & OpenRouter `reasoning`.
    let reasoning = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !reasoning.is_empty() {
        parts.push(ContentPart::Reasoning {
            text: reasoning.to_string(),
            signature: None,
        });
    }
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            parts.push(ContentPart::Text(text.to_string()));
        }
    }
    for tc in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let raw_args = tc
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        parts.push(ContentPart::ToolCall {
            id: tc
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: tc
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            args: parse_string_args(raw_args),
        });
    }

    Ok(ChatResponse {
        parts,
        stop: parse_finish_reason(choice.get("finish_reason").and_then(Value::as_str)),
        usage: body.get("usage").map(parse_usage).unwrap_or_default(),
        model_used: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        provider: provider_id.to_string(),
    })
}

// ---------------------------------------------------------------------------
// SSE streaming parser (pure state machine)
// ---------------------------------------------------------------------------

pub(crate) struct CompatSse {
    provider_id: String,
    text: String,
    reasoning: String,
    /// tool index → (id, name, accumulated string args)
    tool_calls: Vec<(String, String, String)>,
    started: std::collections::HashSet<usize>,
    usage: NormalizedUsage,
    finish: Option<StopReason>,
    model_used: String,
    finished: bool,
}

impl CompatSse {
    pub fn new(provider_id: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            text: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            started: std::collections::HashSet::new(),
            usage: NormalizedUsage::default(),
            finish: None,
            model_used: String::new(),
            finished: false,
        }
    }

    fn handle_chunk(&mut self, chunk: &Value, out: &mut Vec<StreamEvent>) {
        if let Some(m) = chunk.get("model").and_then(Value::as_str) {
            if self.model_used.is_empty() {
                self.model_used = m.to_string();
            }
        }
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = parse_usage(u);
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return;
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(parse_finish_reason(Some(fr)));
        }
        let Some(delta) = choice.get("delta") else {
            return;
        };

        if let Some(t) = delta.get("content").and_then(Value::as_str) {
            if !t.is_empty() {
                self.text.push_str(t);
                out.push(StreamEvent::TextDelta(t.to_string()));
            }
        }
        let r = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !r.is_empty() {
            self.reasoning.push_str(r);
            out.push(StreamEvent::ReasoningDelta(r.to_string()));
        }
        for tc in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            while self.tool_calls.len() <= index {
                self.tool_calls
                    .push((String::new(), String::new(), String::new()));
            }
            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                self.tool_calls[index].0 = id.to_string();
            }
            if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str) {
                self.tool_calls[index].1 = name.to_string();
            }
            if !self.started.contains(&index) && !self.tool_calls[index].1.is_empty() {
                self.started.insert(index);
                out.push(StreamEvent::ToolCallStart {
                    index,
                    id: self.tool_calls[index].0.clone(),
                    name: self.tool_calls[index].1.clone(),
                });
            }
            if let Some(frag) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                if !frag.is_empty() {
                    self.tool_calls[index].2.push_str(frag);
                    out.push(StreamEvent::ToolCallDelta {
                        index,
                        args_fragment: frag.to_string(),
                    });
                }
            }
        }
    }
}

impl SseParser for CompatSse {
    fn on_line(&mut self, line: &str, out: &mut Vec<StreamEvent>) {
        let Some(data) = sse_data(line) else { return };
        if data == "[DONE]" {
            self.finished = true;
            return;
        }
        if let Ok(chunk) = serde_json::from_str::<Value>(data) {
            self.handle_chunk(&chunk, out);
        }
    }

    fn finished(&self) -> bool {
        self.finished
    }

    fn finalize(&mut self) -> Result<StreamEvent, LlmError> {
        let mut parts = Vec::new();
        if !self.reasoning.is_empty() {
            parts.push(ContentPart::Reasoning {
                text: std::mem::take(&mut self.reasoning),
                signature: None,
            });
        }
        if !self.text.is_empty() {
            parts.push(ContentPart::Text(std::mem::take(&mut self.text)));
        }
        for (id, name, raw_args) in self.tool_calls.drain(..) {
            if name.is_empty() && raw_args.is_empty() {
                continue;
            }
            parts.push(ContentPart::ToolCall {
                id,
                name,
                args: parse_string_args(&raw_args),
            });
        }
        Ok(StreamEvent::Done(ChatResponse {
            parts,
            stop: self.finish.take().unwrap_or(StopReason::EndTurn),
            usage: self.usage,
            model_used: std::mem::take(&mut self.model_used),
            provider: std::mem::take(&mut self.provider_id),
        }))
    }
}

// ---------------------------------------------------------------------------
// HTTP execution
// ---------------------------------------------------------------------------

impl OpenAiCompatProvider {
    async fn send(&self, req: &ChatRequest, stream: bool) -> Result<reqwest::Response, LlmError> {
        let body = build_request_body_with(req, stream, self.verbatim_model_id);
        let mut http = self
            .client
            .as_ref()
            .unwrap_or_else(|| http_client())
            .post(self.chat_url())
            .header("content-type", "application/json");
        if !self.auth.api_key.is_empty() {
            http = http.bearer_auth(&self.auth.api_key);
        }
        for (name, value) in &self.extra_headers {
            http = http.header(name, value);
        }
        let response = http
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

    /// One-shot completion that **also** hands back the choice's token
    /// logprobs, when the request asked for them and the server obliged.
    ///
    /// O11: this is the seam the local-inference backend needed and used to
    /// justify a second reqwest client. The logprobs ride alongside
    /// [`ChatResponse`] rather than inside it so the 40-odd existing
    /// `ChatResponse` construction sites stay untouched — and so the
    /// normalized response keeps meaning "what the model said", not "what the
    /// sampler thought".
    ///
    /// Returns `(response, None)` for every server that returns no logprobs —
    /// never an error. `complete()` is exactly this with the second element
    /// dropped.
    pub async fn complete_with_logprobs(
        &self,
        req: &ChatRequest,
    ) -> Result<(ChatResponse, Option<ChoiceLogprobs>), LlmError> {
        let response = self.send(req, false).await?;
        let text = response.text().await.map_err(|e| classify_transport(&e))?;
        let body: Value =
            serde_json::from_str(&text).map_err(|e| LlmError::Parse(snippet(&e.to_string())))?;
        let logprobs = parse_logprobs(&body);
        Ok((parse_response(&body, &self.id)?, logprobs))
    }
}

#[async_trait]
impl ChatProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.complete_with_logprobs(req).await.map(|(r, _)| r)
    }

    /// Real SSE streaming (delta chunks).
    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let response = self.send(req, true).await?;
        Ok(drive_sse(response, CompatSse::new(&self.id)))
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
    fn presets_ported_from_gateway_table() {
        // Names + env vars must stay aligned with the gateway runtime table.
        for (name, env, base) in [
            (
                "deepseek",
                "DEEPSEEK_API_KEY",
                "https://api.deepseek.com/v1",
            ),
            ("minimax", "MINIMAX_API_KEY", "https://api.minimax.io/v1"),
            ("groq", "GROQ_API_KEY", "https://api.groq.com/openai/v1"),
            (
                "together",
                "TOGETHER_API_KEY",
                "https://api.together.xyz/v1",
            ),
            ("mistral", "MISTRAL_API_KEY", "https://api.mistral.ai/v1"),
            (
                "openrouter",
                "OPENROUTER_API_KEY",
                "https://openrouter.ai/api/v1",
            ),
            ("xai", "XAI_API_KEY", "https://api.x.ai/v1"),
            (
                "qwen",
                "DASHSCOPE_API_KEY",
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
            ),
        ] {
            let p = preset(name).unwrap_or_else(|| panic!("missing preset {name}"));
            assert_eq!(p.env_key, env);
            assert_eq!(p.base_url, base);
        }
        assert!(preset("no-such").is_none());
        assert!(OpenAiCompatProvider::from_preset("no-such", ApiAuth::new("k")).is_none());
    }

    #[test]
    fn auth_base_url_overrides_preset() {
        let p = OpenAiCompatProvider::from_preset(
            "deepseek",
            ApiAuth::new("k").with_base_url("http://localhost:8000/v1/"),
        )
        .unwrap();
        assert_eq!(p.chat_url(), "http://localhost:8000/v1/chat/completions");
        let p = OpenAiCompatProvider::from_preset("deepseek", ApiAuth::new("k")).unwrap();
        assert_eq!(p.chat_url(), "https://api.deepseek.com/v1/chat/completions");
    }

    #[test]
    fn build_reasoning_effort_is_a_top_level_key_and_absent_when_none() {
        let plain = build_request_body(&ChatRequest::new("deepseek/deepseek-v3.2"), false);
        assert!(plain.get("reasoning_effort").is_none(), "{plain}");

        let mut req = ChatRequest::new("deepseek/deepseek-v3.2");
        req.reasoning_effort = Some("medium".to_string());
        let body = build_request_body(&req, false);
        // chat/completions spelling: top-level scalar, NOT the Responses
        // API's nested `reasoning: {effort}`.
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("reasoning").is_none(), "{body}");
    }

    #[test]
    fn build_system_message_and_plain_text_user() {
        let mut req = ChatRequest::new("deepseek/deepseek-v3.2");
        req.system = vec![SystemBlock::cached("rules"), SystemBlock::uncached("queue")];
        req.messages.push(ChatMessage::user("hi"));
        let body = build_request_body(&req, false);
        assert_eq!(body["model"], "deepseek-v3.2");
        assert_eq!(body["stream"], false);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(
            messages[0],
            json!({"role": "system", "content": "rules\n\nqueue"})
        );
        // Plain string content for text-only (max compatibility).
        assert_eq!(messages[1], json!({"role": "user", "content": "hi"}));
    }

    #[test]
    fn build_tool_cycle_with_string_arguments() {
        let mut req = ChatRequest::new("deepseek/deepseek-v3.2");
        req.messages.push(ChatMessage::user("calc"));
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
        let body = build_request_body(&req, false);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], Value::Null);
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            r#"{"expr":"1+1"}"#
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn build_tools_tool_choice_image_and_stream_options() {
        let mut req = ChatRequest::new("xai/grok-4.1-fast");
        req.tools.push(ToolDef {
            name: "f".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        });
        req.tool_choice = ToolChoice::Tool("f".into());
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![
                ContentPart::Text("look".into()),
                ContentPart::Image {
                    media_type: "image/png".into(),
                    data_base64: "aGk=".into(),
                },
            ],
        });
        let body = build_request_body(&req, true);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        // Nested function shape (chat/completions dialect).
        assert_eq!(body["tools"][0]["function"]["name"], "f");
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "function": {"name": "f"}})
        );
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,aGk=");
    }

    #[test]
    fn parse_text_tool_calls_and_usage() {
        let body = json!({
            "model": "deepseek-v3.2",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "using tool",
                    "tool_calls": [{
                        "id": "call_7",
                        "type": "function",
                        "function": {"name": "calc", "arguments": "{\"a\":1}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "prompt_cache_hit_tokens": 60}
        });
        let resp = parse_response(&body, "deepseek").expect("parse");
        assert_eq!(resp.stop, StopReason::ToolUse);
        assert_eq!(resp.text(), "using tool");
        // STRING args parsed to a Value.
        assert_eq!(resp.tool_calls()[0].2, &json!({"a": 1}));
        // DeepSeek cache hit tokens split out of prompt_tokens.
        assert_eq!(resp.usage.input_tokens, 40);
        assert_eq!(resp.usage.cache_read_tokens, 60);
        assert_eq!(resp.usage.output_tokens, 20);
        assert_eq!(resp.provider, "deepseek");
    }

    #[test]
    fn parse_openai_style_cached_tokens_detail() {
        let body = json!({
            "choices": [{"message": {"content": "x"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 100, "completion_tokens": 1,
                "prompt_tokens_details": {"cached_tokens": 75}
            }
        });
        let resp = parse_response(&body, "groq").expect("parse");
        assert_eq!(resp.usage.cache_read_tokens, 75);
        assert_eq!(resp.usage.input_tokens, 25);
    }

    #[test]
    fn parse_reasoning_content_fields() {
        // DeepSeek shape.
        let body = json!({
            "choices": [{"message": {"content": "4", "reasoning_content": "2+2..."}, "finish_reason": "stop"}]
        });
        let resp = parse_response(&body, "deepseek").expect("parse");
        assert!(matches!(&resp.parts[0], ContentPart::Reasoning { text, .. } if text == "2+2..."));
        // Qwen / OpenRouter shape.
        let body = json!({
            "choices": [{"message": {"content": "4", "reasoning": "thinking..."}, "finish_reason": "stop"}]
        });
        let resp = parse_response(&body, "qwen").expect("parse");
        assert!(
            matches!(&resp.parts[0], ContentPart::Reasoning { text, .. } if text == "thinking...")
        );
    }

    #[test]
    fn parse_malformed_tool_args_fall_back_to_string() {
        let body = json!({
            "choices": [{"message": {"tool_calls": [{
                "id": "c", "function": {"name": "f", "arguments": "{broken"}
            }]}, "finish_reason": "tool_calls"}]
        });
        let resp = parse_response(&body, "x").expect("parse");
        assert_eq!(resp.tool_calls()[0].2, &Value::String("{broken".into()));
    }

    #[test]
    fn parse_finish_reasons_and_missing_choices() {
        for (raw, expected) in [
            ("stop", StopReason::EndTurn),
            ("length", StopReason::MaxTokens),
            ("content_filter", StopReason::ContentFilter),
        ] {
            let body = json!({"choices": [{"message": {"content": "x"}, "finish_reason": raw}]});
            assert_eq!(parse_response(&body, "p").unwrap().stop, expected);
        }
        assert!(matches!(
            parse_response(&json!({"choices": []}), "p"),
            Err(LlmError::Parse(_))
        ));
    }

    #[test]
    fn sse_text_reasoning_and_usage_accumulate() {
        let mut p = CompatSse::new("deepseek");
        let mut out = Vec::new();
        for line in [
            r#"data: {"model":"deepseek-v3.2","choices":[{"delta":{"reasoning_content":"think"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":3}}"#,
            "data: [DONE]",
        ] {
            p.on_line(line, &mut out);
        }
        assert!(p.finished());
        assert_eq!(out[0], StreamEvent::ReasoningDelta("think".into()));
        assert_eq!(out[1], StreamEvent::TextDelta("Hel".into()));
        let StreamEvent::Done(resp) = p.finalize().unwrap() else {
            panic!()
        };
        assert_eq!(resp.text(), "Hello");
        assert_eq!(resp.stop, StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 9);
        assert_eq!(resp.usage.output_tokens, 3);
        assert_eq!(resp.model_used, "deepseek-v3.2");
        assert!(matches!(&resp.parts[0], ContentPart::Reasoning { text, .. } if text == "think"));
    }

    #[test]
    fn sse_streamed_tool_call_assembles_arguments() {
        let mut p = CompatSse::new("groq");
        let mut out = Vec::new();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_3","function":{"name":"calc","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"2}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ] {
            p.on_line(line, &mut out);
        }
        assert!(
            matches!(&out[0], StreamEvent::ToolCallStart { id, name, index: 0 } if id == "call_3" && name == "calc")
        );
        assert!(
            matches!(&out[1], StreamEvent::ToolCallDelta { args_fragment, .. } if args_fragment == "{\"a\":")
        );
        let StreamEvent::Done(resp) = p.finalize().unwrap() else {
            panic!()
        };
        assert_eq!(resp.stop, StopReason::ToolUse);
        assert_eq!(resp.tool_calls()[0].2, &json!({"a": 2}));
    }

    // ── O11: sampling / stop / logprob knobs + verbatim model id ─────────

    /// A request that sets none of the O11 fields must serialize to the exact
    /// same body as before they existed — the guard that absorbing the
    /// local-inference client changed nothing for hosted compat providers.
    #[test]
    fn o11_fields_absent_when_unset() {
        let req = ChatRequest::new("deepseek/deepseek-v3.2");
        let body = build_request_body(&req, false);
        for key in ["top_p", "stop", "logprobs", "top_logprobs"] {
            assert!(
                body.get(key).is_none(),
                "{key} must be absent from a request that did not ask for it: {body}"
            );
        }
    }

    #[test]
    fn o11_top_p_stop_and_logprobs_map_to_openai_keys() {
        let mut req = ChatRequest::new("local-model");
        req.top_p = Some(0.9);
        req.stop = vec!["</s>".into(), "\n\n".into()];
        req.logprobs = Some(true);
        req.top_logprobs = Some(2);
        let body = build_request_body(&req, false);
        // `Value` widens f32→f64, so compare at f32 precision (the same
        // widening `temperature` has always had on this surface).
        assert_eq!(body["top_p"].as_f64().map(|v| v as f32), Some(0.9_f32));
        assert_eq!(body["stop"], json!(["</s>", "\n\n"]));
        assert_eq!(body["logprobs"], json!(true));
        assert_eq!(body["top_logprobs"], json!(2));
    }

    /// A HuggingFace-style local model id contains a slash that is part of the
    /// name, not a provider qualifier. Stripping it would name a model the
    /// local server does not serve.
    #[test]
    fn o11_verbatim_model_id_keeps_the_slash() {
        let req = ChatRequest::new("qwen/qwen3-4b");
        assert_eq!(build_request_body(&req, false)["model"], json!("qwen3-4b"));
        assert_eq!(
            build_request_body_with(&req, false, true)["model"],
            json!("qwen/qwen3-4b")
        );
    }

    #[test]
    fn o11_parse_logprobs_extracts_tokens_and_candidates() {
        let body: Value = serde_json::from_str(
            r#"{"choices":[{"message":{"role":"assistant","content":"ab"},"finish_reason":"stop",
                "logprobs":{"content":[
                    {"logprob":-0.1,"top_logprobs":[{"logprob":-0.1},{"logprob":-2.3}]},
                    {"logprob":-0.7,"top_logprobs":[{"logprob":-0.7},{"logprob":-1.4}]}
                ]}}]}"#,
        )
        .unwrap();
        let lp = parse_logprobs(&body).expect("logprobs present");
        assert_eq!(lp.finish_reason.as_deref(), Some("stop"));
        assert_eq!(lp.tokens.len(), 2);
        assert_eq!(lp.tokens[0].logprob, -0.1);
        assert_eq!(lp.tokens[1].top_logprobs.as_deref(), Some(&[-0.7, -1.4][..]));
    }

    /// A server that ignores `logprobs`, and one that returns an empty list,
    /// must both read as "no signal" — never an error, never a fabricated 0.
    #[test]
    fn o11_parse_logprobs_is_none_when_absent_or_empty() {
        let absent: Value =
            serde_json::from_str(r#"{"choices":[{"message":{"content":"hi"}}]}"#).unwrap();
        assert!(parse_logprobs(&absent).is_none());
        let empty: Value =
            serde_json::from_str(r#"{"choices":[{"message":{"content":""},"logprobs":{"content":[]}}]}"#)
                .unwrap();
        assert!(parse_logprobs(&empty).is_none());
    }
}
