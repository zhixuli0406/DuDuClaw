//! Odoo Helpdesk / Project as a support-queue data source.
//!
//! This module turns `helpdesk.ticket` (Enterprise) or `project.task`
//! (Community) rows into the ticket/staffing series the decision twin's pilot
//! contract expects. It deliberately stops one step short of the contract type
//! itself: `SupportPilotExport` lives in `duduclaw-gateway`, which depends on
//! *this* crate, so naming it here would invert the dependency. The caller
//! assembles the final export around [`SupportQueueExtract`] — the field names
//! below are byte-identical to the contract's, and the gateway carries a
//! round-trip test that proves it.
//!
//! ## What is measured and what is a proxy
//!
//! * `ticket_id`, `created_at_utc`, `resolved_at_utc` — measured. Odoo stores
//!   naive UTC datetimes (`"2026-09-01 08:30:00"`); they are converted to
//!   RFC3339 `Z` form here and nowhere else.
//! * `agents` — **proxy**. Odoo has no staffing roster; this counts the
//!   distinct assignees who created or closed a ticket on that day. A day with
//!   no activity therefore reports zero assignees, and the caller decides what
//!   floor to apply. Read [`daily_assignee_counts`] before drawing any
//!   conclusion that depends on the staffing series.
//! * `fixed_extra_capacity` — always `0`. Odoo has no such lane.
//!
//! ## Closure semantics
//!
//! * `helpdesk.ticket` → `close_date`. Exact.
//! * `project.task` → `date_end` when set, else `date_last_stage_update` **but
//!   only when the task sits in a folded stage** (`stage_id.fold = true`, the
//!   Odoo convention for "this column means done"). A task in an unfolded
//!   stage is open regardless of how recently its stage changed — using
//!   `date_last_stage_update` alone would report every touched task as closed.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::connector::OdooConnector;

/// Rows fetched per page. Odoo's default `search_read` limit is 80; 500 keeps
/// the round trips down without building a giant response.
pub const PAGE_SIZE: usize = 500;
/// Hard ceiling on one export, well under the pilot contract's own 1e6.
pub const MAX_TICKETS: usize = 200_000;

/// Queue-id prefix minted for every Odoo-derived pilot.
pub const QUEUE_PREFIX: &str = "odoo:";

/// The two models this adapter understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportModel {
    /// Odoo Enterprise helpdesk.
    HelpdeskTicket,
    /// Odoo Community project tasks, used as a support queue.
    ProjectTask,
}

impl SupportModel {
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "helpdesk.ticket" => Some(Self::HelpdeskTicket),
            "project.task" => Some(Self::ProjectTask),
            _ => None,
        }
    }

    pub fn model_name(self) -> &'static str {
        match self {
            Self::HelpdeskTicket => "helpdesk.ticket",
            Self::ProjectTask => "project.task",
        }
    }

    /// The many2one field naming the queue this row belongs to.
    pub fn queue_field(self) -> &'static str {
        match self {
            Self::HelpdeskTicket => "team_id",
            Self::ProjectTask => "project_id",
        }
    }

    /// Fields read from Odoo. Only these — no subject, no description, no
    /// customer: a pilot source artifact must not carry ticket content.
    pub fn fields(self) -> &'static [&'static str] {
        match self {
            Self::HelpdeskTicket => &["id", "create_date", "close_date", "team_id", "user_id"],
            Self::ProjectTask => &[
                "id",
                "create_date",
                "date_end",
                "date_last_stage_update",
                "stage_id",
                "project_id",
                "user_ids",
            ],
        }
    }
}

