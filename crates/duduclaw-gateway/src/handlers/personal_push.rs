//! Live push events about another person's Personal employee (native Linux
//! isolation v1.81, task P1c). Protected mode only.
//!
//! The dashboard fan-out (`server.rs::handle_socket`) filters every prepared
//! frame per connection through `PushGate::filter_prepared`, which knows the
//! subscriber. After the existing rules admit a frame, [`withheld`] drops it
//! when it is a task (`task.*`, `goal.*`), activity (`activity.new`),
//! approval (`approval.*`) or employee (`agent.*`, `autonomy.changed`, …)
//! event about a Personal employee the subscriber does not own — the same
//! set `personal_visibility::hidden_from` hides from its listings (so an
//! Admin who is not the owner, a manager, and the legacy `system` context get
//! nothing; the owner gets everything as before). Nothing is stripped to an
//! id: the event is simply not sent. The ownership mirror is read fresh per
//! frame (only for frames that name an employee); unreadable ⇒ withheld.
//!
//! Subjects of a frame: the task's employee from the frame's own snapshot
//! (`PreparedPush.task`, read once per broadcast from `tasks.db`), the
//! approval card's employee (`PreparedPush.approval`), and the payload's
//! `agent_id` / `assigned_to`. The legacy synchronous `PushGate::filter`
//! (tests and one-shot callers, `filter_push_event`) is not changed.
use std::collections::BTreeSet;

use super::*;
use super::push_filter::{PreparedPush, PushRule, classify};

/// Rules whose events are about one employee's work.
fn about_an_employee(rule: PushRule) -> bool {
    matches!(rule, PushRule::Task | PushRule::Approval | PushRule::Agent | PushRule::Autonomy)
}

/// The employees `frame` is about (empty for system-wide events).
pub(super) fn frame_subjects(frame: &PreparedPush) -> BTreeSet<String> {
    let mut subjects = BTreeSet::new();
    let Ok(parsed) = serde_json::from_str::<Value>(&frame.line) else {
        return subjects;
    };
    let event = parsed.get("event").or_else(|| parsed.get("type")).and_then(Value::as_str).unwrap_or("");
    if !about_an_employee(classify(event)) {
        return subjects;
    }
    if let Some((_, Some(owner), _)) = &frame.task {
        subjects.insert(owner.clone());
    }
    if let Some((record, _)) = &frame.approval {
        subjects.insert(record.agent_id.clone());
    }
    if let Some(payload) = parsed.get("payload") {
        for key in ["agent_id", "assigned_to"] {
            if let Some(agent) = payload.get(key).and_then(Value::as_str) {
                subjects.insert(agent.to_owned());
            }
        }
    }
    subjects.retain(|agent| !agent.is_empty());
    subjects
}

/// `true` when `frame` must not reach the subscriber `user_id`: one of its
/// subjects is a Personal employee `user_id` does not own. Always `false`
/// outside protected mode and for frames that name no employee.
pub(super) fn withheld(home: &Path, protected: bool, user_id: &str, frame: &PreparedPush) -> bool {
    if !protected {
        return false;
    }
    let subjects = frame_subjects(frame);
    if subjects.is_empty() {
        return false;
    }
    match crate::personal_visibility::hidden_from(home, true, user_id) {
        Ok(hidden) => subjects.iter().any(|agent| hidden.contains(agent)),
        Err(_) => true,
    }
}
