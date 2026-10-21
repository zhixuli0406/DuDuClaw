use super::*;

// ── User sentiment detection ───────────────────────────────

/// Detect user satisfaction heuristic from message text (zero LLM cost).
///
/// Positive signals: gratitude, approval, emoji thumbs-up, CJK equivalents.
/// Negative signals: corrections, complaints, error reports, CJK equivalents.
/// Returns `None` if no clear signal detected (neutral message).
pub(super) fn detect_user_sentiment(text: &str) -> Option<Sentiment> {
    let lower = text.to_lowercase();
    let positive_signals = [
        "thanks",
        "thank you",
        "great",
        "good",
        "perfect",
        "awesome",
        "nice",
        "\u{1f44d}",                // 👍
        "\u{1f389}",                // 🎉
        "\u{2705}",                 // ✅
        "\u{8b1d}\u{8b1d}",         // 謝謝
        "\u{611f}\u{8b1d}",         // 感謝
        "\u{5b8c}\u{7f8e}",         // 完美
        "\u{597d}\u{7684}",         // 好的
        "\u{8b9a}",                 // 讚
        "\u{592a}\u{597d}\u{4e86}", // 太好了
        "\u{5f88}\u{597d}",         // 很好
    ];
    let negative_signals = [
        "no",
        "wrong",
        "incorrect",
        "fix",
        "error",
        "bug",
        "\u{4e0d}\u{5c0d}",         // 不對
        "\u{932f}\u{4e86}",         // 錯了
        "\u{91cd}\u{4f86}",         // 重來
        "\u{4e0d}\u{884c}",         // 不行
        "\u{4fee}\u{6b63}",         // 修正
        "\u{6709}\u{554f}\u{984c}", // 有問題
    ];

    if positive_signals.iter().any(|s| lower.contains(s)) {
        Some(Sentiment::Positive)
    } else if negative_signals.iter().any(|s| lower.contains(s)) {
        Some(Sentiment::Negative)
    } else {
        None
    }
}

// ── Public API ──────────────────────────────────────────────

/// Build a reply for an incoming user message (no user tracking).
///
/// Strategy:
/// 1. Try Python Claude Code SDK (subprocess) — uses rotator + budget tracking
/// 2. Fallback to direct Anthropic API (Rust reqwest) — single key only
/// 3. Fallback to static error message
pub async fn build_reply(text: &str, ctx: &ReplyContext) -> String {
    build_guarded_reply_with_session(text, ctx, "default", "anonymous", None)
        .await
        .into_legacy_parts()
        .0
}

/// RFC-23: restore any `<REDACT:...>` tokens in the reply text using the
/// agent's per-session vault. Caller is always `owner` since the text is
/// destined for the channel's end-user. Errors are swallowed (return the
/// raw text — tokens stay verbatim, which is safe).
pub(super) async fn restore_for_channel(
    text: String,
    ctx: &ReplyContext,
    agent_id: &str,
    session_id: &str,
) -> String {
    let Some(manager) = ctx.redaction_manager.as_ref() else {
        return text;
    };
    // Quick scan: if there's no `<REDACT:` substring at all, skip the
    // pipeline construction entirely (hot path).
    if !text.contains(duduclaw_redaction::token::TOKEN_PREFIX) {
        return text;
    }
    let pipeline = match manager.pipeline(agent_id, Some(session_id.to_string())) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, agent = %agent_id, "redaction: pipeline build failed; returning raw text");
            return text;
        }
    };
    let caller = duduclaw_redaction::Caller::owner(agent_id);
    pipeline
        .restore(
            &text,
            &caller,
            duduclaw_redaction::RestoreTarget::UserChannel,
        )
        .unwrap_or_else(|e| {
            warn!(error = %e, "redaction: restore failed; returning raw text");
            text
        })
}

/// zh-TW canned reply used when a response is blocked by a CONTRACT.toml
/// `must_not` boundary (P2-3). Deliberately generic — never echoes the
/// violating content.
pub(super) const CONTRACT_BLOCK_MESSAGE: &str = "⚠️ 這則回覆因違反行為契約邊界而被攔截，未送出。";

/// Why [`check_outbound_text`] refused a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutboundBlock {
    /// The opt-in `[guardrails]` output scan (short reason, never content).
    Guardrail(String),
    /// CONTRACT.toml `must_not` rules that matched (rule names only).
    Contract(Vec<String>),
}

impl OutboundBlock {
    /// Audit lines for `security_audit.jsonl` (names/reasons only).
    pub(crate) fn audit_rules(&self) -> Vec<String> {
        match self {
            OutboundBlock::Guardrail(reason) => vec![format!("guardrail: {reason}")],
            OutboundBlock::Contract(rules) => rules.clone(),
        }
    }
}

/// Outcome of the outbound content checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutboundVerdict {
    /// Send as-is.
    Allow,
    /// Send this guardrail-redacted text instead (it passed the contract).
    Redacted(String),
    /// Do not send.
    Blocked(OutboundBlock),
}

