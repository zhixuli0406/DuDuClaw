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
//! kinds recognised. A string the table cannot type keeps the v1.83.0
//! behaviour for that string (the legacy 4xx rule refuses; anything else
//! falls back) and is reported as [`FallbackTrigger::Unclassified`].
//!
//! The fallback leg always runs its own full gates: `execute_with_failover`
//! re-checks product policy and the budget for the fallback runtime and model,
//! and the fallback runtime's own `execute` runs its spawn gate
//! (`prepare_mcp_config_for_spawn`) exactly as a primary call would.

use crate::work_outcome::WorkOutcome;

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
    /// `crate::autonomous_runtime::was_denied()` sampled for this call.
    Error { error: &'a str, autonomy_denied: bool },
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
    let (error, autonomy_denied) = match failure {
        PrimaryFailure::NotRegistered => return Eligible(FallbackTrigger::NotRegistered),
        PrimaryFailure::Cooldown => return Eligible(FallbackTrigger::Cooldown),
        PrimaryFailure::Empty => return Eligible(FallbackTrigger::EmptyReply),
        PrimaryFailure::Error { error, autonomy_denied } => (error, autonomy_denied),
    };

    // ── Refusals (checked first; fail closed) ──────────────────────────
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
        return if engine_local_config_refusal_falls_back() {
            Eligible(FallbackTrigger::EngineLocalConfig)
        } else {
            Refused(FallbackRefusal::EngineLocalConfig)
        };
    }
    if duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(error) {
        return Refused(FallbackRefusal::EmployeeToolConfig);
    }
    let lower = error.to_lowercase();
    // `duduclaw_core::product_profile` refusals raised inside a runtime.
    if lower.contains("product profile does not allow") {
        return Refused(FallbackRefusal::Policy);
    }
    if lower.contains("content_policy_violation") || lower.contains("content policy violation") {
        return Refused(FallbackRefusal::ContentPolicy);
    }
    if lower.contains("billing_hard_limit") {
        return Refused(FallbackRefusal::BillingHardLimit);
    }

    // ── Eligible kinds ─────────────────────────────────────────────────
    if is_auth_failure(&lower) {
        return Eligible(FallbackTrigger::AuthFailure);
    }
    if is_model_unavailable(&lower) {
        return Eligible(FallbackTrigger::ModelUnavailable);
    }
    if is_server_or_rate_limit(error, &lower) {
        return Eligible(FallbackTrigger::ServerOrRateLimit);
    }
    // v1.83.0 refused these 4xx strings; no new rule types them.
    if legacy_request_rejected(&lower) {
        return Refused(FallbackRefusal::RequestRejected);
    }
    if is_process_error(&lower) {
        return Eligible(FallbackTrigger::ProcessError);
    }
    Eligible(FallbackTrigger::Unclassified)
}

/// One switch, so the pending owner decision is a one-line change.
fn engine_local_config_refusal_falls_back() -> bool {
    ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK
}

/// `"401 "`-style status mention (not inside a longer number, followed by a
/// space, `)`, `:` or `,`), as the runtimes format them
/// (`API error (401 Unauthorized)`, `HTTP 403 …`). Char-based, no slicing.
fn has_status(lower: &str, codes: &[&str]) -> bool {
    let chars: Vec<char> = lower.chars().collect();
    codes.iter().any(|code| {
        let code: Vec<char> = code.chars().collect();
        chars.windows(code.len()).enumerate().any(|(at, window)| {
            window == code.as_slice()
                && (at == 0 || !chars[at - 1].is_ascii_digit())
                && matches!(chars.get(at + code.len()).copied(), Some(' ' | ')' | ':' | ','))
        })
    })
}

/// The v1.83.0 `is_non_retryable` 4xx substrings, kept verbatim for strings
/// no new rule types.
fn legacy_request_rejected(lower: &str) -> bool {
    lower.contains("400 ") || lower.contains("404 ") || lower.contains("422 ")
        || lower.contains("400 bad request")
}

