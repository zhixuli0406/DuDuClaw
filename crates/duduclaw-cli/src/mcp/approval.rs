use super::*;

/// TTL for an install-class approval raised on the stdio path. Mirrors the
/// WS PolicyKernel `Ask` gate (5 min) so a human has a realistic window to
/// approve from the dashboard inbox before it auto-denies.
pub(crate) const INSTALL_APPROVAL_TTL_SECONDS: i64 = 300;

/// Poll cadence while blocking on an install-class approval decision.
pub(crate) const INSTALL_APPROVAL_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Max chars of the (partly external) approval summary persisted + surfaced in
/// the inbox. CJK-safe via `truncate_chars` — never a raw byte slice.
pub(crate) const INSTALL_APPROVAL_SUMMARY_MAX_CHARS: usize = 300;

/// Tools that mutate the agent's tool surface by installing / attaching an
/// external capability. A non-admin caller must get admin approval before one
/// of these runs on the stdio path. `agent.toml [capabilities]
/// approval_required_tools` can add more tools explicitly.
pub(crate) fn is_install_class_tool(tool_name: &str) -> bool {
    matches!(tool_name, "skill_hub_install")
}

/// Decide whether an install-class tool call must obtain approval before it
/// runs.
///
/// F1 (WP5 dead-gate fix): the caller being an "admin" is **NOT** a bypass.
/// The default internal MCP principal always holds `Scope::Admin`, and the MCP
/// tool path is exactly the agent-autonomous (LLM-issued `tool_call`) path WP5
/// must gate — humans install via the dashboard `skills.install` route, which
/// has its own `require_admin!` gate. So `caller_is_admin` is ignored here.
///
/// Approval is required when EITHER:
///   * the tool is install-class AND the operator has not explicitly exempted
///     the agent via `[capabilities] auto_approve_install = true`, OR
///   * the agent's `agent.toml` explicitly lists the tool in
///     `approval_required_tools` (operator intent — always honoured, and it
///     overrides the `auto_approve_install` exemption).
///
/// Pure + deterministic so the branch logic is unit-testable without a broker.
pub(crate) fn install_approval_required(agent_dir: &Path, tool_name: &str, _caller_is_admin: bool) -> bool {
    // Explicit per-tool listing always forces approval (wins over any exemption).
    if duduclaw_gateway::approval::tool_requires_approval(agent_dir, tool_name) {
        return true;
    }
    // Install-class tools reached via MCP need approval unless the operator has
    // explicitly opted this agent out.
    is_install_class_tool(tool_name) && !duduclaw_gateway::approval::auto_approve_install(agent_dir)
}

/// What an approval is asked for. Decides the `action_kind` the inbox and
/// channel notifications render, and the wording the agent gets back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalSubject<'a> {
    /// Installing a skill / tool (`mcp_install`, rendered 「安裝新技能／工具」).
    Install,
    /// Calling one tool (`mcp_call`, rendered 「執行高風險工具」).
    ToolCall(&'a str),
}

