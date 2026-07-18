//! Channel-side push + decision for the autonomous goal loop (P2a).
//!
//! Two directions, mirroring `install_notify.rs` (free functions that open the
//! stores from `home_dir`, so they work from both the channel inbound
//! dispatchers and the goal-loop driver, neither of which shares a handler):
//!
//! - **Outbound** — when a goal task is parked `needs_human` (iteration cap /
//!   deadline / judge rejection at retry budget), [`notify_goal_needs_human`]
//!   pushes an approval message to the agent's **default channel** (its
//!   `agent.toml [proactive] notify_channel/notify_chat_id`, the same
//!   destination the GVU silence-breaker uses) with three buttons —
//!   retry / mark-done / abort. The autonomy kickoff gate
//!   ([`notify_goal_kickoff`]) pushes an approve/deny pair before the first
//!   dispatch of a Collaborator/Consultant agent's goal.
//! - **Inbound** — a button press carrying `duduclaw:goal_*` is routed by the
//!   per-channel dispatcher to [`decide_from_channel`], which applies the
//!   decision (task-store transition for needs_human, ApprovalBroker decide for
//!   kickoff) and records it on the Activity Feed.
//!
//! ## Authorization posture (honest scope note)
//!
//! Unlike install-approval — which maps the clicking account to a dashboard
//! user + role — a goal task has **no owner/source column** on `TaskRow` yet
//! (that arrives with the P5 `/goal` entry point). So the push goes to the
//! agent's own control channel and the decision is *not* bound to a specific
//! presser. The fail-closed guards that remain are: (a) the action id must
//! decode cleanly, (b) `resolve_needs_human` only transitions FROM
//! `needs_human` (a stale / double press is a no-op), and (c) the ApprovalBroker
//! refuses to change a terminal state. Everything is best-effort and fail-soft:
//! a missing token / unconfigured destination is logged, never panics.

use std::path::Path;

use serde_json::json;
use tracing::{info, warn};

use crate::channel_format;
use crate::task_store::{ActivityRow, TaskRow, TaskStore};

/// Channels that can render inline goal buttons today (same four as install).
fn channel_supports_buttons(channel: &str) -> bool {
    matches!(channel, "telegram" | "slack" | "discord" | "line")
}

/// The agent's default notification destination — `agent.toml [proactive]
/// notify_channel` + `notify_chat_id`. Returns `None` when either is unset
/// (the agent has no configured control channel; nothing to push to).
fn agent_notify_target(home_dir: &Path, agent_id: &str) -> Option<(String, String)> {
    let agent_toml = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = std::fs::read_to_string(&agent_toml).ok()?;
    let table: toml::Value = content.parse().ok()?;
    let proactive = table.get("proactive").and_then(|v| v.as_table())?;
    let channel = proactive.get("notify_channel").and_then(|v| v.as_str())?;
    let chat_id = proactive.get("notify_chat_id").and_then(|v| v.as_str())?;
    if channel.trim().is_empty() || chat_id.trim().is_empty() {
        return None;
    }
    Some((channel.to_string(), chat_id.to_string()))
}

/// Resolve the bot token for `channel`: the agent's own (walking `reports_to`)
/// first, then the global `config.toml [channels]` token — matching the
/// cron/delegation forwarding cascade.
async fn channel_token(home_dir: &Path, agent_id: &str, channel: &str) -> Option<String> {
    if let Some(tok) =
        crate::config_crypto::resolve_agent_channel_token_via_reports_to(home_dir, agent_id, channel)
    {
        if !tok.is_empty() {
            return Some(tok);
        }
    }
    let field = crate::otp_delivery::token_field(channel)?;
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field)
        .await
        .filter(|t| !t.is_empty())
}

