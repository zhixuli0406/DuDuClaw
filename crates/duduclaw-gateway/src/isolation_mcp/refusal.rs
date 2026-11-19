//! R8t: why an isolated run failed, for operators — without trusting stderr.
//!
//! The runtime's stderr comes from a process running as the employee inside
//! the sandbox, so it is untrusted. It is never shown, logged or interpreted:
//! [`refusal_reason`] only *selects* one entry of the fixed allow-list shared
//! with the API worker (`duduclaw_api_worker::REFUSAL_REASONS`) and returns
//! that entry's own `&'static str`. Everything derived from it (class, log
//! fields, end-user message) is a fixed string from this module.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use duduclaw_api_worker::{REFUSAL_LINE_PREFIX, REFUSAL_REASONS};

/// Only the last 4 KiB of the drained stderr are looked at.
pub(crate) const SCAN_LIMIT: usize = 4 * 1024;

/// The protected-mode default refusal (also every unrecognised failure).
pub(crate) const DEFAULT_MESSAGE: &str =
    "受保護模式尚無這次工作的隔離執行授權，或所選執行設定尚未通過驗證；請由管理員檢查服務狀態。";

/// The last complete line within the last [`SCAN_LIMIT`] bytes that is
/// exactly `REFUSAL_LINE_PREFIX + reason` for an allow-listed reason. A line
/// cut by the window, or carrying anything else (`\r`, spaces, escapes, NUL),
/// is not recognised. The result is the allow-list's own string, never a
/// slice of `stderr`.
pub(crate) fn refusal_reason(stderr: &[u8]) -> Option<&'static str> {
    let start = stderr.len().saturating_sub(SCAN_LIMIT);
    let mut tail = &stderr[start..];
    if start > 0 && stderr[start - 1] != b'\n' {
        // The window starts inside a line: that partial line is not a line.
        let newline = tail.iter().position(|byte| *byte == b'\n')?;
        tail = &tail[newline + 1..];
    }
    tail.split(|byte| *byte == b'\n').rev().find_map(|line| {
        let reason = line.strip_prefix(REFUSAL_LINE_PREFIX.as_bytes())?;
        REFUSAL_REASONS.iter().copied().find(|known| known.as_bytes() == reason)
    })
}

/// Fixed operator-facing classes of the worker's refusal reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusalClass {
    Launch, Input, McpProjection, EmployeeCredentials, ProviderTransport, ProviderRefused,
    ProviderResponse, McpBridge, Timeout, Limits, Output,
}

impl RefusalClass {
    pub(crate) const ALL: [Self; 11] = [Self::Launch, Self::Input, Self::McpProjection, Self::EmployeeCredentials,
        Self::ProviderTransport, Self::ProviderRefused, Self::ProviderResponse, Self::McpBridge, Self::Timeout,
        Self::Limits, Self::Output];
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Input => "input",
            Self::McpProjection => "mcp_projection",
            Self::EmployeeCredentials => "employee_credentials",
            Self::ProviderTransport => "provider_transport",
            Self::ProviderRefused => "provider_refused",
            Self::ProviderResponse => "provider_response",
            Self::McpBridge => "mcp_bridge",
            Self::Timeout => "timeout",
            Self::Limits => "limits",
            Self::Output => "output",
        }
    }
}

use self::RefusalClass as C;

