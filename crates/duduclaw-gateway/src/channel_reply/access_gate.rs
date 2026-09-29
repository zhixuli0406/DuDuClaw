use super::*;

/// Append any pending "rules changed" / "model switched" FYI line(s) — see
/// `pending_agent_notice` — to an outgoing reply, then clear the flag(s) so
/// the same change is never announced twice. A reply that is already empty
/// (silent-by-design drops: circuit-breaker denial, blocked/unpaired user,
/// injection-scan block) is left untouched — turning a deliberate silence
/// into a visible message would defeat the gate that produced it.
pub(super) fn append_pending_agent_notice(
    reply: String,
    home_dir: &std::path::Path,
    agent_id: &str,
) -> String {
    if reply.is_empty() {
        return reply;
    }
    match crate::pending_agent_notice::take_pending_notice_suffix(home_dir, agent_id) {
        Some(suffix) => format!("{reply}\n\n{suffix}"),
        None => reply,
    }
}

/// Channel session-id prefixes subject to the user access gate. Internal
/// sessions ("default", cron/bus/heartbeat ids) are never gated.
pub(super) const GATED_CHANNELS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "line",
    "whatsapp",
    "feishu",
    "googlechat",
    "teams",
    "webchat",
    "wecom",
    "dingtalk",
];

/// Record a `channel_failures.jsonl` line for a reply that is intentionally
/// dropped by design (pairing/access/failsafe/circuit-breaker gates). The
/// gate's safety semantics are unchanged — it still never replies to the
/// user — this only makes "why did this person get silence" answerable from
/// the dashboard/doctor tooling instead of requiring a live debug session.
/// Best-effort: a write failure is logged, never propagated.
pub(crate) fn record_silent_reply(
    home_dir: &std::path::Path,
    session_id: &str,
    user_id: &str,
    reason: &str,
) {
    let rec = serde_json::json!({
        "event": "channel_reply_silent",
        "session_id": session_id,
        // W2-4: which platform the person was silenced on. `null` for
        // non-channel sessions.
        "channel": crate::trajectory_guard::channel_from_session_id(session_id),
        "user_id": user_id,
        "reason": reason,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Err(e) = crate::trajectory_guard::append_anomaly(home_dir, &rec) {
        warn!(error = %e, "silent-reply audit: 寫入 channel_failures.jsonl 失敗");
    }
}

/// Central per-user access gate (allowlist / blocklist / pairing).
/// Called once at the top of the reply pipeline so every channel is covered
/// by one enforcement point. `pub(crate)` so the per-channel chat-command
/// intercepts (telegram/discord/line/slack), which run BEFORE this pipeline,
/// can apply the SAME gate before executing a command — an unpaired or
/// blocked user must not be able to run /undo //rollback //new //handoff.
/// Returns:
/// - `None` — allowed; continue to the AI pipeline.
/// - `Some("")` — blocked; every channel skips sending an empty reply, so
///   blocked users are silently ignored.
/// - `Some(text)` — early reply (pairing hint, or the `/pair` verdict).
///
/// Defaults are fully open: with no `allowed_users` / `blocked_users` /
/// `require_pairing` settings stored, this returns `None` unconditionally.
pub(crate) async fn check_user_access_gate(
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    text: &str,
) -> Option<String> {
    let channel = session_id.split(':').next().unwrap_or("");
    if !GATED_CHANNELS.contains(&channel) {
        return None;
    }

    let settings = &ctx.channel_settings;
    let require_pairing = settings
        .get_bool(
            channel,
            "global",
            crate::channel_settings::keys::REQUIRE_PAIRING,
            false,
        )
        .await;
    let parse_list = |v: Option<String>| -> Option<Vec<String>> {
        let v = v?;
        if v.is_empty() {
            return None;
        }
        serde_json::from_str::<Vec<String>>(&v)
            .ok()
            .filter(|l| !l.is_empty())
    };
    let allowed = parse_list(
        settings
            .get(
                channel,
                "global",
                crate::channel_settings::keys::ALLOWED_USERS,
            )
            .await,
    );
    let blocked = parse_list(
        settings
            .get(
                channel,
                "global",
                crate::channel_settings::keys::BLOCKED_USERS,
            )
            .await,
    )
    .unwrap_or_default();

    // Fast path: nothing configured → open access, zero overhead beyond reads.
    if !require_pairing && allowed.is_none() && blocked.is_empty() {
        return None;
    }

    // `/pair <code>` must be usable by not-yet-approved users — intercept it
    // before the access decision. Codes are operator-generated via the
    // `pairing_generate` MCP tool for either the user id or the session id.
    let trimmed = text.trim();
    if let Some(code) = trimmed.strip_prefix("/pair ").map(str::trim) {
        if !code.is_empty() {
            // Blocked users may not pair.
            if blocked.iter().any(|b| b == user_id || b == session_id) {
                record_silent_reply(
                    &ctx.home_dir,
                    session_id,
                    user_id,
                    "silent_by_design: pair_blocked",
                );
                return Some(String::new());
            }
            let ok = ctx.access_control.verify_pairing_code(user_id, code).await
                || ctx
                    .access_control
                    .verify_pairing_code(session_id, code)
                    .await;
            return Some(if ok {
                "✅ 配對成功，現在可以開始對話了。".to_string()
            } else {
                "❌ 配對碼錯誤或已過期，請向管理員索取新的配對碼。".to_string()
            });
        }
    }

    match ctx
        .access_control
        .check_access_dual(
            user_id,
            session_id,
            allowed.as_deref(),
            &blocked,
            require_pairing,
        )
        .await
    {
        crate::access_control::AccessDecision::Allowed => None,
        crate::access_control::AccessDecision::Blocked => {
            record_silent_reply(
                &ctx.home_dir,
                session_id,
                user_id,
                "silent_by_design: access_blocked",
            );
            Some(String::new())
        }
        crate::access_control::AccessDecision::RequirePairing => {
            Some("🔒 尚未配對。請向管理員索取配對碼，並輸入：/pair <配對碼>".to_string())
        }
    }
}

/// Membership check for the per-channel `admin_users` JSON list. Pure —
/// unit-tested. Exact equality against any provided identity (never
/// substring). Missing / empty / malformed list ⇒ NOT admin (fail-closed).
pub(crate) fn admin_list_contains(list_json: Option<&str>, identities: &[&str]) -> bool {
    let Some(raw) = list_json else { return false };
    let Ok(list) = serde_json::from_str::<Vec<String>>(raw) else {
        return false;
    };
    list.iter()
        .any(|a| !a.is_empty() && identities.iter().any(|id| a == id))
}

/// Real per-channel admin status for admin-gated chat commands
/// (`!STOP` / `!STOP ALL` / `!RESUME`). Reads the `admin_users` channel
/// setting (JSON array of user/chat ids, global scope) and matches any of
/// the caller's identities exactly. Fail-closed: no `admin_users`
/// configured ⇒ nobody is admin on that channel — safety words then only
/// work where an admin identity has been configured (previously every
/// channel hardcoded `is_admin = true`, letting any group member halt the
/// platform).
pub(crate) async fn is_channel_admin(
    ctx: &ReplyContext,
    channel: &str,
    identities: &[&str],
) -> bool {
    let raw = ctx
        .channel_settings
        .get(
            channel,
            "global",
            crate::channel_settings::keys::ADMIN_USERS,
        )
        .await;
    admin_list_contains(raw.as_deref(), identities)
}

