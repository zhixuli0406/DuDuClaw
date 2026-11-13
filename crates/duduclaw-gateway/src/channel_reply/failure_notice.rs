//! Display-only terminal outcomes. No diagnostic text may enter this API.
use super::{FailureReason, ReplyContext};

tokio::task_local! {
    pub(crate) static FAILURE_TURN: String;
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
