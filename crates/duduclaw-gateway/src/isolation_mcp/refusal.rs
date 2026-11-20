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

/// R8x review round 3: the ONE list of refusal sites. Each entry
/// `Variant => "label"` generates, from this single source:
/// * the [`RefusalSite`] variant and its arm in [`RefusalSite::as_str`],
/// * its entry in [`RefusalSite::ALL`] and in [`SITES`] (same order),
/// * one arm `("label") => { …::RefusalSite::Variant }` of [`site!`],
/// * one `const` assertion that `site!("label").as_str() == "label"`.
///
/// The inner `site!` arms need no metavariables (they name the variant by
/// the absolute `crate::` path, the macro being crate-local), so there is no
/// `$`-escaping problem on stable. Labels are captured as `tt` so they reach
/// the inner matcher as plain literal tokens, not opaque fragments.
macro_rules! sites {
    ($($variant:ident => $label:tt),+ $(,)?) => {
        /// R8x (F3): one entry of [`SITES`], as a type. Every site recorder
        /// (`runtime::refuse`/`keep`/`note_site`, [`Sited`], the boot/ingress
        /// step labels) takes this, never a string.
        ///
        /// Closure (R8x review round 3): a plain enum generated from the one
        /// list in `sites!`. There is no constructor, no builder and no
        /// function from a string to a site; [`site!`] is a literal-token
        /// lookup with one arm per listed label, so an unlisted literal
        /// (including `unlabelled`, which is not an entry) is a "no rules
        /// expected" compile error in every profile.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub(crate) enum RefusalSite { $($variant),+ }

        impl RefusalSite {
            /// Every site, in [`SITES`] order.
            pub(crate) const ALL: &'static [RefusalSite] = &[$(RefusalSite::$variant),+];

            /// The fixed `site` log value.
            pub(crate) const fn as_str(self) -> &'static str {
                match self { $(RefusalSite::$variant => $label),+ }
            }
        }

        /// R8x: every place a protected turn can be refused, as the fixed
        /// `site` log value. Grouped by prefix: `preflight.*`/`authorize.*`
        /// (coarse gates, no authority consumed), `utility.*` (a utility call
        /// refused by design before any runtime, see
        /// [`note_utility_refusal`]), `authorization.*` (taking
        /// or issuing the one-time run authority), `plan.*` (the
        /// root-published run plan), `launch.*` (the protected MCP launch
        /// context), `spawn.*` (asking spawnd to start the run;
        /// `spawn.backend.*` is spawnd's or the transport's fixed refusal
        /// class), `run.*` (the run started and then failed) and `output.*`
        /// (the run exited successfully but its result was unusable). Closed
        /// set, enforced by the compiler: generated with [`RefusalSite`] and
        /// [`site!`] from the one `sites!` list. The `unlabelled` log text is
        /// deliberately not an entry (asserted at compile time below).
        pub(crate) const SITES: &[&str] = &[$($label),+];

        /// R8x (F3): the only way to name a refusal site:
        /// `site!("plan.readiness")`. One arm per listed label; any other
        /// token is a compile error, never a run-time panic or `unlabelled`.
        macro_rules! site {
            $( ($label) => { crate::isolation_mcp::refusal::RefusalSite::$variant }; )+
        }
        pub(crate) use site;

        // One compile-time assertion per arm: `site!` of the label yields
        // the variant whose `as_str()` is that label.
        $( const _: () = assert!(const_str_eq(site!($label).as_str(), $label),
            "a site! arm does not map to its own label"); )+

        /// Every `site!` arm, evaluated, in [`SITES`] order (for tests).
        #[cfg(test)]
        const SITE_ARMS: &[RefusalSite] = &[$(site!($label)),+];
    };
}