/// P5 outer progress board: a phase transition of a goal task, pushed as a
/// short (1–3 line) zh-TW note to the conversation that launched the goal.
///
/// This is a *notification*, not an approval — it is delivered for every
/// autonomy level (Observer/Approver included). The interactive needs_human /
/// kickoff approvals (with buttons) are separate ([`notify_goal_needs_human`] /
/// [`notify_goal_kickoff`]); [`GoalProgress::NeedsHuman`] / [`GoalProgress::Kickoff`]
/// here are the plain heads-up that mirror them to the launching conversation.
#[derive(Debug, Clone)]
pub enum GoalProgress {
    /// A work message was enqueued for iteration `iter` of `cap`. `retry` marks
    /// a stall re-dispatch that carried prior feedback.
    Dispatched { iter: u32, cap: u32, retry: bool },
    /// The agent produced a result; the acceptance judge is reviewing it.
    Reviewing,
    /// Iteration `iter`/`cap` failed acceptance; the loop is retrying with the
    /// judge feedback (summarised from `task.judge_feedback`).
    Rejected { iter: u32, cap: u32 },
    /// The goal reached `done` (judge-accepted or human-marked).
    Done,
    /// The goal parked `needs_human` (a buttoned approval was pushed separately).
    NeedsHuman,
    /// The goal is waiting on a kickoff approval before its first dispatch.
    Kickoff,
}

/// Resolve the SOURCE conversation of a goal task — the `source_channel` /
/// `source_chat_id` stamped by the `/goal` entry point. `None` when the task was
/// not launched from a channel command (callers then fall back to `[proactive]`).
fn task_source_target(task: &TaskRow) -> Option<(String, String)> {
    let channel = task
        .source_channel
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let chat_id = task
        .source_chat_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some((channel.to_string(), chat_id.to_string()))
}

/// Render the zh-TW one-to-three-line progress line for a phase transition.
fn progress_body(task: &TaskRow, progress: &GoalProgress) -> String {
    let short = duduclaw_core::truncate_chars(&task.id, 8);
    let title = duduclaw_core::truncate_chars(&task.title, 60);
    match progress {
        GoalProgress::Dispatched { iter, cap, retry } => {
            let verb = if *retry { "重試" } else { "開始執行" };
            format!("🐾 目標 #{short} {verb}（第 {iter}/{cap} 輪）：{title}")
        }
        GoalProgress::Reviewing => {
            format!("🔍 目標 #{short} 已產出結果，驗收中…")
        }
        GoalProgress::Rejected { iter, cap } => {
            let fb = task
                .judge_feedback
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("(未提供原因)");
            format!(
                "↩️ 目標 #{short} 第 {iter}/{cap} 輪未通過，修正後重試。\n原因：{}",
                duduclaw_core::truncate_chars(fb, 200)
            )
        }
        GoalProgress::Done => {
            let sum = task
                .result_summary
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("(無結果摘要)");
            format!(
                "✅ 目標 #{short} 已完成。\n{}",
                duduclaw_core::truncate_chars(sum, 300)
            )
        }
        GoalProgress::NeedsHuman => {
            format!("🧭 目標 #{short} 卡住了，需要你的決定（已另外推送審批按鈕）。")
        }
        GoalProgress::Kickoff => {
            format!("⏳ 目標 #{short} 需先核准才會開始自主執行：{title}")
        }
    }
}

/// Push one goal-loop progress line to the task's SOURCE conversation
/// (`source_channel`/`source_chat_id`), falling back to the agent's
/// `[proactive]` destination; when neither exists the push is silent (the driver
/// still records the transition on the Activity Feed). Best-effort — a missing
/// token / send failure is logged, never panics. Returns whether a message was
/// delivered.
pub async fn notify_goal_progress(home_dir: &Path, task: &TaskRow, progress: GoalProgress) -> bool {
    let Some((channel, chat_id)) =
        task_source_target(task).or_else(|| agent_notify_target(home_dir, &task.assigned_to))
    else {
        // No source and no [proactive] destination — Activity-only, silent.
        return false;
    };
    let Some(token) = channel_token(home_dir, &task.assigned_to, &channel).await else {
        info!(task = %task.id, %channel, "goal-progress: no bot token; skipping push");
        return false;
    };
    let text = progress_body(task, &progress);
    let http = reqwest::Client::new();
    send_plain_text(&http, &channel, &token, &chat_id, &text).await
}