/// Every allow-listed reason with exactly one class (enforced by tests).
const CLASSES: &[(&str, RefusalClass)] = &[
    ("worker launch refused", C::Launch), ("unsupported platform", C::Launch),
    ("sealed input unavailable", C::Input), ("sealed input invalid", C::Input), ("sealed input read failed", C::Input),
    ("input fencing failed", C::Input), ("input timeout", C::Input), ("input read failed", C::Input),
    ("input mismatch", C::Input), ("request invalid", C::Input), ("invalid request", C::Input),
    ("request too large", C::Input), ("provider request invalid", C::Input), ("invalid Gemini model", C::Input),
    ("MCP projection unavailable", C::McpProjection), ("MCP projection invalid", C::McpProjection),
    ("invalid MCP projection", C::McpProjection),
    ("employee credentials unavailable", C::EmployeeCredentials),
    ("employee credentials invalid", C::EmployeeCredentials),
    ("employee credentials too large", C::EmployeeCredentials),
    ("invalid employee credentials", C::EmployeeCredentials), ("invalid employee credential", C::EmployeeCredentials),
    ("invalid endpoint", C::EmployeeCredentials), ("duplicate employee provider", C::EmployeeCredentials),
    ("employee provider required", C::EmployeeCredentials), ("employee provider unavailable", C::EmployeeCredentials),
    ("HTTP transport unavailable", C::ProviderTransport), ("provider transport failed", C::ProviderTransport),
    ("provider response failed", C::ProviderTransport),
    ("provider refused request", C::ProviderRefused),
    ("provider response invalid", C::ProviderResponse), ("provider usage missing", C::ProviderResponse),
    ("provider choice invalid", C::ProviderResponse), ("provider message missing", C::ProviderResponse),
    ("provider role invalid", C::ProviderResponse), ("provider calls invalid", C::ProviderResponse),
    ("provider completion incomplete", C::ProviderResponse), ("empty provider answer", C::ProviderResponse),
    ("invalid tool call", C::ProviderResponse), ("unadvertised tool", C::ProviderResponse),
    ("invalid tool arguments", C::ProviderResponse), ("duplicate tool call", C::ProviderResponse),
    ("invalid provider field", C::ProviderResponse), ("invalid tool replay", C::ProviderResponse),
    ("invalid replay", C::ProviderResponse), ("invalid replay protocol", C::ProviderResponse),
    ("missing tool replay", C::ProviderResponse), ("invalid role", C::ProviderResponse),
    ("invalid tool", C::ProviderResponse), ("invalid schema", C::ProviderResponse),
    ("invalid protocol", C::ProviderResponse), ("invalid provider message", C::ProviderResponse),
    ("invalid response", C::ProviderResponse), ("unsupported provider block", C::ProviderResponse),
    ("incomplete provider response", C::ProviderResponse), ("unsupported provider content", C::ProviderResponse),
    ("incomplete provider call", C::ProviderResponse), ("invalid candidates", C::ProviderResponse),
    ("invalid arguments", C::ProviderResponse), ("usage overflow", C::ProviderResponse),
    ("MCP bridge unavailable", C::McpBridge), ("MCP input unavailable", C::McpBridge),
    ("MCP output unavailable", C::McpBridge), ("MCP request invalid", C::McpBridge), ("MCP write failed", C::McpBridge),
    ("MCP read failed", C::McpBridge), ("MCP disconnected", C::McpBridge), ("MCP response invalid", C::McpBridge),
    ("MCP request refused", C::McpBridge), ("MCP result missing", C::McpBridge), ("MCP authority changed", C::McpBridge),
    ("incomplete tool inventory", C::McpBridge), ("invalid tool inventory", C::McpBridge),
    ("invalid tool name", C::McpBridge), ("invalid tool schema", C::McpBridge), ("tool result invalid", C::McpBridge),
    ("API execution timeout", C::Timeout),
    ("MCP request too large", C::Limits), ("MCP response too large", C::Limits), ("MCP notification limit", C::Limits),
    ("response too large", C::Limits), ("conversation too large", C::Limits),
    ("provider response too large", C::Limits), ("tool round limit reached", C::Limits),
    ("tool inventory too large", C::Limits),
    ("response invalid", C::Output), ("output failed", C::Output),
];

pub(crate) fn class_of(reason: &str) -> Option<RefusalClass> {
    CLASSES.iter().find(|(known, _)| *known == reason).map(|(_, class)| *class)
}

/// The end-user sentence for a refused protected API run: one of a few fixed
/// zh-TW sentences by class, otherwise [`DEFAULT_MESSAGE`]. No input content.
pub(crate) fn operator_message(class: Option<RefusalClass>) -> &'static str {
    match class {
        Some(RefusalClass::EmployeeCredentials) =>
            "這位 AI 員工尚未設定模型連線資料，或設定內容無法使用；請管理員到『受保護模式 → 模型連線資料』檢查。",
        Some(RefusalClass::ProviderTransport) => "連不到這位 AI 員工設定的模型服務；請管理員檢查端點位址與對外連線。",
        Some(RefusalClass::ProviderRefused) => "模型服務拒絕了這次請求；請管理員檢查這位 AI 員工的金鑰與模型名稱。",
        Some(RefusalClass::ProviderResponse) => "模型服務的回覆無法使用；請管理員檢查端點是否為相容的服務。",
        Some(RefusalClass::Timeout) => "這次工作超過時間上限而停止。",
        _ => DEFAULT_MESSAGE,
    }
}

/// The spawn-gate error carrying [`operator_message`]; the prefix keeps the
/// existing "stop rotation/fallbacks" handling of protected refusals.
pub(crate) fn user_error(class: Option<RefusalClass>) -> String {
    duduclaw_agent::mcp_spawn_gate::spawn_gate_error(operator_message(class))
}

/// How the run ended, for the `exit` log field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunExit { Code(i32), Signal, Timeout, Incomplete }

impl RunExit {
    pub(crate) fn from_status(status: std::process::ExitStatus) -> Self {
        status.code().map_or(Self::Signal, Self::Code)
    }
}

