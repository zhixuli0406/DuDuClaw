//! The typed failure kind a runtime had in hand when it failed (v1.84
//! security review L2), carried beside the error text to the fallback rule.
//!
//! `AgentRuntime::execute` returns `Result<_, String>`, and a provider error
//! string embeds provider text, which can echo text an attacker chose (a
//! prompt, a file name, a tool result). Deciding fallback eligibility by
//! searching that whole string let an echoed "invalid model" turn a refused
//! request rejection into an eligible one, and an echoed "content policy
//! violation" suppress an eligible fallback.
//!
//! A runtime that knows WHAT failed — the HTTP status of the response, the
//! provider's own error identifier (`error.code` / `error.type`, matched
//! exactly, never the message), a duduclaw-llm [`duduclaw_llm::LlmError`]
//! variant — records that kind with [`typed`], bound to the exact error string
//! it returns. The failover manager runs the primary call inside [`capture`]
//! and hands the kind to the rule only when the recorded string is the error
//! it received ([`kind_for`]), so a wrapped or different error never borrows
//! a stale kind. Without a kind the rule matches only the anchored message
//! shapes DuDuClaw's own code formats.

use std::sync::{Arc, Mutex};

/// What a runtime knew about its failure. See [`super::fallback_rule`] for
/// the decision each kind gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// HTTP 401 / 403; `LlmError::Auth`.
    Auth,
    /// HTTP 429; `LlmError::RateLimited`.
    RateLimited,
    /// HTTP 5xx (including 529).
    Server,
    /// The provider's error code `model_not_found`.
    ModelUnavailable,
    /// HTTP 400 / 404 / 422 without a typed provider code; `LlmError::InvalidRequest`.
    RequestRejected,
    /// Provider code `content_policy_violation` / `content_filter`; `LlmError::ContentFilter`.
    ContentPolicy,
    /// Provider code `billing_hard_limit_reached`.
    BillingHardLimit,
    /// HTTP 402, provider code `insufficient_quota`; `LlmError::Billing`.
    Billing,
    /// `LlmError::ContextWindowExceeded`.
    ContextWindow,
    /// No response: connect / DNS / TLS failure or a client-side timeout.
    Network,
    /// The response arrived but could not be parsed.
    Parse,
    /// Any other HTTP status.
    OtherStatus(u16),
}

type Recorded = Option<(FailureKind, String)>;

tokio::task_local! {
    static RECORDED: Arc<Mutex<Recorded>>;
}

/// Record `kind` for `message` (the exact error string about to be returned)
/// and hand the message back. Outside a [`capture`] scope this is a no-op.
pub(crate) fn typed(kind: FailureKind, message: String) -> String {
    let _ = RECORDED.try_with(|slot| {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((kind, message.clone()));
    });
    message
}

/// Run one runtime call and return what it recorded last.
pub(crate) async fn capture<F: std::future::Future>(future: F) -> (F::Output, Recorded) {
    let slot = Arc::new(Mutex::new(None));
    let output = RECORDED.scope(slot.clone(), future).await;
    let recorded = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    (output, recorded)
}

/// The recorded kind, only when it was recorded for exactly `error`.
pub(crate) fn kind_for(recorded: Option<&(FailureKind, String)>, error: &str) -> Option<FailureKind> {
    recorded.filter(|(_, text)| text == error).map(|(kind, _)| *kind)
}

/// Kind of a non-success HTTP response: the provider's typed error code for a
/// 4xx when it has one, otherwise the status. The body's free text is never
/// searched.
pub(crate) fn from_http(status: u16, body: &str) -> FailureKind {
    if (400..500).contains(&status) {
        if let Some(kind) = provider_code(body) {
            return kind;
        }
    }
    match status {
        401 | 403 => FailureKind::Auth,
        402 => FailureKind::Billing,
        429 => FailureKind::RateLimited,
        400 | 404 | 422 => FailureKind::RequestRejected,
        500..=599 => FailureKind::Server,
        other => FailureKind::OtherStatus(other),
    }
}

