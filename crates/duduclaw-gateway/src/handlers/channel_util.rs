//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// W0-2 `channels.test` structured result: `{sent, mode, detail}`.
/// `mode` is `"live"` (an actual send was attempted) or `"credential_only"`
/// (no destination was known, so only credential presence was verified — the
/// front end must not render this as a green success).
pub(crate) fn channel_test_result(sent: bool, mode: &str, detail: &str) -> WsFrame {
    WsFrame::ok_response("", json!({ "sent": sent, "mode": mode, "detail": detail }))
}

/// Does this agent own a distinct per-agent bot token for `platform`? Only
/// discord/telegram/slack support per-agent tokens today. An agent that owns
/// its own token is not reachable through a different (e.g. the global)
/// bot's token for the same platform.
pub(crate) fn agent_has_own_channel_token(
    agent: &duduclaw_agent::registry::LoadedAgent,
    platform: &str,
) -> bool {
    let Some(ch) = &agent.config.channels else {
        return false;
    };
    match platform {
        "discord" => ch.discord.as_ref().is_some_and(|d| {
            !d.bot_token.is_empty() || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
        }),
        "telegram" => ch.telegram.as_ref().is_some_and(|t| {
            !t.bot_token.is_empty() || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
        }),
        "slack" => ch.slack.as_ref().is_some_and(|s| {
            !s.bot_token.is_empty() || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
        }),
        _ => false,
    }
}

/// The agent's own `agent.toml [proactive] notify_chat_id` — but only when
/// `notify_channel` matches `platform`; otherwise the destination is for a
/// different channel entirely and must not be borrowed.
pub(crate) fn agent_proactive_target(
    agent: &duduclaw_agent::registry::LoadedAgent,
    platform: &str,
) -> Option<String> {
    let p = &agent.config.proactive;
    if p.notify_channel != platform {
        return None;
    }
    let chat_id = p.notify_chat_id.trim();
    if chat_id.is_empty() {
        None
    } else {
        Some(chat_id.to_string())
    }
}

/// Map a raw `ChannelSendError` message (platform HTTP status + body, or a
/// transport-level `reqwest` error) into one zh-TW sentence a non-technical
/// dashboard user can act on. Mirrors the style of `channel_reply.rs`'s
/// `FailureReason` classification: the raw string (which may include
/// technical API jargon) is logged by the caller and never forwarded to the
/// UI — only this classified sentence is.
pub(crate) fn classify_channel_send_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("no stored conversation reference")
        || lower.contains("no stored sessionwebhook")
    {
        "此對話尚未有過互動紀錄——請先讓對方在該通道傳一則訊息給機器人，之後測試才找得到可送達的對象。".to_string()
    } else if lower.contains("expired") && lower.contains("sessionwebhook") {
        "對話連線已過期（此類通道只能在對方最近一次發話後的一段時間內回覆），請請對方再傳一則訊息後重試。".to_string()
    } else if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("invalid_auth")
        || lower.contains("not_authed")
    {
        "憑證無效或已被平台撤銷，請重新產生 token 後再設定一次。".to_string()
    } else if lower.contains("403")
        || lower.contains("forbidden")
        || lower.contains("not_in_channel")
    {
        "權限不足——機器人可能尚未加入該群組/頻道，或該功能尚未在平台後台開通。".to_string()
    } else if lower.contains("404")
        || lower.contains("not found")
        || lower.contains("chat_not_found")
        || lower.contains("channel_not_found")
    {
        "找不到目的地——目的地 ID 可能有誤，或機器人尚未加入該對話。".to_string()
    } else if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
    {
        "已達平台 API 呼叫頻率上限，請稍後再試一次。".to_string()
    } else if lower.contains("ssrf") || lower.contains("domain mismatch") {
        "安全性檢查阻擋了這次發送，請確認通道設定正確。".to_string()
    } else if lower.contains("error sending request")
        || lower.contains("dns error")
        || lower.contains("connect")
        || lower.contains("timed out")
        || lower.contains("timeout")
    {
        "網路連線失敗，請確認伺服器對外連線正常後重試。".to_string()
    } else {
        "平台拒絕了這則訊息，詳細原因已記錄於伺服器日誌，請聯絡管理員查看。".to_string()
    }
}