impl std::fmt::Display for RunExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => write!(f, "{code}"),
            Self::Signal => f.write_str("signal"),
            Self::Timeout => f.write_str("timeout"),
            Self::Incomplete => f.write_str("incomplete"),
        }
    }
}

/// The fixed `runtime` log field (a registered CLI's id is not logged).
pub(crate) fn runtime_label(runtime: &duduclaw_isolation::execution::RuntimeKind) -> &'static str {
    use duduclaw_isolation::execution::RuntimeKind;
    match runtime {
        RuntimeKind::Claude => "claude",
        RuntimeKind::Codex => "codex",
        RuntimeKind::Grok => "grok",
        RuntimeKind::OpenAiCompat => "openai_compat",
        RuntimeKind::Registered(_) => "registered",
    }
}

/// The single `isolated run refused` warning. Only fixed strings, the agent
/// label the neighbouring warnings use ([`agent_label`]: an invalid id is
/// replaced, never logged), the exit and the stderr *length*.
pub(crate) fn log_refused(runtime: &'static str, agent: &str, reason: Option<&'static str>, exit: RunExit,
    stderr_len: usize) {
    let agent = agent_label(agent);
    let class = reason.and_then(class_of).map_or("unrecognised", RefusalClass::as_str);
    tracing::warn!(runtime, agent, reason = reason.unwrap_or("unrecognised"), class, exit = %exit, stderr_len,
        "isolated run refused");
}

/// R8x: the fixed `site` log text when a refused protected turn recorded no
/// site (a forgotten site still produces exactly one line, never none). This
/// is log text only, not a [`RefusalSite`]: no site value carries it, and only
/// the two loggers below print it, for `None`.
const UNLABELLED_SITE: &str = "unlabelled";

/// The `site` log value: the recorded site, or the fixed `unlabelled` text.
fn site_text(site: Option<RefusalSite>) -> &'static str {
    site.map_or(UNLABELLED_SITE, RefusalSite::as_str)
}

/// R8x: every place a protected turn can be refused, as the fixed `site` log
/// value. Grouped by prefix: `preflight.*`/`authorize.*` (coarse gates, no
/// authority consumed), `authorization.*` (taking or issuing the one-time
/// run authority), `plan.*` (the root-published run plan), `launch.*` (the
/// protected MCP launch context), `spawn.*` (asking spawnd to start the run;
/// `spawn.backend.*` is spawnd's or the transport's fixed refusal class),
/// `run.*` (the run started and then failed) and `output.*` (the run exited
/// successfully but its result was unusable). Closed set, enforced by the
/// compiler: a site exists only as a [`RefusalSite`], whose sole builder
/// ([`site!`]) rejects at compile time any literal not listed here. The
/// `unlabelled` log text is deliberately not an entry (asserted at compile
/// time below), so no site can carry it.
pub(crate) const SITES: &[&str] = &[
    // Coarse gates (no authority taken or issued yet).
    "preflight.model", "preflight.runtime_policy", "preflight.model_policy", "preflight.redaction",
    "preflight.interceptor", "preflight.mcp_isolation", "preflight.explore_lane", "preflight.utility_call",
    "preflight.claude_options",
    "authorize.agent_dir_missing", "authorize.effort", "authorize.utility_call", "authorize.work_dir_override",
    "authorize.agent_dir", "authorize.capabilities_policy", "authorize.native_gate", "authorize.runtime_kind",
    "authorize.explore_lane", "authorize.tool_restrictions", "authorize.codex_home", "authorize.product_policy",
    "authorize.interceptor", "authorize.runtime_id", "authorize.max_turns",
    "step_limit.agent_dir", "step_limit.unavailable",
    // The one-time run authority.
    "authorization.consumed", "authorization.installed_mismatch", "authorization.agent_label",
    "authorization.home", "authorization.principal_context", "authorization.background_issue",
    "authorization.ingress_issue",
    "authorization.ingress.no_scope", "authorization.ingress.consumed",
    "authorization.ingress.capture.no_web_credential", "authorization.ingress.capture.web_identity",
    "authorization.ingress.capture.channel_conflict", "authorization.ingress.capture.channel_mapping",
    "authorization.ingress.capture.requester", "authorization.ingress.capture.user_db",
    "authorization.ingress.capture.user_inactive", "authorization.ingress.capture.epoch",
    "authorization.ingress.capture.blocking",
    "authorization.ingress.agent_path", "authorization.ingress.home_mismatch", "authorization.ingress.no_turn",
    "authorization.ingress.no_session", "authorization.ingress.no_attended_proof",
    "authorization.ingress.attended_proof_invalid", "authorization.ingress.nested_dispatch",
    "authorization.ingress.principal_mismatch", "authorization.ingress.proof_expired",
    "authorization.ingress.identity_recheck", "authorization.ingress.blocking",
    "authorization.issue.boot_owner", "authorization.issue.product_authority", "authorization.issue.lifecycle",
    "authorization.issue.snapshot", "authorization.issue.epoch", "authorization.issue.principal",
    "authorization.issue.binding", "authorization.issue.policy_revision", "authorization.issue.tools",
    "authorization.issue.human", "authorization.issue.selectors", "authorization.issue.capability",
    "authorization.issue.users_recheck", "authorization.issue.policy_changed",
    "authorization.issue.lifecycle_recheck", "authorization.issue.snapshot_recheck",
    "authorization.issue.facts_recheck",
    // The request and the root-published plan.
    "request.encode",
    "plan.unavailable", "plan.readiness", "plan.boot_owner", "plan.home_mismatch", "plan.employee_unconfirmed",
    "plan.root_publication", "plan.blocking",
    // The protected launch context.
    "launch.readiness", "launch.boot_owner", "launch.boundary_unhealthy", "launch.employee_unconfirmed",
    "launch.employee_mismatch", "launch.product_prepare", "launch.binding", "launch.blocking",
    "launch.resolver", "launch.connectors", "launch.worker_plans", "launch.factories", "launch.dispatcher",
    "launch.protected_launch", "launch.authority_pairing",
    // Asking spawnd to start the run.
    "spawn.prepare", "spawn.not_protected", "spawn.admission",
    "spawn.backend.unavailable", "spawn.backend.denied", "spawn.backend.would_deadlock",
    "spawn.backend.not_found", "spawn.backend.timeout", "spawn.backend.io", "spawn.backend.protocol",
    "spawn.start.permission_denied", "spawn.start.not_found", "spawn.start.timed_out", "spawn.start.other",
    // The run started.
    "run.stdin_missing", "run.stdout_missing", "run.stderr_missing", "run.input", "run.output_drain",
    "run.stderr_drain", "run.wait", "run.refused",
    // The run exited successfully; its result was refused.
    "output.parse", "output.empty", "output.utf8",
];