/// Render the zh-TW needs_human approval body for a goal task.
fn needs_human_body(task: &TaskRow) -> String {
    let reason = task
        .judge_feedback
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(未提供原因)");
    format!(
        "🧭 自主目標任務卡住，需要您的決定\n\
         任務：{title}\n\
         目標：{goal}\n\
         卡住原因：{reason}\n\
         編號：{id}\n\n\
         請選擇：重試 / 標記完成 / 放棄。",
        title = task.title,
        goal = duduclaw_core::truncate_chars(&task.description, 200),
        reason = duduclaw_core::truncate_chars(reason, 300),
        id = task.id,
    )
}

/// Push the needs_human approval (with buttons where supported, else plain text
/// with a dashboard hint) to the agent's default channel. Best-effort.
///
/// Returns `true` when a message was delivered (so the driver can mark it
/// notified), `false` when there was nothing to push to / the send failed.
pub async fn notify_goal_needs_human(home_dir: &Path, task: &TaskRow) -> bool {
    let Some((channel, chat_id)) = agent_notify_target(home_dir, &task.assigned_to) else {
        info!(task = %task.id, agent = %task.assigned_to,
              "goal-notify: agent has no [proactive] notify destination; skipping push");
        return false;
    };
    let Some(token) = channel_token(home_dir, &task.assigned_to, &channel).await else {
        info!(task = %task.id, %channel, "goal-notify: no bot token; skipping push");
        return false;
    };
    let http = reqwest::Client::new();
    let body = needs_human_body(task);
    if channel_supports_buttons(&channel) {
        let markup = goal_button_markup(&channel, &task.id);
        match send_with_markup(&http, &channel, &token, &chat_id, &body, markup).await {
            Ok(()) => true,
            Err(e) => {
                warn!(task = %task.id, %channel, error = %e, "goal-notify: button push failed");
                false
            }
        }
    } else {
        let text = format!("{body}\n\n請至儀表板任務看板處理此「需人工」任務。");
        send_plain_text(&http, &channel, &token, &chat_id, &text).await
    }
}

/// Push a text-only needs_human notice (no buttons) — used for `Observer`
/// autonomy, where the loop does not wait for a human. Best-effort.
pub async fn notify_goal_observer(home_dir: &Path, task: &TaskRow, resolution: &str) -> bool {
    let Some((channel, chat_id)) = agent_notify_target(home_dir, &task.assigned_to) else {
        return false;
    };
    let Some(token) = channel_token(home_dir, &task.assigned_to, &channel).await else {
        return false;
    };
    let reason = task
        .judge_feedback
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(未提供原因)");
    let text = format!(
        "🤖 自主目標任務結束（Observer 全自動模式，不等待人工）\n\
         任務：{title}\n\
         結果：{resolution}\n\
         原因：{reason}\n\
         編號：{id}",
        title = task.title,
        reason = duduclaw_core::truncate_chars(reason, 300),
        id = task.id,
    );
    let http = reqwest::Client::new();
    send_plain_text(&http, &channel, &token, &chat_id, &text).await
}

/// Push a kickoff approve/deny gate to the agent's default channel. `summary`
/// is the human-readable "goal + iteration cap" line. Best-effort; returns
/// whether a message was delivered.
pub async fn notify_goal_kickoff(
    home_dir: &Path,
    agent_id: &str,
    approval_id: &str,
    summary: &str,
) -> bool {
    let Some((channel, chat_id)) = agent_notify_target(home_dir, agent_id) else {
        info!(agent = %agent_id, "goal-notify: no notify destination for kickoff; skipping");
        return false;
    };
    let Some(token) = channel_token(home_dir, agent_id, &channel).await else {
        return false;
    };
    let body = format!(
        "🚀 自主目標啟動前需要您的核准\n{summary}\n\n請選擇：開始 / 拒絕。"
    );
    let http = reqwest::Client::new();
    if channel_supports_buttons(&channel) {
        let markup = kickoff_button_markup(&channel, approval_id);
        match send_with_markup(&http, &channel, &token, &chat_id, &body, markup).await {
            Ok(()) => true,
            Err(e) => {
                warn!(agent = %agent_id, %channel, error = %e, "goal-notify: kickoff push failed");
                false
            }
        }
    } else {
        let text = format!("{body}\n\n（此通道無按鈕，請至儀表板核准。）");
        send_plain_text(&http, &channel, &token, &chat_id, &text).await
    }
}

