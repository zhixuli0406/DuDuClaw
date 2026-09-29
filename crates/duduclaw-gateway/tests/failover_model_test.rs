//! TDD tests for layer 2 of the failover stack — model-level
//! timeout/overload automatic fallback (`failover::model`, formerly the
//! top-level `llm_fallback` module; O1, 2026-09-29).
//!
//! Tests cover:
//! - `is_llm_fallback_error()`: recognises all trigger error strings
//! - `should_attempt_model_fallback()`: guards against same-model loops
//! - `format_fallback_error_message()`: combined error message formatting
//! - `FailoverManager::model_fallback_for()`: the combined decision the
//!   `claude_runner` call sites make (regression guard for the O1 merge —
//!   it must agree with the two predicates on every input).
//!
//! Run with: cargo test -p duduclaw-gateway --test failover_model_test

use duduclaw_gateway::failover::FailoverManager;
use duduclaw_gateway::failover::model::{
    format_fallback_error_message, is_llm_fallback_error, should_attempt_model_fallback,
};

// ── is_llm_fallback_error ─────────────────────────────────────────────────────

#[test]
fn fallback_error_hard_timeout() {
    assert!(
        is_llm_fallback_error("claude CLI hard timeout (1800s, no output)"),
        "hard timeout must trigger fallback"
    );
}

#[test]
fn fallback_error_hard_timeout_case_insensitive() {
    assert!(
        is_llm_fallback_error("HARD TIMEOUT occurred"),
        "case-insensitive match required"
    );
}

#[test]
fn fallback_error_503_service_unavailable() {
    assert!(
        is_llm_fallback_error("HTTP 503 Service Unavailable"),
        "503 must trigger fallback"
    );
}

#[test]
fn fallback_error_status_503() {
    assert!(
        is_llm_fallback_error("status 503 from Anthropic"),
        "status 503 phrase must trigger fallback"
    );
}

#[test]
fn fallback_error_service_unavailable_phrase() {
    assert!(
        is_llm_fallback_error("service unavailable at this time"),
        "service unavailable phrase must trigger fallback"
    );
}

#[test]
fn fallback_error_overloaded() {
    assert!(
        is_llm_fallback_error("Anthropic API is overloaded"),
        "overloaded must trigger fallback"
    );
}

#[test]
fn fallback_error_429() {
    assert!(
        is_llm_fallback_error("HTTP 429 Too Many Requests"),
        "429 must trigger fallback"
    );
}

#[test]
fn fallback_error_rate_limit_space() {
    assert!(
        is_llm_fallback_error("rate limit exceeded"),
        "rate limit (space) must trigger fallback"
    );
}

#[test]
fn fallback_error_rate_limit_hyphen() {
    assert!(
        is_llm_fallback_error("rate-limit hit"),
        "rate-limit (hyphen) must trigger fallback"
    );
}

#[test]
fn fallback_error_ratelimit_no_separator() {
    assert!(
        is_llm_fallback_error("ratelimit reached"),
        "ratelimit (no separator) must trigger fallback"
    );
}

#[test]
fn fallback_error_normal_error_does_not_trigger() {
    assert!(
        !is_llm_fallback_error("Internal Server Error 500"),
        "plain 500 must NOT trigger model fallback"
    );
}

#[test]
fn fallback_error_network_error_does_not_trigger() {
    assert!(
        !is_llm_fallback_error("Connection refused"),
        "connection refused must NOT trigger model fallback"
    );
}

#[test]
fn fallback_error_empty_string_does_not_trigger() {
    assert!(
        !is_llm_fallback_error(""),
        "empty error string must not trigger fallback"
    );
}

#[test]
fn fallback_error_unrelated_does_not_trigger() {
    assert!(
        !is_llm_fallback_error("Agent 'coder' not found in registry"),
        "unrelated error must not trigger fallback"
    );
}

#[test]
fn fallback_error_billing_error_does_not_trigger() {
    // Billing exhaustion is a separate error class — model fallback won't help.
    assert!(
        !is_llm_fallback_error("credit balance insufficient"),
        "billing error must NOT trigger model fallback"
    );
}