/// R8x (F3): one entry of [`SITES`], as a type. Every site recorder
/// (`runtime::refuse`/`keep`/`note_site`, [`Sited`], the boot/ingress step
/// labels) takes this, never a string. Built only by [`site!`], which checks
/// the literal at compile time: a label missing from [`SITES`] (including
/// `unlabelled`, which is not an entry) does not compile.
///
/// Closure (R8x review round 2): the field is private to this module, so
/// `RefusalSite(..)` does not compile elsewhere; there is no other public or
/// crate-visible constant or constructor. The one builder,
/// [`RefusalSite::__from_listed`], is crate-visible only because `site!`
/// expands at its call site; it additionally needs a [`__ListedSiteToken`],
/// which only this module can create (private field) and which reaches the
/// macro only through the hidden [`__LISTED_SITE`] constant.
/// `macro_rules!` has no definition-site privacy on stable Rust, so anything
/// the expansion names, crate code could name too: a direct (non-`const`)
/// call is therefore not a compile error, and the backstop is the source-scan
/// test `the_site_builder_is_named_only_by_site_macro`, which fails if either
/// hidden name appears in any crate source file other than this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefusalSite(&'static str);

/// Proof that the caller is [`site!`]'s expansion. Not constructible outside
/// this module (private field); see [`RefusalSite`].
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct __ListedSiteToken(());

/// The one [`__ListedSiteToken`], named only by [`site!`]'s expansion.
#[doc(hidden)]
pub(crate) const __LISTED_SITE: __ListedSiteToken = __ListedSiteToken(());

impl RefusalSite {
    /// Only for [`site!`], which evaluates it in a `const` item, so an
    /// unlisted label is a const-evaluation (build) error in every profile.
    /// Panics on a label outside [`SITES`].
    #[doc(hidden)]
    pub(crate) const fn __from_listed(label: &'static str, _token: __ListedSiteToken) -> Self {
        let mut index = 0;
        while index < SITES.len() {
            if const_str_eq(SITES[index], label) { return Self(SITES[index]); }
            index += 1;
        }
        panic!("unknown refusal site: list the label in isolation_mcp::refusal::SITES")
    }

    /// The fixed `site` log value.
    pub(crate) const fn as_str(self) -> &'static str { self.0 }
}

// Compile-time invariant (every build): `unlabelled` is log text, never a
// listed site, so no `RefusalSite` can carry it.
const _: () = {
    let mut index = 0;
    while index < SITES.len() {
        assert!(!const_str_eq(SITES[index], UNLABELLED_SITE), "`unlabelled` must not be a refusal site");
        index += 1;
    }
};