sites! {
    // Coarse gates (no authority taken or issued yet).
    PreflightModel => "preflight.model",
    PreflightRuntimePolicy => "preflight.runtime_policy",
    PreflightModelPolicy => "preflight.model_policy",
    PreflightRedaction => "preflight.redaction",
    PreflightInterceptor => "preflight.interceptor",
    PreflightMcpIsolation => "preflight.mcp_isolation",
    PreflightExploreLane => "preflight.explore_lane",
    PreflightUtilityCall => "preflight.utility_call",
    PreflightClaudeOptions => "preflight.claude_options",
    AuthorizeAgentDirMissing => "authorize.agent_dir_missing",
    AuthorizeEffort => "authorize.effort",
    AuthorizeUtilityCall => "authorize.utility_call",
    AuthorizeWorkDirOverride => "authorize.work_dir_override",
    AuthorizeAgentDir => "authorize.agent_dir",
    AuthorizeCapabilitiesPolicy => "authorize.capabilities_policy",
    AuthorizeNativeGate => "authorize.native_gate",
    AuthorizeRuntimeKind => "authorize.runtime_kind",
    AuthorizeExploreLane => "authorize.explore_lane",
    AuthorizeToolRestrictions => "authorize.tool_restrictions",
    AuthorizeCodexHome => "authorize.codex_home",
    AuthorizeProductPolicy => "authorize.product_policy",
    AuthorizeInterceptor => "authorize.interceptor",
    AuthorizeRuntimeId => "authorize.runtime_id",
    AuthorizeMaxTurns => "authorize.max_turns",
    StepLimitAgentDir => "step_limit.agent_dir",
    StepLimitUnavailable => "step_limit.unavailable",
    // B0: utility calls refused by design before any runtime is selected or
    // entered (`note_utility_refusal`). `utility.in_run_judge`: a judge called
    // from inside an isolated MCP request (`isolation_egress`'s refusing
    // runner); `utility.global_fallback`: an employee whose runtime has no
    // utility profile would have used the global utility provider
    // (`runtime_config::resolve_utility_profile`).
    UtilityInRunJudge => "utility.in_run_judge",
    UtilityGlobalFallback => "utility.global_fallback",
    // The one-time run authority.
    AuthorizationConsumed => "authorization.consumed",
    AuthorizationInstalledMismatch => "authorization.installed_mismatch",
    AuthorizationAgentLabel => "authorization.agent_label",
    AuthorizationHome => "authorization.home",
    AuthorizationPrincipalContext => "authorization.principal_context",
    AuthorizationBackgroundIssue => "authorization.background_issue",
    AuthorizationIngressIssue => "authorization.ingress_issue",
    AuthorizationIngressNoScope => "authorization.ingress.no_scope",
    AuthorizationIngressConsumed => "authorization.ingress.consumed",
    AuthorizationIngressCaptureNoWebCredential => "authorization.ingress.capture.no_web_credential",
    AuthorizationIngressCaptureWebIdentity => "authorization.ingress.capture.web_identity",
    AuthorizationIngressCaptureChannelConflict => "authorization.ingress.capture.channel_conflict",
    AuthorizationIngressCaptureChannelMapping => "authorization.ingress.capture.channel_mapping",
    AuthorizationIngressCaptureRequester => "authorization.ingress.capture.requester",
    AuthorizationIngressCaptureUserDb => "authorization.ingress.capture.user_db",
    AuthorizationIngressCaptureUserInactive => "authorization.ingress.capture.user_inactive",
    AuthorizationIngressCaptureEpoch => "authorization.ingress.capture.epoch",
    AuthorizationIngressCaptureBlocking => "authorization.ingress.capture.blocking",
    AuthorizationIngressAgentPath => "authorization.ingress.agent_path",
    AuthorizationIngressHomeMismatch => "authorization.ingress.home_mismatch",
    AuthorizationIngressNoTurn => "authorization.ingress.no_turn",
    AuthorizationIngressNoSession => "authorization.ingress.no_session",
    AuthorizationIngressNoAttendedProof => "authorization.ingress.no_attended_proof",
    AuthorizationIngressAttendedProofInvalid => "authorization.ingress.attended_proof_invalid",
    AuthorizationIngressNestedDispatch => "authorization.ingress.nested_dispatch",
    AuthorizationIngressPrincipalMismatch => "authorization.ingress.principal_mismatch",
    AuthorizationIngressProofExpired => "authorization.ingress.proof_expired",
    AuthorizationIngressIdentityRecheck => "authorization.ingress.identity_recheck",
    AuthorizationIngressBlocking => "authorization.ingress.blocking",
    AuthorizationIssueBootOwner => "authorization.issue.boot_owner",
    AuthorizationIssueProductAuthority => "authorization.issue.product_authority",
    AuthorizationIssueLifecycle => "authorization.issue.lifecycle",
    AuthorizationIssueSnapshot => "authorization.issue.snapshot",
    AuthorizationIssueEpoch => "authorization.issue.epoch",
    AuthorizationIssuePrincipal => "authorization.issue.principal",
    AuthorizationIssueBinding => "authorization.issue.binding",
    AuthorizationIssuePolicyRevision => "authorization.issue.policy_revision",
    AuthorizationIssueTools => "authorization.issue.tools",
    AuthorizationIssueHuman => "authorization.issue.human",
    AuthorizationIssueSelectors => "authorization.issue.selectors",
    AuthorizationIssueCapability => "authorization.issue.capability",
    AuthorizationIssueUsersRecheck => "authorization.issue.users_recheck",
    AuthorizationIssuePolicyChanged => "authorization.issue.policy_changed",
    AuthorizationIssueLifecycleRecheck => "authorization.issue.lifecycle_recheck",
    AuthorizationIssueSnapshotRecheck => "authorization.issue.snapshot_recheck",
    AuthorizationIssueFactsRecheck => "authorization.issue.facts_recheck",
    // The request and the root-published plan.
    RequestEncode => "request.encode",
    PlanUnavailable => "plan.unavailable",
    PlanReadiness => "plan.readiness",
    PlanBootOwner => "plan.boot_owner",
    PlanHomeMismatch => "plan.home_mismatch",
    PlanEmployeeUnconfirmed => "plan.employee_unconfirmed",
    PlanRootPublication => "plan.root_publication",
    PlanBlocking => "plan.blocking",
    // The protected launch context.
    LaunchReadiness => "launch.readiness",
    LaunchBootOwner => "launch.boot_owner",
    LaunchBoundaryUnhealthy => "launch.boundary_unhealthy",
    LaunchEmployeeUnconfirmed => "launch.employee_unconfirmed",
    LaunchEmployeeMismatch => "launch.employee_mismatch",
    LaunchProductPrepare => "launch.product_prepare",
    LaunchBinding => "launch.binding",
    LaunchBlocking => "launch.blocking",
    LaunchResolver => "launch.resolver",
    LaunchConnectors => "launch.connectors",
    LaunchWorkerPlans => "launch.worker_plans",
    LaunchFactories => "launch.factories",
    LaunchDispatcher => "launch.dispatcher",
    LaunchProtectedLaunch => "launch.protected_launch",
    LaunchAuthorityPairing => "launch.authority_pairing",
    // R8z-2: the launch preparation after spawnd reserved the run
    // (`launch::LaunchStep`: the gateway's run binding, then the MCP-boundary
    // launch). Recorded before the outer `spawn.backend.*` mapping.
    LaunchBindPreparedMissing => "launch.bind.prepared_missing",
    LaunchBindStateLock => "launch.bind.state_lock",
    LaunchBindRevokedBefore => "launch.bind.revoked_before",
    LaunchBindRecheckBackground => "launch.bind.recheck.background",
    LaunchBindRecheckExpired => "launch.bind.recheck.expired",
    LaunchBindRecheckAgent => "launch.bind.recheck.agent",
    LaunchBindRecheckIdentity => "launch.bind.recheck.identity",
    LaunchBindRecheckPolicyRevision => "launch.bind.recheck.policy_revision",
    LaunchBindRecheckRevisionChanged => "launch.bind.recheck.revision_changed",
    LaunchBindRecheckEpoch => "launch.bind.recheck.epoch",
    LaunchBindLiveSnapshot => "launch.bind.live_snapshot",
    LaunchBindBindingMismatch => "launch.bind.binding_mismatch",
    LaunchBindEpochAfterSnapshot => "launch.bind.epoch_after_snapshot",
    LaunchBindAssociations => "launch.bind.associations",
    LaunchBindRevokedAfter => "launch.bind.revoked_after",
    LaunchBindVerifyBlocking => "launch.bind.verify_blocking",
    // R8z-3: the MCP-boundary launch preparation (`launch_steps::PrepareStep`,
    // mapped in `launch_sites.rs`): `launch.bind.inner_prepare.<label>`.
    LaunchBindInnerPrepareRevoked => "launch.bind.inner_prepare.revoked",
    LaunchBindInnerPrepareReservationUnavailable => "launch.bind.inner_prepare.reservation.unavailable",
    LaunchBindInnerPrepareReservationDenied => "launch.bind.inner_prepare.reservation.denied",
    LaunchBindInnerPrepareReservationWouldDeadlock => "launch.bind.inner_prepare.reservation.would_deadlock",
    LaunchBindInnerPrepareReservationNotFound => "launch.bind.inner_prepare.reservation.not_found",
    LaunchBindInnerPrepareReservationTimeout => "launch.bind.inner_prepare.reservation.timeout",
    LaunchBindInnerPrepareReservationIo => "launch.bind.inner_prepare.reservation.io",
    LaunchBindInnerPrepareReservationProtocol => "launch.bind.inner_prepare.reservation.protocol",
    LaunchBindInnerPrepareEmployeeMismatch => "launch.bind.inner_prepare.employee_mismatch",
    LaunchBindInnerPrepareStateLock => "launch.bind.inner_prepare.state_lock",
    LaunchBindInnerPreparePreparedMissing => "launch.bind.inner_prepare.prepared_missing",
    LaunchBindInnerPreparePlanClass => "launch.bind.inner_prepare.plan_class",
    LaunchBindInnerPreparePlanRuntime => "launch.bind.inner_prepare.plan_runtime",
    LaunchBindInnerPreparePlanPty => "launch.bind.inner_prepare.plan_pty",
    LaunchBindInnerPrepareRegistryPublicationRead => "launch.bind.inner_prepare.registry.publication_read",
    LaunchBindInnerPrepareRegistryGeneration => "launch.bind.inner_prepare.registry.generation",
    LaunchBindInnerPrepareRegistryRuntimeMissing => "launch.bind.inner_prepare.registry.runtime_missing",
    LaunchBindInnerPrepareRegistryRuntimeDuplicate => "launch.bind.inner_prepare.registry.runtime_duplicate",
    LaunchBindInnerPrepareRegistryAcceptanceMissing => "launch.bind.inner_prepare.registry.acceptance_missing",
    LaunchBindInnerPrepareRegistryScriptExecutable => "launch.bind.inner_prepare.registry.script_executable",
    LaunchBindInnerPrepareRegistryMcpDisabled => "launch.bind.inner_prepare.registry.mcp_disabled",
    LaunchBindInnerPrepareRegistryProjectionInvalid => "launch.bind.inner_prepare.registry.projection_invalid",
    LaunchBindInnerPrepareRegistryUnreported => "launch.bind.inner_prepare.registry.unreported",
    LaunchBindInnerPrepareRegistryAcceptanceRuntime => "launch.bind.inner_prepare.registry.acceptance.runtime",
    LaunchBindInnerPrepareRegistryAcceptanceVersion => "launch.bind.inner_prepare.registry.acceptance.version",
    LaunchBindInnerPrepareRegistryAcceptanceRuntimeDigest => "launch.bind.inner_prepare.registry.acceptance.runtime_digest",
    LaunchBindInnerPrepareRegistryAcceptanceBridgeDigest => "launch.bind.inner_prepare.registry.acceptance.bridge_digest",
    LaunchBindInnerPrepareRegistryAcceptanceProjectionDigest => "launch.bind.inner_prepare.registry.acceptance.projection_digest",
    LaunchBindInnerPrepareRegistryAcceptanceInheritedFd => "launch.bind.inner_prepare.registry.acceptance.inherited_fd",
    LaunchBindInnerPrepareRegistryAcceptanceInitializeExit => "launch.bind.inner_prepare.registry.acceptance.initialize_exit",
    LaunchBindInnerPrepareRegistryAcceptanceParentExecExit => "launch.bind.inner_prepare.registry.acceptance.parent_exec_exit",
    LaunchBindInnerPrepareRegistryAcceptanceMcpChildExecExit => "launch.bind.inner_prepare.registry.acceptance.mcp_child_exec_exit",
    LaunchBindInnerPrepareRegistryAcceptanceExclusiveConfigExit => "launch.bind.inner_prepare.registry.acceptance.exclusive_config_exit",
    LaunchBindInnerPrepareRegistryAcceptanceTranscriptDigest => "launch.bind.inner_prepare.registry.acceptance.transcript_digest",
    LaunchBindInnerPrepareRegistrationRuntime => "launch.bind.inner_prepare.registration_runtime",
    LaunchBindInnerPrepareProjection => "launch.bind.inner_prepare.projection",
    LaunchBindInnerPrepareArgv => "launch.bind.inner_prepare.argv",
    LaunchBindInnerPrepareClock => "launch.bind.inner_prepare.clock",
    LaunchBindInnerPrepareIssueLiveSnapshot => "launch.bind.inner_prepare.issue.live_snapshot",
    LaunchBindInnerPrepareIssueNotReserved => "launch.bind.inner_prepare.issue.not_reserved",
    LaunchBindInnerPrepareIssueReservation => "launch.bind.inner_prepare.issue.reservation",
    LaunchBindInnerPrepareIssueExpiry => "launch.bind.inner_prepare.issue.expiry",
    LaunchBindInnerPrepareIssueTime => "launch.bind.inner_prepare.issue.time",
    LaunchBindInnerPrepareIssuePrincipal => "launch.bind.inner_prepare.issue.principal",
    LaunchBindInnerPrepareIssueBinding => "launch.bind.inner_prepare.issue.binding",
    LaunchBindInnerPrepareIssuePolicyRevision => "launch.bind.inner_prepare.issue.policy_revision",
    LaunchBindInnerPrepareIssueAgentSlug => "launch.bind.inner_prepare.issue.agent_slug",
    LaunchBindInnerPrepareIssueBindingPrincipal => "launch.bind.inner_prepare.issue.binding_principal",
    LaunchBindInnerPrepareIssueGrants => "launch.bind.inner_prepare.issue.grants",
    // R8z-4: the rule of the capability encoding that refused.
    LaunchBindInnerPrepareIssueEncodeResourceMode => "launch.bind.inner_prepare.issue.encode.resource_mode",
    LaunchBindInnerPrepareIssueEncodeInterval => "launch.bind.inner_prepare.issue.encode.interval",
    LaunchBindInnerPrepareIssueEncodeDuplicateGrant => "launch.bind.inner_prepare.issue.encode.duplicate_grant",
    LaunchBindInnerPrepareIssueEncodeOther => "launch.bind.inner_prepare.issue.encode.other",
    LaunchBindInnerPrepareIssueRandom => "launch.bind.inner_prepare.issue.random",
    LaunchBindInnerPrepareIssueSigner => "launch.bind.inner_prepare.issue.signer",
    LaunchBindInnerPrepareIssueStateLock => "launch.bind.inner_prepare.issue.state_lock",
    LaunchBindInnerPrepareIssueCapacity => "launch.bind.inner_prepare.issue.capacity",
    LaunchBindInnerPrepareIssueDuplicate => "launch.bind.inner_prepare.issue.duplicate",
    LaunchBindInnerPrepareVerifyBlocking => "launch.bind.inner_prepare.verify_blocking",
    LaunchBindInnerPreparePendingDuplicate => "launch.bind.inner_prepare.pending_duplicate",
    LaunchBindInnerPrepareRevokedAfter => "launch.bind.inner_prepare.revoked_after",
    // R8z-3: the MCP-boundary activation after StartRun
    // (`launch_steps::ActivateStep`): `launch.activate.inner_activate.<label>`.
    LaunchActivateMcpFdMissing => "launch.activate.inner_activate.mcp_fd_missing",
    LaunchActivateMcpReceiptUnavailable => "launch.activate.inner_activate.mcp_receipt.unavailable",
    LaunchActivateMcpReceiptDenied => "launch.activate.inner_activate.mcp_receipt.denied",
    LaunchActivateMcpReceiptWouldDeadlock => "launch.activate.inner_activate.mcp_receipt.would_deadlock",
    LaunchActivateMcpReceiptNotFound => "launch.activate.inner_activate.mcp_receipt.not_found",
    LaunchActivateMcpReceiptTimeout => "launch.activate.inner_activate.mcp_receipt.timeout",
    LaunchActivateMcpReceiptIo => "launch.activate.inner_activate.mcp_receipt.io",
    LaunchActivateMcpReceiptProtocol => "launch.activate.inner_activate.mcp_receipt.protocol",
    LaunchActivateStateLock => "launch.activate.inner_activate.state_lock",
    LaunchActivatePendingMissing => "launch.activate.inner_activate.pending_missing",
    LaunchActivateReceiptControl => "launch.activate.inner_activate.receipt_control",
    LaunchActivateBridgePlatform => "launch.activate.inner_activate.bridge.platform",
    LaunchActivateBridgeRegistryPublicationRead => "launch.activate.inner_activate.bridge.registry.publication_read",
    LaunchActivateBridgeRegistryGeneration => "launch.activate.inner_activate.bridge.registry.generation",
    LaunchActivateBridgeRegistryRuntimeMissing => "launch.activate.inner_activate.bridge.registry.runtime_missing",
    LaunchActivateBridgeRegistryRuntimeDuplicate => "launch.activate.inner_activate.bridge.registry.runtime_duplicate",
    LaunchActivateBridgeRegistryAcceptanceMissing => "launch.activate.inner_activate.bridge.registry.acceptance_missing",
    LaunchActivateBridgeRegistryScriptExecutable => "launch.activate.inner_activate.bridge.registry.script_executable",
    LaunchActivateBridgeRegistryMcpDisabled => "launch.activate.inner_activate.bridge.registry.mcp_disabled",
    LaunchActivateBridgeRegistryProjectionInvalid => "launch.activate.inner_activate.bridge.registry.projection_invalid",
    LaunchActivateBridgeRegistryUnreported => "launch.activate.inner_activate.bridge.registry.unreported",
    LaunchActivateBridgeRegistryAcceptanceRuntime => "launch.activate.inner_activate.bridge.registry.acceptance.runtime",
    LaunchActivateBridgeRegistryAcceptanceVersion => "launch.activate.inner_activate.bridge.registry.acceptance.version",
    LaunchActivateBridgeRegistryAcceptanceRuntimeDigest => "launch.activate.inner_activate.bridge.registry.acceptance.runtime_digest",
    LaunchActivateBridgeRegistryAcceptanceBridgeDigest => "launch.activate.inner_activate.bridge.registry.acceptance.bridge_digest",
    LaunchActivateBridgeRegistryAcceptanceProjectionDigest => "launch.activate.inner_activate.bridge.registry.acceptance.projection_digest",
    LaunchActivateBridgeRegistryAcceptanceInheritedFd => "launch.activate.inner_activate.bridge.registry.acceptance.inherited_fd",
    LaunchActivateBridgeRegistryAcceptanceInitializeExit => "launch.activate.inner_activate.bridge.registry.acceptance.initialize_exit",
    LaunchActivateBridgeRegistryAcceptanceParentExecExit => "launch.activate.inner_activate.bridge.registry.acceptance.parent_exec_exit",
    LaunchActivateBridgeRegistryAcceptanceMcpChildExecExit => "launch.activate.inner_activate.bridge.registry.acceptance.mcp_child_exec_exit",
    LaunchActivateBridgeRegistryAcceptanceExclusiveConfigExit => "launch.activate.inner_activate.bridge.registry.acceptance.exclusive_config_exit",
    LaunchActivateBridgeRegistryAcceptanceTranscriptDigest => "launch.activate.inner_activate.bridge.registry.acceptance.transcript_digest",
    LaunchActivateBridgeRegistrationRuntime => "launch.activate.inner_activate.bridge.registration_runtime",
    LaunchActivateBridgeProjection => "launch.activate.inner_activate.bridge.projection",
    LaunchActivateBridgePublishPublicationRead => "launch.activate.inner_activate.bridge.publish.publication_read",
    LaunchActivateBridgePublishGeneration => "launch.activate.inner_activate.bridge.publish.generation",
    LaunchActivateBridgePublishRuntimeMissing => "launch.activate.inner_activate.bridge.publish.runtime_missing",
    LaunchActivateBridgePublishRuntimeDuplicate => "launch.activate.inner_activate.bridge.publish.runtime_duplicate",
    LaunchActivateBridgePublishAcceptanceMissing => "launch.activate.inner_activate.bridge.publish.acceptance_missing",
    LaunchActivateBridgePublishScriptExecutable => "launch.activate.inner_activate.bridge.publish.script_executable",
    LaunchActivateBridgePublishMcpDisabled => "launch.activate.inner_activate.bridge.publish.mcp_disabled",
    LaunchActivateBridgePublishProjectionInvalid => "launch.activate.inner_activate.bridge.publish.projection_invalid",
    LaunchActivateBridgePublishUnreported => "launch.activate.inner_activate.bridge.publish.unreported",
    LaunchActivateBridgePublishAcceptanceRuntime => "launch.activate.inner_activate.bridge.publish.acceptance.runtime",
    LaunchActivateBridgePublishAcceptanceVersion => "launch.activate.inner_activate.bridge.publish.acceptance.version",
    LaunchActivateBridgePublishAcceptanceRuntimeDigest => "launch.activate.inner_activate.bridge.publish.acceptance.runtime_digest",
    LaunchActivateBridgePublishAcceptanceBridgeDigest => "launch.activate.inner_activate.bridge.publish.acceptance.bridge_digest",
    LaunchActivateBridgePublishAcceptanceProjectionDigest => "launch.activate.inner_activate.bridge.publish.acceptance.projection_digest",
    LaunchActivateBridgePublishAcceptanceInheritedFd => "launch.activate.inner_activate.bridge.publish.acceptance.inherited_fd",
    LaunchActivateBridgePublishAcceptanceInitializeExit => "launch.activate.inner_activate.bridge.publish.acceptance.initialize_exit",
    LaunchActivateBridgePublishAcceptanceParentExecExit => "launch.activate.inner_activate.bridge.publish.acceptance.parent_exec_exit",
    LaunchActivateBridgePublishAcceptanceMcpChildExecExit => "launch.activate.inner_activate.bridge.publish.acceptance.mcp_child_exec_exit",
    LaunchActivateBridgePublishAcceptanceExclusiveConfigExit => "launch.activate.inner_activate.bridge.publish.acceptance.exclusive_config_exit",
    LaunchActivateBridgePublishAcceptanceTranscriptDigest => "launch.activate.inner_activate.bridge.publish.acceptance.transcript_digest",
    LaunchActivateBridgePublishPolicy => "launch.activate.inner_activate.bridge.publish.policy",
    LaunchActivateBridgePublishDerive => "launch.activate.inner_activate.bridge.publish.derive",
    LaunchActivateBridgePublishRecord => "launch.activate.inner_activate.bridge.publish.record",
    LaunchActivateBridgePublishRegistrationChanged => "launch.activate.inner_activate.bridge.publish.registration_changed",
    LaunchActivateBridgePublishRunReceiptRead => "launch.activate.inner_activate.bridge.publish.run_receipt_read",
    LaunchActivateBridgePublishReceiptGeneration => "launch.activate.inner_activate.bridge.publish.receipt.generation",
    LaunchActivateBridgePublishReceiptRun => "launch.activate.inner_activate.bridge.publish.receipt.run",
    LaunchActivateBridgePublishReceiptEmployee => "launch.activate.inner_activate.bridge.publish.receipt.employee",
    LaunchActivateBridgePublishReceiptRuntime => "launch.activate.inner_activate.bridge.publish.receipt.runtime",
    LaunchActivateBridgePublishReceiptDigests => "launch.activate.inner_activate.bridge.publish.receipt.digests",
    LaunchActivateBridgePublishReceiptProjectionDigest => "launch.activate.inner_activate.bridge.publish.receipt.projection_digest",
    LaunchActivateBridgePublishReceiptMount => "launch.activate.inner_activate.bridge.publish.receipt.mount",
    LaunchActivateBridgePublishReceiptAcceptance => "launch.activate.inner_activate.bridge.publish.receipt.acceptance",
    LaunchActivateBridgeReceiptBinding => "launch.activate.inner_activate.bridge.receipt_binding",
    LaunchActivateProjectionDigest => "launch.activate.inner_activate.projection_digest",
    LaunchActivateDelivery => "launch.activate.inner_activate.delivery",
    LaunchActivateClock => "launch.activate.inner_activate.clock",
    LaunchActivateBoundaryActivateDelivery => "launch.activate.inner_activate.boundary_activate.delivery",
    LaunchActivateBoundaryActivateUnknown => "launch.activate.inner_activate.boundary_activate.unknown",
    LaunchActivateBoundaryActivateBinding => "launch.activate.inner_activate.boundary_activate.binding",
    LaunchActivateBoundaryActivateTime => "launch.activate.inner_activate.boundary_activate.time",
    LaunchActivateBoundaryActivateLiveSnapshot => "launch.activate.inner_activate.boundary_activate.live_snapshot",
    LaunchActivateBoundaryActivateLiveMismatch => "launch.activate.inner_activate.boundary_activate.live_mismatch",
    LaunchActivateBoundaryActivateRandom => "launch.activate.inner_activate.boundary_activate.random",
    LaunchActivateBoundaryActivateStateLock => "launch.activate.inner_activate.boundary_activate.state_lock",
    LaunchActivateBoundaryActivateRevoked => "launch.activate.inner_activate.boundary_activate.revoked",
    LaunchActivateVerifyBlocking => "launch.activate.inner_activate.verify_blocking",
    LaunchActivateActiveDuplicate => "launch.activate.inner_activate.active_duplicate",
    LaunchActivateRevokedAfter => "launch.activate.inner_activate.revoked_after",
    // Asking spawnd to start the run.
    SpawnPrepare => "spawn.prepare",
    SpawnNotProtected => "spawn.not_protected",
    SpawnAdmission => "spawn.admission",
    SpawnBackendUnavailable => "spawn.backend.unavailable",
    SpawnBackendDenied => "spawn.backend.denied",
    SpawnBackendWouldDeadlock => "spawn.backend.would_deadlock",
    SpawnBackendNotFound => "spawn.backend.not_found",
    SpawnBackendTimeout => "spawn.backend.timeout",
    SpawnBackendIo => "spawn.backend.io",
    SpawnBackendProtocol => "spawn.backend.protocol",
    SpawnStartPermissionDenied => "spawn.start.permission_denied",
    SpawnStartNotFound => "spawn.start.not_found",
    SpawnStartTimedOut => "spawn.start.timed_out",
    SpawnStartOther => "spawn.start.other",
    // The run started.
    RunStdinMissing => "run.stdin_missing",
    RunStdoutMissing => "run.stdout_missing",
    RunStderrMissing => "run.stderr_missing",
    RunInput => "run.input",
    RunOutputDrain => "run.output_drain",
    RunStderrDrain => "run.stderr_drain",
    RunWait => "run.wait",
    RunRefused => "run.refused",
    // The run exited successfully; its result was refused.
    OutputParse => "output.parse",
    OutputEmpty => "output.empty",
    OutputUtf8 => "output.utf8",
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

// Compile-time invariants (every build) of the generated list: labels are
// non-empty, lowercase `[a-z0-9_.]`, pairwise distinct, and `ALL[i]` is the
// variant whose `as_str()` is `SITES[i]`.
const _: () = {
    assert!(RefusalSite::ALL.len() == SITES.len(), "ALL and SITES differ in length");
    // R8z-3: the list roughly doubled (the MCP-boundary launch steps), so the
    // pairwise distinctness check compares a per-label FNV-1a hash first and
    // the bytes only on a hash match; equal labels always have equal hashes,
    // so the check is unchanged and stays far inside the const-eval budget.
    const N: usize = SITES.len();
    let mut hashes = [0u64; N];
    let mut index = 0;
    while index < N {
        let bytes = SITES[index].as_bytes();
        assert!(!bytes.is_empty(), "empty refusal site label");
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            assert!(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'_',
                "a refusal site label is not lowercase [a-z0-9_.]");
            hash = (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3);
            at += 1;
        }
        hashes[index] = hash;
        assert!(const_str_eq(RefusalSite::ALL[index].as_str(), SITES[index]), "ALL and SITES out of order");
        index += 1;
    }
    let mut index = 0;
    while index < N {
        let mut other = index + 1;
        while other < N {
            assert!(hashes[index] != hashes[other] || !const_str_eq(SITES[index], SITES[other]),
                "duplicate refusal site label");
            other += 1;
        }
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

// User-turn warnings retain their existing admission-bound volume. Expected
// background refusals use DEBUG plus a bounded, process-lifetime INFO notice.
const BACKGROUND_NOTICE_LIMIT: usize = 1024;
type BackgroundNotices = std::collections::HashSet<(String, &'static str)>;
static BACKGROUND_NOTICES: std::sync::OnceLock<std::sync::Mutex<BackgroundNotices>> = std::sync::OnceLock::new();

fn first_background_notice(seen: &mut BackgroundNotices, agent: &str, site: &'static str) -> bool {
    if seen.len() >= BACKGROUND_NOTICE_LIMIT { return false; }
    seen.insert((agent.to_owned(), site))
}

fn log_expected_background(runtime: &'static str, agent: &str, site: Option<RefusalSite>) -> bool {
    let expected = crate::runtime_dispatch::in_utility_call()
        || (site == Some(site!("authorization.ingress.no_scope"))
            // Evolution maps to Judge; employee turns and scheduled/delegated
            // work use distinct classes and must retain their warnings.
            && duduclaw_core::execution_gate::capture_class()
                == duduclaw_core::execution_gate::ExecutionClass::Judge);
    if !expected { return false; }
    let site = site_text(site);
    tracing::debug!(runtime, agent, site, "isolated background call not available in protected mode");
    let first = BACKGROUND_NOTICES.get_or_init(Default::default).lock()
        .map(|mut seen| first_background_notice(&mut seen, agent, site)).unwrap_or(false);
    if first {
        tracing::info!(agent, site, "protected mode: background model calls are not run for this employee");
    }
    true
}

/// The refusal log for a protected turn that never reached a
/// started run. Fixed fields only: runtime label, employee label, site label
/// (`unlabelled` when no site was recorded).
pub(crate) fn log_not_started(runtime: &'static str, agent: &str, site: Option<RefusalSite>) {
    let agent = agent_label(agent);
    if log_expected_background(runtime, agent, site) { return; }
    tracing::warn!(runtime, agent, site = site_text(site), "isolated run not started");
}

/// The refusal log for a run that started but was refused without the
/// R8t line (its pipes were missing, or it exited 0 with unusable output).
/// `site` as in [`log_not_started`].
pub(crate) fn log_refused_after_start(runtime: &'static str, agent: &str, site: Option<RefusalSite>) {
    let agent = agent_label(agent);
    if log_expected_background(runtime, agent, site) { return; }
    tracing::warn!(runtime, agent, site = site_text(site), "isolated run refused after start");
}

/// B0: the fixed `runtime` log value of a configured provider, the same
/// labels as [`runtime_label`] (every registered CLI is `registered`).
pub(crate) fn provider_label(provider: duduclaw_core::types::RuntimeType) -> &'static str {
    use duduclaw_core::types::RuntimeType;
    match provider {
        RuntimeType::Claude => "claude",
        RuntimeType::Codex => "codex",
        RuntimeType::Grok => "grok",
        RuntimeType::OpenAiCompat => "openai_compat",
        _ => "registered",
    }
}

/// B0: the record of a utility (background) call that protected mode refuses
/// by design before it reaches any runtime's protected path (an in-run judge,
/// the global utility fallback, the Claude utility lane). It is the standard
/// by-design background refusal: [`log_not_started`] under the utility-call
/// scope, i.e. one DEBUG `isolated background call not available in protected
/// mode` plus at most one INFO per employee and site, never a WARN. Inside an
/// open protected turn (Linux; `runtime::observed`) only the site is recorded
/// and that turn's single logging point writes the line. The caller returns
/// its own fixed error; nothing here reads or logs request content.
pub(crate) fn note_utility_refusal(runtime: &'static str, agent: &str, site: RefusalSite) {
    #[cfg(target_os = "linux")]
    if super::runtime::in_observed_turn() {
        super::runtime::note_site(site);
        return;
    }
    crate::runtime_dispatch::UTILITY_CALL.sync_scope((), || log_not_started(runtime, agent, Some(site)));
}

/// Test fixtures shared by the B0 utility-refusal tests in other modules.
#[cfg(test)]
pub(crate) mod test_support {
    /// Run `f` with `execution_gate::isolation_required()` true on THIS
    /// thread only: a thread-local protected-mode marker plus a thread-local
    /// isolation-install fact (never latched process-wide, so parallel tests
    /// are unaffected).
    pub(crate) fn with_isolation_required<R>(f: impl FnOnce() -> R) -> R {
        let facts = tempfile::tempdir().unwrap();
        let marker = facts.path().join("protected-mode");
        std::fs::write(&marker, b"native-systemd\n").unwrap();
        let sources = duduclaw_core::protected_mode::Sources { env: None, marker,
            service_home: facts.path().join("no-service-home") };
        let isolation = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(isolation.path().join("var/lib/duduclaw-spawnd")).unwrap();
        duduclaw_core::protected_mode::with_thread_sources(sources, ||
            duduclaw_core::isolation_install::with_thread_root(isolation.path().to_path_buf(), || {
                assert!(duduclaw_core::execution_gate::isolation_required());
                f()
            }))
    }

    #[derive(Clone)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    /// Run `future` to completion on a current-thread runtime on THIS thread
    /// (so thread-local fixtures apply) and return its output with every log
    /// line written meanwhile at DEBUG and above.
    pub(crate) fn run_capturing_logs<F: std::future::Future>(future: F) -> (F::Output, String) {
        let log = Capture(Default::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
            .with_max_level(tracing::Level::DEBUG).with_writer(move || writer.clone()).finish();
        let output = tracing::subscriber::with_default(subscriber, ||
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future));
        let bytes = log.0.lock().unwrap().clone();
        (output, String::from_utf8(bytes).unwrap())
    }

    /// The B0 by-design utility refusal was logged for `site`: exactly one
    /// DEBUG line (plus at most the one process-lifetime INFO notice, which an
    /// earlier test may already have taken), never the WARN refusal lines.
    /// (A caller's own fallback warning, e.g. "fail-closed to Chat", is not a
    /// refusal line and is allowed.)
    pub(crate) fn assert_utility_refusal_logged(logs: &str, site: super::RefusalSite) {
        let debug: Vec<&str> = logs.lines().filter(|line| line.contains("DEBUG")
            && line.contains("isolated background call not available in protected mode")).collect();
        assert_eq!(debug.len(), 1, "{logs}");
        assert!(debug[0].contains(&format!("site=\"{}\"", site.as_str())), "{logs}");
        for line in logs.lines().filter(|line| line.contains("protected mode: background model calls are not run")) {
            assert!(line.contains("INFO") && line.contains(&format!("site=\"{}\"", site.as_str())), "{logs}");
        }
        assert!(!logs.contains("isolated run not started") && !logs.contains("isolated run refused"), "{logs}");
    }
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

    fn capture_background_logs(future: impl std::future::Future<Output = ()>) -> String {
        let log = LogCapture(Default::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
            .with_max_level(tracing::Level::DEBUG).with_writer(move || writer.clone()).finish();
        tracing::subscriber::with_default(subscriber, || {
            tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(future);
        });
        let bytes = log.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    fn assert_background_logs(logs: &str, info_count: usize, site: RefusalSite) {
        assert!(!logs.contains("WARN"), "{logs}");
        assert_eq!(logs.lines().filter(|line| line.contains("DEBUG")).count(), 1, "{logs}");
        assert_eq!(logs.lines().filter(|line| line.contains("INFO")).count(), info_count, "{logs}");
        assert!(logs.contains("isolated background call not available in protected mode"), "{logs}");
        assert!(logs.contains("runtime=\"openai_compat\""), "{logs}");
        assert!(logs.contains(&format!("site=\"{}\"", site.as_str())), "{logs}");
        if info_count != 0 {
            let info = logs.lines().find(|line| line.contains("INFO")).unwrap();
            assert!(info.contains("protected mode: background model calls are not run for this employee"), "{logs}");
            assert!(info.contains("agent=") && info.contains("site="), "{logs}");
            assert!(!info.contains("runtime="), "{logs}");
        }
    }

    #[test]
    fn utility_refusals_are_debug_with_one_info_per_agent_and_site() {
        // Classification follows the task-local even for a site other than *.utility_call.
        for site in [site!("authorize.utility_call"), site!("preflight.utility_call"), site!("plan.root_publication")] {
            for attempt in 0..2 {
                let logs = capture_background_logs(crate::runtime_dispatch::UTILITY_CALL.scope((), async {
                    log_not_started("openai_compat", "r8z9-utility", Some(site));
                }));
                assert_background_logs(&logs, usize::from(attempt == 0), site);
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn observed_utility_refusal_keeps_the_returned_error() {
        let site = site!("authorize.utility_call");
        let logs = capture_background_logs(crate::runtime_dispatch::UTILITY_CALL.scope((), async {
            let expected = user_error(None);
            let result = crate::isolation_mcp::runtime::observed("openai_compat", "r8z9-observed", async {
                crate::isolation_mcp::runtime::note_site(site);
                Err::<(), String>(expected.clone())
            }).await;
            assert_eq!(result, Err(expected));
        }));
        assert_background_logs(&logs, 1, site);
    }

    #[test]
    fn evolution_no_scope_refusal_is_expected_background() {
        let site = site!("authorization.ingress.no_scope");
        let class = crate::process_group::request_class(crate::cost_telemetry::RequestType::Evolution);
        assert_eq!(class, duduclaw_core::execution_gate::ExecutionClass::Judge);
        for attempt in 0..2 {
            let logs = capture_background_logs(crate::process_group::with_class(class, async {
                log_not_started("openai_compat", "r8z9-evolution", Some(site));
            }));
            assert_background_logs(&logs, usize::from(attempt == 0), site);
        }
    }

    #[test]
    fn autonomous_work_no_scope_refusals_keep_the_warning() {
        use duduclaw_core::execution_gate::ExecutionClass;
        let site = site!("authorization.ingress.no_scope");
        for class in [ExecutionClass::EmployeeMain, ExecutionClass::Scheduled,
            ExecutionClass::Dispatch, ExecutionClass::Reminder] {
            let logs = capture_background_logs(crate::process_group::with_class(class,
                crate::autonomous_runtime::scope(
                    std::path::Path::new("unused-background-log-fixture"), "r8z9-work", &[], async {
                        log_not_started("openai_compat", "r8z9-work", Some(site));
                    },
                ),
            ));
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("WARN") && logs.contains("isolated run not started"), "{logs}");
            assert!(logs.contains("runtime=\"openai_compat\"") && logs.contains("agent=\"r8z9-work\""), "{logs}");
            assert!(logs.contains(&format!("site=\"{}\"", site.as_str())), "{logs}");
        }
    }

    #[test]
    fn user_no_scope_and_other_user_refusals_keep_the_warning() {
        for site in [site!("authorization.ingress.no_scope"), site!("plan.root_publication")] {
            let logs = capture_background_logs(async {
                log_not_started("openai_compat", "r8z9-user", Some(site));
            });
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("WARN") && logs.contains("isolated run not started"), "{logs}");
            assert!(logs.contains("runtime=\"openai_compat\"") && logs.contains("agent=\"r8z9-user\""), "{logs}");
            assert!(logs.contains(&format!("site=\"{}\"", site.as_str())), "{logs}");
        }
        // An autonomous scope alone does not downgrade any other refusal site.
        let logs = capture_background_logs(crate::autonomous_runtime::scope(
            std::path::Path::new("unused-background-log-fixture"), "r8z9-other", &[], async {
                log_not_started("openai_compat", "r8z9-other", Some(site!("plan.root_publication")));
            },
        ));
        assert_eq!(logs.lines().count(), 1, "{logs}");
        assert!(logs.contains("WARN") && logs.contains("isolated run not started"), "{logs}");
    }

    #[test]
    fn background_notice_set_is_bounded() {
        let mut seen = BackgroundNotices::new();
        assert!(first_background_notice(&mut seen, "first", "authorize.utility_call"));
        assert!(!first_background_notice(&mut seen, "first", "authorize.utility_call"));
        for index in 1..BACKGROUND_NOTICE_LIMIT {
            assert!(first_background_notice(&mut seen, &format!("agent-{index}"), "authorize.utility_call"));
        }
        assert!(!first_background_notice(&mut seen, "overflow", "authorize.utility_call"));
        assert!(!first_background_notice(&mut seen, "first", "authorize.utility_call"));
        assert_eq!(seen.len(), BACKGROUND_NOTICE_LIMIT);
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

    /// R8x review round 3: every `site!` arm, generated from the same
    /// `sites!` list as the enum, yields the variant whose `as_str()` is its
    /// own label (also a per-arm `const` assertion, i.e. a build error), and
    /// the arms, `ALL` and `SITES` line up one to one.
    #[test]
    fn every_listed_site_round_trips_at_compile_time() {
        assert_eq!(SITE_ARMS.len(), SITES.len());
        assert_eq!(SITE_ARMS, RefusalSite::ALL);
        for (site, label) in SITE_ARMS.iter().zip(SITES) {
            assert_eq!(site.as_str(), *label);
        }
        // And for a sample of literals, through the macro in a `const`.
        const FROM_MACRO: [RefusalSite; 3] = [site!("preflight.model"), site!("spawn.admission"), site!("output.utf8")];
        assert_eq!(FROM_MACRO, [RefusalSite::PreflightModel, RefusalSite::SpawnAdmission, RefusalSite::OutputUtf8]);
        assert_eq!(FROM_MACRO.map(RefusalSite::as_str), ["preflight.model", "spawn.admission", "output.utf8"]);
    }

    /// R8x review round 3: each variant's label is unique, non-empty and
    /// lowercase `[a-z0-9_.]`; `ALL` and `SITES` have the same length and
    /// order. (Also asserted at compile time in the module.)
    #[test]
    fn every_site_label_is_unique_and_well_formed() {
        assert_eq!(RefusalSite::ALL.len(), SITES.len());
        let mut seen = std::collections::BTreeSet::new();
        for (site, label) in RefusalSite::ALL.iter().zip(SITES) {
            let text = site.as_str();
            assert_eq!(text, *label, "{site:?}");
            assert!(!text.is_empty(), "{site:?}");
            assert!(text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_'),
                "{text}");
            assert!(seen.insert(text), "duplicate label {text}");
        }
        for (index, site) in RefusalSite::ALL.iter().enumerate() {
            assert!(!RefusalSite::ALL[index + 1..].contains(site), "{site:?} is listed twice in ALL");
        }
    }

    /// R8x F3: the site type is closed. It is a plain enum with no string
    /// constructor; `site!` with an unlisted label (or `unlabelled`) has no
    /// matching arm, a compile error with no run-time counterpart. Here: no
    /// site carries `unlabelled` or a near-miss of a listed label.
    #[test]
    fn refusal_sites_are_a_closed_type() {
        for label in ["plan.not_a_site", "", UNLABELLED_SITE, "plan.readiness ", "Plan.readiness"] {
            assert!(RefusalSite::ALL.iter().all(|site| site.as_str() != label), "{label:?}");
            assert!(!SITES.contains(&label), "{label:?}");
        }
        assert_eq!(site!("spawn.admission"), RefusalSite::SpawnAdmission);
        assert_eq!(site!("spawn.admission").as_str(), "spawn.admission");
        assert_eq!(Sited::new(site!("launch.binding"), ()).site(), site!("launch.binding"));
    }

    #[test]
    fn runtime_labels_are_fixed() {
        use duduclaw_isolation::{bounded::Text, execution::RuntimeKind};
        let registered = RuntimeKind::Registered(Text::new("generic-cli-v1.qwen".into()).unwrap());
        assert_eq!(runtime_label(&registered), "registered");
        assert_eq!(runtime_label(&RuntimeKind::OpenAiCompat), "openai_compat");
    }
}
