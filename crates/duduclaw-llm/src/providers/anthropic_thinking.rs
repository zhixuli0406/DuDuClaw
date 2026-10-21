//! Anthropic `thinking` / `output_config.effort` request shaping, per model
//! family.
//!
//! The Messages API stopped accepting one thinking shape for every model:
//!
//! | family | thinking when [`ReasoningHint`] is on | omitting `thinking` |
//! |---|---|---|
//! | [`ThinkingFamily::LegacyBudget`] (Claude 3.x, 4, 4.1, 4.5) | `{"type":"enabled","budget_tokens":N}` | no thinking |
//! | [`ThinkingFamily::AdaptiveOptIn`] (Opus 4.6/4.7/4.8, Sonnet 4.6, unknown ids) | `{"type":"adaptive"}` + `output_config.effort` | no thinking |
//! | [`ThinkingFamily::AdaptiveAlwaysOn`] (Opus 5/5.5, Sonnet 5/5.5, Haiku 5.5, Fable, Mythos) | `{"type":"adaptive"}` + `output_config.effort` | thinks anyway |
//!
//! `budget_tokens` is rejected with a 400 on every adaptive model from Opus
//! 4.7 on, and `{"type":"disabled"}` is a 400 on Opus 5.5 (and Sonnet 5.5) —
//! so this module never sends `disabled`: [`ReasoningHint::Off`] with no
//! display request means "no `thinking` key at all".
//!
//! `display` (see [`ThinkingDisplay`]) is only attached when the model will
//! think anyway: an always-on model, or a request that already turned
//! thinking on. Asking for a summary never turns thinking on for an opt-in
//! model (that would change what the turn costs). `display: "updates"` is a
//! beta that only a few models accept; everything else falls back to
//! `summarized`, and the caller adds [`THINKING_UPDATES_BETA`] only when the
//! body really carries `updates` (see [`thinking_beta_header`]).
//!
//! Model ids are classified by exact token sequence after normalisation
//! (provider prefix, `[1m]` suffix, date / `latest` / Bedrock `v1:0` tails
//! removed) — never by substring, so `claude-opus-5-5` is not mistaken for
//! `claude-opus-5`.

use serde_json::{Value, json};

use crate::types::{ChatRequest, ReasoningHint, ThinkingDisplay};

/// Beta header value for `thinking.display = "updates"`.
pub const THINKING_UPDATES_BETA: &str = "thinking-display-updates-2026-08-18";

/// How a model family takes its thinking configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingFamily {
    /// Pre-4.6 models: `enabled` + `budget_tokens`, no `display`.
    LegacyBudget,
    /// Adaptive models that do not think unless asked (and unknown ids).
    AdaptiveOptIn,
    /// Adaptive models that think on every request.
    AdaptiveAlwaysOn,
}

/// Lower-case `-`-separated tokens of the model name with the vendor prefix
/// and version tails removed: `"anthropic/claude-opus-4-5-20251101"` →
/// `["opus","4","5"]`.
fn model_tokens(model: &str) -> Vec<String> {
    let lower = model.to_ascii_lowercase();
    let bare = lower.rsplit('/').next().unwrap_or(&lower);
    // `[1m]` context suffix and Vertex `@date` snapshots.
    let bare = bare.split(['[', '@']).next().unwrap_or(bare);
    // Bedrock-style `us.anthropic.claude-…` / `anthropic.claude-…`.
    let bare = match bare.find("claude-") {
        Some(pos)
            if bare[..pos]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.') =>
        {
            &bare[pos + "claude-".len()..]
        }
        _ => bare,
    };
    bare.split('-')
        .take_while(|t| !is_version_tail(t))
        .map(str::to_string)
        .collect()
}

/// A token that ends the model name: a dated snapshot (`20251101`),
/// `latest`, or a Bedrock revision (`v1:0`).
fn is_version_tail(token: &str) -> bool {
    (token.len() == 8 && token.bytes().all(|b| b.is_ascii_digit()))
        || token == "latest"
        || token.contains(':')
        || token.is_empty()
}

/// Classify a model id (with or without the `anthropic/` prefix).
pub fn thinking_family(model: &str) -> ThinkingFamily {
    let tokens = model_tokens(model);
    let t: Vec<&str> = tokens.iter().map(String::as_str).collect();
    match t.as_slice() {
        ["3", ..] => ThinkingFamily::LegacyBudget,
        ["opus" | "sonnet", "4"]
        | ["opus" | "sonnet", "4", "0"]
        | ["opus", "4", "1"]
        | ["opus" | "sonnet" | "haiku", "4", "5"] => ThinkingFamily::LegacyBudget,
        ["opus", "4", "6" | "7" | "8"] | ["sonnet", "4", "6"] => ThinkingFamily::AdaptiveOptIn,
        ["opus" | "sonnet", "5"] | ["opus" | "sonnet" | "haiku", "5", "5"] => {
            ThinkingFamily::AdaptiveAlwaysOn
        }
        ["fable" | "mythos", ..] => ThinkingFamily::AdaptiveAlwaysOn,
        _ => ThinkingFamily::AdaptiveOptIn,
    }
}

