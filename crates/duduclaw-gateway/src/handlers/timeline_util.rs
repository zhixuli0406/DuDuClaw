//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── Work Timeline (G11) row derivation ─────────────────────────────────────
//
// Pure functions (no I/O) so the Gantt-row shaping is unit-testable. Rows come
// only from stores that carry REAL timestamps:
//   • task board  — ranged bars (created/claimed → completed) or honest instants
//   • activity    — instants (`ended_at == started_at`); the UI renders dots
//   • heartbeat   — `last_run` instants only (run durations are not persisted)

/// Hard cap on rows returned by `timeline.list` (stated in the response).
pub(crate) const TIMELINE_ROW_CAP: usize = 2000;

/// One lane row of the company work timeline.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct TimelineRow {
    pub agent_id: String,
    /// "task" | "delegation" | "heartbeat" | "skill" | "autopilot" | "governance" | "activity"
    pub kind: String,
    pub label: String,
    /// RFC3339.
    pub started_at: String,
    /// RFC3339. `None` = still running (UI extends to now);
    /// equal to `started_at` = a point-in-time instant (UI renders a dot).
    pub ended_at: Option<String>,
    pub status: String,
    pub ref_id: String,
}

pub(crate) fn parse_timeline_ts(s: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Classify an activity `event_type` into a timeline lane kind.
pub(crate) fn timeline_kind_for_event(event_type: &str) -> &'static str {
    let t = event_type.to_ascii_lowercase();
    if t.contains("delegat") {
        "delegation"
    } else if t.contains("heartbeat") {
        "heartbeat"
    } else if t.contains("skill") {
        "skill"
    } else if t.contains("autopilot") {
        "autopilot"
    } else if t.contains("governance") || t.contains("security") {
        "governance"
    } else {
        "activity"
    }
}

/// Derive a timeline row from one task, or `None` when the task has no
/// parseable timestamps or falls entirely outside the `[from, to]` window.
///
/// Honesty rules (never invent a duration):
///   • done / cancelled / failed → bar `start → completed_at` (fallback
///     `updated_at`, the moment the terminal status was written)
///   • in_progress → running bar (`ended_at = None`)
///   • blocked / in_review with a real claim → bar `claimed_at → updated_at`
///     (the moment the task entered its current state)
///   • anything never started (todo / backlog / unclaimed blocked) → an
///     instant at `created_at`
pub(crate) fn timeline_row_from_task(
    task: &TaskRow,
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> Option<TimelineRow> {
    let claimed = task
        .claimed_at
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(parse_timeline_ts);
    let created = parse_timeline_ts(&task.created_at);
    let start = claimed.or(created)?;

    let end: Option<chrono::DateTime<Utc>> = match task.status.as_str() {
        "in_progress" => None,
        "done" | "cancelled" | "failed" => Some(
            task.completed_at
                .as_deref()
                .and_then(parse_timeline_ts)
                .or_else(|| parse_timeline_ts(&task.updated_at))
                .unwrap_or(start),
        ),
        "blocked" | "in_review" => {
            if claimed.is_some() {
                Some(parse_timeline_ts(&task.updated_at).unwrap_or(start))
            } else {
                // Never started: a queue-state instant, not a fake bar.
                Some(start)
            }
        }
        // todo / backlog / unknown: queued, no real work interval.
        _ => Some(start),
    };
    // A terminal timestamp earlier than the start is data noise, not a
    // negative-width bar — clamp to an instant.
    let end = end.map(|e| e.max(start));

    // Window intersection (running bars extend to `now`).
    let effective_end = end.unwrap_or(now);
    if effective_end < from || start > to {
        return None;
    }

    Some(TimelineRow {
        agent_id: task.assigned_to.clone(),
        kind: "task".into(),
        label: duduclaw_core::truncate_chars(&task.title, 120),
        started_at: start.to_rfc3339(),
        ended_at: end.map(|e| e.to_rfc3339()),
        status: task.status.clone(),
        ref_id: task.id.clone(),
    })
}

/// Derive an instant row from one activity event (or `None` if outside the
/// window / unparseable / redundant with task bars).
pub(crate) fn timeline_row_from_activity(
    event: &ActivityRow,
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
) -> Option<TimelineRow> {
    // `task_created` duplicates the start edge of the task bar emitted by
    // `timeline_row_from_task` — drop it to avoid double-marking lanes.
    if event.event_type == "task_created" {
        return None;
    }
    let ts = parse_timeline_ts(&event.timestamp)?;
    if ts < from || ts > to {
        return None;
    }
    let rfc = ts.to_rfc3339();
    Some(TimelineRow {
        agent_id: event.agent_id.clone(),
        kind: timeline_kind_for_event(&event.event_type).into(),
        label: duduclaw_core::truncate_chars(&event.summary, 120),
        started_at: rfc.clone(),
        ended_at: Some(rfc),
        status: event.event_type.clone(),
        ref_id: event.id.clone(),
    })
}

/// Merge tasks + activity instants + heartbeat `last_run` instants into a
/// window-filtered, chronologically sorted, capped row set.
/// Returns `(rows, truncated)`.
pub(crate) fn derive_timeline_rows(
    tasks: &[TaskRow],
    activities: &[ActivityRow],
    heartbeats: &[(String, Option<String>)],
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> (Vec<TimelineRow>, bool) {
    let mut rows: Vec<TimelineRow> = Vec::new();
    for task in tasks {
        if let Some(r) = timeline_row_from_task(task, from, to, now) {
            rows.push(r);
        }
    }
    for event in activities {
        if let Some(r) = timeline_row_from_activity(event, from, to) {
            rows.push(r);
        }
    }
    for (agent_id, last_run) in heartbeats {
        let Some(ts) = last_run.as_deref().and_then(parse_timeline_ts) else {
            continue;
        };
        if ts < from || ts > to {
            continue;
        }
        let rfc = ts.to_rfc3339();
        rows.push(TimelineRow {
            agent_id: agent_id.clone(),
            kind: "heartbeat".into(),
            label: String::new(),
            started_at: rfc.clone(),
            ended_at: Some(rfc),
            status: "fired".into(),
            ref_id: format!("heartbeat:{agent_id}"),
        });
    }
    rows.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
            .then_with(|| a.ref_id.cmp(&b.ref_id))
    });
    // `truncated` must also cover the case where the activity FETCH hit its
    // row cap entirely inside the window: the derived row count then stays at
    // or under the cap, but events older than the oldest fetched activity —
    // yet still newer than `from` — were never loaded (LOW, 2026-07 review).
    let fetch_hit_cap = activities.len() >= TIMELINE_ROW_CAP;
    let oldest_fetched_newer_than_from = activities
        .iter()
        .filter_map(|a| parse_timeline_ts(&a.timestamp))
        .min()
        .map(|ts| ts > from)
        .unwrap_or(false);
    let truncated =
        rows.len() > TIMELINE_ROW_CAP || (fetch_hit_cap && oldest_fetched_newer_than_from);
    rows.truncate(TIMELINE_ROW_CAP);
    (rows, truncated)
}