/// The provider's own error identifier (`{"error":{"code"|"type": …}}`),
/// matched exactly. `None` when the body is not that JSON shape or carries no
/// identifier this table knows.
pub(crate) fn provider_code(body: &str) -> Option<FailureKind> {
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let error = value.get("error")?;
    ["code", "type"]
        .iter()
        .filter_map(|key| error.get(*key).and_then(|v| v.as_str()))
        .find_map(|id| match id.to_ascii_lowercase().as_str() {
            "model_not_found" => Some(FailureKind::ModelUnavailable),
            "content_policy_violation" | "content_filter" => Some(FailureKind::ContentPolicy),
            "billing_hard_limit_reached" => Some(FailureKind::BillingHardLimit),
            "insufficient_quota" => Some(FailureKind::Billing),
            _ => None,
        })
}

/// Kind of a duduclaw-llm error. `None` for `MaxTurns`, which the runtime
/// already reports as a typed work result.
pub(crate) fn from_llm(error: &duduclaw_llm::LlmError) -> Option<FailureKind> {
    use duduclaw_llm::LlmError as E;
    Some(match error {
        E::Auth => FailureKind::Auth,
        E::RateLimited { .. } => FailureKind::RateLimited,
        E::Billing => FailureKind::Billing,
        E::Timeout | E::Network(_) => FailureKind::Network,
        E::ContextWindowExceeded => FailureKind::ContextWindow,
        E::ContentFilter => FailureKind::ContentPolicy,
        E::InvalidRequest(snippet) => provider_code(snippet).unwrap_or(FailureKind::RequestRejected),
        E::Http { status, body_snippet } => from_http(*status, body_snippet),
        E::Parse(_) => FailureKind::Parse,
        E::MaxTurns { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_kind_uses_status_and_typed_codes_never_message_text() {
        let echo = r#"{"error":{"message":"invalid model; 401 ; content policy violation; billing_hard_limit","type":"invalid_request_error"}}"#;
        assert_eq!(from_http(400, echo), FailureKind::RequestRejected);
        assert_eq!(from_http(500, echo), FailureKind::Server);
        assert_eq!(from_http(429, "billing_hard_limit_reached"), FailureKind::RateLimited);
        assert_eq!(from_http(404, r#"{"error":{"message":"x","code":"model_not_found"}}"#), FailureKind::ModelUnavailable);
        assert_eq!(from_http(400, r#"{"error":{"code":"content_policy_violation"}}"#), FailureKind::ContentPolicy);
        assert_eq!(from_http(400, r#"{"error":{"code":"billing_hard_limit_reached"}}"#), FailureKind::BillingHardLimit);
        assert_eq!(from_http(429, r#"{"error":{"code":"insufficient_quota"}}"#), FailureKind::Billing);
        // A 5xx keeps its status whatever code it carries.
        assert_eq!(from_http(503, r#"{"error":{"code":"content_filter"}}"#), FailureKind::Server);
        assert_eq!(from_http(401, "x"), FailureKind::Auth);
        assert_eq!(from_http(409, "x"), FailureKind::OtherStatus(409));
    }

    #[test]
    fn llm_errors_map_by_variant() {
        use duduclaw_llm::LlmError as E;
        assert_eq!(from_llm(&E::InvalidRequest("invalid model 401 content policy violation".into())), Some(FailureKind::RequestRejected));
        assert_eq!(from_llm(&E::InvalidRequest(r#"{"error":{"code":"model_not_found"}}"#.into())), Some(FailureKind::ModelUnavailable));
        assert_eq!(from_llm(&E::Http { status: 502, body_snippet: "billing_hard_limit".into() }), Some(FailureKind::Server));
        assert_eq!(from_llm(&E::ContentFilter), Some(FailureKind::ContentPolicy));
        assert_eq!(from_llm(&E::Auth), Some(FailureKind::Auth));
        assert_eq!(from_llm(&E::MaxTurns { limit: 3 }), None);
    }

    #[tokio::test]
    async fn a_kind_binds_to_its_exact_error_string() {
        let (out, recorded) = capture(async { Err::<(), _>(typed(FailureKind::Auth, "API error (401): x".into())) }).await;
        let error = out.unwrap_err();
        assert_eq!(kind_for(recorded.as_ref(), &error), Some(FailureKind::Auth));
        assert_eq!(kind_for(recorded.as_ref(), "wrapped: API error (401): x"), None);
        // Outside a scope nothing is recorded and nothing panics.
        assert_eq!(typed(FailureKind::Server, "e".into()), "e");
        let (_, nothing) = capture(async {}).await;
        assert!(nothing.is_none());
    }
}
