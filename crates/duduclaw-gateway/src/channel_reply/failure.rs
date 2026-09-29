/// Classified failure category for `claude` CLI / Python SDK calls.
///
/// Drives the user-facing fallback message so we tell the user *why*
/// it actually failed (rate limit, timeout, etc.) rather than always
/// suggesting they re-run `claude auth status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureReason {
    /// `claude` binary was not found on the filesystem.
    BinaryMissing,
    /// All rotator accounts exhausted due to rate-limit / usage-limit / 429.
    RateLimited,
    /// Billing / credit exhausted (402, insufficient_quota).
    Billing,
    /// Claude CLI reported "Not logged in" / authentication failure.
    /// Distinct from BinaryMissing (binary exists, just not authenticated).
    AuthFailed,
    /// 30-minute hard timeout tripped.
    Timeout,
    /// Subprocess failed to spawn or exited non-zero without recognizable cause.
    SpawnError,
    /// CLI returned empty output after trimming.
    EmptyResponse,
    /// No rotator accounts configured.
    NoAccounts,
    /// WP10 M4 — accounts ARE configured, but every one is in a billing-class
    /// (24 h) cooldown. Recovery is hours away, so say so.
    AccountsCoolingDownLong,
    /// WP10 M4 — accounts are cooling down after a rate limit or a transient
    /// error. Recovery is minutes away.
    AccountsCoolingDownShort,
    /// WP10 M4 — nothing is selectable but the reason is not attributable to a
    /// cooldown. Wording must cover both horizons rather than guess.
    AccountsCoolingDownUnknown,
    /// Fallback — unrecognized error string.
    Unknown,
}

impl FailureReason {
    /// Stable snake_case token for the `failure:` playbook signal namespace
    /// (WP1.3, §1.3 — `playbook::signals::TurnSignals::with_failure_reason`).
    /// Not yet wired into live turn-signal assembly: that needs the
    /// PREVIOUS turn's settled failure to be threaded into the NEXT turn's
    /// prompt build, which nothing in this codebase currently persists
    /// cross-turn (see the WP1.2/1.3 implementation report for the reasoning
    /// on scoping this out for now). This method exists so the vocabulary is
    /// complete and independently testable ahead of that follow-up.
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::BinaryMissing => "binary_missing",
            Self::RateLimited => "rate_limited",
            Self::Billing => "billing",
            Self::AuthFailed => "auth_failed",
            Self::Timeout => "timeout",
            Self::SpawnError => "spawn_error",
            Self::EmptyResponse => "empty_response",
            Self::NoAccounts => "no_accounts",
            Self::AccountsCoolingDownLong => "accounts_cooling_down_long",
            Self::AccountsCoolingDownShort => "accounts_cooling_down_short",
            Self::AccountsCoolingDownUnknown => "accounts_cooling_down_unknown",
            Self::Unknown => "unknown",
        }
    }
}

/// B2b (Honest Lying, arXiv:2605.29463): programmatic evidence for the
/// RFC-24 decision-gap mistake recorded above. Both preconditions —
/// `list_open_decisions()` returning empty and `mentions_decision_reference`
/// matching the user's own text — are deterministic checks over structured
/// data (a SQLite query result + a keyword scan), never the agent's
/// self-report of what it did, so this call site can always attach
/// evidence rather than leaving the mistake unverified. Runtime-agnostic:
/// the signal comes from `duduclaw-memory` + `decision_capture`, not from
/// any particular CLI backend's output shape.
pub(super) fn decision_gap_evidence(user_text: &str) -> crate::gvu::mistake_notebook::TrajectoryEvidence {
    let span = duduclaw_core::truncate_chars(user_text, 300);
    crate::gvu::mistake_notebook::TrajectoryEvidence {
        tool_name: None,
        error_kind: "assertion_failed".to_string(),
        assertion_failed: Some(
            "list_open_decisions() returned empty but mentions_decision_reference(user_text) matched"
                .to_string(),
        ),
        source_span: Some(span),
    }
}

/// B2b: programmatic evidence for the zero-LLM `ConversationOutcome`
/// failure signal (`prediction::outcome::extract`). Every field feeding
/// `outcome.is_failure()` — satisfaction, task_completed, correction_count —
/// is pattern-matched over the user's own message text (`outcome.rs`'s
/// `detect_satisfaction` / `detect_task_completion` / `count_corrections`),
/// never the agent's self-report of how the conversation went. Runtime-
/// agnostic: it reads session messages, not a specific CLI backend's
/// stream-json shape.
pub(super) fn conversation_outcome_evidence(
    outcome: &crate::prediction::outcome::ConversationOutcome,
    last_user_text: &str,
) -> crate::gvu::mistake_notebook::TrajectoryEvidence {
    let assertion = duduclaw_core::truncate_chars(
        &format!(
            "ConversationOutcome::is_failure(): satisfaction={:?} task_completed={:?} correction_count={}",
            outcome.satisfaction, outcome.task_completed, outcome.correction_count
        ),
        300,
    );
    let span = duduclaw_core::truncate_chars(last_user_text, 300);
    crate::gvu::mistake_notebook::TrajectoryEvidence {
        tool_name: None,
        error_kind: "assertion_failed".to_string(),
        assertion_failed: Some(assertion),
        source_span: Some(span),
    }
}

