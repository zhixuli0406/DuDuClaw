//! RM-181 (v1.84.0): the ONE rule for "may the explicitly configured
//! fallback engine run after this primary failure?".
//!
//! Layer 3 of the failover stack used to borrow `is_non_retryable`, which
//! was written for account/model rotation. This module replaces that with an
//! explicit table. Every row is pinned by `tests::classifier_table`.
//!
//! Refusals are checked FIRST (fail closed): a failure that is a result of
//! the work itself, a budget/policy/autonomy decision, or a refusal of the
//! employee's own tool configuration stops here. Only then are the eligible
//! kinds recognised.
//!
//! The error text of a provider failure can echo attacker-chosen text, so
//! (v1.84 security review L2) nothing below the typed markers searches it:
//!
//! 1. a [`FailureKind`] the runtime recorded for exactly this error (HTTP
//!    status, the provider's error code identifier, a duduclaw-llm error
//!    variant) decides on its own;
//! 2. otherwise only the anchored message shapes DuDuClaw's own runtimes
//!    format are recognised (`API error (<status> …`, `<Engine> CLI exited
//!    with `, `Empty response from `, …) — the text after that prefix, which
//!    may carry model or provider output, is never read;
//! 3. a string neither typed nor DuDuClaw-formatted keeps exactly its v1.83.0
//!    outcome (the whole `is_non_retryable` list refuses — content policy,
//!    billing hard limit, the 4xx substrings AND the authentication
//!    substrings; anything else falls back, reported as
//!    [`FallbackTrigger::Unclassified`]).
//!
//! Design rule (v1.84 security re-review F2): an authentication failure
//! falls back ONLY when a typed [`FailureKind`] or an anchored,
//! DuDuClaw-written prefix identifies it. Untyped text that merely mentions
//! `401 ` or `invalid_api_key` is refused, as in v1.83.0.
//!
//! The fallback leg always runs its own full gates: `execute_with_failover`
//! re-checks product policy and the budget for the fallback runtime and model,
//! and the fallback runtime's own `execute` runs its spawn gate
//! (`prepare_mcp_config_for_spawn`) exactly as a primary call would.

use crate::work_outcome::WorkOutcome;
use super::failure_kind::FailureKind;

/// Owner decision pending (RM-181): a spawn-gate style refusal that is about
/// the ENGINE's own host configuration (Codex external configuration layers,
/// Grok's global MCP sources) — not about the employee — may hand over to the
/// configured fallback engine. `false` restores the v1.83.0 behaviour (stop
/// everything, like an employee `.mcp.json` refusal). Only a refusal carrying
/// the explicit engine-local marker
/// ([`duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error`]) is
/// eligible; nothing is inferred from message text.
pub(crate) const ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK: bool = true;

/// What happened on the primary leg, as the failover manager saw it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PrimaryFailure<'a> {
    NotRegistered,
    Cooldown,
    Empty,
    /// The runtime returned an error. `autonomy_denied` is
    /// `crate::autonomous_runtime::was_denied()` sampled for this call;
    /// `kind` is what the runtime recorded for exactly this error
    /// ([`super::failure_kind::kind_for`]).
    Error { error: &'a str, autonomy_denied: bool, kind: Option<FailureKind> },
}

/// Why the configured fallback engine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackTrigger {
    NotRegistered,
    Cooldown,
    EmptyReply,
    AuthFailure,
    ModelUnavailable,
    ServerOrRateLimit,
    ProcessError,
    /// Engine-local host configuration refusal, while
    /// [`ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK`] is `true`.
    EngineLocalConfig,
    /// The conversation's required context does not fit this engine's single
    /// command-line argument. Eligible only when the fallback engine has no
    /// such limit ([`apply_fallback_engine_limits`]).
    PromptTooLarge,
    /// Not typed by this table; v1.83.0 also fell back on it.
    Unclassified,
}

/// The kind of work limit a typed work result reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkLimitKind {
    MaxTurns,
    TimeoutTotal,
    TimeoutNoProgress,
}

/// Why the configured fallback engine does NOT run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackRefusal {
    Budget,
    Policy,
    AutonomyPaused,
    /// A typed work result: the work itself hit its limit; v1.83.0 does not
    /// re-run it on another engine.
    WorkLimit(WorkLimitKind),
    /// The employee's own tool configuration (`.mcp.json` spawn gate).
    EmployeeToolConfig,
    /// Engine-local host configuration refusal while
    /// [`ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK`] is `false`.
    EngineLocalConfig,
    ContentPolicy,
    BillingHardLimit,
    /// A 4xx request rejection that names no model and no credential — the
    /// v1.83.0 rule for these strings, kept.
    RequestRejected,
    /// Untyped, not DuDuClaw-formatted text that matches one of the v1.83.0
    /// authentication substrings (`401 `, `403 `, `invalid_api_key`, …).
    /// v1.83.0 did not fall back on these; only a typed kind or an anchored
    /// DuDuClaw prefix makes an authentication failure eligible.
    UntypedAuthFailure,
    /// The conversation's required context is too large for the primary AND
    /// the fallback engine (both pass the prompt as one command-line argument).
    PromptTooLarge,
}

