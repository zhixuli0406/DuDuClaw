use super::*;

/// Lowercase + collapse all whitespace runs to a single space, so
/// containment checks are robust to formatting differences between a
/// wiki page and an extracted fact. CJK-safe (no byte slicing).
pub(super) fn normalize_for_dedup(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Wiki/memory injection dedup: keep only facts whose normalized text is
/// NOT already present in the (normalized) prompt built so far — the
/// prompt already contains the injected wiki pages, so a contained fact
/// would be pure duplication. Trivially short facts (< 6 chars) are kept
/// as-is: containment matches on them are too noisy to trust.
pub(super) fn filter_facts_not_in_prompt(facts: &[String], prompt: &str) -> Vec<String> {
    let normalized_prompt = normalize_for_dedup(prompt);
    facts
        .iter()
        .filter(|f| {
            let nf = normalize_for_dedup(f);
            nf.chars().count() < 6 || !normalized_prompt.contains(&nf)
        })
        .cloned()
        .collect()
}

/// Classify an error string produced by `call_claude_cli_rotated` or the Direct API fallback.
pub(crate) fn classify_cli_failure(err: &str) -> FailureReason {
    let lower = err.to_lowercase();

    if lower.contains("claude cli not found") {
        return FailureReason::BinaryMissing;
    }
    // Auth failures come through the stream-json `is_error` branch as
    // "claude CLI stream error: Not logged in · Please run /login" or
    // "claude CLI assistant error: authentication_failed".
    //
    // 2026-09-08 incident (DESIGN-account-credential-hardening-2026-09 §D2):
    // an org whose Claude Code subscription access was revoked answers with
    // `oauth_org_not_allowed` / `oauth_not_allowed_for_organization`, and a
    // pasted short-lived access token answers with
    // "OAuth access token is invalid". Neither matched here, so 18 hours of
    // dead-token failures classified as `Unknown` — the rotator kept retrying
    // them on the generic error path and the user-facing message pointed at
    // the debug log instead of the account page.
    if lower.contains("not logged in")
        || lower.contains("authentication_failed")
        || lower.contains("please run /login")
        || lower.contains("oauth_org_not_allowed")
        || lower.contains("oauth_not_allowed_for_organization")
        || lower.contains("not allowed for this organization")
        || lower.contains("invalid bearer token")
        || lower.contains("oauth access token is invalid")
        || lower.contains("disabled claude subscription access")
    {
        return FailureReason::AuthFailed;
    }
    if lower.contains("hard timeout") {
        return FailureReason::Timeout;
    }
    if lower.contains("empty response") {
        return FailureReason::EmptyResponse;
    }
    // WP10 M4: tiered "nothing selectable" markers emitted by
    // `rotate_cli_spawn`. Checked before the generic no-accounts test because
    // they are strictly more specific.
    if lower.contains("no accounts available: billing cooldown") {
        return FailureReason::AccountsCoolingDownLong;
    }
    if lower.contains("no accounts available: short cooldown") {
        return FailureReason::AccountsCoolingDownShort;
    }
    if lower.contains("no accounts available: reason unknown") {
        return FailureReason::AccountsCoolingDownUnknown;
    }
    if lower.contains("no accounts") || lower.contains("no account configured") {
        return FailureReason::NoAccounts;
    }
    // Reuse the shared billing/rate classifiers so we stay in sync with claude_runner.
    if crate::claude_runner::is_billing_error(err) {
        return FailureReason::Billing;
    }
    if crate::claude_runner::is_rate_limit_error(err) {
        return FailureReason::RateLimited;
    }
    if lower.contains("spawn error")
        || lower.contains("no such file")
        || lower.contains("exit ")
        || lower.contains("read error")
    {
        return FailureReason::SpawnError;
    }
    FailureReason::Unknown
}

/// Sub-classify an [`FailureReason::AuthFailed`] error into the two kinds a
/// human has to act on differently.
///
/// * `"org_disabled"` — Anthropic revoked this organization's Claude Code
///   subscription access (HTTP 403). A new token from the same org will fail
///   the same way; the fix is an API key or an admin conversation.
/// * `"invalid_token"` — the credential itself is wrong or expired (HTTP
///   401). Re-running `claude setup-token` fixes it.
/// * `None` — not an auth failure at all.
///
/// The string form feeds the operator-facing outage message
/// (`auth_outage.rs`); [`auth_failure_kind_for`] maps the same verdict onto
/// the rotator's typed `AuthFailureKind` for the state machine.
pub(crate) fn auth_failure_kind_hint(err: &str) -> Option<&'static str> {
    if classify_cli_failure(err) != FailureReason::AuthFailed {
        return None;
    }
    let lower = err.to_lowercase();
    if lower.contains("oauth_org_not_allowed")
        || lower.contains("oauth_not_allowed_for_organization")
        || lower.contains("not allowed for this organization")
        || lower.contains("disabled claude subscription access")
    {
        return Some("org_disabled");
    }
    Some("invalid_token")
}

