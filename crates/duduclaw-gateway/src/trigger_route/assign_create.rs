//! Creating the goal task of a `/交辦`, and the `/交辦 狀態` list.

use std::path::Path;

use duduclaw_auth::GrantSource;
use sha2::{Digest, Sha256};

use super::assign::{AssignCaller, AssignReply, AssignedTask, CreateSpec, principal_of};
use super::assign_texts as t;
use super::texts::Lang;
use crate::chat_commands::GoalCommand;
use crate::goal_create_core::{GoalCreateRequest, create_goal_task};
use crate::goal_loop::{CHAT_ASSIGN_WINDOW_MINUTES, chat_assign_caps};
use crate::task_store::TaskStore;

/// A task id derived from the incoming message, so the same message (LINE
/// redelivers webhooks; durable ingress may run an event again after a
/// restart) never creates a second task. Shaped like a UUID (version nibble
/// 5, RFC 4122 variant) so every consumer that expects one keeps working.
pub(crate) fn task_id_for_event(channel: &str, conversation: &str, event: &str) -> String {
    let digest = Sha256::digest(format!("assign\0{channel}\0{conversation}\0{event}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// `LINE 交辦` and the like (activity line, display only).
fn source_label(channel: &str) -> String {
    let name = match channel {
        "line" => "LINE",
        "telegram" => "Telegram",
        "discord" => "Discord",
        "slack" => "Slack",
        "webchat" => "WebChat",
        "whatsapp" => "WhatsApp",
        "feishu" => "Feishu",
        "teams" => "Teams",
        other => other,
    };
    format!("{name} 交辦")
}

/// The display name of a dashboard account (`None` when unreadable).
pub(crate) fn account_name(home: &Path, user_id: &str) -> Option<String> {
    let db = crate::decision_notify::open_user_db(home).ok().flatten()?;
    let user = db.get_user(user_id).ok().flatten()?;
    let name = user.display_name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Create the task. The rights were decided before; this applies the
/// chat-assign caps, then writes.
pub(crate) async fn create(home: &Path, lang: Lang, spec: CreateSpec, caller: &AssignCaller<'_>) -> AssignReply {
    let failed = || AssignReply {
        text: t::failed(lang).to_string(),
        list: None,
        task: None,
    };
    let Some(principal) = principal_of(&caller.access) else {
        return AssignReply {
            text: t::needs_identity(lang).to_string(),
            list: None,
            task: None,
        };
    };
    let GoalCommand::Create {
        description,
        acceptance_criteria,
        outcome,
        duration_hours,
        risk_boundary,
        plan_first,
    } = spec.goal
    else {
        return failed();
    };
    // Owner decision (PR #194 review M2): a per-task risk boundary replaces
    // the deployment baseline, so only an Admin may set one from a chat.
    let asked_boundary = risk_boundary.as_deref().is_some_and(|b| !b.trim().is_empty());
    let boundary_ignored = asked_boundary && !super::assign::sender_is_admin(caller);
    let risk_boundary = if boundary_ignored { None } else { risk_boundary };
    let store = match TaskStore::open(home) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "chat assignment: task store unavailable");
            return failed();
        }
    };
    let task_id = caller
        .event
        .filter(|e| !e.is_empty())
        .map(|e| task_id_for_event(caller.channel, caller.conversation, e));
    let shown = |task: &crate::task_store::TaskRow| {
        confirmation(home, lang, &spec.agent, &spec.agent_name, &spec.grant, task, boundary_ignored, caller)
    };
    // A redelivered message: answer with the task it already created.
    if let Some(id) = &task_id {
        match existing_for(&store, id, &principal).await {
            Existing::Mine(task) => return shown(&task),
            Existing::Other => {
                tracing::warn!(task_id = %id, "chat assignment: derived task id belongs to someone else");
                return failed();
            }
            Existing::None => {}
        }
    }
    if let Some(refusal) = over_cap(home, lang, &store, &principal, &spec.agent, caller).await {
        return refusal;
    }
    let via = spec.grant.label();
    let req = GoalCreateRequest {
        agent_id: spec.agent.clone(),
        created_by: format!("assign:{}", caller.channel),
        description,
        acceptance_criteria,
        outcome,
        duration_hours,
        risk_boundary,
        plan_first,
        source_label: source_label(caller.channel),
        on_behalf_of: Some(principal.clone()),
        created_via: Some(caller.channel.to_string()),
        source: Some(crate::chat_commands::source_from_session(caller.conversation)),
        task_id: task_id.clone(),
        grant_via: Some(via.clone()),
        ..Default::default()
    };
    match create_goal_task(home, &store, req).await {
        Ok(created) => {
            super::assign_audit::assigned(caller, &spec.agent, &created.task.id, &via);
            shown(&created.task)
        }
        Err(e) => {
            // The same message handled twice at once (webhook redelivery
            // racing durable ingress): the other run's insert won.
            if let Some(id) = &task_id {
                if let Existing::Mine(task) = existing_for(&store, id, &principal).await {
                    return shown(&task);
                }
            }
            tracing::warn!(agent = %spec.agent, error = %e, "chat assignment: create failed");
            failed()
        }
    }
}

/// What a derived task id already points at.
enum Existing {
    None,
    /// A task this principal created from the same message.
    Mine(crate::task_store::TaskRow),
    /// Somebody else's (never overwritten, never shown).
    Other,
}

async fn existing_for(store: &TaskStore, id: &str, principal: &str) -> Existing {
    match store.get_task(id).await {
        Ok(Some(task)) if task.on_behalf_of.as_deref() == Some(principal) => Existing::Mine(task),
        Ok(Some(_)) => Existing::Other,
        _ => Existing::None,
    }
}

/// The chat-assign caps (PR #194 review M1): at most `per_window` tasks
/// every [`CHAT_ASSIGN_WINDOW_MINUTES`] and `max_open` unfinished ones per
/// person, counted from `tasks.db` (plan-first tasks included, since each
/// one costs a planning call). A count that cannot be read refuses.
async fn over_cap(
    home: &Path,
    lang: Lang,
    store: &TaskStore,
    principal: &str,
    agent: &str,
    caller: &AssignCaller<'_>,
) -> Option<AssignReply> {
    let (per_window, max_open) = chat_assign_caps(home);
    let refuse = || {
        Some(super::assign::refusal(
            caller,
            Some(agent),
            "rate_limited",
            t::too_many(lang, per_window, CHAT_ASSIGN_WINDOW_MINUTES, max_open),
        ))
    };
    match store.chat_assign_counts(principal, CHAT_ASSIGN_WINDOW_MINUTES).await {
        Ok((recent, open)) if recent < i64::from(per_window) && open < i64::from(max_open) => None,
        Ok(_) => refuse(),
        Err(e) => {
            tracing::warn!(error = %e, "chat assignment: cannot count earlier assignments");
            Some(AssignReply {
                text: t::failed(lang).to_string(),
                list: None,
                task: None,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn confirmation(
    home: &Path,
    lang: Lang,
    agent: &str,
    agent_name: &str,
    grant: &GrantSource,
    task: &crate::task_store::TaskRow,
    boundary_ignored: bool,
    caller: &AssignCaller<'_>,
) -> AssignReply {
    let owner = match grant {
        GrantSource::Team { via_user, .. } => {
            Some(account_name(home, via_user).unwrap_or_else(|| short_user(via_user)))
        }
        _ => None,
    };
    let short = duduclaw_core::truncate_chars(&task.id, 8);
    let description = duduclaw_core::truncate_chars(&task.title, 200);
    let text = t::confirmation(
        lang,
        &t::Confirmation {
            agent_name,
            short_id: &short,
            description: &description,
            team_owner: owner.as_deref(),
            progress_here: caller.channel != "webchat",
            plan_first: task.plan_pending.is_some() || task.status == "needs_human",
            boundary_ignored,
        },
    );
    AssignReply {
        text,
        list: None,
        task: Some(AssignedTask {
            id: task.id.clone(),
            agent: agent.to_string(),
        }),
    }
}

/// `user:<first 8 characters>` for an account whose name cannot be read.
fn short_user(user_id: &str) -> String {
    format!("user:{}", duduclaw_core::truncate_chars(user_id, 8))
}

/// `/交辦 狀態`: the sender's unfinished assigned tasks (at most 20). A task
/// limited to an audience that does not include this channel shows its
/// number only.
pub(crate) async fn status_text(home: &Path, lang: Lang, caller: &AssignCaller<'_>) -> String {
    let Some(principal) = principal_of(&caller.access) else {
        return t::needs_identity(lang).to_string();
    };
    let store = match TaskStore::open(home) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "chat assignment status: task store unavailable");
            return t::failed(lang).to_string();
        }
    };
    let rows = match store.list_by_principal(&principal, true, 20).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "chat assignment status: list failed");
            return t::failed(lang).to_string();
        }
    };
    if rows.is_empty() {
        return t::status_empty(lang).to_string();
    }
    let names = agent_names(home, &rows);
    let mut out = t::status_heading(lang).to_string();
    for task in &rows {
        let title = if crate::goal_notify_private::channel_may_carry(home, &task.id, caller.channel) {
            duduclaw_core::truncate_chars(&task.title, 40)
        } else {
            t::title_hidden(lang).to_string()
        };
        let agent = names
            .iter()
            .find(|(id, _)| id == &task.assigned_to)
            .map(|(_, n)| n.as_str())
            .unwrap_or(task.assigned_to.as_str());
        out.push_str(&format!(
            "\n• #{} 【{agent}】[{}] {title}",
            duduclaw_core::truncate_chars(&task.id, 8),
            t::status_label(lang, &task.status),
        ));
    }
    out
}

/// Display names of the employees the rows are assigned to, read from their
/// `agent.toml` (the registry is not needed for a name).
fn agent_names(home: &Path, rows: &[crate::task_store::TaskRow]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for r in rows {
        if out.iter().any(|(id, _)| id == &r.assigned_to) || !duduclaw_core::is_valid_agent_id(&r.assigned_to) {
            continue;
        }
        let name = std::fs::read_to_string(home.join("agents").join(&r.assigned_to).join("agent.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .and_then(|t| {
                t.get("agent")
                    .and_then(|a| a.get("display_name"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
            });
        if let Some(name) = name {
            out.push((r.assigned_to.clone(), name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_task_ids_are_stable_uuid_shaped_and_distinct() {
        let a = task_id_for_event("line", "line:U1", "ev1");
        assert_eq!(a, task_id_for_event("line", "line:U1", "ev1"));
        assert_ne!(a, task_id_for_event("line", "line:U1", "ev2"));
        assert_ne!(a, task_id_for_event("line", "line:U2", "ev1"));
        let parsed = uuid::Uuid::parse_str(&a).expect("uuid shaped");
        assert_eq!(parsed.get_version_num(), 5);
    }
}
