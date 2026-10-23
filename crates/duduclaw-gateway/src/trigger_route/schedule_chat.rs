//! Scheduled assignments from a chat: `/交辦 @員工 任務 || 時間:每週一 9:00`
//! creates one (`crate::assign_schedule`), `/交辦 排程` lists the sender's,
//! `/交辦 排程 暫停|恢復|刪除 <編號>` controls them. The right to assign was
//! decided by [`super::assign::plan`] exactly as for an immediate `/交辦`;
//! every fire re-checks it on the sender's live account.

use std::path::Path;

use chrono::Utc;

use super::access::Sender;
use super::assign::{AssignCaller, CreateSpec, ScheduleCommand};
use super::assign_texts as at;
use super::schedule_texts as t;
use super::texts::Lang;
use crate::assign_schedule::{
    AssignKind, AssignPayload, CreateError, Creator, ManageAction, NewSchedule, ScheduleSpec, Zone,
    manage, natural,
};
use crate::chat_commands::GoalCommand;
use crate::task_store::{AssignScheduleRow, TaskStore};

/// `||` segment prefixes that carry the schedule.
const WHEN_PREFIXES: &[&str] = &[
    "時間:",
    "時間：",
    "排程:",
    "排程：",
    "schedule:",
    "when:",
    "at:",
];

/// Words that open the schedules sub-command (`/交辦 排程 …`).
const SCHEDULES_WORDS: &[&str] = &["排程", "schedules", "schedule"];

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Take the `時間:` / `排程:` segment out of an assignment text. The other
/// segments are kept byte for byte (an `outcome:` JSON payload included);
/// nothing after an `outcome:` segment is looked at.
pub(crate) fn split_schedule(task: &str) -> (Option<String>, String) {
    let mut when = None;
    let mut kept: Vec<&str> = Vec::new();
    let mut in_outcome = false;
    for (i, seg) in task.split("||").enumerate() {
        let head = seg.trim_start();
        if i > 0 && strip_prefix_ci(head, "outcome:").is_some() {
            in_outcome = true;
        }
        let value = if i > 0 && !in_outcome {
            WHEN_PREFIXES.iter().find_map(|p| strip_prefix_ci(head, p))
        } else {
            None
        };
        match value {
            Some(v) if when.is_none() => when = Some(v.trim().to_string()),
            // A second schedule segment is ambiguous: keep it, so the text
            // reads oddly instead of silently picking one.
            _ => kept.push(seg),
        }
    }
    (when, kept.join("||"))
}

/// The tail after `排程` / `schedules`, when `rest` starts with that word
/// (followed by whitespace or the end).
pub fn strip_schedules_word(rest: &str) -> Option<&str> {
    SCHEDULES_WORDS.iter().find_map(|w| {
        let tail = strip_prefix_ci(rest, w)?;
        (tail.is_empty() || tail.starts_with(char::is_whitespace)).then(|| tail.trim())
    })
}

/// `""` → list; `暫停 <id>` / `pause <id>` …
pub fn parse_command(sub: &str) -> ScheduleCommand {
    let sub = sub.trim();
    if sub.is_empty() {
        return ScheduleCommand::List;
    }
    let (verb, arg) = sub.split_once(char::is_whitespace).unwrap_or((sub, ""));
    let arg = arg
        .trim()
        .trim_start_matches(['#', '＃'])
        .trim()
        .to_string();
    if arg.is_empty() || arg.contains(char::is_whitespace) {
        return ScheduleCommand::Usage;
    }
    let verb_l = verb.to_ascii_lowercase();
    match verb_l.as_str() {
        "暫停" | "暂停" | "一時停止" | "pause" => ScheduleCommand::Pause(arg),
        "恢復" | "恢复" | "繼續" | "再開" | "resume" => ScheduleCommand::Resume(arg),
        "刪除" | "删除" | "移除" | "取消" | "削除" | "remove" | "delete" => {
            ScheduleCommand::Remove(arg)
        }
        _ => ScheduleCommand::Usage,
    }
}

/// The dashboard account behind a chat sender.
fn creator_of(caller: &AssignCaller<'_>) -> Option<Creator> {
    match &caller.access.sender {
        Sender::Admin { user_id: Some(id) } => Creator::new(id, true),
        Sender::Account { user_id, .. } => Creator::new(user_id, false),
        Sender::Admin { user_id: None } | Sender::Unmapped => None,
    }
}

