use super::*;

/// Append any pending "rules changed" / "model switched" FYI line(s) — see
/// `pending_agent_notice` — to an outgoing reply, then clear the flag(s) so
/// the same change is never announced twice. External senders leave it pending.
/// A reply that is already empty
/// (silent-by-design drops: circuit-breaker denial, blocked/unpaired user,
/// injection-scan block) is left untouched — turning a deliberate silence
/// into a visible message would defeat the gate that produced it.
pub(super) fn append_pending_agent_notice(
    reply: String,
    home_dir: &std::path::Path,
    agent_id: &str,
    session_id: &str,
    user_id: &str,
) -> String {
    if reply.is_empty() || !sender_is_internal_for_reply(home_dir, session_id, user_id, agent_id) {
        return reply;
    }
    match crate::pending_agent_notice::take_pending_notice_suffix(home_dir, agent_id) {
        Some(suffix) => format!("{reply}\n\n{suffix}"),
        None => reply,
    }
}

/// Share the thinking policy's sender identity checks; unknown/public sessions fail closed.
pub(super) fn sender_is_internal_for_reply(
    home: &Path, session: &str, user: &str, agent: &str,
) -> bool {
    if super::assembly::public_session(session) { return false; }
    crate::trajectory_guard::channel_from_session_id(session).is_some_and(|channel| {
        crate::thinking::channel_policy::sender_is_internal(home, channel, user, agent)
    })
}

#[cfg(test)]
mod notice_audience_tests {
    use super::*;

    #[test]
    fn only_verified_staff_consume_pending_notices() {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let employee = db.create_user("staff@test.local", "Staff", "StaffPass-12345",
            duduclaw_auth::UserRole::Employee).unwrap();
        db.bind_agent(&employee.id, "a", duduclaw_auth::AccessLevel::Operator).unwrap();
        db.bind_channel_identity(&employee.id, "googlechat", "users/101", true).unwrap();
        db.bind_channel_identity(&employee.id, "googlechat", "users/102", false).unwrap();
        crate::pending_agent_notice::mark_model_changed(home.path(), "a");
        assert_eq!(append_pending_agent_notice("answer".into(), home.path(), "a",
            "googlechat:spaces/a", "users/102"), "answer");
        assert_eq!(append_pending_agent_notice("".into(), home.path(), "a",
            "googlechat:spaces/a", "users/101"), "");
        assert_eq!(append_pending_agent_notice("answer".into(), home.path(), "a",
            "googlechat:spaces/a", "users/101"), "answer\n\n（已切換至新模型）");
        assert_eq!(append_pending_agent_notice("answer".into(), home.path(), "a",
            "googlechat:spaces/a", "users/101"), "answer");
        assert!(!sender_is_internal_for_reply(home.path(), "odoolivechat:1", "users/101", "a"));
    }

    #[test]
    fn external_turns_leave_notice_pending() {
        let home = tempfile::tempdir().unwrap();
        crate::pending_agent_notice::mark_model_changed(home.path(), "a");
        for session in ["googlechat:spaces/a", "telegram:1", "odoolivechat:1", "unknown:1"] {
            assert_eq!(append_pending_agent_notice("answer".into(), home.path(), "a", session, "visitor"), "answer");
        }
        assert_eq!(crate::pending_agent_notice::take_pending_notice_suffix(home.path(), "a"),
            Some("（已切換至新模型）".into()));
    }
}

/// Policy snapshot tests plus the helpers the per-channel gate tests share.
#[cfg(test)]
#[path = "access_gate_tests.rs"]
pub(crate) mod access_gate_tests;

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

tokio::task_local! {
    /// Set when the current turn ends in a deliberate silence (see
    /// [`track_silence`]).
    static DELIBERATE_SILENCE: std::sync::Arc<std::sync::atomic::AtomicBool>;
}

/// Run a turn and report whether its empty answer (if any) was a
/// deliberate silence (access gate, failsafe mute, breaker, rate limit,
/// takeover) rather than a model that produced nothing. WebChat uses it to
/// tell the visitor about a genuinely empty answer without revealing that a
/// protection kept it quiet (B10 review L1).
pub(crate) async fn track_silence<F: std::future::Future>(turn: F) -> (F::Output, bool) {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let out = DELIBERATE_SILENCE.scope(flag.clone(), turn).await;
    (out, flag.load(std::sync::atomic::Ordering::SeqCst))
}

