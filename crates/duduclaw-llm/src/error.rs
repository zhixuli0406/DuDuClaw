//! Classified LLM errors — aligned with the gateway's `FailureReason`
//! semantics (RateLimited / Billing / Auth / Timeout / ...), but structured
//! instead of string-matched, so the router and (later) the account rotator
//! can branch without re-parsing provider error text.

use std::time::Duration;

use duduclaw_core::truncate_bytes;

/// Max bytes of provider error body kept in an error snippet (CJK-safe).
const SNIPPET_MAX_BYTES: usize = 300;

/// Classified LLM error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// 429 / rate-limit / overloaded-with-Retry-After.
    #[error("rate limited (retry_after={retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },

    /// Billing / credit exhausted (402, `insufficient_quota`).
    #[error("billing / credit exhausted")]
    Billing,

    /// Invalid or missing API key (401/403).
    #[error("authentication failed")]
    Auth,

    /// Request timed out client-side.
    #[error("request timed out")]
    Timeout,

    /// Prompt exceeds the model's context window.
    #[error("context window exceeded")]
    ContextWindowExceeded,

    /// Provider refused the content (safety filter).
    #[error("content filtered by provider")]
    ContentFilter,

    /// Malformed request (our bug or unsupported feature) — not retryable.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Unclassified HTTP error.
    #[error("http {status}: {body_snippet}")]
    Http { status: u16, body_snippet: String },

    /// Connection / DNS / TLS failure.
    #[error("network error: {0}")]
    Network(String),

    /// Response body did not parse as the expected shape.
    #[error("parse error: {0}")]
    Parse(String),
}

impl LlmError {
    /// Same provider+model may succeed if retried after a delay.
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::RateLimited { .. } | LlmError::Timeout | LlmError::Network(_) => true,
            LlmError::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }

    /// A different model/provider in the fallback chain is worth trying.
    ///
    /// Includes everything retryable plus errors that are specific to this
    /// provider/account/model: `Billing` (this account is out of credit),
    /// `Auth` (this key is bad), `ContextWindowExceeded` (a bigger-window
    /// candidate may fit). `InvalidRequest` / `ContentFilter` / `Parse` are
    /// NOT failover — they would reproduce or must surface to the caller.
    pub fn is_failover(&self) -> bool {
        self.is_retryable()
            || matches!(
                self,
                LlmError::Billing | LlmError::Auth | LlmError::ContextWindowExceeded
            )
    }
}

/// Truncate an error body for embedding in an [`LlmError`] (CJK-safe — never
/// slices mid-codepoint, per project convention).
pub fn snippet(body: &str) -> String {
    truncate_bytes(body, SNIPPET_MAX_BYTES).to_string()
}

/// Classify a non-2xx HTTP response into an [`LlmError`].
///
/// Shared by all providers; provider modules may pre-classify from their
/// native error `type` field and fall back to this. Follows the gateway's
/// `classify_cli_failure` keyword sets so both layers agree on what counts
/// as billing vs. rate limiting. Unknown statuses map to `Http` (fail
/// closed: `Http{4xx}` is neither retryable nor failover).
pub fn classify_http(status: u16, body: &str, retry_after: Option<Duration>) -> LlmError {
    let lower = body.to_lowercase();

    // Billing signals first: OpenAI reports quota exhaustion as HTTP 429
    // with `insufficient_quota`, which must NOT be treated as a rate limit
    // (2-minute cooldown) but as billing (24h cooldown).
    if status == 402
        || lower.contains("insufficient_quota")
        || lower.contains("insufficient balance")
        || lower.contains("billing_not_active")
        || lower.contains("credit balance is too low")
    {
        return LlmError::Billing;
    }
    if status == 429 || lower.contains("rate_limit") || lower.contains("rate limit") {
        return LlmError::RateLimited { retry_after };
    }
    if status == 401 || status == 403 {
        return LlmError::Auth;
    }
    if status == 408 || lower.contains("timeout") && status < 500 {
        return LlmError::Timeout;
    }
    if is_context_window_message(&lower) {
        return LlmError::ContextWindowExceeded;
    }
    if status == 400 || status == 404 || status == 422 {
        return LlmError::InvalidRequest(snippet(body));
    }
    // 529 = Anthropic "overloaded" — retryable, keep as Http 5xx-class.
    LlmError::Http { status, body_snippet: snippet(body) }
}