impl FallbackRefusal {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Policy => "policy",
            Self::AutonomyPaused => "autonomy_paused",
            Self::WorkLimit(WorkLimitKind::MaxTurns) => "work_max_turns",
            Self::WorkLimit(WorkLimitKind::TimeoutTotal) => "work_timeout_total",
            Self::WorkLimit(WorkLimitKind::TimeoutNoProgress) => "work_timeout_no_progress",
            Self::EmployeeToolConfig => "employee_tool_config",
            Self::EngineLocalConfig => "engine_local_config",
            Self::ContentPolicy => "content_policy",
            Self::BillingHardLimit => "billing_hard_limit",
            Self::RequestRejected => "request_rejected",
            Self::UntypedAuthFailure => "untyped_auth_failure",
            Self::PromptTooLarge => "prompt_too_large",
        }
    }

    /// Plain-words reason, completing 「已設定備援引擎 X，但…」.
    pub(crate) fn zh_tw(self) -> &'static str {
        match self {
            Self::WorkLimit(WorkLimitKind::TimeoutTotal) => "這次的失敗屬於任務本身的時間上限，不會換引擎重跑",
            Self::WorkLimit(WorkLimitKind::TimeoutNoProgress) => "這次的失敗屬於任務本身長時間沒有進展的上限，不會換引擎重跑",
            Self::WorkLimit(WorkLimitKind::MaxTurns) => "這次的失敗屬於任務本身的步驟上限，不會換引擎重跑",
            Self::Budget => "這次是預算限制擋下的，換引擎也同樣受限，所以沒有改用",
            Self::Policy => "產品政策不允許這次的執行，所以沒有改用",
            Self::AutonomyPaused => "自主工作目前已暫停，不會換引擎執行",
            Self::EmployeeToolConfig => "這位員工的工具設定沒有通過啟動前檢查，換引擎也必須通過同一項檢查，所以沒有改用",
            Self::EngineLocalConfig => "原選引擎在本機的設定沒有通過啟動前檢查，依目前規則不會換引擎重跑",
            Self::ContentPolicy => "這次的內容被服務的內容政策拒絕，不會換引擎重跑",
            Self::BillingHardLimit => "原選引擎的帳務已達硬性上限，依規則不會換引擎重跑",
            Self::RequestRejected => "原選引擎拒絕了這次的請求內容，換引擎重送通常也會被拒，所以沒有改用",
            Self::UntypedAuthFailure => "原選引擎回報了看起來像登入或憑證的錯誤，但無法確認來源，依規則不會換引擎重跑；請先檢查原選引擎的登入狀態",
            Self::PromptTooLarge => "這段對話必要的內容超過備援引擎一次能送出的大小，換引擎也送不出去，所以沒有改用",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackDecision {
    Eligible(FallbackTrigger),
    Refused(FallbackRefusal),
}

/// Classify the primary leg's failure. Pure apart from the budget marker
/// decoder; the autonomy state is passed in.
pub(crate) fn classify_primary_failure(failure: PrimaryFailure<'_>) -> FallbackDecision {
    use FallbackDecision::{Eligible, Refused};
    let (error, autonomy_denied, kind) = match failure {
        PrimaryFailure::NotRegistered => return Eligible(FallbackTrigger::NotRegistered),
        PrimaryFailure::Cooldown => return Eligible(FallbackTrigger::Cooldown),
        PrimaryFailure::Empty => return Eligible(FallbackTrigger::EmptyReply),
        PrimaryFailure::Error { error, autonomy_denied, kind } => (error, autonomy_denied, kind),
    };

    // Only unwrap gateway-authored rotation envelopes, never search the body.
    let error = error.strip_prefix("All accounts exhausted. Last error: ")
        .or_else(|| error.strip_prefix("no accounts available: ")).unwrap_or(error);

    // ── Refusals (checked first; fail closed) ──────────────────────────
    if crate::entry_gate::background_refusal_code(error).is_some() {
        return Refused(FallbackRefusal::Policy);
    }
    if crate::budget::budget_refusal_message(error).is_some() {
        return Refused(FallbackRefusal::Budget);
    }
    if autonomy_denied || error.starts_with("AUTONOMY_PAUSED:") {
        return Refused(FallbackRefusal::AutonomyPaused);
    }
    if let Some(outcome) = WorkOutcome::decode(error) {
        return Refused(FallbackRefusal::WorkLimit(match outcome {
            WorkOutcome::MaxTurns { .. } => WorkLimitKind::MaxTurns,
            WorkOutcome::TimeoutTotal { .. } => WorkLimitKind::TimeoutTotal,
            WorkOutcome::TimeoutNoProgress { .. } => WorkLimitKind::TimeoutNoProgress,
        }));
    }
    if duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(error) {
        // DuDuClaw's own wording at the anchored position after both markers.
        if crate::runtime::prompt_budget::is_prompt_too_large_error(error) {
            return Eligible(FallbackTrigger::PromptTooLarge);
        }
        return if engine_local_config_refusal_falls_back() {
            Eligible(FallbackTrigger::EngineLocalConfig)
        } else {
            Refused(FallbackRefusal::EngineLocalConfig)
        };
    }
    if duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(error) {
        return Refused(FallbackRefusal::EmployeeToolConfig);
    }
    // `duduclaw_core::product_profile` refusals raised inside a runtime are
    // returned as-is (`?`), so their wording starts the string.
    if starts_with_ignore_ascii_case(error, "product profile does not allow") {
        return Refused(FallbackRefusal::Policy);
    }

    // 1. The runtime's typed kind for exactly this error decides alone.
    if let Some(kind) = kind {
        return decide_kind(kind);
    }
    // 2. An anchored message shape DuDuClaw's runtimes format.
    if let Some(decision) = anchored_engine_message(error) {
        return decision;
    }
    // 3. Neither typed nor DuDuClaw-formatted: the v1.83.0 outcome.
    untyped_v183_outcome(error)
}

/// RM-204: a prompt-too-large refusal may hand over only to an engine without
/// the single-argument limit (Claude, Codex, the API runtime). When the
/// configured fallback has the same limit, it would refuse the same way.
pub(crate) fn apply_fallback_engine_limits(
    decision: FallbackDecision,
    fallback: Option<&crate::runtime::RuntimeType>,
) -> FallbackDecision {
    match decision {
        FallbackDecision::Eligible(FallbackTrigger::PromptTooLarge)
            if fallback.is_none_or(|fb| crate::runtime::prompt_budget::runtime_has_single_argument_limit(*fb)) =>
        {
            FallbackDecision::Refused(FallbackRefusal::PromptTooLarge)
        }
        other => other,
    }
}

/// One switch, so the pending owner decision is a one-line change.
fn engine_local_config_refusal_falls_back() -> bool {
    ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK
}

fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// 1. The decision for a typed kind. The error text is not read.
fn decide_kind(kind: FailureKind) -> FallbackDecision {
    use FallbackDecision::{Eligible, Refused};
    match kind {
        FailureKind::Auth => Eligible(FallbackTrigger::AuthFailure),
        FailureKind::RateLimited | FailureKind::Server => Eligible(FallbackTrigger::ServerOrRateLimit),
        FailureKind::ModelUnavailable => Eligible(FallbackTrigger::ModelUnavailable),
        FailureKind::RequestRejected => Refused(FallbackRefusal::RequestRejected),
        FailureKind::ContentPolicy => Refused(FallbackRefusal::ContentPolicy),
        FailureKind::BillingHardLimit => Refused(FallbackRefusal::BillingHardLimit),
        // This account, this window or this connection — not the request;
        // v1.83.0 fell back on these strings too.
        FailureKind::Billing | FailureKind::ContextWindow | FailureKind::Network | FailureKind::Parse => {
            Eligible(FallbackTrigger::Unclassified)
        }
        FailureKind::OtherStatus(status) => decide_status(status),
    }
}

/// An HTTP status alone. 400 / 404 / 422 keep the v1.83.0 refusal: a model
/// that does not exist is recognised only from the provider's typed code.
fn decide_status(status: u16) -> FallbackDecision {
    use FallbackDecision::{Eligible, Refused};
    match status {
        401 | 403 => Eligible(FallbackTrigger::AuthFailure),
        429 | 500..=599 => Eligible(FallbackTrigger::ServerOrRateLimit),
        400 | 404 | 422 => Refused(FallbackRefusal::RequestRejected),
        _ => Eligible(FallbackTrigger::Unclassified),
    }
}

/// Prefixes DuDuClaw's runtimes write before an HTTP status
/// (`openai_compat`: `API error (<status>)…`, `SSE error (<status>)…`;
/// `direct_api`: `Anthropic API error (<status>)…`).
const HTTP_STATUS_PREFIXES: &[&str] = &["API error (", "SSE error (", "Anthropic API error ("];

/// Process-level failures as DuDuClaw's runtimes word them, at the start of
/// the string (`Empty response from <engine> …`, the generic CLI's spawn
/// error, `process_group::backend_error` operations).
const PROCESS_PREFIXES: &[&str] = &[
    "Empty response from ",
    "spawn error: failed to start ",
    "Failed to spawn ",
    "Failed to run ",
    "Failed to read codex output: ",
    "claude CLI spawn error: ",
    "claude CLI stream error [",
    "claude CLI exit ",
    "claude CLI assistant error: ",
];

/// `grok.rs::grok_auth_error`.
const GROK_AUTH_PREFIX: &str = "Grok CLI authentication failure (";

/// `<engine> CLI exited with …` (Codex, Grok, the generic CLIs).
const EXITED_MARKER: &str = " CLI exited with ";

/// `generic_cli::GenericCliError::AuthRequired`.
const NOT_LOGGED_IN_MARKER: &str = " CLI is not logged in (authentication required): ";

/// 2. An anchored, DuDuClaw-formatted engine message. Only the prefix is
/// matched; everything after it (stderr, provider body) is never read.
fn anchored_engine_message(error: &str) -> Option<FallbackDecision> {
    use FallbackDecision::Eligible;
    if let Some(status) = anchored_http_status(error) {
        return Some(decide_status(status));
    }
    if error.starts_with(GROK_AUTH_PREFIX) || engine_label_before(error, NOT_LOGGED_IN_MARKER) {
        return Some(Eligible(FallbackTrigger::AuthFailure));
    }
    if engine_label_before(error, EXITED_MARKER) || PROCESS_PREFIXES.iter().any(|p| error.starts_with(p)) {
        return Some(Eligible(FallbackTrigger::ProcessError));
    }
    None
}

/// The three status digits right after an [`HTTP_STATUS_PREFIXES`] prefix.
fn anchored_http_status(error: &str) -> Option<u16> {
    let rest = HTTP_STATUS_PREFIXES.iter().find_map(|prefix| error.strip_prefix(prefix))?;
    let digits = rest.get(..3)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) || rest[3..].starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `marker` with only an engine label before it (`Codex`, `Grok`, `kimi`):
/// the string starts with `<label><marker>`.
fn engine_label_before(error: &str, marker: &str) -> bool {
    error.split_once(marker).is_some_and(|(head, _)| {
        !head.is_empty() && head.len() <= 32
            && head.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    })
}

/// 3. A string no runtime typed and DuDuClaw did not format keeps exactly the
/// v1.83.0 outcome: its `is_non_retryable` text rule (content policy, billing
/// hard limit, the 4xx substrings and the authentication substrings refuse;
/// anything else falls back). Pinned against a copy of the v1.83.0 predicate
/// by `tests::legacy_branch_matches_the_v183_predicate`.
fn untyped_v183_outcome(error: &str) -> FallbackDecision {
    use FallbackDecision::{Eligible, Refused};
    let lower = error.to_lowercase();
    if lower.contains("content_policy_violation") || lower.contains("content policy violation") {
        Refused(FallbackRefusal::ContentPolicy)
    } else if lower.contains("billing_hard_limit") {
        Refused(FallbackRefusal::BillingHardLimit)
    } else if legacy_auth_rejected(&lower) {
        Refused(FallbackRefusal::UntypedAuthFailure)
    } else if legacy_request_rejected(&lower) {
        Refused(FallbackRefusal::RequestRejected)
    } else {
        Eligible(FallbackTrigger::Unclassified)
    }
}

/// The v1.83.0 `is_non_retryable` authentication substrings, kept verbatim
/// (lower-cased `contains`) for strings no rule types (F2).
fn legacy_auth_rejected(lower: &str) -> bool {
    lower.contains("401 ") || lower.contains("403 ") || lower.contains("invalid_api_key")
        || lower.contains("401 unauthorized") || lower.contains("403 forbidden")
}

/// The v1.83.0 `is_non_retryable` 4xx substrings, kept verbatim for strings
/// no rule types.
fn legacy_request_rejected(lower: &str) -> bool {
    lower.contains("400 ") || lower.contains("404 ") || lower.contains("422 ")
        || lower.contains("400 bad request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use FallbackDecision::{Eligible, Refused};

    fn err(error: &str) -> FallbackDecision {
        classify_primary_failure(PrimaryFailure::Error { error, autonomy_denied: false, kind: None })
    }

    fn typed(error: &str, kind: FailureKind) -> FallbackDecision {
        classify_primary_failure(PrimaryFailure::Error { error, autonomy_denied: false, kind: Some(kind) })
    }

    #[test]
    fn classifier_table() {
        let engine_local = duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error("Codex 外部設定 /etc/codex/config.toml：定義了 mcp_servers");
        let employee_gate = duduclaw_agent::mcp_spawn_gate::spawn_gate_error("owned worker MCP configuration refused");
        let budget = format!("{}US$402.00", crate::budget::BUDGET_REFUSAL_PREFIX);
        let max_turns = WorkOutcome::MaxTurns { limit: 7, partial: String::new() }.encode();
        let total = WorkOutcome::TimeoutTotal { seconds: 60, partial: "部分".into() }.encode();
        let idle = WorkOutcome::TimeoutNoProgress { seconds: 60, tool: None, partial: String::new() }.encode();
        let rows: Vec<(&str, FallbackDecision)> = vec![
            // ── NOT eligible ──
            (budget.as_str(), Refused(FallbackRefusal::Budget)),
            ("AUTONOMY_PAUSED: operator paused autonomous work", Refused(FallbackRefusal::AutonomyPaused)),
            (max_turns.as_str(), Refused(FallbackRefusal::WorkLimit(WorkLimitKind::MaxTurns))),
            (total.as_str(), Refused(FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutTotal))),
            (idle.as_str(), Refused(FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutNoProgress))),
            (employee_gate.as_str(), Refused(FallbackRefusal::EmployeeToolConfig)),
            ("Product profile does not allow runtime 'grok'", Refused(FallbackRefusal::Policy)),
            // Anchored DuDuClaw shape: the status decides, the body is not read
            // (in production `openai_compat` also records the provider's code;
            // see `typed_rows` below).
            ("API error (400 Bad Request): content_policy_violation", Refused(FallbackRefusal::RequestRejected)),
            // Untyped, not DuDuClaw-formatted: the v1.83.0 outcome.
            ("Content policy violation detected", Refused(FallbackRefusal::ContentPolicy)),
            ("400 bad request: invalid JSON", Refused(FallbackRefusal::RequestRejected)),
            ("API error (422 Unprocessable Entity): messages too long", Refused(FallbackRefusal::RequestRejected)),
            // L2: without the provider's typed code, a model name in a 4xx body
            // no longer turns the v1.83.0 refusal into a fallback.
            ("API error (404 Not Found): The model `opus` does not exist", Refused(FallbackRefusal::RequestRejected)),
            ("API error (400 Bad Request): invalid model 'opus'", Refused(FallbackRefusal::RequestRejected)),
            ("API error (422 Unprocessable Entity): unknown model grok-x", Refused(FallbackRefusal::RequestRejected)),
            // Claude wrapper/prefix classification must ignore provider-written bodies.
            ("claude CLI stream error [error]: 404 model not found", Eligible(FallbackTrigger::ProcessError)),
            ("claude CLI exit 1: invalid_api_key", Eligible(FallbackTrigger::ProcessError)),
            ("claude CLI assistant error: content policy violation", Eligible(FallbackTrigger::ProcessError)),
            ("Empty response from claude CLI", Eligible(FallbackTrigger::ProcessError)),
            ("All accounts exhausted. Last error: claude CLI stream error [error]: 404 not found", Eligible(FallbackTrigger::ProcessError)),
            ("All accounts exhausted. Last error: claude CLI exit 1", Eligible(FallbackTrigger::ProcessError)),
            ("no accounts available: claude CLI assistant error: 400 bad request", Eligible(FallbackTrigger::ProcessError)),
            ("All accounts exhausted. Last error: Empty response from claude CLI", Eligible(FallbackTrigger::ProcessError)),
            // ── Eligible ──
            (engine_local.as_str(), Eligible(FallbackTrigger::EngineLocalConfig)),
            ("Empty response from Codex CLI (exit 0); stderr tail: ", Eligible(FallbackTrigger::ProcessError)),
            ("Grok CLI exited with 1: boom", Eligible(FallbackTrigger::ProcessError)),
            ("spawn error: failed to start kimi CLI: No such file or directory", Eligible(FallbackTrigger::ProcessError)),
            ("Codex CLI exited with code 101: thread 'main' panicked", Eligible(FallbackTrigger::ProcessError)),
            ("API error (403 Forbidden): access denied", Eligible(FallbackTrigger::AuthFailure)),
            ("Grok CLI authentication failure (not logged in / 憑證失效): stderr tail: x", Eligible(FallbackTrigger::AuthFailure)),
            ("kimi CLI is not logged in (authentication required): run login", Eligible(FallbackTrigger::AuthFailure)),
            ("API error (503 Service Unavailable): overloaded", Eligible(FallbackTrigger::ServerOrRateLimit)),
            ("SSE error (502 Bad Gateway): upstream", Eligible(FallbackTrigger::ServerOrRateLimit)),
            ("API error (429 Too Many Requests): billing_hard_limit_reached", Eligible(FallbackTrigger::ServerOrRateLimit)),
            ("Anthropic API error (401 Unauthorized): x", Eligible(FallbackTrigger::AuthFailure)),
            // F2: untyped, not DuDuClaw-formatted authentication text keeps the
            // v1.83.0 outcome — it does NOT fall back.
            ("401 invalid bearer token; misleading claude /login", Refused(FallbackRefusal::UntypedAuthFailure)),
            ("invalid_api_key: Incorrect API key provided", Refused(FallbackRefusal::UntypedAuthFailure)),
            ("Error: 403 Forbidden", Refused(FallbackRefusal::UntypedAuthFailure)),
            // Untyped and not DuDuClaw-formatted: the v1.83.0 outcome (falls back).
            ("codex: your login expired, please sign in again", Eligible(FallbackTrigger::Unclassified)),
            ("error: model_not_found", Eligible(FallbackTrigger::Unclassified)),
            ("500 internal server error", Eligible(FallbackTrigger::Unclassified)),
            ("429 rate limited", Eligible(FallbackTrigger::Unclassified)),
            ("connection refused", Eligible(FallbackTrigger::Unclassified)),
            ("connection timed out", Eligible(FallbackTrigger::Unclassified)),
        ];
        for (error, expected) in rows {
            assert_eq!(err(error), expected, "{error}");
        }
        // The kinds production runtimes record decide those same strings.
        let typed_rows = [
            ("API error (400 Bad Request): content_policy_violation", FailureKind::ContentPolicy, Refused(FallbackRefusal::ContentPolicy)),
            ("API error (429 Too Many Requests): billing_hard_limit_reached", FailureKind::BillingHardLimit, Refused(FallbackRefusal::BillingHardLimit)),
            ("API error (404 Not Found): The model `opus` does not exist", FailureKind::ModelUnavailable, Eligible(FallbackTrigger::ModelUnavailable)),
            ("openai-compat tool loop error: authentication failed", FailureKind::Auth, Eligible(FallbackTrigger::AuthFailure)),
            ("openai-compat tool loop error: invalid request: x", FailureKind::RequestRejected, Refused(FallbackRefusal::RequestRejected)),
            ("HTTP request: error sending request", FailureKind::Network, Eligible(FallbackTrigger::Unclassified)),
            ("API error (409 Conflict): x", FailureKind::OtherStatus(409), Eligible(FallbackTrigger::Unclassified)),
        ];
        for (error, kind, expected) in typed_rows {
            assert_eq!(typed(error, kind), expected, "{error}");
        }
        assert_eq!(classify_primary_failure(PrimaryFailure::NotRegistered), Eligible(FallbackTrigger::NotRegistered));
        assert_eq!(classify_primary_failure(PrimaryFailure::Cooldown), Eligible(FallbackTrigger::Cooldown));
        assert_eq!(classify_primary_failure(PrimaryFailure::Empty), Eligible(FallbackTrigger::EmptyReply));
        // The autonomy gate's task-local verdict refuses whatever the text says.
        assert_eq!(
            classify_primary_failure(PrimaryFailure::Error { error: "Failed to run stub: paused", autonomy_denied: true, kind: Some(FailureKind::Server) }),
            Refused(FallbackRefusal::AutonomyPaused)
        );
    }

    /// L2: a body that echoes "invalid model", "401 ", "content policy
    /// violation" or "billing_hard_limit" is decided by the structured kind.
    #[test]
    fn echoed_body_text_never_overrides_the_structured_kind() {
        for body in ["invalid model 'x'", "401 unauthorized", "content policy violation", "billing_hard_limit_reached",
            "model_not_found 500 internal server error not logged in"] {
            let error = format!("API error (400 Bad Request): {body}");
            assert_eq!(typed(&error, FailureKind::RequestRejected), Refused(FallbackRefusal::RequestRejected), "{body}");
            assert_eq!(err(&error), Refused(FallbackRefusal::RequestRejected), "anchored status, {body}");
            let error = format!("API error (500 Internal Server Error): {body}");
            assert_eq!(typed(&error, FailureKind::Server), Eligible(FallbackTrigger::ServerOrRateLimit), "{body}");
            assert_eq!(err(&error), Eligible(FallbackTrigger::ServerOrRateLimit), "anchored status, {body}");
            let error = format!("openai-compat tool loop error: invalid request: {body}");
            assert_eq!(typed(&error, FailureKind::RequestRejected), Refused(FallbackRefusal::RequestRejected), "{body}");
            let error = format!("openai-compat tool loop error: rate limited: {body}");
            assert_eq!(typed(&error, FailureKind::RateLimited), Eligible(FallbackTrigger::ServerOrRateLimit), "{body}");
            // A CLI's stderr after DuDuClaw's prefix is never read either.
            for prefix in ["Codex CLI exited with exit status: 1: ", "Grok CLI exited with 1: ", "Empty response from Grok CLI (exit 0); stderr tail: "] {
                let error = format!("{prefix}{body}");
                assert_eq!(err(&error), Eligible(FallbackTrigger::ProcessError), "{error}");
            }
        }
        // Refusals still win over any kind.
        let gate = duduclaw_agent::mcp_spawn_gate::spawn_gate_error("x");
        assert_eq!(typed(&gate, FailureKind::Server), Refused(FallbackRefusal::EmployeeToolConfig));
        let work = WorkOutcome::TimeoutTotal { seconds: 1, partial: String::new() }.encode();
        assert_eq!(typed(&work, FailureKind::Auth), Refused(FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutTotal)));
        assert_eq!(typed("Product profile does not allow model 'x'", FailureKind::Server), Refused(FallbackRefusal::Policy));
        // The policy wording counts only at the start of the string.
        assert_eq!(err("Grok CLI exited with 1: Product profile does not allow"), Eligible(FallbackTrigger::ProcessError));
        // A label with spaces before the marker is not DuDuClaw's prefix.
        assert_eq!(err("the model said Codex CLI exited with 1 and 400 bad"), Refused(FallbackRefusal::RequestRejected));
    }

    /// Reference oracle: the v1.83.0 `failover::is_non_retryable` text rule
    /// (lower-cased `contains` on every listed substring), copied here so the
    /// legacy branch can never drift from it again (F2).
    fn v183_is_non_retryable(error: &str) -> bool {
        let lower = error.to_lowercase();
        lower.contains("400 ")
            || lower.contains("401 ")
            || lower.contains("403 ")
            || lower.contains("404 ")
            || lower.contains("422 ")
            || lower.contains("invalid_api_key")
            || lower.contains("401 unauthorized")
            || lower.contains("403 forbidden")
            || lower.contains("400 bad request")
            || lower.contains("content_policy_violation")
            || lower.contains("content policy violation")
            || lower.contains("billing_hard_limit")
    }

    /// F2: every representative v1.83.0 error string gets exactly the v1.83.0
    /// outcome from the legacy branch — directly, and through the full rule
    /// (none of these strings is typed or carries a DuDuClaw prefix).
    #[test]
    fn legacy_branch_matches_the_v183_predicate() {
        let strings = [
            "401 invalid bearer token; misleading claude /login",
            "Error: 401 Unauthorized",
            "HTTP 401 unauthorized: token expired",
            "403 forbidden: organization disabled",
            "request failed with status 403 access denied",
            "invalid_api_key: Incorrect API key provided",
            "INVALID_API_KEY",
            "{\"error\":{\"code\":\"invalid_api_key\"}}",
            "400 bad request: invalid JSON",
            "status 404 not found",
            "422 unprocessable entity",
            "content_policy_violation",
            "Content policy violation detected",
            "billing_hard_limit_reached",
            "500 internal server error",
            "502 bad gateway",
            "429 rate limited",
            "connection refused",
            "connection timed out",
            "codex: your login expired, please sign in again",
            "authentication_error: please log in",
            "Unauthorized",
            "error 401",
            "request id 14012 failed",
            "token 4013 expired",
            "error: model_not_found",
            "",
        ];
        for error in strings {
            let legacy = untyped_v183_outcome(error);
            assert_eq!(matches!(legacy, Refused(_)), v183_is_non_retryable(error), "legacy branch: {error:?}");
            assert_eq!(err(error), legacy, "full rule reaches the legacy branch: {error:?}");
        }
        // A typed kind or an anchored DuDuClaw prefix is what makes an
        // authentication failure eligible.
        assert_eq!(typed("401 invalid bearer token", FailureKind::Auth), Eligible(FallbackTrigger::AuthFailure));
        assert_eq!(err("API error (401 Unauthorized): invalid bearer token"), Eligible(FallbackTrigger::AuthFailure));
        assert_eq!(err("Grok CLI authentication failure (not logged in): x"), Eligible(FallbackTrigger::AuthFailure));
    }

    /// RM-204: a prompt that does not fit one argument falls back only to an
    /// engine without that limit.
    #[test]
    fn prompt_too_large_is_eligible_only_for_an_engine_without_the_argument_limit() {
        use crate::runtime::RuntimeType;
        let error = crate::runtime::prompt_budget::prompt_too_large_error("grok");
        let decision = err(&error);
        assert_eq!(decision, Eligible(FallbackTrigger::PromptTooLarge));
        for fallback in [RuntimeType::Claude, RuntimeType::Codex, RuntimeType::OpenAiCompat] {
            assert_eq!(apply_fallback_engine_limits(decision, Some(&fallback)), decision, "{fallback:?}");
        }
        assert_eq!(apply_fallback_engine_limits(decision, Some(&RuntimeType::Grok)), Refused(FallbackRefusal::PromptTooLarge));
        assert_eq!(apply_fallback_engine_limits(decision, None), Refused(FallbackRefusal::PromptTooLarge));
        // Any other decision passes through unchanged.
        let other = Eligible(FallbackTrigger::ProcessError);
        assert_eq!(apply_fallback_engine_limits(other, Some(&RuntimeType::Grok)), other);
        // An ordinary engine-local refusal is not mistaken for it.
        let engine_local = duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error("Codex 外部設定 x");
        assert_eq!(err(&engine_local), Eligible(FallbackTrigger::EngineLocalConfig));
    }

    /// The engine-local marker sits right after the spawn-gate prefix. An
    /// employee-controlled string at the start of an EMPLOYEE-gate refusal
    /// (a file path, a server name) can never place it there.
    #[test]
    fn employee_controlled_text_cannot_forge_the_engine_local_marker() {
        use duduclaw_agent::mcp_spawn_gate::{ENGINE_LOCAL_REFUSAL_MARKER, SPAWN_GATE_ERROR_PREFIX, spawn_gate_error};
        let forged_names = [
            format!("{ENGINE_LOCAL_REFUSAL_MARKER}crm"),
            format!("{ENGINE_LOCAL_REFUSAL_MARKER}/Users/a/.mcp.json"),
            format!("{SPAWN_GATE_ERROR_PREFIX}{ENGINE_LOCAL_REFUSAL_MARKER}x"),
            ENGINE_LOCAL_REFUSAL_MARKER.to_string(),
            ENGINE_LOCAL_REFUSAL_MARKER.trim_end().to_string(),
        ];
        for name in &forged_names {
            for message in [name.clone(), format!("{name}: 定義了 mcp_servers"), format!("{name} 無法讀取")] {
                let error = spawn_gate_error(&message);
                assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
                assert!(!duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error), "{error}");
                assert_eq!(err(&error), Refused(FallbackRefusal::EmployeeToolConfig), "{error}");
            }
        }
    }

    #[test]
    fn refusals_win_over_eligible_text_and_markers_are_anchored() {
        // A work result or a gate refusal that also mentions 500/401 still stops.
        let gate = duduclaw_agent::mcp_spawn_gate::spawn_gate_error("500 401 not logged in");
        assert_eq!(err(&gate), Refused(FallbackRefusal::EmployeeToolConfig));
        let work = WorkOutcome::TimeoutTotal { seconds: 60, partial: "503 overloaded".into() }.encode();
        assert_eq!(err(&work), Refused(FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutTotal)));
        // A marker in the middle of provider text is not a typed refusal.
        let embedded = format!("Grok CLI exited with 1: {}", duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error("x"));
        assert_eq!(err(&embedded), Eligible(FallbackTrigger::ProcessError));
        // Status digits inside a longer number are not a status.
        assert_eq!(err("request id 14012 failed"), Eligible(FallbackTrigger::Unclassified));
    }

    #[test]
    fn every_refusal_has_a_plain_zh_tw_reason() {
        for refusal in [
            FallbackRefusal::Budget, FallbackRefusal::Policy, FallbackRefusal::AutonomyPaused,
            FallbackRefusal::WorkLimit(WorkLimitKind::MaxTurns), FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutTotal),
            FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutNoProgress), FallbackRefusal::EmployeeToolConfig,
            FallbackRefusal::EngineLocalConfig, FallbackRefusal::ContentPolicy, FallbackRefusal::BillingHardLimit,
            FallbackRefusal::RequestRejected, FallbackRefusal::UntypedAuthFailure, FallbackRefusal::PromptTooLarge,
        ] {
            let text = refusal.zh_tw();
            assert!(!text.is_empty() && !text.contains(".json") && !text.contains("mcp"), "{refusal:?}");
            assert!(!refusal.token().is_empty());
        }
    }
}