/// Mark the current turn's silence as deliberate (no-op outside
/// [`track_silence`]). [`record_silent_reply`] does this itself.
pub(crate) fn mark_deliberate_silence() {
    let _ = DELIBERATE_SILENCE.try_with(|f| f.store(true, std::sync::atomic::Ordering::SeqCst));
}

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
    mark_deliberate_silence();
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

/// Typed settings from one successful SQL snapshot. Never client-supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UserAccessPolicy {
    pub allowed: Option<Vec<String>>,
    pub blocked: Vec<String>,
    pub require_pairing: bool,
}

impl UserAccessPolicy {
    /// Nothing configured: the fully open default of a personal install.
    pub(crate) fn is_open(&self) -> bool {
        !self.require_pairing && self.allowed.is_none() && self.blocked.is_empty()
    }
}

/// Why the user access policy could not be established. Either way the
/// gates deny (fail closed); the variant only feeds the audit reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessPolicyError {
    /// The settings store could not be read: SQLite / I/O error, revoked
    /// I/O authority, or only the non-durable in-memory fallback store is
    /// open (it cannot see a policy saved on disk).
    Unavailable,
    /// A stored access value is malformed: `allowed_users` /
    /// `blocked_users` is not a JSON array of strings, or
    /// `require_pairing` is anything other than exactly `true` / `false`.
    Malformed,
}

impl AccessPolicyError {
    fn audit_reason(self) -> &'static str {
        match self {
            Self::Unavailable => "silent_by_design: access_policy_unavailable",
            Self::Malformed => "silent_by_design: access_policy_malformed",
        }
    }
}

/// The three per-channel keys that make up the user access policy.
pub(crate) const USER_ACCESS_POLICY_KEYS: &[&str] = &[
    crate::channel_settings::keys::ALLOWED_USERS,
    crate::channel_settings::keys::BLOCKED_USERS,
    crate::channel_settings::keys::REQUIRE_PAIRING,
];

/// Parse the `global`-scope `(scope, key, value)` rows of one snapshot.
/// Missing keys (and a list stored as the empty string, the legacy
/// "cleared" encoding) mean "not configured"; anything unparsable is
/// [`AccessPolicyError::Malformed`]. Pure — unit-tested.
pub(crate) fn parse_user_access_rows(
    rows: &[(String, String, String)],
) -> Result<UserAccessPolicy, AccessPolicyError> {
    use crate::channel_settings::keys;
    let value = |key: &str| {
        rows.iter()
            .find(|(scope, k, _)| scope == "global" && k == key)
            .map(|(_, _, v)| v.as_str())
    };
    let list = |key: &str| -> Result<Vec<String>, AccessPolicyError> {
        match value(key) {
            None | Some("") => Ok(Vec::new()),
            Some(raw) => serde_json::from_str::<Vec<String>>(raw)
                .map_err(|_| AccessPolicyError::Malformed),
        }
    };
    let allowed = list(keys::ALLOWED_USERS)?;
    let blocked = list(keys::BLOCKED_USERS)?;
    let require_pairing = match value(keys::REQUIRE_PAIRING) {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(AccessPolicyError::Malformed),
    };
    Ok(UserAccessPolicy {
        allowed: (!allowed.is_empty()).then_some(allowed),
        blocked,
        require_pairing,
    })
}

/// THE user access policy snapshot: one authoritative SQLite read of the
/// channel's `global` access keys (never the read cache), so a read or parse
/// failure can never look like "not configured". `Ok` with
/// [`UserAccessPolicy::is_open`] only when the read succeeded and none of
/// the keys is set. Every gate in this module goes through it.
pub(crate) async fn read_user_access_policy(
    ctx: &ReplyContext,
    channel: &str,
) -> Result<UserAccessPolicy, AccessPolicyError> {
    let rows = ctx
        .channel_settings
        .read_global_rows_raw(channel, USER_ACCESS_POLICY_KEYS)
        .await
        .map_err(|e| {
            warn!(channel, error = %e, "user access policy unreadable — denying");
            AccessPolicyError::Unavailable
        })?;
    parse_user_access_rows(&rows).inspect_err(|_| {
        warn!(channel, "user access policy malformed — denying");
    })
}

/// The deny a gate returns when the policy cannot be established: the same
/// silence a blocked sender gets (no new wording, no internal detail — a
/// public visitor learns nothing), plus an audit line saying why.
fn deny_unknown_policy(
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    err: AccessPolicyError,
) -> String {
    record_silent_reply(&ctx.home_dir, session_id, user_id, err.audit_reason());
    String::new()
}