fn is_auth_failure(lower: &str) -> bool {
    has_status(lower, &["401", "403"])
        || [
            "unauthorized",
            "forbidden",
            "invalid_api_key",
            "invalid api key",
            "incorrect api key",
            "invalid bearer token",
            "not logged in",
            "not signed in",
            "not authenticated",
            "unauthenticated",
            "authentication failed",
            "authentication failure",
            "authentication required",
            "authentication_failed",
            "login required",
            "login expired",
            "session expired",
            "oauth access token is invalid",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn is_model_unavailable(lower: &str) -> bool {
    [
        "model_not_found",
        "model not found",
        "no such model",
        "unknown model",
        "invalid model",
        "unsupported model",
        "model is not supported",
        "model does not exist",
        "model is not available",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || (has_status(lower, &["400", "404", "422"]) && lower.contains("model"))
}

fn is_server_or_rate_limit(error: &str, lower: &str) -> bool {
    has_status(lower, &["429", "500", "502", "503", "504", "529"])
        || crate::claude_runner::is_rate_limit_error(error)
        || [
            "internal server error",
            "bad gateway",
            "service unavailable",
            "gateway timeout",
            "overloaded",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn is_process_error(lower: &str) -> bool {
    [
        "empty response",
        "exited with",
        "exit code",
        "exit status",
        "non-zero exit",
        "spawn error",
        "failed to run",
        "failed to start",
        "no such file",
        "killed by signal",
        "terminated by signal",
        "crashed",
        "panicked",
        "broken pipe",
        "read error",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use FallbackDecision::{Eligible, Refused};

    fn err(error: &str) -> FallbackDecision {
        classify_primary_failure(PrimaryFailure::Error { error, autonomy_denied: false })
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
            ("API error (400 Bad Request): content_policy_violation", Refused(FallbackRefusal::ContentPolicy)),
            ("Content policy violation detected", Refused(FallbackRefusal::ContentPolicy)),
            ("API error (429 Too Many Requests): billing_hard_limit_reached", Refused(FallbackRefusal::BillingHardLimit)),
            ("400 bad request: invalid JSON", Refused(FallbackRefusal::RequestRejected)),
            ("API error (422 Unprocessable Entity): messages too long", Refused(FallbackRefusal::RequestRejected)),
            // ── Eligible ──
            (engine_local.as_str(), Eligible(FallbackTrigger::EngineLocalConfig)),
            ("Empty response from Codex CLI (exit 0); stderr tail: ", Eligible(FallbackTrigger::ProcessError)),
            ("Grok CLI exited with 1: boom", Eligible(FallbackTrigger::ProcessError)),
            ("spawn error: failed to start kimi CLI: No such file or directory", Eligible(FallbackTrigger::ProcessError)),
            ("Codex CLI exited with code 101: thread 'main' panicked", Eligible(FallbackTrigger::ProcessError)),
            ("401 invalid bearer token; misleading claude /login", Eligible(FallbackTrigger::AuthFailure)),
            ("API error (403 Forbidden): access denied", Eligible(FallbackTrigger::AuthFailure)),
            ("invalid_api_key: Incorrect API key provided", Eligible(FallbackTrigger::AuthFailure)),
            ("Grok CLI authentication failure (not logged in / 憑證失效): stderr tail: x", Eligible(FallbackTrigger::AuthFailure)),
            ("kimi CLI is not logged in (authentication required): run login", Eligible(FallbackTrigger::AuthFailure)),
            ("codex: your login expired, please sign in again", Eligible(FallbackTrigger::AuthFailure)),
            ("API error (404 Not Found): The model `opus` does not exist", Eligible(FallbackTrigger::ModelUnavailable)),
            ("API error (400 Bad Request): invalid model 'opus'", Eligible(FallbackTrigger::ModelUnavailable)),
            ("API error (422 Unprocessable Entity): unknown model grok-x", Eligible(FallbackTrigger::ModelUnavailable)),
            ("error: model_not_found", Eligible(FallbackTrigger::ModelUnavailable)),
            ("500 internal server error", Eligible(FallbackTrigger::ServerOrRateLimit)),
            ("API error (503 Service Unavailable): overloaded", Eligible(FallbackTrigger::ServerOrRateLimit)),
            ("429 rate limited", Eligible(FallbackTrigger::ServerOrRateLimit)),
            ("SSE error (502 Bad Gateway): upstream", Eligible(FallbackTrigger::ServerOrRateLimit)),
            // Untyped: keeps the v1.83.0 behaviour (falls back).
            ("connection refused", Eligible(FallbackTrigger::Unclassified)),
            ("connection timed out", Eligible(FallbackTrigger::Unclassified)),
        ];
        for (error, expected) in rows {
            assert_eq!(err(error), expected, "{error}");
        }
        assert_eq!(classify_primary_failure(PrimaryFailure::NotRegistered), Eligible(FallbackTrigger::NotRegistered));
        assert_eq!(classify_primary_failure(PrimaryFailure::Cooldown), Eligible(FallbackTrigger::Cooldown));
        assert_eq!(classify_primary_failure(PrimaryFailure::Empty), Eligible(FallbackTrigger::EmptyReply));
        // The autonomy gate's task-local verdict refuses whatever the text says.
        assert_eq!(
            classify_primary_failure(PrimaryFailure::Error { error: "Failed to run stub: paused", autonomy_denied: true }),
            Refused(FallbackRefusal::AutonomyPaused)
        );
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
            FallbackRefusal::RequestRejected,
        ] {
            let text = refusal.zh_tw();
            assert!(!text.is_empty() && !text.contains(".json") && !text.contains("mcp"), "{refusal:?}");
            assert!(!refusal.token().is_empty());
        }
    }
}