impl ApprovalSubject<'_> {
    /// The `approvals.db` `action_kind`.
    pub(crate) fn action_kind(&self) -> &'static str {
        match self {
            Self::Install => "mcp_install",
            Self::ToolCall(_) => "mcp_call",
        }
    }

    /// The subject for a tool going through the per-agent approval gate:
    /// install-class tools keep the install wording, every other tool is a
    /// tool call.
    pub(crate) fn for_tool(tool_name: &str) -> ApprovalSubject<'_> {
        if is_install_class_tool(tool_name) {
            ApprovalSubject::Install
        } else {
            ApprovalSubject::ToolCall(tool_name)
        }
    }

    pub(crate) fn denied_message(&self, approval_id: &str) -> String {
        match self {
            Self::Install => format!("安裝要求已被管理員拒絕（審核編號 {approval_id}）。"),
            Self::ToolCall(tool) => {
                format!("工具「{tool}」的呼叫已被管理員拒絕（審核編號 {approval_id}）。")
            }
        }
    }

    pub(crate) fn expired_message(&self, approval_id: &str) -> String {
        match self {
            Self::Install => {
                format!("安裝要求逾時未核可，已自動拒絕（fail-closed，審核編號 {approval_id}）。")
            }
            Self::ToolCall(tool) => {
                format!("工具「{tool}」的呼叫逾時未核可，已自動拒絕（審核編號 {approval_id}）。")
            }
        }
    }

    /// The request could not be filed, the status was still pending, or
    /// waiting for it failed.
    pub(crate) fn failed_message(&self, what: &str) -> String {
        match self {
            Self::Install => format!("{what}，已拒絕安裝（fail-closed）。"),
            Self::ToolCall(tool) => format!("{what}，工具「{tool}」的呼叫已拒絕。"),
        }
    }

    /// The broker could not be opened at all.
    pub(crate) fn broker_unavailable_message(&self) -> String {
        match self {
            Self::Install => {
                "審批系統暫時無法使用，已拒絕安裝（fail-closed）。請稍後再試或由管理員手動安裝。"
                    .to_string()
            }
            Self::ToolCall(tool) => format!(
                "審批系統暫時無法使用，工具「{tool}」的呼叫已拒絕。請稍後再試或由管理員手動處理。"
            ),
        }
    }
}

/// Run one approval round against a broker: request → block on decision.
/// Denial, TTL-expiry, and request failure all map to `Denied` (fail-closed).
/// `simulation` stamps a D1 narrative on the row
/// (`ApprovalBroker::request_with_simulation`) for the channel push.
pub(crate) async fn run_approval(
    broker: &duduclaw_gateway::approval::ApprovalBroker,
    agent_id: &str,
    subject: ApprovalSubject<'_>,
    summary: &str,
    payload: Value,
    ttl_seconds: i64,
    poll: std::time::Duration,
    simulation: Option<Value>,
) -> InstallApprovalOutcome {
    use duduclaw_gateway::approval::ApprovalStatus;

    // External content (skill name/description) is truncated before it is
    // persisted or shown in the inbox (CJK-safe, no raw byte slicing).
    let summary = duduclaw_core::truncate_chars(summary, INSTALL_APPROVAL_SUMMARY_MAX_CHARS);
    let kind = subject.action_kind();

    let requested = match simulation {
        Some(sim) => {
            broker
                .request_with_simulation(agent_id, kind, &summary, payload, ttl_seconds, sim)
                .await
        }
        None => broker.request(agent_id, kind, &summary, payload, ttl_seconds).await,
    };
    let approval_id = match requested {
        Ok(id) => id,
        Err(e) => {
            warn!(error = %e, "approval request failed — denying (fail-closed)");
            return InstallApprovalOutcome::Denied(subject.failed_message("審批系統無法建立審核請求"));
        }
    };

    match broker.await_decision(&approval_id, poll).await {
        Ok(ApprovalStatus::Approved) => InstallApprovalOutcome::Proceed,
        Ok(ApprovalStatus::Denied) => InstallApprovalOutcome::Denied(subject.denied_message(&approval_id.to_string())),
        Ok(ApprovalStatus::Expired) => InstallApprovalOutcome::Denied(subject.expired_message(&approval_id.to_string())),
        Ok(ApprovalStatus::Pending) => {
            InstallApprovalOutcome::Denied(subject.failed_message("審核狀態異常（仍為待審）"))
        }
        Err(e) => {
            warn!(error = %e, "await_decision failed — denying (fail-closed)");
            InstallApprovalOutcome::Denied(subject.failed_message("等待審核決定時發生錯誤"))
        }
    }
}

/// [`run_approval`] for an install (`mcp_install`, install wording). Split
/// out from [`gate_install_approval`] so tests can drive it with an
/// in-memory broker and a short TTL/poll. `pub(crate)`: see
/// [`InstallApprovalOutcome`]'s doc comment.
pub(crate) async fn run_install_approval(
    broker: &duduclaw_gateway::approval::ApprovalBroker,
    agent_id: &str,
    summary: &str,
    payload: Value,
    ttl_seconds: i64,
    poll: std::time::Duration,
) -> InstallApprovalOutcome {
    run_approval(broker, agent_id, ApprovalSubject::Install, summary, payload, ttl_seconds, poll, None)
        .await
}