// Additional edge-case: bare "503" or "429" without HTTP context must NOT trigger
// (avoids matching "request_id: abc503xyz" or other incidental digit sequences)

#[test]
fn fallback_error_bare_503_without_context_does_not_trigger() {
    // A log message containing "503" as part of something unrelated
    assert!(
        !is_llm_fallback_error("error code 5030: invalid parameter"),
        "5030 must NOT trigger (not an HTTP 503)"
    );
}

#[test]
fn fallback_error_http_503_with_context_triggers() {
    assert!(
        is_llm_fallback_error("http 503 returned by upstream"),
        "http 503 phrase must trigger fallback"
    );
}

#[test]
fn fallback_error_bare_429_without_context_does_not_trigger() {
    assert!(
        !is_llm_fallback_error("processed 4290 tokens successfully"),
        "4290 must NOT trigger (not an HTTP 429)"
    );
}

#[test]
fn fallback_error_http_429_with_context_triggers() {
    assert!(
        is_llm_fallback_error("http 429 Too Many Requests"),
        "http 429 phrase must trigger fallback"
    );
}

// ── should_attempt_model_fallback ─────────────────────────────────────────────

#[test]
fn should_fallback_when_models_differ() {
    assert!(
        should_attempt_model_fallback("claude-sonnet-4-5", "claude-haiku-4-5"),
        "different models should attempt fallback"
    );
}

#[test]
fn should_not_fallback_when_models_same() {
    assert!(
        !should_attempt_model_fallback("claude-haiku-4-5", "claude-haiku-4-5"),
        "same model must NOT attempt fallback (avoids infinite loop)"
    );
}

#[test]
fn should_not_fallback_when_fallback_empty() {
    assert!(
        !should_attempt_model_fallback("claude-sonnet-4-5", ""),
        "empty fallback model must not attempt fallback"
    );
}

#[test]
fn should_not_fallback_when_primary_empty() {
    assert!(
        !should_attempt_model_fallback("", "claude-haiku-4-5"),
        "empty primary model must not attempt fallback"
    );
}

#[test]
fn should_not_fallback_when_both_empty() {
    assert!(
        !should_attempt_model_fallback("", ""),
        "both empty must not attempt fallback"
    );
}

#[test]
fn should_fallback_with_opus_to_haiku() {
    // Typical production scenario: heavy model → light model
    assert!(
        should_attempt_model_fallback("claude-opus-4-5", "claude-haiku-4-5"),
        "opus → haiku fallback should be attempted"
    );
}

#[test]
fn should_fallback_with_any_distinct_strings() {
    // Model names are opaque strings — any two distinct non-empty values qualify.
    assert!(
        should_attempt_model_fallback("model-a", "model-b"),
        "any two distinct non-empty model names should qualify"
    );
}

#[test]
fn should_not_fallback_when_fallback_whitespace_only() {
    assert!(
        !should_attempt_model_fallback("claude-sonnet-4-5", "   "),
        "whitespace-only fallback must not attempt fallback"
    );
}

#[test]
fn should_not_fallback_when_primary_whitespace_only() {
    assert!(
        !should_attempt_model_fallback("   ", "claude-haiku-4-5"),
        "whitespace-only primary must not attempt fallback"
    );
}

// ── format_fallback_error_message ─────────────────────────────────────────────

#[test]
fn format_error_contains_primary_model() {
    let msg = format_fallback_error_message(
        "claude-sonnet-4-5",
        "timeout",
        "claude-haiku-4-5",
        "also failed",
    );
    assert!(
        msg.contains("claude-sonnet-4-5"),
        "output must name the primary model; got: {msg}"
    );
}

#[test]
fn format_error_contains_fallback_model() {
    let msg = format_fallback_error_message(
        "claude-sonnet-4-5",
        "timeout",
        "claude-haiku-4-5",
        "also failed",
    );
    assert!(
        msg.contains("claude-haiku-4-5"),
        "output must name the fallback model; got: {msg}"
    );
}