/// Provider-agnostic detection of "prompt too long" error messages.
///
/// Covers: Anthropic `prompt is too long`, OpenAI `context_length_exceeded`
/// / `maximum context length`, Gemini `exceeds the maximum number of
/// tokens`, DeepSeek/vLLM `maximum context length`.
fn is_context_window_message(lower_body: &str) -> bool {
    lower_body.contains("context_length_exceeded")
        || lower_body.contains("maximum context length")
        || lower_body.contains("prompt is too long")
        || lower_body.contains("input is too long")
        || lower_body.contains("exceeds the maximum number of tokens")
        || lower_body.contains("context window")
}

/// Map a `reqwest` transport error (no HTTP status available).
pub fn classify_transport(err: &reqwest::Error) -> LlmError {
    if err.is_timeout() {
        LlmError::Timeout
    } else {
        LlmError::Network(snippet(&err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_429_is_rate_limited_with_retry_after() {
        let e = classify_http(429, r#"{"error":{"type":"rate_limit_error"}}"#, Some(Duration::from_secs(30)));
        assert_eq!(e, LlmError::RateLimited { retry_after: Some(Duration::from_secs(30)) });
        assert!(e.is_retryable());
        assert!(e.is_failover());
    }

    #[test]
    fn classify_429_insufficient_quota_is_billing_not_rate_limit() {
        // OpenAI quota exhaustion arrives as 429 + insufficient_quota.
        let e = classify_http(429, r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#, None);
        assert_eq!(e, LlmError::Billing);
        assert!(!e.is_retryable());
        assert!(e.is_failover());
    }

    #[test]
    fn classify_402_is_billing() {
        assert_eq!(classify_http(402, "Payment Required", None), LlmError::Billing);
    }

    #[test]
    fn classify_401_403_are_auth() {
        assert_eq!(classify_http(401, r#"{"error":{"type":"authentication_error"}}"#, None), LlmError::Auth);
        assert_eq!(classify_http(403, "forbidden", None), LlmError::Auth);
        assert!(LlmError::Auth.is_failover());
        assert!(!LlmError::Auth.is_retryable());
    }

    #[test]
    fn classify_context_window_messages_across_providers() {
        // Anthropic
        let e = classify_http(400, r#"{"error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#, None);
        assert_eq!(e, LlmError::ContextWindowExceeded);
        // OpenAI
        let e = classify_http(400, r#"{"error":{"code":"context_length_exceeded","message":"This model's maximum context length is 400000 tokens"}}"#, None);
        assert_eq!(e, LlmError::ContextWindowExceeded);
        // Gemini
        let e = classify_http(400, r#"{"error":{"message":"The input token count exceeds the maximum number of tokens allowed"}}"#, None);
        assert_eq!(e, LlmError::ContextWindowExceeded);
        assert!(e.is_failover());
    }

    #[test]
    fn classify_400_without_context_keywords_is_invalid_request() {
        let e = classify_http(400, r#"{"error":{"message":"missing field model"}}"#, None);
        assert!(matches!(e, LlmError::InvalidRequest(_)));
        assert!(!e.is_failover());
    }

    #[test]
    fn classify_5xx_is_retryable_http() {
        let e = classify_http(529, r#"{"error":{"type":"overloaded_error"}}"#, None);
        match &e {
            LlmError::Http { status, .. } => assert_eq!(*status, 529),
            other => panic!("expected Http, got {other:?}"),
        }
        assert!(e.is_retryable());
        assert!(e.is_failover());
    }

    #[test]
    fn snippet_is_cjk_safe() {
        // 150 CJK chars = 450 bytes; must not panic and must not cut mid-char.
        let body = "錯".repeat(150);
        let s = snippet(&body);
        assert!(s.len() <= 300);
        assert!(s.chars().all(|c| c == '錯'));
    }

    #[test]
    fn content_filter_and_parse_are_terminal() {
        assert!(!LlmError::ContentFilter.is_failover());
        assert!(!LlmError::Parse("x".into()).is_failover());
    }
}