/// [`run_approval`] for one tool call (`mcp_call`, tool-call wording).
pub(crate) async fn run_tool_approval(
    broker: &duduclaw_gateway::approval::ApprovalBroker,
    agent_id: &str,
    tool_name: &str,
    summary: &str,
    payload: Value,
    ttl_seconds: i64,
    poll: std::time::Duration,
) -> InstallApprovalOutcome {
    run_approval(
        broker,
        agent_id,
        ApprovalSubject::ToolCall(tool_name),
        summary,
        payload,
        ttl_seconds,
        poll,
        None,
    )
    .await
}

/// Gate an install-class tool behind admin approval on the stdio path.
/// Returns [`InstallApprovalOutcome::Proceed`] when no gate applies (admin, or
/// tool not gated) or when a human approved; otherwise a fail-closed `Denied`.
pub(crate) async fn gate_install_approval(
    home_dir: &Path,
    agent_id: &str,
    tool_name: &str,
    summary: &str,
    payload: Value,
    caller_is_admin: bool,
) -> InstallApprovalOutcome {
    // W3-3b (a): `agent_id` is the CALLER. An `eph-*` role member's
    // `[capabilities] approval_required_tools` lives under
    // `agents/.ephemeral/<id>/agent.toml`; reading the plain registry path
    // found nothing and the gate silently opened (fail-OPEN) for every role
    // member.
    let agent_dir = caller_agent_dir(home_dir, agent_id);
    if !install_approval_required(&agent_dir, tool_name, caller_is_admin) {
        return InstallApprovalOutcome::Proceed;
    }

    // Open the on-disk broker only when a gate actually applies. Broker
    // unavailable ⇒ DENY (never fall through to install).
    let broker = match duduclaw_gateway::approval::ApprovalBroker::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "ApprovalBroker unavailable — denying install (fail-closed)");
            return InstallApprovalOutcome::Denied(
                "審批系統暫時無法使用，已拒絕安裝（fail-closed）。請稍後再試或由管理員手動安裝。"
                    .to_string(),
            );
        }
    };
    run_install_approval(
        &broker,
        agent_id,
        summary,
        payload,
        INSTALL_APPROVAL_TTL_SECONDS,
        INSTALL_APPROVAL_POLL,
    )
    .await
}