/// Create the scheduled assignment; the reply text.
pub(crate) async fn create(
    home: &Path,
    lang: Lang,
    spec: CreateSpec,
    when: &str,
    caller: &AssignCaller<'_>,
) -> String {
    let Some(creator) = creator_of(caller) else {
        return at::needs_identity(lang).to_string();
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
        return at::usage(lang).to_string();
    };
    let now = Utc::now();
    // Chat times are read in the gateway host's zone (the confirmation
    // shows the time it understood).
    let zone = Zone::Local;
    let Ok(schedule) = natural::parse_natural(when, now, zone) else {
        return t::when_usage(lang).to_string();
    };
    // Same rule as an immediate `/交辦`: only an Admin's risk boundary applies.
    let boundary_ignored = risk_boundary
        .as_deref()
        .is_some_and(|b| !b.trim().is_empty())
        && !creator.is_admin;
    let payload = AssignPayload {
        description: description.clone(),
        acceptance_criteria,
        outcome,
        duration_hours,
        risk_boundary: if boundary_ignored {
            None
        } else {
            risk_boundary
        },
        plan_first,
        ..Default::default()
    };
    let store = match TaskStore::open(home) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "chat schedule: task store unavailable");
            return at::failed(lang).to_string();
        }
    };
    let req = NewSchedule {
        agent_id: spec.agent.clone(),
        kind: AssignKind::Goal,
        payload,
        spec: schedule.clone(),
        timezone: None,
        created_via: caller.channel.to_string(),
        source: Some(crate::chat_commands::source_from_session(
            caller.conversation,
        )),
    };
    match manage::create(home, &store, &creator, req, now).await {
        Ok(row) => {
            let next = next_text(&row, zone);
            t::created(
                lang,
                &t::Created {
                    agent_name: &spec.agent_name,
                    when: &t::describe(lang, &schedule, zone),
                    next: &next,
                    short_id: &duduclaw_core::truncate_chars(&row.id, 8),
                    description: &duduclaw_core::truncate_chars(&description, 200),
                    boundary_ignored,
                },
            )
        }
        Err(CreateError::Spec(e)) => t::spec_error(lang, &e),
        Err(CreateError::TooMany { max }) => t::too_many(lang, max),
        Err(CreateError::Empty) => at::usage(lang).to_string(),
        Err(e) => {
            tracing::warn!(error = ?e, "chat schedule: create failed");
            at::failed(lang).to_string()
        }
    }
}

fn next_text(row: &AssignScheduleRow, zone: Zone) -> String {
    row.next_fire_at
        .as_deref()
        .and_then(crate::assign_schedule::spec::parse_utc)
        .map(|n| t::local(n, zone))
        .unwrap_or_else(|| "—".into())
}

/// `/交辦 排程 …`.
pub(crate) async fn command(
    home: &Path,
    lang: Lang,
    cmd: &ScheduleCommand,
    caller: &AssignCaller<'_>,
) -> String {
    let Some(creator) = creator_of(caller) else {
        return at::needs_identity(lang).to_string();
    };
    let store = match TaskStore::open(home) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "chat schedule: task store unavailable");
            return at::failed(lang).to_string();
        }
    };
    // In a chat everyone (Admins too) sees only their own; Admins see all
    // in the dashboard.
    let rows = match manage::list(&store, &creator, false).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "chat schedule: list failed");
            return at::failed(lang).to_string();
        }
    };
    let (action, arg) = match cmd {
        ScheduleCommand::List => return list_text(home, lang, &rows),
        ScheduleCommand::Usage => return t::command_usage(lang).to_string(),
        ScheduleCommand::Pause(a) => (ManageAction::Pause, a),
        ScheduleCommand::Resume(a) => (ManageAction::Resume, a),
        ScheduleCommand::Remove(a) => (ManageAction::Remove, a),
    };
    // Exact match on the full id or the 8-character number the list shows,
    // among the sender's own schedules only.
    let Some(row) = rows
        .iter()
        .find(|r| r.id == *arg || duduclaw_core::truncate_chars(&r.id, 8) == *arg)
    else {
        return t::manage_error(lang, &crate::assign_schedule::ManageError::NotFound).to_string();
    };
    match manage::manage(home, &store, &creator, &row.id, action, Utc::now()).await {
        Ok(_) => t::managed(lang, action, &duduclaw_core::truncate_chars(&row.id, 8)),
        Err(e) => t::manage_error(lang, &e).to_string(),
    }
}

fn list_text(home: &Path, lang: Lang, rows: &[AssignScheduleRow]) -> String {
    if rows.is_empty() {
        return t::list_empty(lang).to_string();
    }
    let mut out = t::list_heading(lang).to_string();
    for r in rows.iter().take(20) {
        let zone = Zone::parse(r.timezone.as_deref()).unwrap_or(Zone::Local);
        let when = serde_json::from_str::<ScheduleSpec>(&r.schedule_json)
            .map(|s| t::describe(lang, &s, zone))
            .unwrap_or_default();
        let next = next_text(r, zone);
        out.push_str(&format!(
            "\n• #{} 【{}】{when}｜{}｜{}",
            duduclaw_core::truncate_chars(&r.id, 8),
            agent_name(home, &r.agent_id),
            duduclaw_core::truncate_chars(&r.title, 40),
            t::state(lang, &r.status, r.pause_reason.as_deref(), Some(&next)),
        ));
    }
    out.push_str(t::list_footer(lang));
    out
}

/// The employee's display name from `agent.toml`, else its id.
fn agent_name(home: &Path, agent: &str) -> String {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return agent.to_string();
    }
    std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("agent")
                .and_then(|a| a.get("display_name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| agent.to_string())
}

#[cfg(test)]
#[path = "schedule_chat_tests.rs"]
mod tests;
