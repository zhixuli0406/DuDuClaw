use super::*;

/// Create a one-shot reminder.
///
/// A reminder fires as its `agent_id` (an `agent_callback` reminder wakes
/// that employee with the given prompt), so naming another employee needs
/// [`check_record_change_allowed`] against it — the same relationship
/// `schedule_task` requires for a recurring wake-up. Omitted `agent_id`
/// means the caller itself.
pub(crate) async fn handle_create_reminder(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    use duduclaw_gateway::reminder_scheduler::{
        AppendResult, MAX_FUTURE_DAYS, MAX_MESSAGE_LEN, MAX_PROMPT_LEN, MAX_REMINDERS_PER_AGENT,
        Reminder, ReminderMode, ReminderStatus, append_reminder_checked, is_valid_discord_chat_id,
        parse_time_spec,
    };

    let time_str = params.get("time").and_then(|v| v.as_str()).unwrap_or("");
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let mode_str = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("direct");
    let prompt = params.get("prompt").and_then(|v| v.as_str());
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(actor.id());

    if time_str.is_empty() || channel.is_empty() || chat_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: time, channel, and chat_id are required"}],
            "isError": true
        });
    }

    // Validate agent_id format
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id format"}],
            "isError": true
        });
    }

    if let Err(reason) =
        check_record_change_allowed(home_dir, actor, agent_id, "create_reminder", RecordKind::Reminder)
            .await
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    let mode = match mode_str {
        "agent_callback" => ReminderMode::AgentCallback,
        _ => ReminderMode::Direct,
    };

    // Validate: direct mode needs message, agent_callback needs prompt
    if mode == ReminderMode::Direct && message.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: message is required for direct mode"}],
            "isError": true
        });
    }
    if mode == ReminderMode::AgentCallback && prompt.is_none() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: prompt is required for agent_callback mode"}],
            "isError": true
        });
    }

    // Validate field lengths (resource exhaustion prevention)
    if message.len() > MAX_MESSAGE_LEN {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: message too long ({} chars, max {MAX_MESSAGE_LEN})", message.len())}],
            "isError": true
        });
    }
    if let Some(p) = prompt
        && p.len() > MAX_PROMPT_LEN
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: prompt too long ({} chars, max {MAX_PROMPT_LEN})", p.len())}],
            "isError": true
        });
    }

    // Validate channel — every bot-pushable channel `reminder_scheduler::
    // send_channel_message` can actually deliver to via the unified
    // `channel_sender` factory (BUG-2 fix). WebChat is deliberately excluded:
    // it's a session-scoped WebSocket connection with no persistent bot
    // identity a detached scheduler can push into later (see
    // `reminder_scheduler::resolve_channel_target`'s doc comment).
    if !matches!(
        channel,
        "telegram"
            | "line"
            | "discord"
            | "slack"
            | "whatsapp"
            | "feishu"
            | "googlechat"
            | "teams"
            | "wecom"
            | "dingtalk"
    ) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: unknown channel '{channel}', must be one of telegram/line/discord/slack/whatsapp/feishu/googlechat/teams/wecom/dingtalk")}],
            "isError": true
        });
    }

    // Validate Discord chat_id is numeric at creation time (fail-fast)
    if channel == "discord" && !is_valid_discord_chat_id(chat_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: Discord channel ID must be numeric, got '{chat_id}'")}],
            "isError": true
        });
    }

    // Parse time
    let trigger_at = match parse_time_spec(time_str) {
        Ok(dt) => dt,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid time specification: {e}")}],
                "isError": true
            });
        }
    };

    let now = chrono::Utc::now();

    // Validate trigger is in the future
    if trigger_at <= now {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: trigger time must be in the future"}],
            "isError": true
        });
    }

    // Validate trigger is not too far in the future
    if trigger_at > now + chrono::Duration::days(MAX_FUTURE_DAYS) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: trigger time too far in the future (max {MAX_FUTURE_DAYS} days)")}],
            "isError": true
        });
    }

    let reminder = Reminder {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        trigger_at,
        channel: channel.to_string(),
        chat_id: chat_id.to_string(),
        message: if message.is_empty() {
            None
        } else {
            Some(message.to_string())
        },
        prompt: prompt.map(|s| s.to_string()),
        mode,
        status: ReminderStatus::Pending,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    };

    let id = reminder.id.clone();
    let trigger_display = trigger_at.to_rfc3339();

    // Atomic count-check + append (no TOCTOU race)
    match append_reminder_checked(home_dir, &reminder, MAX_REMINDERS_PER_AGENT).await {
        Ok(AppendResult::Ok) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Reminder created (id: {id}, trigger: {trigger_display}, channel: {channel})"
            )}]
        }),
        Ok(AppendResult::LimitReached(count)) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: agent '{agent_id}' has {count} pending reminders (max {MAX_REMINDERS_PER_AGENT})"
            )}],
            "isError": true
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: failed to save reminder: {e}")}],
            "isError": true
        }),
    }
}

/// List reminders with optional filters.
/// Scoped to the calling agent by default to prevent cross-agent info disclosure.
pub(crate) async fn handle_list_reminders(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    use duduclaw_gateway::reminder_scheduler::list_reminders;

    let status = params.get("status").and_then(|v| v.as_str());
    // Default to caller's own reminders (prevent cross-agent info leak)
    let agent_id = Some(
        params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or(default_agent),
    );

    let reminders = list_reminders(home_dir, status, agent_id).await;

    let entries: Vec<serde_json::Value> = reminders
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "trigger_at": r.trigger_at.to_rfc3339(),
                "channel": r.channel,
                "chat_id": r.chat_id,
                "message": r.message,
                "mode": r.mode,
                "status": r.status,
                "agent_id": r.agent_id,
            })
        })
        .collect();

    let text = if entries.is_empty() {
        "No reminders found.".to_string()
    } else {
        serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
    };

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

/// Cancel a pending reminder.
pub(crate) async fn handle_cancel_reminder(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    use duduclaw_gateway::reminder_scheduler::cancel_reminder;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: id is required"}],
            "isError": true
        });
    }

    match cancel_reminder(home_dir, id, Some(default_agent)).await {
        Ok(true) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Reminder '{id}' cancelled successfully.")}]
        }),
        Ok(false) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Reminder '{id}' not found or already completed.")}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {e}")}],
            "isError": true
        }),
    }
}

// ── Sub-agent management handlers ───────────────────────────