/// Dispatch-layer entry to the install / operator-required approval gate (WP5
/// elevation). Returns `Ok(())` to proceed or `Err(zh-TW message)` on a
/// fail-closed denial.
///
/// This is called once, at the shared `dispatch_tool_call` choke point, so that
/// `agent.toml [capabilities] approval_required_tools` is honoured for **every**
/// tool. Before the elevation the gate lived only inside `handle_skill_hub_install`,
/// so an operator who listed any *other* tool for approval was silently ignored
/// (fail-open). `skill_hub_install` is deliberately excluded here: it keeps its
/// own gate that fires **after** the security scan (so the approver sees the scan
/// result), and gating it at dispatch too would double-prompt and move approval
/// ahead of the scan. Delegates to the same fail-closed [`gate_install_approval`].
pub(crate) async fn gate_tool_approval_dispatch(
    home_dir: &Path,
    agent_id: &str,
    tool_name: &str,
    payload: Value,
) -> std::result::Result<(), String> {
    if tool_name == "skill_hub_install" {
        return Ok(());
    }
    // The eight `computer_*` tools are gated in the gateway, which owns the
    // session: `computer_use_sessions` runs `approval_required_tools` /
    // `irreversible_tools` / `maybe_irreversible_tools` (as always-required)
    // through the ApprovalBroker for every op, because the internal route can
    // be called without passing through this dispatcher. Gating here too
    // would ask the human twice.
    if crate::mcp_dispatch::COMPUTER_USE_TOOLS.contains(&tool_name) {
        return Ok(());
    }
    // W3-3b (a): caller-derived — `.ephemeral/` included (see
    // [`gate_install_approval`] for what the plain path used to miss).
    let agent_dir = caller_agent_dir(home_dir, agent_id);

    // ── P2b ActionGuard: three-value irreversibility gate ────────────────────
    // Stage 1 — static classification, take-the-stricter. `in_always` folds in
    // the legacy `approval_required_tools` + install-class gate
    // (`install_approval_required`) so pre-P2b configs behave **identically**;
    // `irreversible_tools` is the additive always-field, `maybe_irreversible_tools`
    // routes through the judge. caller_is_admin is `false` (F1: the internal MCP
    // principal always holds Admin, and this agent-autonomous path is exactly
    // what WP5 must gate).
    let in_always = install_approval_required(&agent_dir, tool_name, false)
        || duduclaw_gateway::approval::tool_is_irreversible(&agent_dir, tool_name);

    // ── P3-1 VeriOS situation five-classification ASK gate ───────────────────
    // OS ACTION tools (`os_open`, future L5b native desktop actions) route
    // through the situation classifier INSTEAD of the ActionGuard
    // maybe-irreversibility judge. The classifier makes exactly one utility LLM
    // call in the residual case (superseding the maybe-judge — no double LLM
    // call for `os_open`) and its outcome merges take-the-stricter with the
    // ActionGuard STATIC always-list via `in_always`. Grant-gating (§3.65) has
    // already run before this stage, so the two mechanisms are layered, not
    // parallel: grant = "may this agent use the tool this task-phase", ASK gate
    // = "is THIS invocation's situation safe to auto-run". See
    // `situation_classifier.rs` module docs.
    if duduclaw_gateway::situation_classifier::is_os_action_tool(tool_name) {
        return gate_os_situation_dispatch(
            home_dir, agent_id, &agent_dir, tool_name, payload, in_always,
        )
        .await;
    }

    // Non-OS maybe-irreversible tools stay on the ActionGuard judge path.
    let in_maybe = duduclaw_gateway::approval::tool_is_maybe_irreversible(&agent_dir, tool_name);

    use duduclaw_gateway::approval::{
        ActionGate, JudgeVerdict, SimulationNarrative, resolve_action_gate,
    };

    // Stage 2 — resolve. Consult the LLM judge only for a pure maybe-gate.
    // `narrative` carries the D1 simulation (only ever `Some` off the
    // ConsultJudge branch, and only when the judge actually produced one) —
    // it rides along to the approval request below so the human sees it.
    // `findings` (H21) carries the closed-enumeration evidence the judge was
    // actually shown — audited regardless of verdict/error so the trail
    // records exactly what the judge saw, not just what it decided.
    let (gate, audit_status, narrative, findings) =
        match resolve_action_gate(in_always, in_maybe, None) {
            ActionGate::Auto => (
                ActionGate::Auto,
                None,
                None::<SimulationNarrative>,
                Vec::new(),
            ),
            ActionGate::RequireApproval => (ActionGate::RequireApproval, None, None, Vec::new()),
            ActionGate::ConsultJudge => {
                let outcome = action_guard_judge(home_dir, &agent_dir, tool_name, &payload).await;
                let resolved = resolve_action_gate(false, true, Some(outcome.verdict));
                let status = match (outcome.verdict, outcome.errored) {
                    (JudgeVerdict::Safe, _) => "auto_ok",
                    (JudgeVerdict::Risky, true) => "judge_error",
                    (JudgeVerdict::Risky, false) => "escalated",
                };
                let narrative = (!outcome.narrative.is_empty()).then_some(outcome.narrative);
                (resolved, Some(status), narrative, outcome.findings)
            }
        };

    // Audit the ActionGuard verdict into the existing tool_calls.jsonl trail
    // (only when the maybe-judge actually ran). `success` marks whether the
    // call was auto-passed; an escalation / judge error is NOT a pass.
    if let Some(status) = audit_status {
        let mut extras = vec![("action_guard", Value::String(status.to_string()))];
        if let Some(n) = &narrative {
            extras.push(("action_guard_simulation", n.to_json()));
        }
        // H21: record the exact closed-enumeration finding set the judge
        // prompt was built from — never the raw args (those never reached
        // the judge in the first place).
        extras.push((
            "action_guard_findings",
            Value::Array(
                findings
                    .iter()
                    .map(|f| Value::String(f.token().to_string()))
                    .collect(),
            ),
        ));
        duduclaw_security::audit::append_tool_call_with_extras(
            home_dir,
            agent_id,
            tool_name,
            &format!("ActionGuard judge → {status}"),
            matches!(gate, ActionGate::Auto),
            &extras,
        );
    }

    match gate {
        ActionGate::Auto => Ok(()),
        ActionGate::RequireApproval => {
            // The legacy path (approval_required_tools / install-class) keeps its
            // own summary via `install_approval_required`; but an
            // irreversible-only or judge-escalated tool is NOT covered by that
            // predicate, so run the fail-closed broker directly here with an
            // ActionGuard-flavored summary. `run_install_approval` performs the
            // request→block without re-checking membership.
            //
            // D1: when the judge produced a simulation narrative, fold its
            // full text into the human-facing summary ("模擬結果直接作為審批
            // 說明") and stamp it structurally on the approval row
            // (`request_with_simulation`) so the D2 channel push can render
            // the "若核准，接下來預計" forward-trajectory line.
            let mut summary = format!(
                "工具「{tool_name}」判定為不可逆／高風險，需經管理員核可後才能執行（ActionGuard 不可逆性審批閘）"
            );
            if let Some(n) = &narrative {
                let rendered = n.render();
                if !rendered.is_empty() {
                    summary.push_str("\n\n模擬結果：\n");
                    summary.push_str(&rendered);
                }
            }
            let subject = ApprovalSubject::for_tool(tool_name);
            let broker = match duduclaw_gateway::approval::ApprovalBroker::open(home_dir) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "ApprovalBroker unavailable — denying tool call (fail-closed)");
                    return Err(subject.broker_unavailable_message());
                }
            };
            let outcome = run_approval(
                &broker,
                agent_id,
                subject,
                &summary,
                payload,
                INSTALL_APPROVAL_TTL_SECONDS,
                INSTALL_APPROVAL_POLL,
                narrative.as_ref().map(|n| n.to_json()),
            )
            .await;
            match outcome {
                InstallApprovalOutcome::Proceed => Ok(()),
                InstallApprovalOutcome::Denied(msg) => Err(msg),
            }
        }
        // Resolved to a concrete gate above; ConsultJudge cannot reach here.
        ActionGate::ConsultJudge => Ok(()),
    }
}