/// Per-channel button markup for a needs_human goal task.
fn goal_button_markup(channel: &str, task_id: &str) -> serde_json::Value {
    match channel {
        "telegram" => channel_format::telegram_goal_buttons(task_id),
        "discord" => channel_format::discord_goal_buttons(task_id),
        "slack" => channel_format::slack_goal_buttons(task_id),
        "line" => channel_format::line_goal_quick_reply(task_id),
        _ => json!({}),
    }
}

/// Per-channel button markup for a kickoff approval.
fn kickoff_button_markup(channel: &str, approval_id: &str) -> serde_json::Value {
    match channel {
        "telegram" => channel_format::telegram_goal_kickoff_buttons(approval_id),
        "discord" => channel_format::discord_goal_kickoff_buttons(approval_id),
        "slack" => channel_format::slack_goal_kickoff_buttons(approval_id),
        "line" => channel_format::line_goal_kickoff_quick_reply(approval_id),
        _ => json!({}),
    }
}

/// Send a message carrying inline buttons on one of the four button-capable
/// channels. `markup` is the platform-native structure from
/// [`goal_button_markup`] / [`kickoff_button_markup`].
async fn send_with_markup(
    http: &reqwest::Client,
    channel: &str,
    token: &str,
    chat_id: &str,
    text: &str,
    markup: serde_json::Value,
) -> Result<(), String> {
    match channel {
        "telegram" => {
            let url = format!("https://api.telegram.org/bot{token}/sendMessage");
            let body = json!({ "chat_id": chat_id, "text": text, "reply_markup": markup });
            let resp = http.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("telegram HTTP {}", resp.status()));
            }
            Ok(())
        }
        "slack" => {
            let body = json!({
                "channel": chat_id,
                "text": text,
                "blocks": [
                    { "type": "section", "text": { "type": "mrkdwn", "text": text } },
                    markup,
                ],
            });
            let resp = http
                .post("https://slack.com/api/chat.postMessage")
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            if data.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Err(format!(
                    "slack chat.postMessage: {}",
                    data.get("error").and_then(|v| v.as_str()).unwrap_or("unknown")
                ));
            }
            Ok(())
        }
        "discord" => {
            // The linked id is the USER id — open (or reuse) the bot↔user DM
            // channel first; fall back to treating it as a channel id.
            let dm_channel = match http
                .post("https://discord.com/api/v10/users/@me/channels")
                .header("Authorization", format!("Bot {token}"))
                .json(&json!({ "recipient_id": chat_id }))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string))
                    .unwrap_or_else(|| chat_id.to_string()),
                _ => chat_id.to_string(),
            };
            let url = format!("https://discord.com/api/v10/channels/{dm_channel}/messages");
            let body = json!({ "content": text, "components": [markup] });
            let resp = http
                .post(&url)
                .header("Authorization", format!("Bot {token}"))
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("discord HTTP {}", resp.status()));
            }
            Ok(())
        }
        "line" => {
            let body = json!({
                "to": chat_id,
                "messages": [{ "type": "text", "text": text, "quickReply": markup }],
            });
            let resp = http
                .post("https://api.line.me/v2/bot/message/push")
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("line HTTP {}", resp.status()));
            }
            Ok(())
        }
        other => Err(format!("channel {other} has no button sender")),
    }
}

/// Send plain text to a channel via the shared sender factory. Returns whether
/// delivery succeeded. Best-effort (logs, never panics).
async fn send_plain_text(
    http: &reqwest::Client,
    channel: &str,
    token: &str,
    chat_id: &str,
    text: &str,
) -> bool {
    let target = crate::channel_sender::ChannelTarget {
        channel_type: channel.to_string(),
        chat_id: chat_id.to_string(),
        token: token.to_string(),
        extra_id: None,
    };
    let sender = crate::channel_sender::create_sender(&target, http.clone());
    match sender.send_text(text).await {
        Ok(()) => true,
        Err(e) => {
            warn!(%channel, error = %e, "goal-notify: plain send failed");
            false
        }
    }
}