/// Models that accept `thinking.display = "updates"` (beta).
pub fn supports_display_updates(model: &str) -> bool {
    let tokens = model_tokens(model);
    let t: Vec<&str> = tokens.iter().map(String::as_str).collect();
    matches!(
        t.as_slice(),
        ["fable", "5"] | ["fable", "5", "1"] | ["mythos", "5", "1"] | ["opus" | "sonnet", "5", "5"]
    )
}

/// The `display` wire value for this model (`updates` degrades to
/// `summarized` where the beta is not accepted).
fn display_value(display: ThinkingDisplay, model: &str) -> &'static str {
    match display {
        ThinkingDisplay::Updates if !supports_display_updates(model) => {
            ThinkingDisplay::Summarized.as_str()
        }
        other => other.as_str(),
    }
}

fn adaptive(display: Option<ThinkingDisplay>, model: &str) -> Value {
    match display {
        Some(d) => json!({"type": "adaptive", "display": display_value(d, model)}),
        None => json!({"type": "adaptive"}),
    }
}

/// Set `thinking` (and, for adaptive models, the hint-derived
/// `output_config.effort`) on `body`. An explicit
/// [`ChatRequest::reasoning_effort`] is applied by the caller and wins over
/// the hint-derived label.
pub(crate) fn apply_thinking(body: &mut Value, req: &ChatRequest, bare_model: &str) {
    let on = req.reasoning != ReasoningHint::Off;
    let family = thinking_family(bare_model);
    let thinking = match family {
        ThinkingFamily::LegacyBudget => req
            .reasoning
            .budget_tokens()
            .map(|budget| json!({"type": "enabled", "budget_tokens": budget})),
        ThinkingFamily::AdaptiveOptIn => on.then(|| adaptive(req.thinking_display, bare_model)),
        ThinkingFamily::AdaptiveAlwaysOn => (on || req.thinking_display.is_some())
            .then(|| adaptive(req.thinking_display, bare_model)),
    };
    if let Some(t) = thinking {
        body["thinking"] = t;
    }
    if family != ThinkingFamily::LegacyBudget
        && req.reasoning_effort.is_none()
        && let Some(effort) = req.reasoning.effort()
    {
        body["output_config"]["effort"] = json!(effort);
    }
}

