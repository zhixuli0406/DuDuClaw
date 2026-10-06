//! Content-free notices for workflow events (F5-A).
//!
//! Workflow cards are decided in the dashboard (activation: Admin only;
//! step cards: someone with access to the employee who is in the run's
//! audience). Nothing about them is pushed with content: a notice says only
//! that something waits, for which AI employee, and until when. Recipients
//! are the verified chats of the accounts that may act on it, read fresh
//! from the account store each time; a deployment with no verified chats
//! sees these items only in the dashboard inbox.
use std::path::Path;

/// Verified chats of active accounts that pass `may_receive`.
fn links_where(
    home: &Path,
    may_receive: impl Fn(&duduclaw_auth::UserContext) -> bool,
) -> Vec<(String, String)> {
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else {
        return Vec::new();
    };
    let Ok(users) = db.list_users() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for user in users {
        if user.status != duduclaw_auth::UserStatus::Active {
            continue;
        }
        let Ok(ctx) = crate::review_evidence::audience::trusted_dashboard_principal(home, &user.id)
        else {
            continue;
        };
        if !may_receive(&ctx) {
            continue;
        }
        if let Ok(ids) = db.verified_channels_for_user(&user.id) {
            out.extend(ids.into_iter().map(|i| (i.channel, i.channel_user_id)));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Verified chats of active Admin accounts.
pub fn admin_links(home: &Path) -> Vec<(String, String)> {
    links_where(home, |ctx| ctx.is_admin())
}

/// Who may decide a step card of a run of `agent` whose source draft is
/// `source_task` with `audience`: a current Manager or Admin with Operator
/// access to the employee who passes the shared audience rule.
pub async fn step_card_user_ids(
    home: &Path,
    agent: &str,
    source_task: &str,
    audience: &[String],
) -> Vec<String> {
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else {
        return Vec::new();
    };
    let Ok(users) = db.list_users() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for user in users {
        if user.status != duduclaw_auth::UserStatus::Active {
            continue;
        }
        let Ok(ctx) = crate::review_evidence::audience::trusted_dashboard_principal(home, &user.id)
        else {
            continue;
        };
        if !ctx.has_role(duduclaw_auth::UserRole::Manager)
            || !ctx.has_agent_access(agent, duduclaw_auth::AccessLevel::Operator)
        {
            continue;
        }
        if crate::review_evidence::audience::authorize_workflow_audience(
            home,
            &ctx,
            source_task,
            audience,
        )
        .await
        .is_ok()
        {
            out.push(user.id);
        }
    }
    out
}

/// Verified chats of the given accounts.
pub fn links_of(home: &Path, user_ids: &[String]) -> Vec<(String, String)> {
    links_where(home, |ctx| user_ids.iter().any(|id| id == &ctx.user_id))
}

/// Send `text` to each chat through the employee's channel token cascade.
pub async fn send(home: &Path, agent: &str, links: Vec<(String, String)>, text: &str) {
    let http = reqwest::Client::new();
    for (channel, chat_id) in links {
        let Some(token) = crate::goal_notify::channel_token(home, agent, &channel).await else {
            continue;
        };
        crate::channel_sender::send_plain_text(home, &http, &channel, &token, &chat_id, text).await;
    }
}

/// Best-effort Activity Feed row.
pub async fn activity(
    home: &Path,
    event_type: &str,
    agent: &str,
    summary: &str,
    metadata: serde_json::Value,
) {
    if let Ok(tasks) = crate::task_store::TaskStore::open(home) {
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.into(),
            agent_id: agent.into(),
            task_id: None,
            summary: summary.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: serde_json::to_string(&metadata).ok(),
        };
        if let Err(e) = tasks.append_activity(&row).await {
            tracing::warn!(error = %e, event_type, "workflow activity append failed");
        }
    }
}

/// `2026-10-06 09:00 UTC` from an RFC 3339 instant (the raw text if it
/// does not parse).
pub fn readable_time(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|_| rfc3339.to_string())
}

fn agent_line(agent: &str) -> String {
    duduclaw_core::truncate_chars(agent, 64).to_string()
}

/// Notice for a new workflow step card (no workflow name, step or payload).
pub fn step_card_notice(agent: &str, question: bool, deadline: &str) -> String {
    let head = if question {
        "❓ 有一個工作流程步驟在等你回答"
    } else {
        "📥 有一個工作流程步驟在等你決定"
    };
    format!(
        "{head}\nAI 員工：{}\n請開啟儀表板的待辦清單處理（這裡的回覆不會生效）。\n期限：{deadline}",
        agent_line(agent)
    )
}

/// Tell the people who may decide a new step card that it waits.
pub async fn notify_step_card(
    home: &Path,
    run: &super::WorkflowRun,
    source_task: &str,
    question: bool,
) {
    let ids = step_card_user_ids(home, &run.actor, source_task, &run.audience).await;
    let links = links_of(home, &ids);
    if links.is_empty() {
        return;
    }
    let text = step_card_notice(&run.actor, question, &readable_time(&run.deadline_at));
    send(home, &run.actor, links, &text).await;
}