/// One queue's extracted series, ready for the caller to wrap in the pilot
/// contract type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportQueueExtract {
    /// `odoo:<model>:<queue id>`.
    pub queue_id: String,
    pub window_start_utc: String,
    pub data_cutoff_utc: String,
    pub horizon_days: usize,
    pub tickets: Vec<SupportTicketRow>,
    pub staffing: Vec<SupportStaffingRow>,
    /// Distinct Odoo user ids that appear as an assignee anywhere in the
    /// window. Reported so an operator can sanity-check the staffing proxy.
    pub contributing_user_ids: Vec<i64>,
    /// Rows Odoo returned that could not be mapped (unparseable or missing
    /// `create_date`). Reported, never silently dropped to zero.
    pub skipped_rows: usize,
}

/// Mirror of the pilot contract's `TicketEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportTicketRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub ticket_id: String,
    pub created_at_utc: String,
    pub resolved_at_utc: Option<String>,
}

/// Mirror of the pilot contract's `DailyStaffing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportStaffingRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    pub day_utc: String,
    pub agents: u32,
    pub fixed_extra_capacity: u32,
}

/// Convert an Odoo naive-UTC datetime (`"2026-09-01 08:30:00"`) to RFC3339
/// `Z` form. Returns `None` for anything that is not that exact shape — a
/// guessed timestamp is worse than a dropped row.
pub fn odoo_datetime_to_rfc3339(value: &str) -> Option<String> {
    let trimmed = value.trim();
    // Some Odoo deployments already answer with a `T` separator; accept both,
    // reject everything else.
    let normalized = trimmed.replacen(' ', "T", 1);
    let parsed = chrono::NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%dT%H:%M:%S").ok()?;
    Some(
        parsed
            .and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// The other direction: RFC3339 → the naive-UTC string an Odoo domain wants.
pub fn rfc3339_to_odoo_datetime(value: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value.trim()).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}

/// Read a `false`-or-string Odoo field. Odoo returns `false` for unset values
/// of every type, so `null` and `false` both mean absent.
fn opt_str(row: &Value, key: &str) -> Option<String> {
    match row.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Read the id out of a many2one (`[id, "display name"]`) or `false`.
fn many2one_id(row: &Value, key: &str) -> Option<i64> {
    row.get(key)
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(Value::as_i64)
}

/// Read the first id out of a many2many id list (`user_ids`), or `None`.
fn first_many2many_id(row: &Value, key: &str) -> Option<i64> {
    row.get(key)
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(Value::as_i64)
}

/// Assignee id for the staffing proxy: `user_id` on helpdesk tickets, the
/// first entry of `user_ids` on project tasks.
pub fn assignee_id(row: &Value, model: SupportModel) -> Option<i64> {
    match model {
        SupportModel::HelpdeskTicket => many2one_id(row, "user_id"),
        SupportModel::ProjectTask => first_many2many_id(row, "user_ids"),
    }
}

/// The resolution timestamp for one row, in Odoo's own format, or `None` for
/// a still-open item. See the module doc for `project.task`'s two-field rule.
///
/// `folded_stage_ids` are the `stage_id`s an earlier `search_read` on
/// `project.task.type` reported as `fold = true`. An empty set means "no stage
/// is known to be a closing stage", in which case only `date_end` closes a
/// task — the fail-closed direction (an unknown stage never manufactures a
/// resolution).
pub fn resolution_datetime(
    row: &Value,
    model: SupportModel,
    folded_stage_ids: &BTreeSet<i64>,
) -> Option<String> {
    match model {
        SupportModel::HelpdeskTicket => opt_str(row, "close_date"),
        SupportModel::ProjectTask => {
            if let Some(end) = opt_str(row, "date_end") {
                return Some(end);
            }
            let stage = many2one_id(row, "stage_id")?;
            if !folded_stage_ids.contains(&stage) {
                return None;
            }
            opt_str(row, "date_last_stage_update")
        }
    }
}

/// Queue id minted for a model/queue pair.
pub fn queue_id(model: SupportModel, queue: i64) -> String {
    format!("{QUEUE_PREFIX}{}:{queue}", model.model_name())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SupportExportError {
    #[error("invalid support export request: {0}")]
    Invalid(&'static str),
    #[error("odoo call failed: {0}")]
    Rpc(String),
    #[error("model {0} is not available on this Odoo instance")]
    ModelUnavailable(String),
}

/// Per-day distinct-assignee counts — the staffing proxy, exposed on its own
/// so a caller (or a test) can inspect it without building a whole export.
///
/// `day_of` maps a ticket row to a day index, `None` for out-of-window.
pub fn daily_assignee_counts(
    rows: &[Value],
    model: SupportModel,
    horizon_days: usize,
    day_of: impl Fn(&str) -> Option<usize>,
) -> Vec<u32> {
    let mut per_day: Vec<BTreeSet<i64>> = vec![BTreeSet::new(); horizon_days];
    let folded = BTreeSet::new();
    for row in rows {
        let Some(user) = assignee_id(row, model) else {
            continue;
        };
        for stamp in [
            opt_str(row, "create_date"),
            resolution_datetime(row, model, &folded),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(day) = day_of(&stamp) {
                if day < horizon_days {
                    per_day[day].insert(user);
                }
            }
        }
    }
    per_day
        .into_iter()
        .map(|set| u32::try_from(set.len()).unwrap_or(u32::MAX))
        .collect()
}

/// Turn already-fetched Odoo rows into the queue extract. Pure: no clock, no
/// network — this is the whole mapping, and it is what the tests drive.
pub fn build_extract(
    rows: &[Value],
    model: SupportModel,
    queue: i64,
    window_start_utc: &str,
    horizon_days: usize,
    folded_stage_ids: &BTreeSet<i64>,
) -> Result<SupportQueueExtract, SupportExportError> {
    if horizon_days == 0 || horizon_days > 366 {
        return Err(SupportExportError::Invalid(
            "horizon must be 1..=366 complete days",
        ));
    }
    if rows.len() > MAX_TICKETS {
        return Err(SupportExportError::Invalid("too many rows"));
    }
    let start = chrono::DateTime::parse_from_rfc3339(window_start_utc)
        .map_err(|_| SupportExportError::Invalid("window start is not RFC3339"))?
        .with_timezone(&chrono::Utc);
    if start.timestamp() % 86_400 != 0 {
        return Err(SupportExportError::Invalid(
            "window start must be exactly UTC midnight",
        ));
    }
    let cutoff = start + chrono::Duration::days(horizon_days as i64);
    let qid = queue_id(model, queue);

    let day_index = |stamp: &str| -> Option<i64> {
        let rfc = odoo_datetime_to_rfc3339(stamp)?;
        let time = chrono::DateTime::parse_from_rfc3339(&rfc).ok()?;
        Some((time.timestamp() - start.timestamp()).div_euclid(86_400))
    };

    let mut tickets = Vec::new();
    let mut seen = BTreeSet::new();
    let mut users: BTreeSet<i64> = BTreeSet::new();
    let mut per_day: Vec<BTreeSet<i64>> = vec![BTreeSet::new(); horizon_days];
    let mut skipped = 0_usize;

    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_i64) else {
            skipped += 1;
            continue;
        };
        let Some(created_raw) = opt_str(row, "create_date") else {
            skipped += 1;
            continue;
        };
        let Some(created) = odoo_datetime_to_rfc3339(&created_raw) else {
            skipped += 1;
            continue;
        };
        let created_time = chrono::DateTime::parse_from_rfc3339(&created)
            .map_err(|_| SupportExportError::Invalid("converted creation time is invalid"))?;
        if created_time >= cutoff {
            continue;
        }
        let resolved_raw = resolution_datetime(row, model, folded_stage_ids);
        let resolved = resolved_raw
            .as_deref()
            .and_then(odoo_datetime_to_rfc3339)
            .filter(|rfc| {
                chrono::DateTime::parse_from_rfc3339(rfc).is_ok_and(|time| time < cutoff)
            });
        if let Some(rfc) = &resolved {
            let time = chrono::DateTime::parse_from_rfc3339(rfc)
                .map_err(|_| SupportExportError::Invalid("converted resolution is invalid"))?;
            if time < created_time {
                return Err(SupportExportError::Invalid(
                    "odoo row closes before it was created",
                ));
            }
        }
        let resolved_before_window = resolved.as_deref().is_some_and(|rfc| {
            chrono::DateTime::parse_from_rfc3339(rfc).is_ok_and(|time| time < start)
        });
        if created_time < start && resolved_before_window {
            continue;
        }
        let ticket_id = format!("{}-{id}", model.model_name());
        if !seen.insert(ticket_id.clone()) {
            return Err(SupportExportError::Invalid("duplicate odoo row id"));
        }
        if let Some(user) = assignee_id(row, model) {
            users.insert(user);
            for day in [
                day_index(&created_raw),
                resolved_raw.as_deref().and_then(day_index),
            ]
            .into_iter()
            .flatten()
            {
                if (0..horizon_days as i64).contains(&day) {
                    per_day[day as usize].insert(user);
                }
            }
        }
        tickets.push(SupportTicketRow {
            queue_id: Some(qid.clone()),
            ticket_id,
            created_at_utc: created,
            resolved_at_utc: resolved,
        });
    }

    let staffing = (0..horizon_days)
        .map(|day| {
            let date = start + chrono::Duration::days(day as i64);
            SupportStaffingRow {
                queue_id: Some(qid.clone()),
                day_utc: date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                // A quiet day still has someone on the rota; a staffing of
                // zero makes the simulator's capacity fit meaningless, and
                // "nobody worked" is not what an empty day proves.
                agents: u32::try_from(per_day[day].len()).unwrap_or(u32::MAX).max(1),
                fixed_extra_capacity: 0,
            }
        })
        .collect();

    Ok(SupportQueueExtract {
        queue_id: qid,
        window_start_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        data_cutoff_utc: cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        horizon_days,
        tickets,
        staffing,
        contributing_user_ids: users.into_iter().collect(),
        skipped_rows: skipped,
    })
}

/// Odoo search domain for one queue and window.
///
/// Reads everything created before the cutoff that is either still open or
/// was closed at/after the window start — exactly the rows that can produce an
/// arrival, a resolution or an opening-backlog cohort.
pub fn search_domain(
    model: SupportModel,
    queue: i64,
    since_odoo: &str,
    until_odoo: &str,
) -> Vec<Value> {
    let close_field = match model {
        SupportModel::HelpdeskTicket => "close_date",
        // `date_end` is the only closure field usable inside a domain;
        // the folded-stage fallback is resolved row-by-row afterwards, so the
        // domain is deliberately permissive here.
        SupportModel::ProjectTask => "date_end",
    };
    vec![
        json!("&"),
        json!("&"),
        json!([model.queue_field(), "=", queue]),
        json!(["create_date", "<", until_odoo]),
        json!("|"),
        json!([close_field, "=", false]),
        json!([close_field, ">=", since_odoo]),
    ]
}

/// Fetch every matching row, paging through `search_read`.
///
/// Uses `execute_kw` directly rather than [`OdooConnector::search_read`]
/// because the latter has no `offset` parameter; the kwargs are otherwise
/// identical (and still pass through the connector's company-scope injection).
pub async fn fetch_rows(
    connector: &OdooConnector,
    model: SupportModel,
    queue: i64,
    since_utc: &str,
    until_utc: &str,
) -> Result<Vec<Value>, SupportExportError> {
    let since = rfc3339_to_odoo_datetime(since_utc)
        .ok_or(SupportExportError::Invalid("since is not RFC3339"))?;
    let until = rfc3339_to_odoo_datetime(until_utc)
        .ok_or(SupportExportError::Invalid("until is not RFC3339"))?;
    let domain = search_domain(model, queue, &since, &until);
    let mut rows = Vec::new();
    let mut offset = 0_usize;
    loop {
        let page = connector
            .execute_kw(
                model.model_name(),
                "search_read",
                vec![json!(domain)],
                json!({
                    "fields": model.fields(),
                    "limit": PAGE_SIZE,
                    "offset": offset,
                    "order": "id asc",
                    "context": {"lang": "en_US"},
                }),
            )
            .await
            .map_err(SupportExportError::Rpc)?;
        let Some(page) = page.as_array() else {
            return Err(SupportExportError::Rpc(
                "search_read did not return a list".into(),
            ));
        };
        let len = page.len();
        rows.extend(page.iter().cloned());
        if rows.len() > MAX_TICKETS {
            return Err(SupportExportError::Invalid("too many rows"));
        }
        if len < PAGE_SIZE {
            break;
        }
        offset += PAGE_SIZE;
    }
    Ok(rows)
}

/// Stage ids that mean "done" on `project.task`. Empty for helpdesk (whose
/// `close_date` is exact) and empty when the lookup fails — the fail-closed
/// direction described in [`resolution_datetime`].
pub async fn folded_stage_ids(connector: &OdooConnector, model: SupportModel) -> BTreeSet<i64> {
    if model != SupportModel::ProjectTask {
        return BTreeSet::new();
    }
    match connector
        .search_read(
            "project.task.type",
            vec![json!(["fold", "=", true])],
            &["id"],
            500,
        )
        .await
    {
        Ok(Value::Array(rows)) => rows
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_i64))
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// Whether the requested model plausibly exists on this instance, from the
/// connector's already-detected module list.
///
/// `helpdesk` is an Enterprise module; `project` ships in Community. This is a
/// *pre-flight courtesy*, not a gate: when the edition probe never ran the
/// module set is empty, and this answers `true` so the RPC itself reports the
/// real error instead of this function inventing one. Nothing security-
/// relevant depends on it — the connector's own blocklist and per-agent
/// `allowed_models` filter are the actual access controls.
pub fn model_available(connector: &OdooConnector, model: SupportModel) -> bool {
    let modules = &connector.edition_gate.installed_modules;
    if modules.is_empty() {
        return true;
    }
    match model {
        SupportModel::ProjectTask => modules.contains("project"),
        SupportModel::HelpdeskTicket => modules.contains("helpdesk"),
    }
}

/// Convenience: fold count of tickets by day, used by the tests and by any
/// caller wanting a quick sanity number without rebuilding the pilot.
pub fn arrivals_by_day(extract: &SupportQueueExtract) -> Vec<u32> {
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(&extract.window_start_utc) else {
        return vec![0; extract.horizon_days];
    };
    let mut arrivals = vec![0_u32; extract.horizon_days];
    for ticket in &extract.tickets {
        if let Ok(time) = chrono::DateTime::parse_from_rfc3339(&ticket.created_at_utc) {
            let day = (time.timestamp() - start.timestamp()).div_euclid(86_400);
            if (0..extract.horizon_days as i64).contains(&day) {
                arrivals[day as usize] = arrivals[day as usize].saturating_add(1);
            }
        }
    }
    arrivals
}

/// Group the extract's staffing series into a map keyed by day, so a caller
/// can diff two extracts without re-deriving the mapping.
pub fn staffing_by_day(extract: &SupportQueueExtract) -> BTreeMap<String, u32> {
    extract
        .staffing
        .iter()
        .map(|row| (row.day_utc.clone(), row.agents))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "2026-09-01T00:00:00Z";

    fn helpdesk_row(id: i64, created: &str, closed: Option<&str>, user: Option<i64>) -> Value {
        json!({
            "id": id,
            "create_date": created,
            "close_date": closed.map(Value::from).unwrap_or(Value::Bool(false)),
            "team_id": [7, "Support"],
            "user_id": user.map(|u| json!([u, "Agent"])).unwrap_or(Value::Bool(false)),
        })
    }

    fn project_row(
        id: i64,
        created: &str,
        date_end: Option<&str>,
        stage: i64,
        stage_update: &str,
        users: Vec<i64>,
    ) -> Value {
        json!({
            "id": id,
            "create_date": created,
            "date_end": date_end.map(Value::from).unwrap_or(Value::Bool(false)),
            "date_last_stage_update": stage_update,
            "stage_id": [stage, "Stage"],
            "project_id": [3, "Support"],
            "user_ids": users,
        })
    }

    #[test]
    fn odoo_datetime_conversion_round_trips_and_rejects_garbage() {
        assert_eq!(
            odoo_datetime_to_rfc3339("2026-09-01 08:30:00").as_deref(),
            Some("2026-09-01T08:30:00Z")
        );
        // Already-T form is accepted too.
        assert_eq!(
            odoo_datetime_to_rfc3339("2026-09-01T08:30:00").as_deref(),
            Some("2026-09-01T08:30:00Z")
        );
        assert_eq!(odoo_datetime_to_rfc3339("not a date"), None);
        assert_eq!(odoo_datetime_to_rfc3339(""), None);
        assert_eq!(
            rfc3339_to_odoo_datetime("2026-09-01T08:30:00Z").as_deref(),
            Some("2026-09-01 08:30:00")
        );
        // A non-UTC offset is converted, never truncated.
        assert_eq!(
            rfc3339_to_odoo_datetime("2026-09-01T08:30:00+08:00").as_deref(),
            Some("2026-09-01 00:30:00")
        );
    }

    #[test]
    fn helpdesk_extract_maps_creation_and_closure() {
        let rows = vec![
            helpdesk_row(
                1,
                "2026-09-01 08:00:00",
                Some("2026-09-02 09:00:00"),
                Some(5),
            ),
            helpdesk_row(2, "2026-09-03 10:00:00", None, Some(6)),
        ];
        let extract = build_extract(
            &rows,
            SupportModel::HelpdeskTicket,
            7,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(extract.queue_id, "odoo:helpdesk.ticket:7");
        assert_eq!(extract.tickets.len(), 2);
        assert_eq!(extract.tickets[0].ticket_id, "helpdesk.ticket-1");
        assert_eq!(
            extract.tickets[0].resolved_at_utc.as_deref(),
            Some("2026-09-02T09:00:00Z")
        );
        assert!(extract.tickets[1].resolved_at_utc.is_none());
        assert_eq!(extract.staffing.len(), 14);
        assert_eq!(extract.contributing_user_ids, vec![5, 6]);
        assert_eq!(arrivals_by_day(&extract)[0], 1);
        assert_eq!(arrivals_by_day(&extract)[2], 1);
    }

    #[test]
    fn project_task_closes_on_date_end_first() {
        let rows = vec![project_row(
            11,
            "2026-09-01 08:00:00",
            Some("2026-09-02 08:00:00"),
            42,
            "2026-09-05 08:00:00",
            vec![9],
        )];
        let extract = build_extract(
            &rows,
            SupportModel::ProjectTask,
            3,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(
            extract.tickets[0].resolved_at_utc.as_deref(),
            Some("2026-09-02T08:00:00Z"),
            "date_end wins over the stage fallback"
        );
        assert_eq!(extract.queue_id, "odoo:project.task:3");
        assert_eq!(extract.tickets[0].ticket_id, "project.task-11");
    }

    #[test]
    fn project_task_stage_fallback_needs_a_folded_stage() {
        let rows = vec![project_row(
            12,
            "2026-09-01 08:00:00",
            None,
            42,
            "2026-09-04 08:00:00",
            vec![9],
        )];
        // Stage 42 is not known to be folded ⇒ the task is still open.
        let open = build_extract(
            &rows,
            SupportModel::ProjectTask,
            3,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(
            open.tickets[0].resolved_at_utc.is_none(),
            "an unfolded stage must never manufacture a resolution"
        );
        // Declare 42 folded and the same row closes.
        let folded = BTreeSet::from([42_i64]);
        let closed =
            build_extract(&rows, SupportModel::ProjectTask, 3, START, 14, &folded).unwrap();
        assert_eq!(
            closed.tickets[0].resolved_at_utc.as_deref(),
            Some("2026-09-04T08:00:00Z")
        );
    }

    #[test]
    fn rows_outside_the_window_are_handled_like_the_contract_expects() {
        let rows = vec![
            // Closed before the window entirely — dropped.
            helpdesk_row(
                1,
                "2026-08-20 08:00:00",
                Some("2026-08-21 08:00:00"),
                Some(5),
            ),
            // Open since before the window — kept, becomes initial backlog.
            helpdesk_row(2, "2026-08-25 08:00:00", None, Some(5)),
            // Created after the cutoff — dropped.
            helpdesk_row(3, "2026-09-20 08:00:00", None, Some(5)),
            // Closed after the cutoff — kept, reported as still open.
            helpdesk_row(
                4,
                "2026-09-02 08:00:00",
                Some("2026-09-20 08:00:00"),
                Some(5),
            ),
        ];
        let extract = build_extract(
            &rows,
            SupportModel::HelpdeskTicket,
            7,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        let ids: Vec<_> = extract
            .tickets
            .iter()
            .map(|t| t.ticket_id.as_str())
            .collect();
        assert_eq!(ids, vec!["helpdesk.ticket-2", "helpdesk.ticket-4"]);
        assert!(extract.tickets[1].resolved_at_utc.is_none());
    }

    #[test]
    fn empty_result_still_produces_a_complete_staffing_series() {
        let extract = build_extract(
            &[],
            SupportModel::HelpdeskTicket,
            7,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(extract.tickets.is_empty());
        assert_eq!(extract.staffing.len(), 14);
        assert!(extract.staffing.iter().all(|s| s.agents == 1));
        assert!(extract.contributing_user_ids.is_empty());
        assert_eq!(extract.skipped_rows, 0);
    }

    #[test]
    fn unmappable_rows_are_counted_not_hidden() {
        let rows = vec![
            json!({"id": 1, "create_date": false}),
            json!({"create_date": "2026-09-01 08:00:00"}),
            json!({"id": 3, "create_date": "garbage"}),
        ];
        let extract = build_extract(
            &rows,
            SupportModel::HelpdeskTicket,
            7,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(extract.tickets.is_empty());
        assert_eq!(extract.skipped_rows, 3);
    }

    #[test]
    fn closure_before_creation_is_an_error() {
        let rows = vec![helpdesk_row(
            1,
            "2026-09-05 08:00:00",
            Some("2026-09-02 08:00:00"),
            Some(5),
        )];
        assert_eq!(
            build_extract(
                &rows,
                SupportModel::HelpdeskTicket,
                7,
                START,
                14,
                &BTreeSet::new()
            ),
            Err(SupportExportError::Invalid(
                "odoo row closes before it was created"
            ))
        );
    }

    #[test]
    fn staffing_counts_distinct_assignees_per_day_with_a_floor_of_one() {
        let rows = vec![
            helpdesk_row(1, "2026-09-01 08:00:00", None, Some(5)),
            helpdesk_row(2, "2026-09-01 09:00:00", None, Some(6)),
            helpdesk_row(3, "2026-09-01 10:00:00", None, Some(5)),
        ];
        let extract = build_extract(
            &rows,
            SupportModel::HelpdeskTicket,
            7,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        let by_day = staffing_by_day(&extract);
        assert_eq!(by_day["2026-09-01T00:00:00Z"], 2, "5 and 6, not 3 rows");
        assert_eq!(
            by_day["2026-09-02T00:00:00Z"], 1,
            "quiet day keeps the floor"
        );
    }

    #[test]
    fn unassigned_rows_do_not_crash_the_staffing_proxy() {
        let rows = vec![helpdesk_row(1, "2026-09-01 08:00:00", None, None)];
        let extract = build_extract(
            &rows,
            SupportModel::HelpdeskTicket,
            7,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(extract.contributing_user_ids.is_empty());
        assert!(extract.staffing.iter().all(|s| s.agents == 1));
    }

    #[test]
    fn model_parsing_is_exact() {
        assert_eq!(
            SupportModel::parse("helpdesk.ticket"),
            Some(SupportModel::HelpdeskTicket)
        );
        assert_eq!(
            SupportModel::parse(" project.task "),
            Some(SupportModel::ProjectTask)
        );
        assert_eq!(SupportModel::parse("helpdesk.ticket.extra"), None);
        assert_eq!(SupportModel::parse("res.partner"), None);
    }

    #[test]
    fn search_domain_is_shaped_for_both_models() {
        let helpdesk = search_domain(
            SupportModel::HelpdeskTicket,
            7,
            "2026-09-01 00:00:00",
            "2026-09-15 00:00:00",
        );
        assert_eq!(helpdesk[2], json!(["team_id", "=", 7]));
        assert_eq!(helpdesk[5], json!(["close_date", "=", false]));
        let project = search_domain(
            SupportModel::ProjectTask,
            3,
            "2026-09-01 00:00:00",
            "2026-09-15 00:00:00",
        );
        assert_eq!(project[2], json!(["project_id", "=", 3]));
        assert_eq!(project[5], json!(["date_end", "=", false]));
    }

    #[test]
    fn requested_fields_never_include_ticket_content() {
        for model in [SupportModel::HelpdeskTicket, SupportModel::ProjectTask] {
            for field in model.fields() {
                assert!(
                    !["name", "description", "partner_id", "email_from"].contains(field),
                    "{field} would carry ticket content into a pilot source artifact"
                );
            }
        }
    }

    #[test]
    fn horizon_and_midnight_bounds_are_enforced() {
        assert!(
            build_extract(
                &[],
                SupportModel::HelpdeskTicket,
                7,
                START,
                0,
                &BTreeSet::new()
            )
            .is_err()
        );
        assert!(
            build_extract(
                &[],
                SupportModel::HelpdeskTicket,
                7,
                START,
                400,
                &BTreeSet::new()
            )
            .is_err()
        );
        assert_eq!(
            build_extract(
                &[],
                SupportModel::HelpdeskTicket,
                7,
                "2026-09-01T06:00:00Z",
                14,
                &BTreeSet::new()
            ),
            Err(SupportExportError::Invalid(
                "window start must be exactly UTC midnight"
            ))
        );
    }

    #[test]
    fn extract_json_uses_the_pilot_contract_field_names() {
        let rows = vec![helpdesk_row(1, "2026-09-01 08:00:00", None, Some(5))];
        let extract = build_extract(
            &rows,
            SupportModel::HelpdeskTicket,
            7,
            START,
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        let ticket = serde_json::to_value(&extract.tickets[0]).unwrap();
        assert!(ticket.get("ticket_id").is_some());
        assert!(ticket.get("created_at_utc").is_some());
        assert!(ticket.get("queue_id").is_some());
        // `resolved_at_utc` must be present-and-null, not omitted: the
        // contract type has no `default` on it.
        assert_eq!(ticket.get("resolved_at_utc"), Some(&Value::Null));
        let staffing = serde_json::to_value(&extract.staffing[0]).unwrap();
        for key in ["queue_id", "day_utc", "agents", "fixed_extra_capacity"] {
            assert!(staffing.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let rows = vec![
            helpdesk_row(1, "2026-09-01 08:00:00", None, Some(5)),
            helpdesk_row(1, "2026-09-02 08:00:00", None, Some(5)),
        ];
        assert_eq!(
            build_extract(
                &rows,
                SupportModel::HelpdeskTicket,
                7,
                START,
                14,
                &BTreeSet::new()
            ),
            Err(SupportExportError::Invalid("duplicate odoo row id"))
        );
    }
}