/// The outbound checks with no side effects: the opt-in output guardrail
/// first, then CONTRACT.toml `must_not` on the (possibly guardrail-redacted)
/// text. No logging, no audit, no events — the caller decides what a refusal
/// means for its own surface (a channel reply is replaced; a thinking
/// summary is withheld while the reply goes on).
pub(crate) fn check_outbound_text(
    text: &str,
    home_dir: &std::path::Path,
    agent_id: &str,
) -> OutboundVerdict {
    let agent_dir = home_dir.join("agents").join(agent_id);
    let guard_cfg = crate::guardrail::load_guardrail_config(&agent_dir);
    let redacted = if guard_cfg.enabled {
        match crate::guardrail::scan_output(text, &guard_cfg) {
            crate::guardrail::GuardrailAction::Allow => None,
            crate::guardrail::GuardrailAction::Redacted(t) => Some(t),
            crate::guardrail::GuardrailAction::Blocked(reason) => {
                return OutboundVerdict::Blocked(OutboundBlock::Guardrail(reason));
            }
        }
    } else {
        None
    };
    let contract = duduclaw_agent::contract::load_contract(&agent_dir);
    if !contract.boundaries.must_not.is_empty() {
        let checked = redacted.as_deref().unwrap_or(text);
        let result = duduclaw_agent::contract::validate_response(&contract, checked);
        if !result.passed {
            let rules = result.violations.iter().map(|v| v.rule.clone()).collect();
            return OutboundVerdict::Blocked(OutboundBlock::Contract(rules));
        }
    }
    match redacted {
        Some(t) => OutboundVerdict::Redacted(t),
        None => OutboundVerdict::Allow,
    }
}

/// P2-3: enforce CONTRACT.toml `must_not` boundaries on the FINAL user-facing
/// bytes (I9 — validate the artifact that actually takes effect, i.e. AFTER
/// secret restoration in `restore_for_channel`). D-6 = block: a violating reply
/// is replaced with a safe refusal and audited to `security_audit.jsonl`, never
/// sent. Empty `must_not` (or no CONTRACT.toml) → passthrough (no overhead).
/// The output guardrail (opt-in `[guardrails]`) runs first. The checks
/// themselves live in [`check_outbound_text`]; this wrapper adds the logs,
/// the audit row, the security event and the canned replacement.
pub(super) async fn enforce_contract(
    final_text: String,
    home_dir: &std::path::Path,
    agent_id: &str,
) -> String {
    match check_outbound_text(&final_text, home_dir, agent_id) {
        OutboundVerdict::Allow => final_text,
        OutboundVerdict::Redacted(t) => {
            warn!(agent = %agent_id, "guardrail redacted PII in outgoing reply");
            t
        }
        OutboundVerdict::Blocked(block) => {
            match &block {
                OutboundBlock::Guardrail(reason) => {
                    warn!(agent = %agent_id, %reason, "guardrail BLOCKED outgoing reply")
                }
                OutboundBlock::Contract(rules) => warn!(
                    agent = %agent_id, ?rules,
                    "CONTRACT must_not violation — blocking outgoing reply"
                ),
            }
            duduclaw_security::audit::log_contract_violation(
                home_dir,
                agent_id,
                &block.audit_rules(),
            );
            // C1 producer 甲 companion — see `security_autopilot.rs`.
            crate::security_autopilot::emit_contract_violation(agent_id);
            match block {
                OutboundBlock::Guardrail(_) => crate::guardrail::blocked_reply(),
                OutboundBlock::Contract(_) => CONTRACT_BLOCK_MESSAGE.to_string(),
            }
        }
    }
}

/// Best-effort agent-id resolution for outer restore wrappers — mirrors
/// the order used by `build_reply_with_session_inner` but without the
/// trigger-word matcher (the wrapper only needs *some* agent id to pick
/// the per-agent key; if it's wrong, restore yields a miss and the
/// raw token stays in place — that's safe-by-default).
pub(super) async fn resolve_agent_for_restore(ctx: &ReplyContext, session_id: &str) -> String {
    if let Some(name) = get_default_agent(&ctx.home_dir).await {
        return name;
    }
    let reg = ctx.registry.read().await;
    if let Some(a) = reg.main_agent() {
        return a.config.agent.name.clone();
    }
    // Last-ditch: session_id prefix.
    session_id
        .split(':')
        .next()
        .unwrap_or("default")
        .to_string()
}

/// Public alias of [`resolve_agent_for_restore`] for callers outside the reply
/// pipeline that need "which AI employee owns this conversation" — currently
/// [`crate::takeover`], which must attribute an Activity Feed row and a
/// session write without duplicating the resolution order.
pub async fn resolve_agent_for_session(ctx: &ReplyContext, session_id: &str) -> String {
    resolve_agent_for_restore(ctx, session_id).await
}

/// CJK-aware token estimate, exposed for the takeover path which appends a
/// turn to the session without running the AI and must cost it the same way
/// the normal path does.
pub fn estimate_tokens_public(text: &str) -> u32 {
    estimate_tokens(text)
}

/// Build a reply with progress streaming.
///
/// `on_progress` callback receives real-time progress events (keepalive,
/// tool-use details) that the channel handler can forward to the user.
pub async fn build_reply_with_progress(
    text: &str,
    ctx: &ReplyContext,
    on_progress: Option<ProgressCallback>,
) -> String {
    build_guarded_reply_with_session(text, ctx, "default", "anonymous", on_progress)
        .await
        .into_legacy_parts()
        .0
}