#[test]
fn format_error_contains_primary_error() {
    let msg = format_fallback_error_message(
        "model-a",
        "HTTP 429 rate limit",
        "model-b",
        "downstream error",
    );
    assert!(
        msg.contains("HTTP 429 rate limit"),
        "output must include primary error; got: {msg}"
    );
}

#[test]
fn format_error_contains_fallback_error() {
    let msg = format_fallback_error_message(
        "model-a",
        "timeout",
        "model-b",
        "503 Service Unavailable",
    );
    assert!(
        msg.contains("503 Service Unavailable"),
        "output must include fallback error; got: {msg}"
    );
}

#[test]
fn format_error_nonempty_for_all_empty_inputs() {
    // Even with all empty strings, the message should be non-empty and parseable.
    let msg = format_fallback_error_message("", "", "", "");
    assert!(!msg.is_empty(), "formatted error must never be empty");
}

#[test]
fn format_error_indicates_both_failed() {
    // The message must make it clear that both the primary AND the fallback failed.
    let msg = format_fallback_error_message(
        "claude-sonnet-4-5",
        "overloaded",
        "claude-haiku-4-5",
        "overloaded too",
    );
    // Must reference the fallback failure (not just the primary)
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("fallback") || lower.contains("also"),
        "output must communicate that fallback also failed; got: {msg}"
    );
}

#[test]
fn format_error_unicode_special_chars() {
    // Ensure the formatter handles Unicode and special chars without panicking.
    let msg = format_fallback_error_message(
        "模型-A",
        "錯誤: 逾時 🔥",
        "模型-B",
        "亦失敗: 過載",
    );
    assert!(msg.contains("模型-A"), "unicode primary model; got: {msg}");
    assert!(msg.contains("模型-B"), "unicode fallback model; got: {msg}");
}

// ── FailoverManager::model_fallback_for (O1 merge regression) ─────────────────

/// The facade must be exactly `is_llm_fallback_error && should_attempt_model_fallback`
/// on every combination — this is the guard that the O1 merge changed no behavior
/// at the three `claude_runner` call sites.
#[test]
fn model_fallback_for_agrees_with_the_two_predicates() {
    let errors = [
        "claude CLI hard timeout (1800s, no output)",
        "HTTP 503 Service Unavailable",
        "Anthropic API is overloaded",
        "http 429 Too Many Requests",
        "credit balance insufficient",
        "Connection refused",
        "error code 5030: invalid parameter",
        "",
    ];
    let pairs = [
        ("claude-sonnet-4-5", "claude-haiku-4-5"),
        ("claude-sonnet-4-5", "claude-sonnet-4-5"),
        ("claude-sonnet-4-5", "  claude-sonnet-4-5  "),
        ("claude-sonnet-4-5", ""),
        ("", "claude-haiku-4-5"),
        ("  ", "   "),
    ];
    for err in errors {
        for (primary, fallback) in pairs {
            let expected =
                is_llm_fallback_error(err) && should_attempt_model_fallback(primary, fallback);
            let actual = FailoverManager::model_fallback_for(primary, fallback, err);
            assert_eq!(
                actual.is_some(),
                expected,
                "model_fallback_for({primary:?}, {fallback:?}, {err:?}) diverged from the predicates"
            );
            if expected {
                assert_eq!(
                    actual,
                    Some(fallback),
                    "the facade must hand back the fallback model verbatim (untrimmed)"
                );
            }
        }
    }
}

/// A billing error must never reach layer 2 — a lighter model does not fix a
/// depleted balance, and retrying would burn a second call for nothing.
#[test]
fn model_fallback_for_refuses_billing_errors() {
    assert!(
        FailoverManager::model_fallback_for(
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "credit balance is too low"
        )
        .is_none()
    );
}

/// Same-model pairs must never loop, even on a genuine trigger error.
#[test]
fn model_fallback_for_refuses_same_model_loop() {
    assert!(
        FailoverManager::model_fallback_for(
            "claude-haiku-4-5",
            " claude-haiku-4-5 ",
            "Anthropic API is overloaded"
        )
        .is_none()
    );
}
