//! Display-only terminal outcomes. No diagnostic text may enter this API.
use super::{FailureReason, ReplyContext};

tokio::task_local! {
    pub(crate) static FAILURE_TURN: String;
    pub(crate) static FAILURE_DETAILS: (&'static str, Option<crate::work_outcome::WorkOutcome>);
    pub(crate) static FAILURE_SESSION: std::sync::Arc<std::sync::Mutex<Option<(String, String, String)>>>;
}

pub(crate) fn bind_failure_session(sid: &str, generation: &str, agent: &str) {
    let _ = FAILURE_SESSION.try_with(|slot| {
        *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((sid.into(), generation.into(), agent.into()));
    });
}

pub(crate) fn failure_session() -> Option<(String, String, String)> {
    FAILURE_SESSION.try_with(|slot| slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()).ok().flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnFailureReason {
    Timeout,
    MaxTurns,
    TimeoutTotal,
    TimeoutNoProgress,
    Unavailable,
    Quota,
    Cancelled,
    SafetyGate,
    Internal,
    Unknown,
}

impl From<FailureReason> for TurnFailureReason {
    fn from(reason: FailureReason) -> Self {
        match reason {
            FailureReason::Timeout => Self::Timeout,
            FailureReason::MaxTurns => Self::MaxTurns,
            FailureReason::TimeoutTotal => Self::TimeoutTotal,
            FailureReason::TimeoutNoProgress => Self::TimeoutNoProgress,
            FailureReason::BinaryMissing | FailureReason::AuthFailed | FailureReason::NoAccounts
            | FailureReason::AccountsCoolingDownUnknown => Self::Unavailable,
            FailureReason::RateLimited | FailureReason::Billing
            | FailureReason::AccountsCoolingDownLong | FailureReason::AccountsCoolingDownShort => Self::Quota,
            FailureReason::SpawnError | FailureReason::EmptyResponse => Self::Internal,
            FailureReason::Unknown => Self::Unknown,
        }
    }
}

impl TurnFailureReason {
    pub(crate) fn text(self, language: Option<&str>) -> &'static str {
        let language = language.unwrap_or("zh-TW");
        let language = if matches!(language, "en" | "en_US") || language.starts_with("en-") { "en" }
            else if matches!(language, "ja" | "ja-JP" | "ja_JP") { "ja-JP" }
            else if matches!(language, "zh-CN" | "zh_CN") { "zh-CN" }
            else { "zh-TW" };
        match (language, self) {
            ("en", Self::MaxTurns) => "The step budget for this turn is exhausted. Completed work has been kept; reply “continue” to carry on.",
            ("en", Self::TimeoutTotal) => "No final answer was produced before the time limit. Completed work has been kept; reply “continue” to carry on.",
            ("en", Self::TimeoutNoProgress) => "This reply stopped because no progress was received. Completed work has been kept; reply “continue” to carry on.",
            ("ja-JP", Self::MaxTurns) => "このターンのステップ数を使い切りました。完了した作業は保持されています。「続けて」と返信すると再開できます。",
            ("ja-JP", Self::TimeoutTotal) => "制限時間内に最終回答が生成されませんでした。完了した作業は保持されています。「続けて」と返信すると再開できます。",
            ("ja-JP", Self::TimeoutNoProgress) => "進展がないため応答を停止しました。完了した作業は保持されています。「続けて」と返信すると再開できます。",
            ("zh-CN", Self::MaxTurns) => "这一轮的步数用完了。已完成的部分已保留，回复「继续」可以接着做。",
            ("zh-CN", Self::TimeoutTotal) => "在时间上限内未能生成最终答复。已完成的部分已保留，回复「继续」可以接着做。",
            ("zh-CN", Self::TimeoutNoProgress) => "这次回复长时间没有进展，已先停止。已完成的部分已保留，回复「继续」可以接着做。",
            (_, Self::MaxTurns) => "這一輪的步數用完了。已完成的部分已保留，回覆『繼續』可以接著做。",
            (_, Self::TimeoutTotal) => "時限內未產生最終答案。已完成的部分已保留，回覆『繼續』可以接著做。",
            (_, Self::TimeoutNoProgress) => "這次回覆長時間沒有進展，已先停止。已完成的部分已保留，回覆『繼續』可以接著做。",

            ("en", Self::Timeout) => "This turn received no reply because the response timed out.",
            ("en", Self::Unavailable) => "This turn received no reply because the model or account is unavailable.",
            ("en", Self::Quota) => "This turn received no reply because the usage limit or available credit was exhausted.",
            ("en", Self::Cancelled) => "This turn received no reply because the user cancelled it.",
            ("en", Self::SafetyGate) => "This turn received no reply because a safety check stopped it.",
            ("en", Self::Internal) => "This turn received no reply because an internal error occurred.",
            ("en", Self::Unknown) => "This turn received no reply. Please try again later.",
            ("ja-JP", Self::Timeout) => "応答がタイムアウトしたため、このメッセージへの返信はありません。",
            ("ja-JP", Self::Unavailable) => "モデルまたはアカウントを利用できないため、このメッセージへの返信はありません。",
            ("ja-JP", Self::Quota) => "利用上限または残高が不足したため、このメッセージへの返信はありません。",
            ("ja-JP", Self::Cancelled) => "キャンセルされたため、このメッセージへの返信はありません。",
            ("ja-JP", Self::SafetyGate) => "安全確認で停止されたため、このメッセージへの返信はありません。",
            ("ja-JP", Self::Internal) => "内部エラーが発生したため、このメッセージへの返信はありません。",
            ("ja-JP", Self::Unknown) => "このメッセージへの返信はありません。しばらくしてからもう一度お試しください。",
            ("zh-CN", Self::Timeout) => "本次未收到回复，因为响应超时。",
            ("zh-CN", Self::Unavailable) => "本次未收到回复，因为模型或账号目前无法使用。",
            ("zh-CN", Self::Quota) => "本次未收到回复，因为使用量或可用额度已达上限。",
            ("zh-CN", Self::Cancelled) => "本次未收到回复，因为用户已取消这次请求。",
            ("zh-CN", Self::SafetyGate) => "本次未收到回复，因为安全检查已停止这次请求。",
            ("zh-CN", Self::Internal) => "本次未收到回复，因为发生内部错误。",
            ("zh-CN", Self::Unknown) => "本次未收到回复，请稍后重试。",
            (_, Self::Timeout) => "本次未收到回覆，因為回應逾時。",
            (_, Self::Unavailable) => "本次未收到回覆，因為模型或帳號目前無法使用。",
            (_, Self::Quota) => "本次未收到回覆，因為使用量或可用額度已達上限。",
            (_, Self::Cancelled) => "本次未收到回覆，因為使用者已取消這次請求。",
            (_, Self::SafetyGate) => "本次未收到回覆，因為安全檢查已停止這次請求。",
            (_, Self::Internal) => "本次未收到回覆，因為發生內部錯誤。",
            (_, Self::Unknown) => "本次未收到回覆，請稍後再試。",
        }
    }
}

/// Write only after this turn's user row was successfully stored. Pre-persist
/// exits leave no notice: it would describe the previous, already answered turn.
/// Failure handling never creates sessions or changes live reply signals.
pub(super) async fn persist_failure_notice(
    ctx: &ReplyContext, sid: &str, agent: &str, turn: &str,
    reason: TurnFailureReason, generation: Option<&str>,
) {
    // Bind only after the user row was successfully stored. No unbound writes,
    // including missing generations and failures before an employee is routed.
    let Some((stored_sid, stored_generation, stored_agent)) = failure_session() else { return; };
    if stored_sid != sid || stored_agent != agent || generation != Some(stored_generation.as_str()) { return; }
    if ctx.session_manager.session_agent(sid).await.ok().flatten().as_deref() != Some(agent) { return; }
    // Latch before the best-effort notice write: even if it fails, WebChat
    // must never substitute the live diagnostic as an ordinary model reply.
    let _ = super::ccr_delivery::CCR_TURN_DELIVERY.try_with(|delivery| {
        delivery.failure_notice_only.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let language = crate::prompt_identity::resolve_fixed_message_language_for_id(&ctx.home_dir, agent).await;
    if let Err(error) = ctx.session_manager.record_turn_failure(sid, turn, reason, language.as_deref(), generation).await {
        tracing::warn!(%error, "Turn failure notice persistence failed");
    }
}

/// Alignment errors keep their existing transport reply; only a post-persist
/// error can add a display notice.
pub(super) async fn model_alignment_gate(
    ctx: &ReplyContext, sid: &str, agent: &str, turn: &str,
    model: &str, provider: duduclaw_core::types::RuntimeType, generation: Option<&str>,
) -> Result<(), String> {
    let result = crate::runtime_config::ensure_model_provider(model, provider);
    if result.is_err() {
        if generation.is_some() {
            persist_failure_notice(ctx, sid, agent, turn, TurnFailureReason::Unavailable, generation).await;
        }
    }
    result
}

pub(super) async fn active_provider_gate(
    ctx: &ReplyContext, sid: &str, agent: &str, turn: &str,
    dir: &std::path::Path, generation: Option<&str>,
) -> Result<(), String> {
    let result = crate::runtime_config::ensure_active_agent_provider(dir);
    if result.is_err() {
        persist_failure_notice(ctx, sid, agent, turn, TurnFailureReason::Unavailable, generation).await;
    }
    result
}

#[cfg(test)]
#[path = "failure_notice_tests.rs"]
mod tests;

/// Shared by every channel for post-contract empty output and unwinding adapters.
pub(super) async fn persist_bound_failure(ctx: &ReplyContext, reason: TurnFailureReason) {
    let Some((sid, generation, agent)) = failure_session() else { return; };
    let Ok(turn) = FAILURE_TURN.try_with(Clone::clone) else { return; };
    persist_failure_notice(ctx, &sid, &agent, &turn, reason, Some(&generation)).await;
}

pub(super) async fn observe_turn<F: std::future::Future>(ctx: &ReplyContext, work: F) -> F::Output {
    use futures_util::FutureExt;
    let turn = crate::webchat::current_turn_id().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let run = FAILURE_TURN.scope(turn, async {
        match std::panic::AssertUnwindSafe(work).catch_unwind().await {
            Ok(output) => output,
            Err(panic) => {
                persist_bound_failure(ctx, TurnFailureReason::Internal).await;
                std::panic::resume_unwind(panic)
            }
        }
    });
    if FAILURE_SESSION.try_with(|_| ()).is_ok() { run.await }
    else { FAILURE_SESSION.scope(std::sync::Arc::new(std::sync::Mutex::new(None)), run).await }
}

pub(crate) fn notice_suffix(language: Option<&str>, code: Option<&str>, reference: &str) -> String {
    let id = duduclaw_core::truncate_chars(reference, 8);
    let (reason, reference) = match language.unwrap_or("zh-TW") {
        "en" | "en-US" | "en_US" => ("Reason code", "Reference"),
        "ja" | "ja-JP" | "ja_JP" => ("理由コード", "参照番号"),
        "zh-CN" | "zh_CN" => ("原因代码", "参考编号"),
        _ => ("原因代碼", "參考編號"),
    };
    match code { Some(code) => format!("（{reason}：{code}｜{reference}：{id}）"), None => format!("（{reference}：{id}）") }
}
impl TurnFailureReason {
    pub(crate) fn code(self) -> &'static str { match self {
        Self::Timeout | Self::TimeoutTotal => "timeout_total", Self::MaxTurns => "max_turns",
        Self::TimeoutNoProgress => "timeout_no_progress", Self::Unavailable => "unavailable",
        Self::Quota => "quota", Self::Cancelled => "cancelled", Self::SafetyGate => "safety_gate",
        Self::Internal => "internal", Self::Unknown => "unknown",
    } }
    pub(crate) fn render(self, language: Option<&str>, reference: &str, visitor: bool) -> String {
        let reason = if visitor { Self::Unknown } else { self };
        let details = FAILURE_DETAILS.try_with(Clone::clone).ok();
        let text = if visitor { reason.text(language).into() } else { details.as_ref().and_then(|(_, outcome)| outcome.as_ref()).map(|o| work_notice(o, language)).unwrap_or_else(|| reason.text(language).into()) };
        let code = details.as_ref().map(|(code, _)| *code).unwrap_or(self.code());
        format!("{text}\n{}", notice_suffix(language, (!visitor).then_some(code), reference))
    }
}
pub(crate) fn work_notice(outcome: &crate::work_outcome::WorkOutcome, language: Option<&str>) -> String {
    use crate::work_outcome::WorkOutcome;
    let lang = match language.unwrap_or("zh-TW") { "en-US" | "en_US" => "en", "ja_JP" => "ja-JP", "zh_CN" => "zh-CN", other => other };
    match outcome {
        WorkOutcome::MaxTurns { limit, .. } => match lang {
            "en" => format!("The step budget for this turn is exhausted (limit: {limit} steps). Completed work has been kept; reply “continue” to carry on."),
            "ja" | "ja-JP" => format!("このターンのステップ数を使い切りました（上限 {limit} ステップ）。完了した作業は保持されています。「続けて」と返信すると再開できます。"),
            "zh-CN" | "zh_CN" => format!("这一轮的步数用完了（上限 {limit} 步）。已完成的部分已保留，回复「继续」可以接着做。"),
            _ => format!("這一輪的步數用完了（上限 {limit} 步）。已完成的部分已保留，回覆『繼續』可以接著做。"),
        },
        WorkOutcome::TimeoutTotal { seconds, .. } => {
            if *seconds == 0 { return TurnFailureReason::TimeoutTotal.text(language).into(); }
            let duration = timeout_duration(*seconds, lang);
            match lang {
                "en" => format!("No final answer was produced within the {duration} time limit. Completed work has been kept; reply “continue” to carry on."),
                "ja" | "ja-JP" => format!("{duration} の制限時間内に最終回答が生成されませんでした。完了した作業は保持されています。「続けて」と返信すると再開できます。"),
                "zh-CN" | "zh_CN" => format!("已达 {duration} 的时间上限，尚未生成最终答复。已完成的部分已保留，回复「继续」可以接着做。"),
                _ => format!("已達 {duration} 的時間上限，尚未產生最終答案。已完成的部分已保留，回覆『繼續』可以接著做。"),
            }
        }
        WorkOutcome::TimeoutNoProgress { seconds, tool, .. } => {
            let minutes = seconds.div_ceil(60);
            match lang {
                "en" => format!("This reply made no progress{} for {minutes} minutes and was stopped. Completed work has been kept; reply “continue” to carry on.", tool.as_ref().map(|s| format!(" in ‘{s}’")).unwrap_or_default()),
                "ja" | "ja-JP" => format!("この応答は{}{minutes} 分間進展がないため停止しました。完了した作業は保持されています。「続けて」と返信すると再開できます。", tool.as_ref().map(|s| format!("「{s}」で ")).unwrap_or_default()),
                "zh-CN" | "zh_CN" => format!("这次回复{} {minutes} 分钟没有进展，已先停止。已完成的部分已保留，回复「继续」可以接着做。", tool.as_ref().map(|s| format!("在「{s}」停了")).unwrap_or_default()),
                _ => format!("這次回覆{}{minutes} 分鐘沒有進展，已先停止。已完成的部分已保留，回覆『繼續』可以接著做。", tool.as_ref().map(|s| format!("在『{s}』停了 ")).unwrap_or_else(|| "已有 ".into())),
            }
        }
    }
}

fn timeout_duration(seconds: u64, lang: &str) -> String {
    let minutes = seconds >= 60 && seconds % 60 == 0;
    let value = if minutes { seconds / 60 } else { seconds };
    let unit = match (lang, minutes) {
        ("en", true) => if value == 1 { "minute" } else { "minutes" },
        ("en", false) => if value == 1 { "second" } else { "seconds" },
        ("ja" | "ja-JP", true) => "分",
        ("zh-CN" | "zh_CN", true) => "分钟",
        (_, true) => "分鐘",
        (_, false) => "秒",
    };
    format!("{value} {unit}")
}

#[cfg(test)]
mod work_notice_tests {
    use super::*;

    #[test]
    fn total_timeout_notice_names_applied_duration_or_uses_zero_fallback() {
        for (language, expected) in [
            ("zh-TW", [(600, "10 分鐘"), (95, "95 秒"), (60, "1 分鐘"), (1, "1 秒")]),
            ("en", [(600, "10 minutes"), (95, "95 seconds"), (60, "1 minute"), (1, "1 second")]),
        ] {
            for (seconds, duration) in expected {
                let outcome = crate::work_outcome::WorkOutcome::TimeoutTotal { seconds, partial: String::new() };
                let expected = if language == "en" {
                    format!("No final answer was produced within the {duration} time limit. Completed work has been kept; reply “continue” to carry on.")
                } else {
                    format!("已達 {duration} 的時間上限，尚未產生最終答案。已完成的部分已保留，回覆『繼續』可以接著做。")
                };
                assert_eq!(work_notice(&outcome, Some(language)), expected);
            }
            let outcome = crate::work_outcome::WorkOutcome::TimeoutTotal { seconds: 0, partial: String::new() };
            assert_eq!(work_notice(&outcome, Some(language)), TurnFailureReason::TimeoutTotal.text(Some(language)));
        }
    }
}

pub(crate) fn attach_failure_reference(row: &mut serde_json::Value, code: &str, reference: &str) {
    row["reason_code"] = serde_json::Value::String(code.into());
    row["reference_id"] = serde_json::Value::String(reference.into());
}

/// Store source-derived partial text under the same generation and delivery
/// guards as an ordinary assistant reply; fixed notices stay out of model history.
pub(super) async fn persist_work_partial(ctx: &ReplyContext, sid: &str, agent: &str, text: &str, generation: Option<&str>) -> bool {
    let Some((stored_sid, stored_generation, stored_agent)) = failure_session() else { return false; };
    if stored_sid != sid || stored_agent != agent || generation != Some(stored_generation.as_str())
        || super::assembly::public_session(sid) || text.trim().is_empty()
        || !super::webchat_commit_allowed(agent).await { return false; }
    match super::append_turn_assistant(&ctx.session_manager, sid, text, super::estimate_tokens(text), generation).await {
        Ok(id) => {
            super::record_turn_assistant_message(sid, id);
            let _ = super::ccr_delivery::CCR_TURN_DELIVERY.try_with(|delivery| {
                delivery.failure_notice_only.store(false, std::sync::atomic::Ordering::SeqCst);
            });
            true
        }
        Err(error) => { tracing::warn!(%error, "partial assistant turn persistence failed"); false }
    }
}