/// Handle a goal-loop button action from a channel.
///
/// Returns:
/// - `None` — `action_data` is not a goal action (the dispatcher falls through).
/// - `Some(Ok(msg))` — decision handled; `msg` is the zh-TW ack to show.
/// - `Some(Err(msg))` — an error to show the presser.
pub async fn decide_from_channel(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    action_data: &str,
) -> Option<Result<String, String>> {
    let action = channel_format::parse_goal_action(action_data)?;
    match action {
        channel_format::GoalAction::Retry(task_id) => {
            Some(resolve_needs_human(home_dir, channel, channel_user_id, &task_id, "retry").await)
        }
        channel_format::GoalAction::Done(task_id) => {
            Some(resolve_needs_human(home_dir, channel, channel_user_id, &task_id, "done").await)
        }
        channel_format::GoalAction::Abort(task_id) => {
            Some(resolve_needs_human(home_dir, channel, channel_user_id, &task_id, "abort").await)
        }
        channel_format::GoalAction::Kickoff(approval_id, approve) => {
            Some(decide_kickoff(home_dir, channel, channel_user_id, &approval_id, approve).await)
        }
    }
}

/// Apply a needs_human decision to the task store + record it on the Activity
/// Feed. The store transition is fail-closed (only acts from `needs_human`).
async fn resolve_needs_human(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    task_id: &str,
    decision: &str,
) -> Result<String, String> {
    let store = TaskStore::open(home_dir).map_err(|e| format!("開啟任務資料庫失敗：{e}"))?;
    let task = store.get_task(task_id).await.map_err(|e| e.to_string())?;
    let Some(task) = task else {
        return Err("找不到此任務".into());
    };
    let changed = store
        .resolve_needs_human(task_id, decision, "")
        .await
        .map_err(|e| e.to_string())?;
    if !changed {
        return Ok("此任務已被處理過（可能已由他人決定或狀態已改變）。".into());
    }
    let (verb_zh, event) = match decision {
        "retry" => ("重試", "goal_loop.human_decision.retry"),
        "done" => ("標記完成", "goal_loop.human_decision.done"),
        "abort" => ("放棄", "goal_loop.human_decision.abort"),
        _ => ("處理", "goal_loop.human_decision"),
    };
    let summary = format!(
        "人工決定「{verb_zh}」目標任務「{}」（來自 {channel}:{channel_user_id}）",
        task.title
    );
    append_activity(&store, event, &task.assigned_to, Some(task_id), &summary).await;
    Ok(format!("已{verb_zh}此目標任務。"))
}

/// Approve/deny a kickoff approval through the ApprovalBroker. The goal-loop
/// driver polls the approval and starts (or aborts) dispatch on its next tick.
async fn decide_kickoff(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    approval_id: &str,
    approve: bool,
) -> Result<String, String> {
    let broker = crate::approval::ApprovalBroker::open(home_dir)
        .map_err(|e| format!("開啟審批資料庫失敗：{e}"))?;
    let id = crate::approval::ApprovalId::from(approval_id.to_string());
    let decided_by = format!("channel:{channel}:{channel_user_id}");
    broker.decide(&id, approve, &decided_by).await?;
    // Record on the Activity Feed against the approval's agent, best-effort.
    if let Ok(store) = TaskStore::open(home_dir) {
        let agent = broker
            .get(&id)
            .await
            .ok()
            .flatten()
            .map(|r| r.agent_id)
            .unwrap_or_default();
        let verb = if approve { "核准啟動" } else { "拒絕啟動" };
        append_activity(
            &store,
            "goal_loop.kickoff_decision",
            &agent,
            None,
            &format!("人工{verb}自主目標（審批 {approval_id}，來自 {channel}）"),
        )
        .await;
    }
    Ok(if approve {
        "已核准，目標將開始自主執行。".into()
    } else {
        "已拒絕，目標不會啟動。".into()
    })
}

