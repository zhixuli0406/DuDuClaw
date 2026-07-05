//! Normalized request/response types — the single convergence point.
//!
//! Shape follows the Vercel AI SDK v5 content-parts model (messages are a
//! list of typed parts, not a flat string) with Anthropic semantics for
//! caching (`CacheHint`), reasoning replay, and stop reasons. Every provider
//! translates these types to/from its native wire format; nothing
//! provider-specific leaks out of `providers/`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// Marker that splits a monolithic system prompt into separately-cached
/// blocks. Re-exported from the gateway's `direct_api.rs` convention so
/// prompt assemblers keep working unchanged when they migrate to this crate.
pub const CACHE_SPLIT_MARKER: &str = "<!-- duduclaw:cache-split -->";

/// Caching intent for a system block.
///
/// Only [`CacheHint::Explicit`] maps to a native cache breakpoint
/// (Anthropic `cache_control: ephemeral`). `Auto` and `None` are ignored by
/// providers that require explicit breakpoints; providers with implicit
/// prefix caching (OpenAI, Gemini, vLLM/SGLang) ignore the hint entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CacheHint {
    #[default]
    None,
    Auto,
    Explicit,
}

/// One block of the system prompt with a caching hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemBlock {
    pub text: String,
    pub cache: CacheHint,
}

impl SystemBlock {
    pub fn cached(text: impl Into<String>) -> Self {
        Self { text: text.into(), cache: CacheHint::Explicit }
    }

    pub fn uncached(text: impl Into<String>) -> Self {
        Self { text: text.into(), cache: CacheHint::None }
    }
}

/// Conversation role. System content lives in [`ChatRequest::system`], and
/// tool results are content parts of a `User` message (Anthropic semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}

/// A typed content part (Vercel AI SDK v5 shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContentPart {
    Text(String),
    /// Base64-encoded image. `media_type` is a MIME type like `image/png`.
    Image {
        media_type: String,
        data_base64: String,
    },
    /// A tool invocation issued by the assistant. `args` is always a parsed
    /// [`serde_json::Value`] internally — providers that transport arguments
    /// as strings (OpenAI-compat `tool_calls[].function.arguments`) parse at
    /// the boundary.
    ToolCall {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// The result of a tool invocation, sent back in a `User` message.
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    /// Model reasoning ("thinking"). `signature` carries the opaque replay
    /// token — Anthropic thinking-block `signature` or Gemini
    /// `thoughtSignature` — and MUST be echoed back verbatim on the next
    /// request for tool-use loops to keep working.
    Reasoning {
        text: String,
        signature: Option<String>,
    },
}

/// One conversation message: a role plus typed parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub parts: Vec<ContentPart>,
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, parts: vec![ContentPart::Text(text.into())] }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, parts: vec![ContentPart::Text(text.into())] }
    }
}

/// A tool definition (JSON-schema parameters).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool input.
    pub input_schema: serde_json::Value,
}

/// Tool-choice policy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolChoice {
    #[default]
    Auto,
    /// Never call tools.
    None,
    /// Must call some tool (Anthropic `any`, OpenAI `required`, Gemini `ANY`).
    Required,
    /// Must call this specific tool.
    Tool(String),
}

/// Reasoning-effort hint. Maps to OpenAI `reasoning.effort`, Anthropic
/// `thinking.budget_tokens`, Gemini `thinkingConfig.thinkingBudget`.
/// `Off` omits the field entirely (provider default behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReasoningHint {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningHint {
    /// Thinking-token budget used for providers that take a numeric budget.
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            ReasoningHint::Off => None,
            ReasoningHint::Low => Some(2_048),
            ReasoningHint::Medium => Some(8_192),
            ReasoningHint::High => Some(24_576),
        }
    }

    /// Effort label for providers that take a string (OpenAI Responses).
    pub fn effort(self) -> Option<&'static str> {
        match self {
            ReasoningHint::Off => None,
            ReasoningHint::Low => Some("low"),
            ReasoningHint::Medium => Some("medium"),
            ReasoningHint::High => Some("high"),
        }
    }
}

