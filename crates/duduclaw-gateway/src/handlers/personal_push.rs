//! Live push events about another person's Personal employee (native Linux
//! isolation v1.81, tasks P1c + P1d). Protected mode only.
//!
//! The dashboard fan-out (`server.rs::handle_socket`) filters every prepared
//! frame per connection through `PushGate::filter_prepared`, which knows the
//! subscriber. After the existing rules admit a frame, [`withheld`] drops it
//! when it is about a Personal employee the subscriber does not own — the
//! same set `personal_visibility::hidden_from` hides from its listings (so an
//! Admin who is not the owner, a manager, and the legacy `system` context get
//! nothing; the owner gets everything as before). Nothing is stripped to an
//! id: the event is simply not sent. The ownership mirror is read fresh per
//! frame; unreadable ⇒ withheld.
//!
//! P1d: this runs for **every** rule except `System` / `Authenticated` (ids
//! only, no employee content) and `Never` — task, approval, employee,
//! autonomy and every Admin-only or unknown event alike. Subjects of a
//! frame: the task's employee from the frame's own snapshot
//! (`PreparedPush.task`), the approval card's employee
//! (`PreparedPush.approval`), every employee-naming payload key
//! ([`SUBJECT_KEYS`]) and the employee of a payload `task_id` / `plan_id`
//! (read from `tasks.db`; unreadable ⇒ withheld). A frame with no
//! recognizable subject could still concern one, so it is withheld from a
//! subscriber who has any hidden Personal employee — unless its event name
//! is on [`NO_EMPLOYEE_DATA`], an explicit list of events known to carry no
//! employee data. The legacy synchronous `PushGate::filter` (tests and
//! one-shot callers, `filter_push_event`) is not changed.
use std::collections::BTreeSet;

use super::*;
use super::push_filter::{PreparedPush, PushRule, classify};
use crate::personal_visibility::{self as visibility, WorkItem};

/// Payload keys that name an employee (a string, or an array of strings).
pub(super) const SUBJECT_KEYS: &[&str] = &[
    "agent_id", "assigned_to", "agent", "agent_name", "employee", "target_agent", "from_agent", "to_agent",
    "agents",
];

/// Admin-only / unclassified events known to carry no employee data: they
/// are never withheld for lack of a subject. Anything else without a
/// recognizable subject is withheld from a subscriber with hidden Personal
/// employees (fail closed).
/// - `system.update_failed`: the updater's failure code and message.
/// - `auth.cli_login.output` / `auth.cli_login.status`: a provider-login
///   transcript, already restricted to the Admin who started it.
pub(super) const NO_EMPLOYEE_DATA: &[&str] = &["system.update_failed", "auth.cli_login.output", "auth.cli_login.status"];

/// Rules whose events are never about one employee's content.
fn exempt(rule: PushRule) -> bool {
    matches!(rule, PushRule::System | PushRule::Authenticated | PushRule::Never)
}

/// The employees `frame` is about. `Err` = a named task / plan could not be
/// resolved (withhold).
pub(super) fn frame_subjects(home: &Path, frame: &PreparedPush, parsed: &Value) -> Result<BTreeSet<String>, ()> {
    let mut subjects = BTreeSet::new();
    if let Some((_, Some(owner), _)) = &frame.task {
        subjects.insert(owner.clone());
    }
    if let Some((record, _)) = &frame.approval {
        subjects.insert(record.agent_id.clone());
    }
    if let Some(payload) = parsed.get("payload") {
        for key in SUBJECT_KEYS {
            match payload.get(*key) {
                Some(Value::String(agent)) => {
                    subjects.insert(agent.clone());
                }
                Some(Value::Array(agents)) => {
                    subjects.extend(agents.iter().filter_map(Value::as_str).map(str::to_owned));
                }
                _ => {}
            }
        }
        let named = [("task_id", WorkItem::Task), ("plan_id", WorkItem::Plan)];
        for (key, item) in named {
            let Some(id) = payload.get(key).and_then(Value::as_str).filter(|id| !id.is_empty()) else {
                continue;
            };
            if item == WorkItem::Task && frame.task.as_ref().is_some_and(|(task, _, _)| task == id) {
                continue;
            }
            if let Some(agent) = visibility::work_item_employee(home, item, id).map_err(|_| ())? {
                subjects.insert(agent);
            }
        }
    }
    subjects.retain(|agent| !agent.is_empty());
    Ok(subjects)
}

/// `true` when `frame` must not reach the subscriber `user_id`: it is about
/// a Personal employee `user_id` does not own, or it could be (no subject,
/// not on [`NO_EMPLOYEE_DATA`]) and `user_id` has such employees hidden.
/// Always `false` outside protected mode and for `System` /
/// `Authenticated` events.
pub(super) fn withheld(home: &Path, protected: bool, user_id: &str, frame: &PreparedPush) -> bool {
    if !protected {
        return false;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&frame.line) else {
        return true;
    };
    let event = parsed.get("event").or_else(|| parsed.get("type")).and_then(Value::as_str).unwrap_or("");
    if exempt(classify(event)) {
        return false;
    }
    let Ok(subjects) = frame_subjects(home, frame, &parsed) else {
        return true;
    };
    if subjects.is_empty() && NO_EMPLOYEE_DATA.contains(&event) {
        return false;
    }
    let hidden = match visibility::hidden_from(home, true, user_id) {
        Ok(hidden) => hidden,
        Err(_) => return true,
    };
    if hidden.is_empty() {
        return false;
    }
    subjects.is_empty() || subjects.iter().any(|agent| hidden.contains(agent))
}
