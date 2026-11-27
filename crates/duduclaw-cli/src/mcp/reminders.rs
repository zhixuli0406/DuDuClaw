use super::*;

/// Create a one-shot reminder.
///
/// A reminder fires as its `agent_id` (an `agent_callback` reminder wakes
/// that employee with the given prompt), so naming another employee needs
/// [`check_record_change_allowed`] against it — the same relationship
/// a cron `schedule` on `tasks_create` requires for a recurring wake-up. Omitted `agent_id`
/// means the caller itself.
pub(crate) async fn handle_create_reminder(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    use duduclaw_gateway::reminder_scheduler::{
        AppendResult, MAX_FUTURE_DAYS, MAX_MESSAGE_LEN, MAX_PROMPT_LEN, MAX_REMINDERS_PER_AGENT,
        Reminder, ReminderMode, ReminderStatus, append_reminder_checked_admitted, is_valid_discord_chat_id,
        parse_time_spec,
    };

    let time_str = params.get("time").and_then(|v| v.as_str()).unwrap_or("");
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let supplied_channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let supplied_chat = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let mode_str = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("direct");
    let prompt = params.get("prompt").and_then(|v| v.as_str());
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(actor.id());

    // RM-124: a WebChat conversation is never a model-supplied target.
    if let Err(message) = duduclaw_gateway::scheduled_webchat::refuse_supplied_target(
        Some(supplied_channel), Some(supplied_chat), None,
    ) {
        return tool_error(message);
    }

    // Validate agent_id format
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id format"}],
            "isError": true
        });
    }

    let executor = if agent_id == "default" {
        resolve_main_agent_name(home_dir).await
    } else { agent_id.to_string() };
    // H1/H2: the default destination, the stored origin and its proof all
    // come from the gateway's live turn context — never the process env.
    let context = turn_context(home_dir, actor.agent(), &executor).await;
    let target = match resolve_turn_notify_target(
        &serde_json::json!({"notify_channel": supplied_channel, "notify_chat_id": supplied_chat}),
        actor.agent(), &executor, context.as_ref(),
    ) {
        Ok(target) => target,
        Err(message) => return tool_error(&message),
    };
    // The stored origin (channel or WebChat) and the opaque id of its
    // gateway proof record; the gateway re-verifies the record at send.
    let origin = stored_origin(context.as_ref(), &target);
    let webchat_origin = target.delivery() == Delivery::OriginWebchat;
    let channel = target.channel.as_deref().unwrap_or("");
    let chat_id = target.chat.as_deref().unwrap_or("");

    if time_str.is_empty() || channel.is_empty() || chat_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: time, channel, and chat_id are required \
                (in a supported conversation channel/chat_id may be omitted: the reminder then reports back to the conversation this was created from)"}],
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

    let principal = host_on_behalf_of(home_dir, actor.id());
    if mode == ReminderMode::AgentCallback {
        if let Err(reason) = duduclaw_gateway::on_behalf_of::access::check_main_target(
            home_dir, principal.as_deref(), agent_id,
        ).await {
            return tool_error(&reason);
        }
    }

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
    // `channel_sender` factory (BUG-2 fix). WebChat has no bot identity a
    // detached scheduler can push into, so it is accepted ONLY as the
    // gateway-attested origin above (RM-124): the reminder is then appended
    // to that conversation's history at fire time, after an ownership
    // re-check, instead of being sent.
    if !webchat_origin && !matches!(
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
        agent_id: executor.clone(),
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
        // B11: set in a conversation that came through an employee's own
        // bot / app ⇒ delivered through the same one.
        sender_agent: turn_reply_sender(context.as_ref(), channel, chat_id).or_else(|| (channel == "googlechat").then(|| executor.clone())),
        notify_origin: origin.as_ref().map(|(recipient, _)| recipient.clone()),
        notify_origin_proof: origin.map(|(_, proof)| proof),
        on_behalf_of: principal,
        isolation_bound: false, // The append boundary stamps the verified host scope.
    };

    let id = reminder.id.clone();

    let admission = match admit_background_caller(home_dir, actor, "create_reminder").await {
        Ok(guard) => guard,
        Err(reason) => return tool_error(&reason),
    };
    // Atomic count-check + append, with the lease owned by the blocking writer.
    match append_reminder_checked_admitted(
        home_dir, &reminder, MAX_REMINDERS_PER_AGENT, admission,
    ).await {
        Ok(AppendResult::Ok) => created_reminder_receipt(
            home_dir, &id, trigger_at, channel, webchat_origin,
            supplied_channel.is_empty() && supplied_chat.is_empty(),
        ),
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

fn created_reminder_receipt(
    home: &Path,
    id: &str,
    trigger_at: chrono::DateTime<chrono::Utc>,
    channel: &str,
    webchat_origin: bool,
    original_conversation: bool,
) -> Value {
    let trigger_display = duduclaw_core::company_timezone::format_local(
        &duduclaw_core::company_timezone::read(home), trigger_at, "%Y-%m-%d %H:%M",
    );
    let trigger_instant = trigger_at.to_rfc3339();
    if webchat_origin {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Reminder created (id: {id}, trigger: {trigger_instant}, channel: webchat). 提醒時間：{trigger_display}。 \
                 提醒會在時間到時回到這個 WebChat 對話，不會即時推播；重新開啟此對話即可查看。\
                 It will be posted back to this WebChat conversation. There is no live push; reopen this conversation to see it."
            )}]
        })
    } else {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Reminder created (id: {id}, trigger: {trigger_instant}, channel: {channel}). 提醒時間：{trigger_display}。提醒會傳送到 {} 的{}對話。",
                duduclaw_gateway::decision_notify::channel_display_name(channel),
                if original_conversation { "原本這個" } else { "指定" }
            )}]
        })
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn reminder_receipt_shows_company_local_time_and_preserves_instant() {
        let home = tempfile::tempdir().unwrap();
        let at = "2026-12-01T01:00:00Z".parse().unwrap();
        for (config, display) in [
            ("[general]\ncompany_timezone = 'Asia/Taipei'\n", "2026-12-01 09:00 CST"),
            ("", "2026-12-01 01:00 UTC"),
        ] {
            std::fs::write(home.path().join("config.toml"), config).unwrap();
            for webchat in [false, true] {
                let result = created_reminder_receipt(home.path(), "reminder-id", at, "line", webchat, true);
                let text = result["content"][0]["text"].as_str().unwrap();
                assert!(text.contains(display), "{text}");
                assert!(text.contains("id: reminder-id, trigger: 2026-12-01T01:00:00+00:00"), "{text}");
            }
        }
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

    render_reminders(&reminders)
}

pub(crate) fn render_reminders(reminders: &[duduclaw_gateway::reminder_scheduler::Reminder]) -> Value {
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
                "error": r.error,
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
