//! Closed public vocabulary for why a discovery run stopped. The free-form
//! `stop_reason` can carry host paths or CLI stderr, so public views expose
//! only one of [`STOP_CODES`]. Classification compares against the exact
//! strings this crate produces (anchored templates rendered from the error
//! `Display` impls), never loose substring matching.
use super::contracts::AttemptInfraError;

pub const STOP_CODES: &[&str] = &["no_account", "budget_exhausted", "rate_limited",
    "isolation_unavailable", "runtime_unsupported", "cleanup_failed", "integrity_changed",
    "winner_rejected", "evaluator_unavailable", "attempt_failed", "tool_violation", "other"];

// Literal stop reasons written by `online.rs`.
const BUDGET_EXHAUSTED: &str = "budget_exhausted";
const RATE_LIMIT: &str = "rate_limit";
const WINNER_REJECTED: &str = "winner failed its final evaluation";
const WINNER_EVALUATOR_UNAVAILABLE: &str = "winner evaluator unavailable";
const INTEGRITY_CHANGED: &str = "integrity_changed";
// Payload placeholder used to render a variant's Display into a template.
const SLOT: &str = "\u{0}";

/// Exhaustive on purpose: a new variant must pick its public token here.
pub(super) fn code_for(error: &AttemptInfraError) -> &'static str {
    match error {
        AttemptInfraError::CapabilityUnsupported { .. } | AttemptInfraError::StrictUsdUnsupported(_)
            | AttemptInfraError::RuntimeUnsupported(_) => "runtime_unsupported",
        AttemptInfraError::CleanupFailed(_) => "cleanup_failed",
        AttemptInfraError::IsolationUnavailable => "isolation_unavailable",
        AttemptInfraError::NoAccount => "no_account",
        AttemptInfraError::BudgetExhausted => "budget_exhausted",
        AttemptInfraError::RateLimited => "rate_limited",
        AttemptInfraError::Spawn(_) | AttemptInfraError::RetriesExhausted { .. } => "attempt_failed",
        AttemptInfraError::ToolSurfaceViolation { .. } => "tool_violation",
    }
}
/// One instance of every variant with [`SLOT`] standing in for each payload.
pub(super) fn template_variants() -> Vec<AttemptInfraError> {
    vec![
        AttemptInfraError::CapabilityUnsupported { runtime: SLOT.into(), capability: SLOT.into() },
        AttemptInfraError::StrictUsdUnsupported(SLOT.into()),
        AttemptInfraError::CleanupFailed(SLOT.into()),
        AttemptInfraError::IsolationUnavailable,
        AttemptInfraError::RuntimeUnsupported(SLOT.into()),
        AttemptInfraError::NoAccount,
        AttemptInfraError::BudgetExhausted,
        AttemptInfraError::RateLimited,
        AttemptInfraError::Spawn(SLOT.into()),
        AttemptInfraError::RetriesExhausted { retries: 7, last: SLOT.into() },
        AttemptInfraError::ToolSurfaceViolation { runtime: SLOT.into(), tool: SLOT.into() },
    ]
}
fn template(error: &AttemptInfraError) -> Vec<String> {
    let rendered = error.to_string();
    // The numeric retry count cannot hold the slot; mark its rendered digit.
    let rendered = if matches!(error, AttemptInfraError::RetriesExhausted { .. }) {
        rendered.replacen('7', SLOT, 1)
    } else { rendered };
    rendered.split(SLOT).map(str::to_owned).collect()
}
/// Anchored template match: the first part is a prefix, the last a suffix,
/// and the middle parts appear in order between them.
fn matches_template(text: &str, parts: &[String]) -> bool {
    let Some((first, rest)) = parts.split_first() else { return false };
    let Some(mut tail) = text.strip_prefix(first.as_str()) else { return false };
    let Some((last, middle)) = rest.split_last() else { return tail.is_empty() };
    for part in middle {
        match tail.find(part.as_str()) {
            Some(index) => tail = &tail[index + part.len()..],
            None => return false,
        }
    }
    tail.ends_with(last.as_str())
}
/// Map a run's terminal status plus its private stop reason to a public token.
pub fn classify(status: &str, stop_reason: Option<&str>) -> Option<&'static str> {
    match status {
        "rate_limited" => return Some("rate_limited"),
        "budget_exhausted" => return Some("budget_exhausted"),
        _ => {}
    }
    // Match the exact produced text: trimming would break an empty payload slot.
    let reason = stop_reason.filter(|reason| !reason.trim().is_empty())?;
    match reason {
        BUDGET_EXHAUSTED => return Some("budget_exhausted"),
        RATE_LIMIT => return Some("rate_limited"),
        WINNER_REJECTED => return Some("winner_rejected"),
        WINNER_EVALUATOR_UNAVAILABLE => return Some("evaluator_unavailable"),
        _ => {}
    }
    if let Some(rest) = reason.strip_prefix(INTEGRITY_CHANGED) {
        if rest.is_empty() || rest.starts_with(':') || rest.starts_with(' ') { return Some("integrity_changed"); }
    }
    for variant in template_variants() {
        if matches_template(reason, &template(&variant)) { return Some(code_for(&variant)); }
    }
    Some("other")
}
/// Accept a stored token only when it belongs to the closed set.
pub fn normalize(stored: Option<&str>) -> Option<&'static str> {
    let stored = stored?;
    STOP_CODES.iter().copied().find(|code| *code == stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_attempt_infra_error_variant_maps_to_its_token() {
        let cases = [
            (AttemptInfraError::CapabilityUnsupported { runtime: "codex".into(), capability: "max turns: hard".into() }, "runtime_unsupported"),
            (AttemptInfraError::StrictUsdUnsupported("gemini".into()), "runtime_unsupported"),
            (AttemptInfraError::CleanupFailed("docker rm /var/tmp/secret-path failed".into()), "cleanup_failed"),
            (AttemptInfraError::IsolationUnavailable, "isolation_unavailable"),
            (AttemptInfraError::RuntimeUnsupported("antigravity".into()), "runtime_unsupported"),
            (AttemptInfraError::NoAccount, "no_account"),
            (AttemptInfraError::BudgetExhausted, "budget_exhausted"),
            (AttemptInfraError::RateLimited, "rate_limited"),
            (AttemptInfraError::Spawn("No such file: /usr/local/bin/claude".into()), "attempt_failed"),
            (AttemptInfraError::RetriesExhausted { retries: 3, last: "exit 1: stderr".into() }, "attempt_failed"),
            (AttemptInfraError::RetriesExhausted { retries: 12, last: String::new() }, "attempt_failed"),
            (AttemptInfraError::ToolSurfaceViolation { runtime: "grok".into(), tool: "search_tool".into() }, "tool_violation"),
            (AttemptInfraError::ToolSurfaceViolation { runtime: "codex".into(), tool: "unknown".into() }, "tool_violation"),
        ];
        for (error, token) in &cases {
            assert_eq!(code_for(error), *token);
            assert_eq!(classify("degraded", Some(&error.to_string())), Some(*token), "{error}");
        }
        assert_eq!(template_variants().len(), 11, "every variant needs a template");
        assert_eq!(STOP_CODES.len(), 12);
        for variant in template_variants() {
            assert!(STOP_CODES.contains(&code_for(&variant)));
        }
    }
    #[test]
    fn literal_reasons_and_status_map_to_tokens() {
        assert_eq!(classify("degraded", Some("no usable account (all cooling down, exhausted or auth-dead)")), Some("no_account"));
        assert_eq!(classify("budget_exhausted", Some("agent budget exhausted")), Some("budget_exhausted"));
        assert_eq!(classify("budget_exhausted", None), Some("budget_exhausted"));
        assert_eq!(classify("rate_limited", Some("rate_limit")), Some("rate_limited"));
        assert_eq!(classify("degraded", Some("rate_limit")), Some("rate_limited"));
        assert_eq!(classify("degraded", Some("budget_exhausted")), Some("budget_exhausted"));
        assert_eq!(classify("degraded", Some("winner failed its final evaluation")), Some("winner_rejected"));
        assert_eq!(classify("degraded", Some("winner evaluator unavailable")), Some("evaluator_unavailable"));
        assert_eq!(classify("degraded", Some("integrity_changed: /home/x/ws changed")), Some("integrity_changed"));
        assert_eq!(classify("degraded", Some("integrity_changed during scoring: x")), Some("integrity_changed"));
        assert_eq!(classify("degraded", Some("integrity_changedX")), Some("other"));
        assert_eq!(classify("degraded", Some("policy_plan_failed: boom")), Some("other"));
        assert_eq!(classify("degraded", Some("prefix no usable account (all cooling down, exhausted or auth-dead)")), Some("other"));
        assert_eq!(classify("complete", None), None);
        assert_eq!(classify("degraded", Some("  ")), None);
    }
    #[test]
    fn stored_tokens_are_accepted_only_from_the_closed_set() {
        assert_eq!(normalize(Some("no_account")), Some("no_account"));
        assert_eq!(normalize(Some("/etc/passwd")), None);
        assert_eq!(normalize(None), None);
    }
}
