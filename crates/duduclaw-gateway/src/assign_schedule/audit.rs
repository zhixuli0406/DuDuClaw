//! Audit trail of scheduled assignments (`<home>/security_audit.jsonl`, the
//! shared hash-chained log): `assign_schedule_created`,
//! `assign_schedule_paused` / `_resumed` / `_removed` (by a person),
//! `assign_schedule_fired` (a run was created), `assign_schedule_skipped`
//! (a slot passed without a run, with the reason) and
//! `assign_schedule_auto_paused` (the gateway paused it at a fire).

use std::path::Path;

use duduclaw_security::audit::{AuditEvent, Severity, append_audit_event};
use serde_json::json;

use super::manage::{Creator, ManageAction};
use crate::task_store::AssignScheduleRow;

fn write(
    home: &Path,
    kind: &str,
    row: &AssignScheduleRow,
    severity: Severity,
    extra: serde_json::Value,
) {
    let mut details = json!({
        "schedule_id": row.id,
        "creator": format!("user:{}", row.creator_user_id),
        "created_via": row.created_via,
        "kind": row.kind,
    });
    if let (Some(d), Some(e)) = (details.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            d.insert(k.clone(), v.clone());
        }
    }
    tracing::info!(event = kind, schedule = %row.id, agent = %row.agent_id, "scheduled assignment");
    append_audit_event(
        home,
        &AuditEvent::new(kind, &row.agent_id, severity, details),
    );
}

pub(super) fn created(home: &Path, row: &AssignScheduleRow) {
    write(
        home,
        "assign_schedule_created",
        row,
        Severity::Info,
        json!({ "schedule": serde_json::from_str::<serde_json::Value>(&row.schedule_json).ok(), "next_fire_at": row.next_fire_at }),
    );
}

pub(super) fn managed(home: &Path, row: &AssignScheduleRow, actor: &Creator, action: ManageAction) {
    let kind = match action {
        ManageAction::Pause => "assign_schedule_paused",
        ManageAction::Resume => "assign_schedule_resumed",
        ManageAction::Remove => "assign_schedule_removed",
    };
    write(
        home,
        kind,
        row,
        Severity::Info,
        json!({ "by": actor.principal() }),
    );
}

pub(super) fn fired(home: &Path, row: &AssignScheduleRow, slot: &str, task_id: &str) {
    write(
        home,
        "assign_schedule_fired",
        row,
        Severity::Info,
        json!({ "slot": slot, "task_id": task_id }),
    );
}

pub(super) fn skipped(home: &Path, row: &AssignScheduleRow, slot: &str, reason: &str) {
    write(
        home,
        "assign_schedule_skipped",
        row,
        Severity::Warning,
        json!({ "slot": slot, "reason": reason }),
    );
}

pub(super) fn auto_paused(home: &Path, row: &AssignScheduleRow, slot: &str, reason: &str) {
    write(
        home,
        "assign_schedule_auto_paused",
        row,
        Severity::Warning,
        json!({ "slot": slot, "reason": reason }),
    );
}
