//! Slack decision identity status (review L7).
//!
//! Slack decisions and computer-use confirmations are bound to the bot's own
//! verified account: `auth.test` (team, bot user, bot id) plus `bots.info`
//! (the app id). `bots.info` needs the `users:read` scope. Without it the
//! account cannot be established, so every Slack decision and every high-risk
//! confirmation fails closed. That is correct, but used to be invisible: the
//! person deciding saw an English "incomplete decision identity".
//!
//! This module makes it visible in three places:
//! - a status file `<home>/state/slack_decision_identity.json` (one row per
//!   bot label, removed again when the account verifies), read by
//!   `duduclaw doctor`;
//! - one Activity Feed row the first time a bot label fails in this process;
//! - a Chinese reply when someone sends a decision command to that bot.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const STATUS_FILE: &str = "slack_decision_identity.json";

/// Reply to a decision command when this Slack bot has no verified account.
/// Independent of the request id, so it reveals nothing about requests.
pub(crate) const SLACK_IDENTITY_UNAVAILABLE: &str = "這個 Slack 機器人目前無法確認自己的帳號身分（常見原因：bot token 缺少 users:read 權限），所以不能在 Slack 上決定請求。請到儀表板決定，或請管理員補上權限後重新連線。";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SlackIdentityFailure {
    /// `auth.test` or `bots.info`.
    pub step: String,
    /// Slack's `error` field (`missing_scope`, `invalid_auth`, …) or a short
    /// transport description. Never contains the token.
    pub error: String,
    /// Slack's `needed` field when it names the missing scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needed: Option<String>,
    pub at: String,
}

fn status_path(home: &Path) -> PathBuf {
    home.join("state").join(STATUS_FILE)
}

fn read_status(path: &Path) -> BTreeMap<String, SlackIdentityFailure> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_status(
    path: &Path,
    update: impl FnOnce(&mut BTreeMap<String, SlackIdentityFailure>) -> bool,
) {
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let result = duduclaw_core::with_file_lock(path, || {
        let mut rows = read_status(path);
        if !update(&mut rows) {
            return Ok(());
        }
        if rows.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let json = serde_json::to_string_pretty(&rows)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    });
    if let Err(e) = result {
        tracing::debug!(error = %e, "slack decision identity status not written (non-fatal)");
    }
}

fn first_failure_this_process(home: &Path, label: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = format!("{}\u{0}{label}", home.display());
    SEEN.get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(key))
        .unwrap_or(false)
}

/// Record that this bot's decision account could not be verified. Writes the
/// status row every time (the latest reason wins) and an Activity Feed row the
/// first time per bot label and process.
pub(crate) async fn note_slack_identity_failure(
    ctx: &crate::channel_reply::ReplyContext,
    label: &str,
    failure: SlackIdentityFailure,
) {
    tracing::warn!(
        label,
        step = %failure.step,
        error = %failure.error,
        needed = ?failure.needed,
        "Slack decision account could not be verified; Slack decisions and computer-use confirmations are refused for this bot"
    );
    let summary = format!(
        "Slack 機器人「{label}」無法確認自己的帳號身分（{} 失敗：{}{}）。在補上之前，這個機器人上的核准回覆與電腦操作確認都會被拒絕；請在 Slack App 設定補上 users:read 權限後重新連線，或改到儀表板決定。",
        failure.step,
        failure.error,
        failure
            .needed
            .as_deref()
            .map(|n| format!("，需要 {n}"))
            .unwrap_or_default()
    );
    let path = status_path(&ctx.home_dir);
    let owned_label = label.to_owned();
    write_status(&path, move |rows| {
        rows.insert(owned_label, failure);
        true
    });
    if first_failure_this_process(&ctx.home_dir, label) {
        crate::channel_reply::post_conversation_activity(
            &ctx.home_dir,
            &ctx.event_tx,
            "",
            "slack_decision_identity_unavailable",
            summary,
        )
        .await;
    }
}

/// The account verified: drop this bot's failure row, if any.
pub(crate) fn note_slack_identity_ok(home: &Path, label: &str) {
    let path = status_path(home);
    if !path.exists() {
        return;
    }
    write_status(&path, |rows| rows.remove(label).is_some());
}

/// `duduclaw doctor` row: `(warn, message)`. Reads only the status file the
/// gateway writes; it never calls Slack. When the file is absent the row says
/// what the requirement is, because a gateway that has not connected yet has
/// not checked anything.
pub fn slack_identity_doctor(home: &Path) -> (bool, String) {
    let path = status_path(home);
    let rows = read_status(&path);
    if rows.is_empty() {
        return (
            false,
            "沒有紀錄到 Slack 機器人帳號驗證失敗（在 Slack 上核准或回答請求，需要 bot token 有 users:read 權限；gateway 連線時檢查）".into(),
        );
    }
    let detail = rows
        .iter()
        .map(|(label, f)| {
            format!(
                "{label}：{} 失敗（{}{}，{}）",
                f.step,
                f.error,
                f.needed
                    .as_deref()
                    .map(|n| format!("，需要 {n}"))
                    .unwrap_or_default(),
                f.at
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    (
        true,
        format!(
            "{detail}。這些機器人上的核准回覆與電腦操作確認會被拒絕；請在 Slack App 設定補上 users:read 權限並重新安裝到工作區，gateway 重新連線成功後這一列會消失"
        ),
    )
}