const fn const_str_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() { return false; }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] { return false; }
        index += 1;
    }
    true
}

/// R8x (F3): the only way to name a refusal site: `site!("plan.readiness")`.
/// Only a literal is accepted, and it is checked against [`SITES`] in a
/// `const` item, so an unknown label is a compile error in every profile,
/// never a run-time panic or a run-time `unlabelled`.
macro_rules! site {
    ($label:literal) => {{
        const SITE: $crate::isolation_mcp::refusal::RefusalSite =
            $crate::isolation_mcp::refusal::RefusalSite::__from_listed(
                $label, $crate::isolation_mcp::refusal::__LISTED_SITE);
        SITE
    }};
}
pub(crate) use site;

/// R8x (F3): a protected helper's refusal — its error together with the site
/// that refused. There is deliberately no `From` conversion from a bare error:
/// a new `?`/`Err(..)` exit in a helper returning `Sited` does not compile
/// without a [`RefusalSite`]. Building one records the site for the current
/// protected turn (first recorded wins; no-op outside a turn).
/// (The protected callers map every refusal to their fixed sentence, so the
/// fields are read only through `Debug` and by tests.)
#[derive(Debug)]
#[allow(dead_code)]
pub struct Sited<E> { site: RefusalSite, error: E }

impl<E> Sited<E> {
    pub(crate) fn new(site: RefusalSite, error: E) -> Self {
        #[cfg(target_os = "linux")]
        super::runtime::note_site(site);
        Self { site, error }
    }
    #[cfg(test)]
    pub(crate) fn site(&self) -> RefusalSite { self.site }
}

/// The `agent` log value: the employee label when it is a valid agent id,
/// otherwise a fixed placeholder (never arbitrary caller text).
pub(crate) fn agent_label(agent: &str) -> &str {
    if duduclaw_core::is_valid_agent_id(agent) { agent } else { "invalid" }
}

// Log volume (R8x review note): this module's three warnings (`log_refused`
// above, the two below) are, together, written at most once per protected
// turn, so their rate is the rate of protected turns the gateway accepted;
// there is deliberately no limiter here. Turns that
// reach a start pass spawn admission (`crate::autonomous_runtime::admit_current`,
// src/autonomous_runtime.rs:302 — the autonomy pause gate, not a rate limit);
// refusals before it are bounded only by the per-ingress admission of turns
// (channel/WebChat/API/scheduler), which is not centralised in one place.

/// R8x: the single warning for a refused protected turn that never reached a
/// started run. Fixed fields only: runtime label, employee label, site label
/// (`unlabelled` when no site was recorded).
pub(crate) fn log_not_started(runtime: &'static str, agent: &str, site: Option<RefusalSite>) {
    let agent = agent_label(agent);
    tracing::warn!(runtime, agent, site = site_text(site), "isolated run not started");
}

/// R8x: the single warning for a run that started but was refused without the
/// R8t line (its pipes were missing, or it exited 0 with unusable output).
/// `site` as in [`log_not_started`].
pub(crate) fn log_refused_after_start(runtime: &'static str, agent: &str, site: Option<RefusalSite>) {
    let agent = agent_label(agent);
    tracing::warn!(runtime, agent, site = site_text(site), "isolated run refused after start");
}

#[cfg(test)]
mod tests {
    use super::*;
    const PREFIX: &[u8] = REFUSAL_LINE_PREFIX.as_bytes();
    const MARKER: &str = "R8T-PLANTED-MARKER-7f3a";

    fn cat(parts: &[&[u8]]) -> Vec<u8> { parts.concat() }
    fn line(reason: &str) -> Vec<u8> { cat(&[PREFIX, reason.as_bytes(), b"\n"]) }

    #[test]
    fn recognised_line_returns_the_allow_list_entry_not_the_input() {
        for reason in REFUSAL_REASONS {
            let stderr = line(reason);
            let found = refusal_reason(&stderr).unwrap_or_else(|| panic!("{reason}"));
            assert_eq!(found, *reason);
            let input = stderr.as_ptr_range();
            assert!(!input.contains(&found.as_ptr()), "{reason}");
            assert!(!input.contains(&found.as_ptr().wrapping_add(found.len() - 1)), "{reason}");
        }
        // Without the trailing newline too.
        let stderr = cat(&[PREFIX, b"input mismatch"]);
        assert_eq!(refusal_reason(&stderr), Some("input mismatch"));
    }