/// The rotator-facing twin of [`auth_failure_kind_hint`] (D2): the typed
/// `AuthFailureKind` a spawn failure should be booked against, or `None` when
/// the failure is not an authentication failure at all.
///
/// One classifier, two renderings — deriving both from `auth_failure_kind_hint`
/// keeps the operator's message and the account's state machine from ever
/// disagreeing about *why* an account died. Anything auth-shaped that is not
/// recognisably an org rejection is treated as an invalid token: that is the
/// safe default (it points the operator at re-issuing their own credential,
/// an action they can always take).
pub(crate) fn auth_failure_kind_for(
    err: &str,
) -> Option<duduclaw_agent::account_rotator::AuthFailureKind> {
    use duduclaw_agent::account_rotator::AuthFailureKind;
    match auth_failure_kind_hint(err) {
        Some("org_disabled") => Some(AuthFailureKind::OrgDisabled),
        Some(_) => Some(AuthFailureKind::InvalidToken),
        None => None,
    }
}

/// Summarized-failure retry hint (context decontamination, arXiv:2605.08563).
///
/// Returns a one-line deterministic hint for *model-behavior* failures where
/// re-sending the identical prompt tends to reproduce the identical failure.
/// Infra failures (rate limit / billing / auth / spawn / missing binary)
/// return `None`: the model did nothing wrong, so the retry prompt must stay
/// byte-identical to preserve the prompt cache. Zero LLM cost — the summary
/// is synthesized from the failure class, never from raw stderr (which could
/// carry prompt-injection payloads).
pub(crate) fn retry_hint_for(err: &str) -> Option<String> {
    match classify_cli_failure(err) {
        FailureReason::Timeout => Some(
            "A previous attempt at this exact request timed out before completing. \
             Do not repeat the same approach: answer more directly, keep tool use \
             to a minimum, and prefer a shorter response."
                .to_string(),
        ),
        FailureReason::EmptyResponse => Some(
            "A previous attempt at this exact request ended without producing any \
             text. Reply with a direct textual answer."
                .to_string(),
        ),
        _ => None,
    }
}

/// Which dashboard page a classified failure should point the user at
/// (Stripe error-object pattern: every failure carries "where to go look").
///
/// Two groups: failures where the fix is an account/quota action already
/// surfaced on the billing page (rate limit, billing exhaustion, no/cooling
/// accounts) land on [`DeepLinkKind::Billing`]; everything else — a CLI-side
/// problem the user can't self-serve from an account page — lands on
/// [`DeepLinkKind::System`], the same debug-log destination the message text
/// already tells people to check by hand.
pub(super) fn failure_console_link_kind(reason: FailureReason) -> crate::deep_link::DeepLinkKind {
    use crate::deep_link::DeepLinkKind;
    match reason {
        FailureReason::RateLimited
        | FailureReason::Billing
        | FailureReason::NoAccounts
        | FailureReason::AccountsCoolingDownLong
        | FailureReason::AccountsCoolingDownShort
        | FailureReason::AccountsCoolingDownUnknown => DeepLinkKind::Billing,
        FailureReason::BinaryMissing
        | FailureReason::AuthFailed
        | FailureReason::Timeout
        | FailureReason::SpawnError
        | FailureReason::EmptyResponse
        | FailureReason::Unknown => DeepLinkKind::System,
    }
}