/// Best-effort Activity Feed append (telemetry, never control flow).
async fn append_activity(
    store: &TaskStore,
    event_type: &str,
    agent_id: &str,
    task_id: Option<&str>,
    summary: &str,
) {
    let row = ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.to_string(),
        task_id: task_id.map(str::to_string),
        summary: summary.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: None,
    };
    if let Err(e) = store.append_activity(&row).await {
        tracing::debug!(error = %e, "goal-notify: activity append failed (non-fatal)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_task(id: &str) -> TaskRow {
        TaskRow::new(
            id.into(),
            "整理客戶月報".into(),
            "把客戶資料整理成月報並寄出".into(),
            "medium".into(),
            "alice".into(),
            "goal:telegram".into(),
        )
    }

    #[test]
    fn source_target_prefers_stamped_source() {
        let mut t = mk_task("g1");
        assert_eq!(task_source_target(&t), None, "no source columns ⇒ None");
        t.source_channel = Some("telegram".into());
        t.source_chat_id = Some("12345".into());
        assert_eq!(
            task_source_target(&t),
            Some(("telegram".into(), "12345".into()))
        );
        // Blank/whitespace source is ignored (fail back to [proactive]).
        t.source_chat_id = Some("   ".into());
        assert_eq!(task_source_target(&t), None);
    }

    #[test]
    fn progress_body_renders_each_phase() {
        let mut t = mk_task("abcdef0123456789");
        let dispatched = progress_body(
            &t,
            &GoalProgress::Dispatched { iter: 1, cap: 8, retry: false },
        );
        assert!(dispatched.contains("#abcdef01"), "short id (8 chars)");
        assert!(dispatched.contains("第 1/8 輪"));

        let rejected = {
            t.judge_feedback = Some("缺少營收圖表".into());
            progress_body(&t, &GoalProgress::Rejected { iter: 2, cap: 8 })
        };
        assert!(rejected.contains("未通過"));
        assert!(rejected.contains("缺少營收圖表"));

        t.result_summary = Some("已完成月報並寄出".into());
        let done = progress_body(&t, &GoalProgress::Done);
        assert!(done.contains("已完成"));
        assert!(done.contains("已完成月報並寄出"));
    }

    #[tokio::test]
    async fn decide_from_channel_ignores_non_goal_actions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(decide_from_channel(dir.path(), "telegram", "u1", "garbage")
            .await
            .is_none());
        assert!(
            decide_from_channel(dir.path(), "telegram", "u1", "duduclaw:install_approve:x")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn retry_transitions_needs_human_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = TaskRow::new(
            "g1".into(),
            "goal g1".into(),
            "do it".into(),
            "medium".into(),
            "alice".into(),
            "system".into(),
        );
        t.status = "needs_human".into();
        t.goal_mode = true;
        store.insert_task(&t).await.unwrap();

        let out = decide_from_channel(dir.path(), "telegram", "u1", "duduclaw:goal_retry:g1")
            .await
            .unwrap();
        assert!(out.is_ok(), "retry ack: {out:?}");
        assert_eq!(store.get_task("g1").await.unwrap().unwrap().status, "pending");

        // A second press is a no-op (already left needs_human) — fail-closed.
        let again = decide_from_channel(dir.path(), "telegram", "u1", "duduclaw:goal_retry:g1")
            .await
            .unwrap();
        assert!(again.unwrap().contains("已被處理過"));
    }

    #[tokio::test]
    async fn abort_marks_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut t = TaskRow::new(
            "g2".into(),
            "goal g2".into(),
            "do it".into(),
            "medium".into(),
            "alice".into(),
            "system".into(),
        );
        t.status = "needs_human".into();
        store.insert_task(&t).await.unwrap();

        let out = decide_from_channel(dir.path(), "telegram", "u1", "duduclaw:goal_abort:g2")
            .await
            .unwrap();
        assert!(out.is_ok());
        assert_eq!(store.get_task("g2").await.unwrap().unwrap().status, "cancelled");
    }
}