    #[test]
    fn unknown_or_decorated_reasons_are_not_recognised() {
        let known = line("input mismatch");
        for stderr in [
            line("not a known reason"), line(MARKER), line(""), line("input mismatch "), line(" input mismatch"),
            line("Input mismatch"), line("input mismatch\r"), line("input mismatch\0"),
            line("\u{1b}[31minput mismatch\u{1b}[0m"), line("input mismatch: extra"),
            cat(&[b" ", &known]), cat(&[b"\0", &known]), cat(&[b"\x1b[0m", &known]), cat(&[b"prefix ", &known]),
            b"isolated API execution unavailable:input mismatch\n".to_vec(),
            b"isolated API execution unavailable: input mismatch\r\n".to_vec(),
            Vec::new(), b"\n\n\n".to_vec(),
        ] {
            assert_eq!(refusal_reason(&stderr), None, "{:?}", String::from_utf8_lossy(&stderr));
        }
    }

    #[test]
    fn the_last_recognised_line_wins_and_noise_lines_are_skipped() {
        let stderr = cat(&[&line("input mismatch"), b"noise\n", &line("provider refused request"),
            &line("not a known reason"), b"trailing noise without newline"]);
        assert_eq!(refusal_reason(&stderr), Some("provider refused request"));
    }

    #[test]
    fn only_the_last_four_kib_are_scanned() {
        let noise = vec![b'x'; 8 * 1024];
        let target = line("API execution timeout");
        // Long noise ending in a newline, then the line: found.
        let stderr = cat(&[&noise, b"\n", &target]);
        assert_eq!(refusal_reason(&stderr), Some("API execution timeout"));
        // The line, then more than 4 KiB of noise: not found.
        assert_eq!(refusal_reason(&cat(&[&target, &noise])), None);
        // A line that starts one byte before the window is cut: not a line.
        let filler = vec![b'y'; SCAN_LIMIT - target.len() + 1];
        assert_eq!(refusal_reason(&cat(&[&noise, b"\n", &target, &filler])), None);
        // A window cut mid-line exactly at the prefix is not taken as a line.
        let filler = vec![b'\n'; SCAN_LIMIT - target.len()];
        let stderr = cat(&[b"AAAA", &target, &filler]);
        assert_eq!(stderr.len() - SCAN_LIMIT, 4);
        assert_eq!(refusal_reason(&stderr), None);
        // The same line starting exactly at the window, after a newline: found.
        let stderr = cat(&[&noise, b"\n", &target, &filler]);
        assert_eq!(refusal_reason(&stderr), Some("API execution timeout"));
    }

    #[test]
    fn invalid_utf8_around_the_line_is_tolerated() {
        let stderr = cat(&[b"\xff\xfe\xc3\n", &line("provider transport failed"), b"\x80\x80\n"]);
        assert_eq!(refusal_reason(&stderr), Some("provider transport failed"));
        assert_eq!(refusal_reason(&cat(&[PREFIX, b"provider transport failed\xff\n"])), None);
    }

    #[test]
    fn every_allow_listed_reason_has_exactly_one_class_and_no_extra_entry() {
        for reason in REFUSAL_REASONS {
            assert_eq!(CLASSES.iter().filter(|(known, _)| known == reason).count(), 1, "{reason}");
            assert!(class_of(reason).is_some(), "{reason}");
        }
        for (known, _) in CLASSES { assert!(REFUSAL_REASONS.contains(known), "{known}"); }
        assert_eq!(CLASSES.len(), REFUSAL_REASONS.len());
        for class in RefusalClass::ALL {
            assert!(CLASSES.iter().any(|(_, c)| *c == class), "{class:?} is unused");
        }
        assert_eq!(class_of("employee credentials unavailable"), Some(RefusalClass::EmployeeCredentials));
        assert_eq!(class_of("provider transport failed"), Some(RefusalClass::ProviderTransport));
        assert_eq!(class_of("provider refused request"), Some(RefusalClass::ProviderRefused));
        assert_eq!(class_of("provider response invalid"), Some(RefusalClass::ProviderResponse));
        assert_eq!(class_of("API execution timeout"), Some(RefusalClass::Timeout));
        assert_eq!(class_of(MARKER), None);
    }