/// Resolve the dashboard "console" deep link for a classified failure, or
/// `None` when no dashboard base URL is resolvable (`deep_link`'s fail-quiet
/// contract — see `deep_link.rs` module docs). `id` is irrelevant to both
/// `DeepLinkKind::Billing` and `DeepLinkKind::System` (neither is a per-object
/// route today), so an empty string is passed, matching the existing
/// `Channels`/`Billing` call sites in `channel_alerts.rs`/`budget.rs`.
pub(super) fn failure_console_url(home_dir: &Path, reason: FailureReason) -> Option<String> {
    crate::deep_link::deep_link(home_dir, failure_console_link_kind(reason), "")
}

/// Public documentation URL for a classified failure, or `None` when no
/// matching page exists in `docs/` — never a guessed/invented URL (project
/// convention). Only the account-rotation family of failures has a real
/// on-topic doc today (`docs/features/zh-TW/07-account-rotation.md` covers
/// OAuth/API-key rotation, health tracking and cooldown horizons — exactly
/// what `RateLimited`/`Billing`/`NoAccounts`/`AccountsCoolingDown*` are
/// about); the CLI-side failures (`BinaryMissing`/`AuthFailed`/`Timeout`/
/// `SpawnError`/`EmptyResponse`/`Unknown`) have no dedicated troubleshooting
/// doc in the public tree, so this deliberately returns `None` for them
/// rather than pointing at a loosely-related guide.
pub(super) fn failure_doc_url(reason: FailureReason) -> Option<&'static str> {
    match reason {
        FailureReason::RateLimited
        | FailureReason::Billing
        | FailureReason::NoAccounts
        | FailureReason::AccountsCoolingDownLong
        | FailureReason::AccountsCoolingDownShort
        | FailureReason::AccountsCoolingDownUnknown => Some(
            "https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/features/zh-TW/07-account-rotation.md",
        ),
        FailureReason::BinaryMissing
        | FailureReason::AuthFailed
        | FailureReason::Timeout
        | FailureReason::SpawnError
        | FailureReason::EmptyResponse
        | FailureReason::Unknown => None,
    }
}