/// Central per-user access gate (allowlist / blocklist / pairing).
/// Called once at the top of the reply pipeline so every channel is covered
/// by one enforcement point. `pub(crate)` so the per-channel chat-command
/// intercepts, which run BEFORE this pipeline, can apply the SAME gate
/// (prefer [`check_command_access_gate`] there).
/// Returns:
/// - `None` — allowed; continue to the AI pipeline.
/// - `Some("")` — blocked, or the policy could not be read/parsed; every
///   channel skips sending an empty reply, so the sender gets silence.
/// - `Some(text)` — early reply (pairing hint, or the `/pair` verdict).
///
/// Defaults are fully open: when the snapshot read succeeds and none of
/// `allowed_users` / `blocked_users` / `require_pairing` is stored, this
/// returns `None`.
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
    match read_user_access_policy(ctx, channel).await {
        Ok(policy) => {
            check_user_access_gate_with_policy(ctx, session_id, user_id, text, &policy).await
        }
        Err(err) => Some(deny_unknown_policy(ctx, session_id, user_id, err)),
    }
}

/// The gate for chat commands and interactive buttons. Same as
/// [`check_user_access_gate`] (fail-closed snapshot), plus: once a policy
/// exists, a command without a known human sender is denied — a shared
/// conversation id (group / space / channel) never stands in for the person
/// who typed or pressed. Platform permissions checked by the caller only
/// narrow this further.
pub(crate) async fn check_command_access_gate(
    ctx: &ReplyContext,
    session_id: &str,
    sender: &str,
    text: &str,
) -> Option<String> {
    let channel = session_id.split(':').next().unwrap_or("");
    if !GATED_CHANNELS.contains(&channel) {
        return None;
    }
    let policy = match read_user_access_policy(ctx, channel).await {
        Ok(policy) => policy,
        Err(err) => return Some(deny_unknown_policy(ctx, session_id, sender, err)),
    };
    check_command_access_with_policy(ctx, session_id, sender, text, &policy).await
}

/// [`check_command_access_gate`] against a snapshot the caller already read
/// (e.g. Discord's trusted guild/channel read).
pub(crate) async fn check_command_access_with_policy(
    ctx: &ReplyContext,
    session_id: &str,
    sender: &str,
    text: &str,
    policy: &UserAccessPolicy,
) -> Option<String> {
    if !policy.is_open() && sender.trim().is_empty() {
        record_silent_reply(&ctx.home_dir, session_id, sender, "silent_by_design: command_without_sender");
        return Some(String::new());
    }
    check_user_access_gate_with_policy(ctx, session_id, sender, text, policy).await
}

/// Preserve the normal `/pair` contract against the adapter's one validated
/// snapshot. Strict decision routing separately rereads current SQL policy.
pub(crate) async fn check_user_access_gate_with_policy(
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    text: &str,
    policy: &UserAccessPolicy,
) -> Option<String> {
    let allowed = &policy.allowed;
    let blocked = &policy.blocked;
    let require_pairing = policy.require_pairing;
    // Fast path: nothing configured → open access, zero overhead beyond reads.
    if policy.is_open() {
        return None;
    }

    // `/pair <code>` must be usable by not-yet-approved users — intercept it
    // before the access decision. Codes are operator-generated via the
    // `pairing_manage` MCP tool (`action: "generate"`) for either the user
    // id or the session id.
    let trimmed = text.trim();
    if let Some(code) = trimmed.strip_prefix("/pair ").map(str::trim) {
        if !code.is_empty() {
            // Blocked users may not pair (exact match on the sender, the
            // conversation, or a blocked bucket of the conversation).
            let conversation = crate::access_control::pairing_subject(session_id);
            if blocked.iter().any(|b| {
                b == user_id
                    || b == session_id
                    || crate::access_control::pairing_subject(b) == conversation
            }) {
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

/// The checks a channel reply that skips the model (the `@` list, an `@`
/// refusal, "say the task") must still pass, in the order the reply pipeline
/// applies them: the user access gate, the `/takeover` lifecycle command and
/// human takeover. `Some(text)` = send `text` instead (empty = send nothing);
/// `None` = the direct reply may go out.
pub(crate) async fn gate_direct_reply(
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    text: &str,
) -> Option<String> {
    if let Some(reply) = check_user_access_gate(ctx, session_id, user_id, text).await {
        return Some(reply);
    }
    if crate::odoo_livechat::is_public_visitor_session(session_id) {
        return None;
    }
    match crate::takeover::intercept(ctx, session_id, user_id, text).await {
        Some(crate::takeover::Intercepted::Announce(msg)) => Some(msg),
        Some(crate::takeover::Intercepted::Silent) => Some(String::new()),
        None => None,
    }
}