    #[test]
    fn messages_are_fixed_spawn_gate_errors_without_input_bytes() {
        let mut distinct = std::collections::BTreeSet::new();
        for class in RefusalClass::ALL.into_iter().map(Some).chain([None]) {
            let error = user_error(class);
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
            assert_eq!(duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&error), Some(operator_message(class)));
            distinct.insert(operator_message(class));
        }
        // Five per-class sentences plus the default.
        assert_eq!(distinct.len(), 6);
        assert_eq!(operator_message(None), DEFAULT_MESSAGE);
        assert_eq!(operator_message(Some(RefusalClass::McpBridge)), DEFAULT_MESSAGE);
        // A marker planted in stderr never reaches the message.
        for reason in REFUSAL_REASONS {
            let stderr = cat(&[MARKER.as_bytes(), b"\n", &line(reason), MARKER.as_bytes()]);
            let error = user_error(refusal_reason(&stderr).and_then(class_of));
            assert!(!error.contains(MARKER) && !error.contains(reason), "{reason}: {error}");
        }
    }

    #[derive(Clone)]
    struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    #[test]
    fn the_log_line_has_fixed_fields_and_never_stderr_bytes() {
        for (stderr, reason, class) in [
            (cat(&[MARKER.as_bytes(), b"\n", &line("provider refused request")]), "provider refused request", "provider_refused"),
            (cat(&[&line("provider refused request"), MARKER.as_bytes()]), "provider refused request", "provider_refused"),
            (line(MARKER), "unrecognised", "unrecognised"),
            (format!("{MARKER} sk-secret-key\n").into_bytes(), "unrecognised", "unrecognised"),
        ] {
            let log = LogCapture(Default::default());
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            tracing::subscriber::with_default(subscriber, ||
                log_refused("openai_compat", "alice", refusal_reason(&stderr), RunExit::Code(125), stderr.len()));
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("isolated run refused"), "{logs}");
            assert!(!logs.contains(MARKER) && !logs.contains("sk-secret"), "{logs}");
            for field in ["runtime=\"openai_compat\"", "agent=\"alice\"", "exit=125"] {
                assert!(logs.contains(field), "{field}: {logs}");
            }
            assert!(logs.contains(&format!("reason=\"{reason}\"")), "{logs}");
            assert!(logs.contains(&format!("class=\"{class}\"")), "{logs}");
            assert!(logs.contains(&format!("stderr_len={}", stderr.len())), "{logs}");
        }
        assert_eq!(RunExit::Timeout.to_string(), "timeout");
        assert_eq!(RunExit::Signal.to_string(), "signal");
        assert_eq!(RunExit::Incomplete.to_string(), "incomplete");
    }

    /// R8x: the two new lines carry fixed fields only; an agent label that is
    /// not a valid agent id is replaced, never logged.
    #[test]
    fn the_not_started_lines_have_fixed_fields_only() {
        for (agent, shown) in [("alice", "alice"), ("../x y", "invalid")] {
            for after_start in [false, true] {
                let log = LogCapture(Default::default());
                let writer = log.clone();
                let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                    .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
                tracing::subscriber::with_default(subscriber, || if after_start {
                    log_refused_after_start("openai_compat", agent, Some(site!("output.empty")))
                } else {
                    log_not_started("openai_compat", agent, Some(site!("plan.root_publication")))
                });
                let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
                assert_eq!(logs.lines().count(), 1, "{logs}");
                let (message, site) = if after_start { ("isolated run refused after start", "output.empty") }
                    else { ("isolated run not started", "plan.root_publication") };
                assert!(logs.contains(message) && logs.contains(&format!("site=\"{site}\"")), "{logs}");
                assert!(logs.contains("runtime=\"openai_compat\"") && logs.contains(&format!("agent=\"{shown}\"")), "{logs}");
                assert!(!logs.contains("../x"), "{logs}");
                assert!(SITES.contains(&site));
            }
        }
    }

    /// R8x F2: the R8t line applies the same agent label rule as the R8x
    /// lines: an id that is not a valid agent id is logged as `invalid`.
    #[test]
    fn the_refused_line_never_logs_an_invalid_agent_label() {
        const AGENT: &str = "R8X-F2 agent\nforged=\"line\"";
        let log = LogCapture(Default::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
            .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
        tracing::subscriber::with_default(subscriber, ||
            log_refused("registered", AGENT, None, RunExit::Code(1), 0));
        let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        assert_eq!(logs.lines().count(), 1, "{logs}");
        assert!(logs.contains("isolated run refused") && logs.contains("agent=\"invalid\""), "{logs}");
        for part in ["R8X-F2", "forged", "agent\\n", "\nforged"] {
            assert!(!logs.contains(part), "{part}: {logs}");
        }
        assert_eq!(agent_label(AGENT), "invalid");
        assert_eq!(agent_label("alice"), "alice");
    }

    /// R8x: with no recorded site both lines print the fixed `unlabelled`
    /// text — the logger's own fallback, not a site value.
    #[test]
    fn a_turn_without_a_site_logs_unlabelled() {
        for after_start in [false, true] {
            let log = LogCapture(Default::default());
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            tracing::subscriber::with_default(subscriber, || if after_start {
                log_refused_after_start("openai_compat", "alice", None)
            } else {
                log_not_started("openai_compat", "alice", None)
            });
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("site=\"unlabelled\""), "{logs}");
        }
        assert_eq!(site_text(None), "unlabelled");
        assert!(!SITES.contains(&"unlabelled"));
    }

    /// R8x review round 2 (a): every [`SITES`] entry round-trips through the
    /// exact builder `site!` expands to, evaluated in a `const` item, so a
    /// failure here is a build error of the test target, not a test failure.
    #[test]
    fn every_listed_site_round_trips_at_compile_time() {
        const ROUND_TRIP: usize = {
            let mut index = 0;
            while index < SITES.len() {
                let site = RefusalSite::__from_listed(SITES[index], __LISTED_SITE);
                assert!(const_str_eq(site.as_str(), SITES[index]), "a listed site does not round-trip");
                index += 1;
            }
            index
        };
        assert_eq!(ROUND_TRIP, SITES.len());
        // And through the macro itself, for a sample of literals.
        const FROM_MACRO: [RefusalSite; 3] = [site!("preflight.model"), site!("spawn.admission"), site!("output.utf8")];
        assert_eq!(FROM_MACRO.map(RefusalSite::as_str), ["preflight.model", "spawn.admission", "output.utf8"]);
    }

    /// R8x F3: the site type is closed. `site!` with an unlisted label (or
    /// `unlabelled`) is a compile error — that is the primary guard and has no
    /// run-time test; here the same builder is called outside a `const` (only
    /// this module may do so, see the source-scan test below), where it panics
    /// instead.
    #[test]
    fn refusal_sites_are_a_closed_type() {
        for label in ["plan.not_a_site", "", UNLABELLED_SITE, "plan.readiness ", "Plan.readiness"] {
            assert!(std::panic::catch_unwind(|| RefusalSite::__from_listed(label, __LISTED_SITE)).is_err(), "{label:?}");
        }
        assert_eq!(site!("spawn.admission").as_str(), "spawn.admission");
        assert_eq!(Sited::new(site!("launch.binding"), ()).site(), site!("launch.binding"));
    }

    /// R8x review round 2 (b): the backstop for what `macro_rules!` cannot
    /// make private. Every `.rs` file under this crate's `src/` other than
    /// this one must not name the site builder or its token (only `site!`'s
    /// expansion does, and that text lives here); in this file's production
    /// part, each hidden name appears only at its definition and in `site!`,
    /// and the tuple constructor only in the builder.
    #[test]
    fn the_site_builder_is_named_only_by_site_macro() {
        const HIDDEN: [&str; 3] = ["__from_listed", "__LISTED_SITE", "__ListedSiteToken"];
        fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display())) {
                let path = entry.unwrap().path();
                if path.is_dir() { rust_files(&path, out); }
                else if path.extension().is_some_and(|ext| ext == "rs") { out.push(path); }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        let this = src.join("isolation_mcp").join("refusal.rs");
        assert!(files.contains(&this), "scan did not find refusal.rs");
        assert!(files.len() > 10, "scan found too few files: {}", files.len());
        for file in files.iter().filter(|file| **file != this) {
            let text = std::fs::read_to_string(file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            for name in HIDDEN {
                assert!(!text.contains(name), "{} names the site builder `{name}` outside site!", file.display());
            }
        }
        let source = include_str!("refusal.rs");
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        assert!(production.len() < source.len(), "test-module marker not found");
        // Definition + `site!`.
        assert_eq!(production.matches("fn __from_listed(").count(), 1);
        assert_eq!(production.matches("RefusalSite::__from_listed(").count(), 1);
        assert_eq!(production.matches("__from_listed").count(), 2 + doc_mentions(production, "__from_listed"));
        assert_eq!(production.matches("const __LISTED_SITE:").count(), 1);
        assert_eq!(production.matches("refusal::__LISTED_SITE)").count(), 1);
        assert_eq!(production.matches("__LISTED_SITE").count(), 2 + doc_mentions(production, "__LISTED_SITE"));
        // The tuple constructors: the token's one value and the builder's return.
        assert_eq!(production.matches("__ListedSiteToken(())").count(), 2, "struct definition + the one value");
        assert_eq!(production.matches("Self(").count(), 1, "RefusalSite is built only in __from_listed");
    }

    /// Mentions of `name` inside doc/line comments of `source` (not code).
    fn doc_mentions(source: &str, name: &str) -> usize {
        source.lines().filter(|line| line.trim_start().starts_with("//")).map(|line| line.matches(name).count()).sum()
    }

    #[test]
    fn runtime_labels_are_fixed() {
        use duduclaw_isolation::{bounded::Text, execution::RuntimeKind};
        let registered = RuntimeKind::Registered(Text::new("generic-cli-v1.qwen".into()).unwrap());
        assert_eq!(runtime_label(&registered), "registered");
        assert_eq!(runtime_label(&RuntimeKind::OpenAiCompat), "openai_compat");
    }
}