/// Build a zh-TW user-facing message for a classified failure.
///
/// Messages directly tell the user *why* CLI failed (rate limit, billing, etc.)
/// and whether a local model fallback was used. When a dashboard deep link is
/// resolvable (`[dashboard] public_url` or `[gateway] port` in
/// `config.toml` — see `deep_link.rs`), a single "🔎 詳情：<url>" line is
/// appended so the failure carries a concrete "go look here" destination
/// (Stripe error-object pattern), not just a category label. Fail-quiet: no
/// resolvable base URL means no link line, never a dangling/placeholder one.
/// Only the console link is surfaced here — the doc link is dashboard-side
/// (`channel_failures.jsonl`'s `doc_url` field), keeping the channel message
/// to at most one URL.
pub(crate) fn format_fallback_message(
    agent_name: &str,
    reason: FailureReason,
    home_dir: &Path,
) -> String {
    let body = match reason {
        FailureReason::BinaryMissing => format!(
            "{agent_name} 暫時無法回應：系統找不到 Claude Code CLI。\n\
             請確認已安裝，並執行：\n\
             $ claude auth status"
        ),
        FailureReason::AuthFailed => format!(
            "{agent_name} 無法回應：Claude Code 未登入或認證失效。\n\
             請在終端執行：\n\
             $ claude /login\n\
             登入完成後，可繼續對我說話。"
        ),
        FailureReason::RateLimited => format!(
            "{agent_name} 暫時忙線中（API 使用量已達上限），請稍後再試。\n\
             系統會在背景自動偵測恢復，屆時將自動切回 Claude。\n\
             若持續發生，可在儀表板加入備用 OAuth 帳號以啟用自動輪替。"
        ),
        FailureReason::Billing => format!(
            "{agent_name} 無法回應：目前帳號額度已用完。\n\
             請於 Anthropic Console 儲值，或在儀表板切換到其他有效帳號。"
        ),
        FailureReason::Timeout => format!(
            "{agent_name} 這次處理超時（已達 30 分鐘安全上限）。\n\
             請重新送出訊息，或將任務拆成較小的步驟。"
        ),
        FailureReason::SpawnError => format!(
            "{agent_name} 啟動 Claude Code 子程序失敗。\n\
             請查看 ~/.duduclaw/debug.log 取得詳細錯誤。"
        ),
        FailureReason::EmptyResponse => format!(
            "{agent_name} 這次沒有回覆內容（空回應）。\n\
             請重送訊息；若持續發生請回報。"
        ),
        FailureReason::NoAccounts => format!(
            "{agent_name} 目前沒有可用的 Claude 帳號。\n\
             請到儀表板加入 OAuth 或 API Key。"
        ),
        // WP10 M4 — the recovery horizon differs by an order of magnitude
        // between billing exhaustion and a rate-limit cooldown, so the message
        // says which one the user is actually waiting on.
        FailureReason::AccountsCoolingDownLong => format!(
            "{agent_name} 目前無法回應：帳號額度已用盡，正在冷卻中。\n\
             最長可能需要 24 小時才會自動恢復。\n\
             若不想等，可於 Anthropic Console 儲值，或在儀表板加入其他帳號。"
        ),
        FailureReason::AccountsCoolingDownShort => format!(
            "{agent_name} 目前忙線中，帳號正在短暫冷卻。\n\
             通常幾分鐘內會自動恢復，請稍後再送一次。"
        ),
        FailureReason::AccountsCoolingDownUnknown => format!(
            "{agent_name} 目前沒有可用的帳號，系統正在等待恢復。\n\
             若是短暫忙線，幾分鐘內會自動恢復；若是額度用盡，最長可能需要 24 小時。\n\
             可到儀表板查看帳號狀態，或加入其他帳號以立即恢復服務。"
        ),
        FailureReason::Unknown => format!(
            "{agent_name} 暫時無法回應。\n\
             請稍後再試，或查看 ~/.duduclaw/debug.log 取得詳細原因。"
        ),
    };
    match failure_console_url(home_dir, reason) {
        Some(url) => format!("{body}\n🔎 詳情：{url}"),
        None => body,
    }
}

/// Pure routing decision: does `[general] inference_mode` (config.toml)
/// prefer local inference FIRST on the channel-reply path?
///
/// Exact token equality — never a substring check (2026-06 conventions) —
/// and case-sensitive to match the dispatcher's `match mode.as_str()` arms,
/// so both paths agree on what "local" means. Anything else ("hybrid",
/// "claude", absent/empty, typos) keeps the CLI-first behavior unchanged.
pub(super) fn local_inference_first(inference_mode: &str) -> bool {
    inference_mode == "local"
}

/// Translate a raw CLI error into a short zh-TW hint for the user.
pub(super) fn classify_cli_error_hint(err: &str) -> &'static str {
    let reason = classify_cli_failure(err);
    match reason {
        FailureReason::RateLimited => "使用量已達上限",
        FailureReason::Billing => "帳號額度用完",
        FailureReason::AuthFailed => "認證失效",
        FailureReason::Timeout => "處理超時",
        FailureReason::EmptyResponse => "空回應",
        FailureReason::BinaryMissing => "CLI 未安裝",
        FailureReason::NoAccounts => "無可用帳號",
        FailureReason::AccountsCoolingDownLong => "額度用盡冷卻中",
        FailureReason::AccountsCoolingDownShort => "短暫冷卻中",
        FailureReason::AccountsCoolingDownUnknown => "帳號冷卻中",
        FailureReason::SpawnError => "程序啟動失敗",
        _ => "連線異常",
    }
}