/// P3-1: the OS-action situation ASK gate (VeriOS five-classification). Called
/// from [`gate_tool_approval_dispatch`] for OS action tools instead of the
/// ActionGuard maybe-judge. Classifies the call (deterministic Layer 1 → one
/// utility LLM call in the residual case, fail-closed to `anomaly`), audits the
/// classification, then maps the class → decision merged take-the-stricter with
/// the ActionGuard static always-list (`force_approval`):
///
/// - `normal`      → `Ok(())` (proceed; a forced approval still upgrades this).
/// - `anomaly` / `sensitive` → ApprovalBroker human approval (TTL-expiry = DENY).
/// - `missing_info` / `user_choice` → `Err(追問)` — the action does NOT run; the
///   zh-TW message returns to the agent so its LLM can supply the missing target
///   / disambiguate.
///
/// Returns `Ok(())` to proceed or `Err(zh-TW message)` on a fail-closed denial /
/// ask. Fully fail-closed: an unavailable broker denies.
pub(crate) async fn gate_os_situation_dispatch(
    home_dir: &Path,
    agent_id: &str,
    agent_dir: &Path,
    tool_name: &str,
    payload: Value,
    force_approval: bool,
) -> std::result::Result<(), String> {
    use duduclaw_gateway::situation_classifier as sc;

    let args = payload.get("arguments").cloned().unwrap_or(Value::Null);

    // Two-layer classification (deterministic → residual utility LLM call).
    let cr = sc::classify_os_action(home_dir, agent_dir, tool_name, &args).await;
    let decision = sc::merge_with_force_approval(sc::decision_for(cr.class), force_approval);

    // Audit EVERY classification (VeriOS: the label is the auditable decision).
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        agent_id,
        tool_name,
        &format!(
            "situation gate → {} ({}) ⇒ {}",
            cr.class.as_str(),
            cr.source.as_str(),
            decision.kind_str()
        ),
        matches!(decision, sc::SituationDecision::Proceed),
        &[
            (
                "situation_class",
                Value::String(cr.class.as_str().to_string()),
            ),
            (
                "situation_source",
                Value::String(cr.source.as_str().to_string()),
            ),
            (
                "situation_decision",
                Value::String(decision.kind_str().to_string()),
            ),
            ("force_approval", Value::Bool(force_approval)),
        ],
    );

    match decision {
        sc::SituationDecision::Proceed => Ok(()),
        sc::SituationDecision::Ask(msg) => Err(msg),
        sc::SituationDecision::RequireApproval => {
            let summary = format!(
                "工具「{tool_name}」情境判定為「{}」，需經管理員核可後才能執行（VeriOS 情境分類 ASK 閘）",
                cr.class.as_str()
            );
            let subject = ApprovalSubject::for_tool(tool_name);
            let broker = match duduclaw_gateway::approval::ApprovalBroker::open(home_dir) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "ApprovalBroker unavailable — denying OS action (fail-closed)");
                    return Err(subject.broker_unavailable_message());
                }
            };
            match run_approval(
                &broker,
                agent_id,
                subject,
                &summary,
                payload,
                sc::SITUATION_APPROVAL_TTL_SECS,
                INSTALL_APPROVAL_POLL,
                None,
            )
            .await
            {
                InstallApprovalOutcome::Proceed => Ok(()),
                InstallApprovalOutcome::Denied(msg) => Err(msg),
            }
        }
    }
}

