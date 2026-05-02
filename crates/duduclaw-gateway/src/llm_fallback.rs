//! LLM API timeout/overload automatic model fallback helpers.
//!
//! When the primary model (e.g. `claude-sonnet-4-5`) fails with a transient
//! infrastructure error — hard timeout, HTTP 503, 429 rate-limit, or "overloaded"
//! — the gateway automatically retries with the agent's configured `fallback`
//! model (default `claude-haiku-4-5`).
//!
//! This module provides three pure helper functions used by `claude_runner.rs`:
//!
//! - [`is_llm_fallback_error`] — classifies an error string as a fallback trigger.
//! - [`should_attempt_model_fallback`] — guards against same-model retry loops.
//! - [`format_fallback_error_message`] — builds a combined error when both fail.

use std::path::Path;

use tracing::warn;

/// Determine whether an error string should trigger an LLM model-level fallback.
///
/// **Input contract**: this function is only called with error strings produced
/// by DuDuClaw internals (HTTP client errors, CLI subprocess errors, account
/// rotator aggregated errors). It is **never** called with user-supplied input
/// or model response text.
///
/// Triggers on transient infrastructure errors that a different (lighter) model
/// might avoid:
/// - Hard timeout from the CLI subprocess
/// - HTTP 503 / "Service Unavailable" (must appear as a phrase or HTTP status context)
/// - HTTP 429 / rate-limit (any spelling)
/// - "Overloaded" (Anthropic API capacity error)
///
/// Intentionally does **not** trigger on:
/// - Billing/credit errors (a lighter model won't help)
/// - Auth errors
/// - Plain 500 / generic 5xx outside the above set
/// - Network errors (connection refused, DNS, etc.)
///
/// Note: there is deliberate overlap with `is_rate_limit_error` —
/// 429 and "overloaded" trigger both functions so that the account rotator
/// AND the model fallback both fire when appropriate.
pub fn is_llm_fallback_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    // Hard timeout produced by call_claude_streaming
    if lower.contains("hard timeout") {
        return true;
    }
    // HTTP 503: require contextual keyword to avoid matching "5030" or "abc503"
    if lower.contains("service unavailable")
        || lower.contains("http 503")
        || lower.contains("http/503")
        || lower.contains("status 503")
        || lower.contains("status: 503")
    {
        return true;
    }
    // HTTP 429: require contextual keyword or rate-limit phrases
    if lower.contains("http 429")
        || lower.contains("http/429")
        || lower.contains("status 429")
        || lower.contains("status: 429")
        || lower.contains("rate limit")
        || lower.contains("rate-limit")
        || lower.contains("ratelimit")
    {
        return true;
    }
    // Anthropic API capacity error
    if lower.contains("overloaded") {
        return true;
    }
    false
}

/// Whether a model-level fallback attempt should be made.
///
/// Returns `true` only when:
/// 1. Both `primary` and `fallback` are non-empty, non-whitespace strings.
/// 2. The two model names are **different** after trimming — prevents infinite
///    retry loops where the primary and fallback happen to be the same model.
pub fn should_attempt_model_fallback(primary: &str, fallback: &str) -> bool {
    let primary = primary.trim();
    let fallback = fallback.trim();
    !fallback.is_empty() && !primary.is_empty() && fallback != primary
}

/// Format a combined error message when both the primary model and its fallback
/// have failed.
///
/// The result is a single human-readable string that names both models and
/// both errors so operators can diagnose each failure independently.
///
/// # Example
///
/// ```
/// use duduclaw_gateway::llm_fallback::format_fallback_error_message;
///
/// let msg = format_fallback_error_message(
///     "claude-sonnet-4-5",
///     "hard timeout (1800s)",
///     "claude-haiku-4-5",
///     "503 Service Unavailable",
/// );
/// assert!(msg.contains("claude-sonnet-4-5"));
/// assert!(msg.contains("claude-haiku-4-5"));
/// ```
pub fn format_fallback_error_message(
    primary_model: &str,
    primary_err: &str,
    fallback_model: &str,
    fallback_err: &str,
) -> String {
    format!(
        "Primary model ({primary_model}) failed: {primary_err}; \
         Fallback model ({fallback_model}) also failed: {fallback_err}"
    )
}

/// Maximum length (bytes) for `trigger_error` written to the audit log.
///
/// Truncation prevents adversarial or runaway API responses from flooding
/// `security_audit.jsonl` with unbounded data. The remaining suffix is
/// replaced with `…[truncated]` to preserve diagnosability.
const MAX_TRIGGER_ERROR_LOG_BYTES: usize = 512;

/// Emit a `llm_fallback_triggered` security audit event (best-effort).
///
/// Uses `tokio::task::spawn_blocking` to perform the blocking file I/O on the
/// Tokio blocking thread pool. The caller **awaits** completion so the audit
/// event is durably written before the fallback call begins — this is the
/// intentional design. If the spawn panics (e.g., due to an OOM condition in
/// the blocking thread), the JoinError is logged as WARN and swallowed; the
/// audit event may be lost but the fallback call proceeds normally.
///
/// `trigger_error` is truncated to [`MAX_TRIGGER_ERROR_LOG_BYTES`] bytes to
/// prevent log amplification attacks via oversized API error responses.
pub async fn emit_llm_fallback_audit(
    home_dir: &Path,
    agent_id: &str,
    primary_model: &str,
    fallback_model: &str,
    trigger_error: &str,
) {
    let home = home_dir.to_path_buf();
    let agent_id = agent_id.to_string();
    let primary = primary_model.to_string();
    let fallback = fallback_model.to_string();
    // Truncate to prevent unbounded log growth from adversarial API responses.
    let error = {
        let bytes = MAX_TRIGGER_ERROR_LOG_BYTES;
        if trigger_error.len() > bytes {
            // Find safe UTF-8 char boundary
            let safe_end = trigger_error
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= bytes)
                .last()
                .unwrap_or(0);
            format!("{}…[truncated]", &trigger_error[..safe_end])
        } else {
            trigger_error.to_string()
        }
    };

    if let Err(e) = tokio::task::spawn_blocking(move || {
        let event = duduclaw_security::audit::AuditEvent::new(
            "llm_fallback_triggered",
            &agent_id,
            duduclaw_security::audit::Severity::Warning,
            serde_json::json!({
                "primary_model": primary,
                "fallback_model": fallback,
                "trigger_error": error,
            }),
        );
        duduclaw_security::audit::append_audit_event(&home, &event);
    })
    .await
    {
        // JoinError means the blocking task panicked — audit event is lost.
        // Log as WARN and continue (best-effort guarantee: never block the caller).
        warn!("llm_fallback audit spawn_blocking panicked: {e}");
    }
}