/// The `anthropic-beta` value a built body needs, if any.
pub fn thinking_beta_header(body: &Value) -> Option<&'static str> {
    (body.pointer("/thinking/display").and_then(Value::as_str) == Some("updates"))
        .then_some(THINKING_UPDATES_BETA)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::anthropic::build_request_body;

    fn body(model: &str, hint: ReasoningHint, display: Option<ThinkingDisplay>) -> Value {
        let mut req = ChatRequest::new(model);
        req.reasoning = hint;
        req.thinking_display = display;
        build_request_body(&req, false)
    }

    #[test]
    fn classifies_by_exact_tokens() {
        use ThinkingFamily::*;
        for (id, want) in [
            ("claude-3-7-sonnet-latest", LegacyBudget),
            ("claude-3-5-haiku-20241022", LegacyBudget),
            ("claude-haiku-4-5", LegacyBudget),
            ("claude-sonnet-4-5-20250929", LegacyBudget),
            ("anthropic/claude-opus-4-5-20251101", LegacyBudget),
            ("claude-opus-4-1", LegacyBudget),
            ("claude-opus-4-0", LegacyBudget),
            ("claude-sonnet-4", LegacyBudget),
            ("claude-opus-4-6", AdaptiveOptIn),
            ("claude-sonnet-4-6[1m]", AdaptiveOptIn),
            ("claude-opus-4-7", AdaptiveOptIn),
            ("claude-opus-4-8", AdaptiveOptIn),
            ("claude-sonnet-5", AdaptiveAlwaysOn),
            ("claude-opus-5", AdaptiveAlwaysOn),
            ("claude-opus-5-5", AdaptiveAlwaysOn),
            ("claude-sonnet-5-5", AdaptiveAlwaysOn),
            ("claude-haiku-5-5", AdaptiveAlwaysOn),
            ("claude-fable-5-1", AdaptiveAlwaysOn),
            ("claude-mythos-5-1", AdaptiveAlwaysOn),
            ("us.anthropic.claude-opus-4-5-20251101-v1:0", LegacyBudget),
            ("claude-opus-4-5@20251101", LegacyBudget),
            // Unknown ids are treated as adaptive opt-in (never budget).
            ("claude-opus-9", AdaptiveOptIn),
            ("some-local-alias", AdaptiveOptIn),
        ] {
            assert_eq!(thinking_family(id), want, "{id}");
        }
        // Not substring matching: a 5.5 id never reads as 5 and vice versa.
        assert!(supports_display_updates("claude-opus-5-5"));
        assert!(!supports_display_updates("claude-opus-5"));
        assert!(!supports_display_updates("claude-haiku-5-5"));
        assert!(supports_display_updates("claude-fable-5"));
        assert!(supports_display_updates("anthropic/claude-sonnet-5-5"));
        assert!(!supports_display_updates("claude-mythos-5"));
    }

    #[test]
    fn legacy_keeps_budget_and_never_sends_effort_or_display() {
        let b = body(
            "claude-haiku-4-5",
            ReasoningHint::Medium,
            Some(ThinkingDisplay::Summarized),
        );
        assert_eq!(
            b["thinking"],
            json!({"type": "enabled", "budget_tokens": 8192})
        );
        assert!(b.get("output_config").is_none(), "{b}");
        // Display alone never turns thinking on for a legacy model.
        let b = body(
            "claude-sonnet-4-5",
            ReasoningHint::Off,
            Some(ThinkingDisplay::Summarized),
        );
        assert!(b.get("thinking").is_none(), "{b}");
    }

    #[test]
    fn adaptive_opt_in_uses_adaptive_plus_effort_and_no_budget() {
        let b = body("claude-opus-4-7", ReasoningHint::High, None);
        assert_eq!(b["thinking"], json!({"type": "adaptive"}));
        assert_eq!(b["output_config"]["effort"], "high");
        assert!(b.pointer("/thinking/budget_tokens").is_none());
        // Display alone does NOT open thinking on an opt-in model.
        let b = body(
            "claude-opus-4-8",
            ReasoningHint::Off,
            Some(ThinkingDisplay::Summarized),
        );
        assert!(b.get("thinking").is_none(), "{b}");
        assert!(b.get("output_config").is_none(), "{b}");
        // With thinking on, the display rides along.
        let b = body(
            "claude-sonnet-4-6",
            ReasoningHint::Low,
            Some(ThinkingDisplay::Summarized),
        );
        assert_eq!(
            b["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(b["output_config"]["effort"], "low");
    }

    #[test]
    fn explicit_effort_wins_over_the_hint() {
        let mut req = ChatRequest::new("claude-opus-4-7");
        req.reasoning = ReasoningHint::Low;
        req.reasoning_effort = Some("xhigh".into());
        let b = build_request_body(&req, false);
        assert_eq!(b["output_config"]["effort"], "xhigh");
        assert_eq!(b["thinking"], json!({"type": "adaptive"}));
    }

    #[test]
    fn always_on_models_get_display_without_turning_anything_on() {
        let b = body(
            "claude-opus-5",
            ReasoningHint::Off,
            Some(ThinkingDisplay::Summarized),
        );
        assert_eq!(
            b["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert!(b.get("output_config").is_none(), "{b}");
        assert_eq!(thinking_beta_header(&b), None);
        // Updates only where supported, with the beta header.
        let b = body(
            "claude-opus-5-5",
            ReasoningHint::Off,
            Some(ThinkingDisplay::Updates),
        );
        assert_eq!(b["thinking"]["display"], "updates");
        assert_eq!(thinking_beta_header(&b), Some(THINKING_UPDATES_BETA));
        let b = body(
            "claude-haiku-5-5",
            ReasoningHint::Off,
            Some(ThinkingDisplay::Updates),
        );
        assert_eq!(b["thinking"]["display"], "summarized");
        assert_eq!(thinking_beta_header(&b), None);
    }

    #[test]
    fn off_never_sends_a_thinking_key_and_never_disabled() {
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-opus-4-7",
            "claude-haiku-4-5",
            "claude-fable-5-1",
            "unknown-model",
        ] {
            let b = body(model, ReasoningHint::Off, None);
            assert!(b.get("thinking").is_none(), "{model}: {b}");
            assert!(b.get("output_config").is_none(), "{model}: {b}");
            assert!(!b.to_string().contains("disabled"), "{model}: {b}");
            assert_eq!(thinking_beta_header(&b), None);
        }
    }

    #[test]
    fn thinking_on_for_opus_5_5_is_adaptive_and_never_budget() {
        let b = body("anthropic/claude-opus-5-5", ReasoningHint::High, None);
        assert_eq!(b["thinking"], json!({"type": "adaptive"}));
        assert_eq!(b["output_config"]["effort"], "high");
        assert!(!b.to_string().contains("budget_tokens"), "{b}");
    }
}