// ── WP3: capability_request MCP tool (task-scoped grants, PORTICO) ──────────

/// TTL (seconds) the human approval behind `capability_request` waits for a
/// decision. Expiry counts as a denial (ApprovalBroker fail-closed).
pub(crate) const CAPABILITY_REQUEST_APPROVAL_TTL_SECS: i64 = 300;
/// Poll interval while blocking on that approval.
pub(crate) const CAPABILITY_REQUEST_APPROVAL_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// Max chars of the (agent-authored) reason persisted / surfaced to the human
/// approver. CJK-safe via `truncate_chars` — never a raw byte slice.
pub(crate) const CAPABILITY_REQUEST_REASON_MAX_CHARS: usize = 300;

/// `capability_request { tool, reason, task_id? }` — an agent asks for a
/// task-scoped grant for a tool listed in its `scoped_tools`. Files a human
/// approval via the [`ApprovalBroker`](duduclaw_gateway::approval::ApprovalBroker)
/// and, once approved, mints a grant in the
/// [`CapabilityGrantStore`](duduclaw_gateway::capability_grants::CapabilityGrantStore).
///
/// Scope: relies on the `tool_requires_scope` Admin fall-through (unlisted
/// tools default to Admin, which the internal MCP principal always holds and no
/// external client can reach — fail-closed by construction), so no scope-table
/// entry is required.
///
/// Fail-closed throughout: a broker/store that will not open, a denied/expired
/// approval, or a grant-write failure all return an error result and mint no
/// grant.
pub(crate) async fn handle_capability_request(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    use duduclaw_gateway::approval::{ApprovalBroker, ApprovalStatus};
    use duduclaw_gateway::capability_grants::{self, CapabilityGrantStore, GRANTED_BY_REQUEST};

    let tool = args
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let task_id: Option<String> = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if tool.is_empty() {
        return tool_error("capability_request 需要 tool 參數（要申請授權的工具名稱）");
    }
    if reason.is_empty() {
        return tool_error("capability_request 需要 reason 參數（向人工說明用途）");
    }

    // The tool must actually be listed in scoped_tools — otherwise a grant is
    // meaningless (the tool is either freely allowed or hard-denied elsewhere).
    // W3-3b (a): caller-derived, `.ephemeral/` included.
    let agent_dir = caller_agent_dir(home_dir, agent_id);
    let scoped = capability_grants::scoped_tools(&agent_dir);
    if !capability_grants::set_contains_tool(&scoped, tool) {
        return tool_error(&format!(
            "工具「{tool}」不在此代理的 scoped_tools 清單，無需（也無法）申請階段性授權。"
        ));
    }

    // Open the broker + grant store; either unavailable ⇒ fail-closed deny.
    let broker = match ApprovalBroker::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "capability_request: ApprovalBroker unavailable — denying (fail-closed)");
            return tool_error("審批系統暫時無法使用，已拒絕授權申請（fail-closed）。");
        }
    };
    let store = match CapabilityGrantStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "capability_request: grant store unavailable — denying (fail-closed)");
            return tool_error("授權存放區暫時無法使用，已拒絕授權申請（fail-closed）。");
        }
    };

    let reason_trunc = duduclaw_core::truncate_chars(reason, CAPABILITY_REQUEST_REASON_MAX_CHARS);
    let summary = format!("代理 {agent_id} 申請階段性工具授權：{tool}（{reason_trunc}）");
    let payload = serde_json::json!({
        "tool": tool,
        "reason": reason_trunc,
        "task_id": task_id,
    });

    let approval_id = match broker
        .request(
            agent_id,
            "capability_grant",
            &summary,
            payload,
            CAPABILITY_REQUEST_APPROVAL_TTL_SECS,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            warn!(error = %e, "capability_request: approval request failed — denying (fail-closed)");
            return tool_error("無法建立授權審核請求，已拒絕（fail-closed）。");
        }
    };

    match broker
        .await_decision(&approval_id, CAPABILITY_REQUEST_APPROVAL_POLL)
        .await
    {
        Ok(ApprovalStatus::Approved) => {
            let ttl = capability_grants::grant_ttl_secs(&agent_dir);
            match store
                .grant(agent_id, task_id.as_deref(), tool, GRANTED_BY_REQUEST, ttl)
                .await
            {
                Ok(grant_id) => {
                    duduclaw_security::audit::append_tool_call_with_extras(
                        home_dir,
                        agent_id,
                        "capability_request",
                        &format!("grant {tool}"),
                        true,
                        &[
                            ("grant_id", Value::String(grant_id.clone())),
                            ("granted_tool", Value::String(tool.to_string())),
                            (
                                "task_id",
                                task_id
                                    .as_deref()
                                    .map(|t| Value::String(t.to_string()))
                                    .unwrap_or(Value::Null),
                            ),
                            ("granted_by", Value::String(GRANTED_BY_REQUEST.to_string())),
                        ],
                    );
                    tool_text(&format!(
                        "已核准：工具「{tool}」授權 {ttl} 秒（授權編號 {grant_id}）。\
                         任務階段結束或逾時後自動撤銷。"
                    ))
                }
                Err(e) => {
                    warn!(error = %e, "capability_request: grant write failed after approval");
                    tool_error(&format!(
                        "授權雖經核准，但寫入失敗，已拒絕使用（fail-closed）：{e}"
                    ))
                }
            }
        }
        Ok(ApprovalStatus::Denied) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                agent_id,
                "capability_request",
                &format!("deny {tool}"),
                false,
                &[("granted_tool", Value::String(tool.to_string()))],
            );
            tool_error(&format!("授權申請已被拒絕（審核編號 {approval_id}）。"))
        }
        Ok(ApprovalStatus::Expired) => tool_error(&format!(
            "授權申請逾時未核可，已自動拒絕（fail-closed，審核編號 {approval_id}）。"
        )),
        Ok(ApprovalStatus::Pending) => {
            tool_error("審核狀態異常（仍為待審），已拒絕授權申請（fail-closed）。")
        }
        Err(e) => {
            warn!(error = %e, "capability_request: await_decision failed — denying (fail-closed)");
            tool_error("等待授權決定時發生錯誤，已拒絕（fail-closed）。")
        }
    }
}