/// A normalized chat completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Fully-qualified model id: `"anthropic/claude-sonnet-5"`,
    /// `"deepseek/deepseek-v3.2"`. Providers strip their own prefix; a bare
    /// id is passed through unchanged.
    pub model: String,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: ToolChoice,
    pub max_tokens: u32,
    /// Optional JSON Schema for structured output.
    pub response_format: Option<serde_json::Value>,
    pub reasoning: ReasoningHint,
    pub temperature: Option<f32>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: 4096,
            response_format: None,
            reasoning: ReasoningHint::Off,
            temperature: None,
        }
    }

    /// CJK-aware input-token estimate (system + messages + tool schemas).
    ///
    /// Heuristic mirroring the gateway's session-manager estimation: a CJK
    /// codepoint ≈ 1 token, other text ≈ 4 chars/token. Used by the router
    /// for context-window-aware candidate filtering — deliberately
    /// over-estimates slightly rather than under (fail-closed for windows).
    pub fn estimate_input_tokens(&self) -> u64 {
        let mut total = 0u64;
        for block in &self.system {
            total += estimate_tokens(&block.text);
        }
        for msg in &self.messages {
            for part in &msg.parts {
                total += match part {
                    ContentPart::Text(t) => estimate_tokens(t),
                    // ~1.37 tokens/byte for base64 image data is wrong in
                    // both directions per provider; use a flat conservative
                    // charge per image (Anthropic bills ≈ (w*h)/750).
                    ContentPart::Image { .. } => 1_600,
                    ContentPart::ToolCall { args, .. } => estimate_tokens(&args.to_string()) + 16,
                    ContentPart::ToolResult { content, .. } => estimate_tokens(content) + 16,
                    ContentPart::Reasoning { text, .. } => estimate_tokens(text),
                };
            }
        }
        for tool in &self.tools {
            total += estimate_tokens(&tool.description) + estimate_tokens(&tool.input_schema.to_string()) + 8;
        }
        total
    }
}

/// CJK-aware token estimation for a text fragment.
pub fn estimate_tokens(text: &str) -> u64 {
    let mut cjk = 0u64;
    let mut other = 0u64;
    for ch in text.chars() {
        let c = ch as u32;
        // CJK Unified Ideographs + extensions A, Hiragana/Katakana, Hangul,
        // full-width forms — each ≈ 1 token.
        let is_cjk = matches!(c,
            0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF);
        if is_cjk {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// Why generation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    ContentFilter,
    Refusal,
    Other(String),
}

/// Provider-normalized token usage.
///
/// `reasoning_tokens` is a *subset marker* (OpenAI reports reasoning tokens
/// inside `output_tokens`; Gemini reports them separately) — cost math bills
/// reasoning tokens at the output rate, on top of `output_tokens`, only when
/// the provider excludes them from `output_tokens` (Gemini). Providers set
/// the fields so that `output_tokens + reasoning_tokens` is always the total
/// billable output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NormalizedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

impl NormalizedUsage {
    /// Fraction of the prompt served from cache (mirrors CostTelemetry).
    pub fn cache_efficiency(&self) -> f64 {
        let total = self.input_tokens + self.cache_read_tokens + self.cache_write_tokens;
        if total == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f64 / total as f64
    }
}

/// A normalized chat completion response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub parts: Vec<ContentPart>,
    pub stop: StopReason,
    pub usage: NormalizedUsage,
    /// Bare model id the provider reports having used.
    pub model_used: String,
    /// Provider id, e.g. `"anthropic"`, `"deepseek"`.
    pub provider: String,
}

impl ChatResponse {
    /// Concatenated text of all `Text` parts.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// All tool calls in this response.
    pub fn tool_calls(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolCall { id, name, args } => Some((id.as_str(), name.as_str(), args)),
                _ => None,
            })
            .collect()
    }
}

/// Streaming event. Streams always terminate with exactly one
/// [`StreamEvent::Done`] carrying the fully-accumulated response.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart { index: usize, id: String, name: String },
    ToolCallDelta { index: usize, args_fragment: String },
    Done(ChatResponse),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_is_cjk_aware() {
        // 8 ASCII chars → 2 tokens; 4 CJK chars → 4 tokens.
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("嘟嘟爪好"), 4);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn request_estimate_covers_system_and_messages() {
        let mut req = ChatRequest::new("anthropic/claude-haiku-4-5");
        req.system.push(SystemBlock::cached("a".repeat(400)));
        req.messages.push(ChatMessage::user("b".repeat(40)));
        // 400/4 + 40/4 = 110
        assert_eq!(req.estimate_input_tokens(), 110);
    }

    #[test]
    fn response_text_and_tool_calls_accessors() {
        let resp = ChatResponse {
            parts: vec![
                ContentPart::Text("hello ".into()),
                ContentPart::ToolCall {
                    id: "t1".into(),
                    name: "search".into(),
                    args: serde_json::json!({"q": "rust"}),
                },
                ContentPart::Text("world".into()),
            ],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "p".into(),
        };
        assert_eq!(resp.text(), "hello world");
        let calls = resp.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "search");
    }

    #[test]
    fn cache_efficiency_math() {
        let u = NormalizedUsage {
            input_tokens: 100,
            cache_read_tokens: 900,
            ..Default::default()
        };
        assert!((u.cache_efficiency() - 0.9).abs() < 1e-9);
        assert_eq!(NormalizedUsage::default().cache_efficiency(), 0.0);
    }
}
