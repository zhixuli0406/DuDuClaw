//! The DuDuClaw task board as a **real** data source for the decision twin.
//!
//! Until now every pilot the Decision Lab could replay was either synthetic
//! (`decision_synthetic.rs`) or an operator-uploaded file whose upstream
//! nobody could check. The 2026-09-29 feature audit made keeping the whole
//! decision line conditional on it acquiring at least one real feed; this
//! module is the first of them.
//!
//! ## The mapping, and what it approximates
//!
//! | Pilot contract field | Task board source | Honesty note |
//! | --- | --- | --- |
//! | `tickets[].ticket_id` | `tasks.id` | exact |
//! | `tickets[].created_at_utc` | `tasks.created_at` | exact (RFC3339, normalized to UTC here) |
//! | `tickets[].resolved_at_utc` | `tasks.completed_at` | `None` while the task is open — exact |
//! | `queue_id` | `task-board:<assigned_to>` / `task-board:all` | exact |
//! | `staffing[].agents` | `agent.toml [heartbeat] max_concurrent_runs` | **proxy value**, see below |
//! | `staffing[].fixed_extra_capacity` | always `0` | there is no such lane on the task board |
//!
//! `agents` is the one field that is NOT measured. A support queue's "agents"
//! means staffed human capacity for that day; the task board has no such
//! series. `max_concurrent_runs` is the closest structural analogue (how many
//! concurrent runs that agent is allowed), it is a *static* setting, and the
//! exporter repeats today's value across every historical day. Any conclusion
//! that depends on the staffing series varying over time is therefore not
//! supported by this source — which is exactly why the import is marked
//! exploratory and the receipt still carries its limitations list.
//!
//! ## `source_version_hashes` (deliberate deviation from the task sheet)
//!
//! `DecisionStore::import_operator_pilot` and the CLI `decision-import-pilot`
//! both **require** `source_version_hashes == [sha256(canonical (tickets,
//! staffing) JSON)]` — that digest is what binds the stored snapshot to the
//! `SourceArtifact` bytes, and a different value fails the import outright. So
//! the canonical digest goes there, and the provenance the task sheet asked
//! for (the `tasks.db` file digest plus this exporter's version) is carried in
//! `source_lineage`, which is the field the contract reserves for exactly that.
//! See [`TaskBoardExport::source_lineage`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::decision_ingest::{DailyStaffing, SupportPilotExport, TicketEvent};

/// Bumped whenever the mapping above changes, so a stored lineage string says
/// which exporter produced it.
pub const EXPORTER_VERSION: &str = "task-board-export-v1";

/// Queue id for the whole board (every agent's tasks in one queue).
pub const ALL_QUEUE_AGENT: &str = "all";

/// Prefix of every queue id this exporter mints. Callers compare with
/// `starts_with` on purpose — the segment after it is the agent id.
pub const QUEUE_PREFIX: &str = "task-board:";

/// Lineage prefix stamped on every import that came from this exporter. The
/// dashboard uses it to tell a real task-board pilot from a synthetic demo.
pub const LINEAGE_PREFIX: &str = "task-board-export@";

/// Hard ceiling on a single export, mirroring `decision_ingest`'s own caps.
const MAX_TICKETS: usize = 200_000;
const MAX_HORIZON_DAYS: usize = 366;
/// Fallback when an agent has no readable `[heartbeat] max_concurrent_runs`.
const DEFAULT_AGENT_CAPACITY: u32 = 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskBoardExportError {
    #[error("task board database is unreadable: {0}")]
    Db(String),
    #[error("invalid task board export request: {0}")]
    Invalid(&'static str),
    #[error("task board export overflowed")]
    Overflow,
}

/// Which slice of the board to export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskBoardQueue {
    /// One agent's tasks (`task-board:<agent_id>`).
    Agent(String),
    /// Every agent's tasks in one queue (`task-board:all`). Staffing is the
    /// sum of each contributing agent's capacity.
    All,
}

impl TaskBoardQueue {
    /// The exact queue id stamped on every ticket and staffing row.
    pub fn queue_id(&self) -> String {
        match self {
            Self::Agent(agent) => format!("{QUEUE_PREFIX}{agent}"),
            Self::All => format!("{QUEUE_PREFIX}{ALL_QUEUE_AGENT}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskBoardExportOptions {
    pub queue: TaskBoardQueue,
    /// Number of complete UTC days the window covers.
    pub horizon_days: usize,
    /// Inclusive UTC-midnight start of the window. `None` ⇒ the window ends
    /// at the most recent complete UTC midnight.
    pub window_start_utc: Option<DateTime<Utc>>,
    pub seed: u64,
}

impl Default for TaskBoardExportOptions {
    fn default() -> Self {
        Self {
            queue: TaskBoardQueue::All,
            horizon_days: 14,
            window_start_utc: None,
            seed: 0,
        }
    }
}

/// The export plus the provenance strings the import step needs.
#[derive(Debug, Clone)]
pub struct TaskBoardExport {
    pub export: SupportPilotExport,
    pub queue_id: String,
    /// `task-board-export@<sha256 of the tasks.db file>`, truncated to the
    /// contract's 128-character selector limit. This is the provenance field:
    /// `source_version_hashes` is reserved for the canonical content digest.
    pub source_lineage: String,
    /// Digest of the `tasks.db` file the rows were read from.
    pub tasks_db_sha256: String,
    /// Distinct agent ids that contributed a ticket or a capacity row.
    pub contributing_agents: Vec<String>,
}

/// One row read out of `tasks`. Kept separate from the exporter so the pure
/// transform below is unit-testable without SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRowForExport {
    pub id: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub assigned_to: String,
}

fn utc_midnight(time: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(time.year(), time.month(), time.day(), 0, 0, 0)
        .single()
        .unwrap_or(time)
}

/// Parse an RFC3339 timestamp and normalize it to UTC.
///
/// The task board writes `chrono::Utc::now().to_rfc3339()`, but a row could
/// have been written by an older build or hand-edited, so an offset timestamp
/// is converted rather than rejected — unlike the pilot contract, which
/// refuses any non-zero offset. The serialized output is always `Z`-form UTC.
fn parse_to_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn rfc3339_utc(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Read the rows this exporter needs out of an open `tasks.db`.
///
/// Deliberately a plain `SELECT` over the four columns the mapping uses:
/// nothing else from the task board reaches the decision store, so a task
/// title or description can never leak into a pilot source artifact.
pub fn read_task_rows(
    conn: &Connection,
    queue: &TaskBoardQueue,
) -> Result<Vec<TaskRowForExport>, TaskBoardExportError> {
    let sql = match queue {
        TaskBoardQueue::Agent(_) => {
            "SELECT id, created_at, completed_at, assigned_to FROM tasks \
             WHERE assigned_to = ?1 ORDER BY created_at, id"
        }
        TaskBoardQueue::All => {
            "SELECT id, created_at, completed_at, assigned_to FROM tasks ORDER BY created_at, id"
        }
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| TaskBoardExportError::Db(e.to_string()))?;
    let map = |row: &rusqlite::Row<'_>| {
        Ok(TaskRowForExport {
            id: row.get(0)?,
            created_at: row.get(1)?,
            completed_at: row.get(2)?,
            assigned_to: row.get(3)?,
        })
    };
    let rows = match queue {
        TaskBoardQueue::Agent(agent) => stmt
            .query_map([agent.as_str()], map)
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>()),
        TaskBoardQueue::All => stmt
            .query_map([], map)
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>()),
    };
    rows.map_err(|e| TaskBoardExportError::Db(e.to_string()))
}

/// Per-agent daily capacity, the proxy documented at the top of this module.
///
/// Reads `<home>/agents/<id>/agent.toml` `[heartbeat] max_concurrent_runs`
/// leniently (a malformed file, a missing section or a non-integer value all
/// resolve to [`DEFAULT_AGENT_CAPACITY`]) — the same convention every other
/// isolated `config.toml`/`agent.toml` read in this crate follows, so one
/// broken agent file can never fail a whole export.
pub fn agent_daily_capacity(home_dir: &Path, agent_id: &str) -> u32 {
    let path = crate::outcome_spec::agent_work_dir(home_dir, agent_id).join("agent.toml");
    let Ok(content) = std::fs::read_to_string(path) else {
        return DEFAULT_AGENT_CAPACITY;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return DEFAULT_AGENT_CAPACITY;
    };
    table
        .get("heartbeat")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("max_concurrent_runs"))
        .and_then(|v| v.as_integer())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_AGENT_CAPACITY)
}

/// Canonical digest of the source bytes, computed exactly the way
/// `DecisionStore::import_operator_pilot` recomputes it. Any drift here fails
/// the import, which is the intended fail-closed behaviour.
pub fn canonical_source_sha256(
    tickets: &[TicketEvent],
    staffing: &[DailyStaffing],
) -> Result<String, TaskBoardExportError> {
    let bytes = serde_json::to_vec(&(tickets, staffing))
        .map_err(|_| TaskBoardExportError::Invalid("source rows are not serializable"))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Build the pilot export from already-read rows. Pure: no clock, no
/// filesystem, no database — `now` and the capacity table are arguments so the
/// whole mapping is testable.
///
/// `capacity_by_agent` supplies the per-agent proxy capacity; an agent absent
/// from the map contributes [`DEFAULT_AGENT_CAPACITY`].
pub fn build_export(
    rows: &[TaskRowForExport],
    options: &TaskBoardExportOptions,
    capacity_by_agent: &BTreeMap<String, u32>,
    now: DateTime<Utc>,
) -> Result<(SupportPilotExport, Vec<String>), TaskBoardExportError> {
    if options.horizon_days == 0 || options.horizon_days > MAX_HORIZON_DAYS {
        return Err(TaskBoardExportError::Invalid(
            "horizon must be 1..=366 complete days",
        ));
    }
    if rows.len() > MAX_TICKETS {
        return Err(TaskBoardExportError::Invalid("too many task rows"));
    }
    let queue_id = options.queue.queue_id();

    // The window must be complete, so it can never include the day in
    // progress: `data_cutoff_utc` has to cover the whole horizon, and a
    // partially-elapsed final day would report a fake drop in arrivals.
    let today_midnight = utc_midnight(now);
    let start = match options.window_start_utc {
        Some(explicit) => {
            let normalized = utc_midnight(explicit);
            if normalized != explicit {
                return Err(TaskBoardExportError::Invalid(
                    "window start must be exactly UTC midnight",
                ));
            }
            normalized
        }
        None => today_midnight
            .checked_sub_signed(Duration::days(options.horizon_days as i64))
            .ok_or(TaskBoardExportError::Overflow)?,
    };
    let cutoff = start
        .checked_add_signed(Duration::days(options.horizon_days as i64))
        .ok_or(TaskBoardExportError::Overflow)?;
    if cutoff > now {
        return Err(TaskBoardExportError::Invalid(
            "window must end at or before the current time",
        ));
    }

    let mut tickets = Vec::new();
    let mut seen_ids = BTreeSet::new();
    let mut contributing: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let Some(created) = parse_to_utc(&row.created_at) else {
            // A row whose creation time cannot be read has no place on any
            // day bucket. Dropping it is the honest direction: the ticket is
            // simply absent rather than silently dated "now".
            continue;
        };
        // Everything at or after the cutoff belongs to a later window.
        if created >= cutoff {
            continue;
        }
        let resolved = row
            .completed_at
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .and_then(parse_to_utc);
        // A resolution after the cutoff is *not yet visible* in this window:
        // report the ticket as still open rather than inventing a later day.
        let resolved = resolved.filter(|time| *time < cutoff);
        if let Some(time) = resolved {
            if time < created {
                // A completion before creation violates the pilot contract;
                // the import would reject the whole export. Treat the broken
                // pair as "still open" instead of failing the run.
                return Err(TaskBoardExportError::Invalid(
                    "task completed_at precedes created_at",
                ));
            }
        }
        // A ticket entirely before the window that is also resolved before it
        // contributes neither arrivals nor initial backlog.
        if created < start && resolved.is_some_and(|time| time < start) {
            continue;
        }
        if row.id.trim().is_empty() || !seen_ids.insert(row.id.clone()) {
            return Err(TaskBoardExportError::Invalid("duplicate or empty task id"));
        }
        if !row.assigned_to.trim().is_empty() {
            contributing.insert(row.assigned_to.clone());
        }
        tickets.push(TicketEvent {
            queue_id: Some(queue_id.clone()),
            ticket_id: row.id.clone(),
            created_at_utc: rfc3339_utc(created),
            resolved_at_utc: resolved.map(rfc3339_utc),
        });
    }

    // Capacity. For a single-agent queue it is that agent's proxy value; for
    // the whole board it is the sum over every agent that contributed a row,
    // so an empty board reports the floor of 1 rather than 0 (a staffing of
    // zero makes the simulator's capacity fit meaningless).
    let daily_agents: u32 = match &options.queue {
        TaskBoardQueue::Agent(agent) => capacity_by_agent
            .get(agent)
            .copied()
            .unwrap_or(DEFAULT_AGENT_CAPACITY),
        TaskBoardQueue::All => {
            let sum = contributing
                .iter()
                .map(|agent| {
                    capacity_by_agent
                        .get(agent)
                        .copied()
                        .unwrap_or(DEFAULT_AGENT_CAPACITY)
                })
                .try_fold(0_u32, |acc, v| acc.checked_add(v))
                .ok_or(TaskBoardExportError::Overflow)?;
            sum.max(DEFAULT_AGENT_CAPACITY)
        }
    };

    let mut staffing = Vec::with_capacity(options.horizon_days);
    for day in 0..options.horizon_days {
        let date = start
            .checked_add_signed(Duration::days(day as i64))
            .ok_or(TaskBoardExportError::Overflow)?;
        staffing.push(DailyStaffing {
            queue_id: Some(queue_id.clone()),
            day_utc: rfc3339_utc(date),
            agents: daily_agents,
            fixed_extra_capacity: 0,
        });
    }

    let source_sha256 = canonical_source_sha256(&tickets, &staffing)?;
    let export = SupportPilotExport {
        snapshot_id: format!(
            "task-board-{}-{}",
            match &options.queue {
                TaskBoardQueue::Agent(agent) => agent.clone(),
                TaskBoardQueue::All => ALL_QUEUE_AGENT.to_string(),
            },
            start.format("%Y%m%d")
        ),
        baseline_scenario_id: format!("task-board-baseline-{}", start.format("%Y%m%d")),
        window_start_utc: rfc3339_utc(start),
        data_cutoff_utc: rfc3339_utc(cutoff),
        source_version_hashes: vec![source_sha256],
        seed: options.seed,
        horizon_days: options.horizon_days,
        tickets,
        staffing,
    };
    Ok((export, contributing.into_iter().collect()))
}

/// SHA-256 of a file's bytes, streamed so a large `tasks.db` never lands in
/// memory whole.
pub fn file_sha256(path: &Path) -> Result<String, TaskBoardExportError> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).map_err(|e| TaskBoardExportError::Db(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|e| TaskBoardExportError::Db(e.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Full export: open `tasks.db`, read rows, resolve per-agent capacity from
/// `<home>/agents/*/agent.toml`, and build the contract-legal export.
pub fn export_from_home(
    home_dir: &Path,
    options: &TaskBoardExportOptions,
) -> Result<TaskBoardExport, TaskBoardExportError> {
    export_from_db(home_dir, &home_dir.join("tasks.db"), options)
}

/// Same as [`export_from_home`] with an explicit database path, for the CLI
/// (`--db`) and for tests.
pub fn export_from_db(
    home_dir: &Path,
    db_path: &Path,
    options: &TaskBoardExportOptions,
) -> Result<TaskBoardExport, TaskBoardExportError> {
    if !db_path.is_file() {
        return Err(TaskBoardExportError::Db(
            "task board database does not exist".into(),
        ));
    }
    // Read-only on purpose: the exporter must never migrate or write the
    // live task board.
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| TaskBoardExportError::Db(e.to_string()))?;
    let rows = read_task_rows(&conn, &options.queue)?;
    drop(conn);

    let mut capacity = BTreeMap::new();
    for row in &rows {
        if !row.assigned_to.trim().is_empty() && !capacity.contains_key(&row.assigned_to) {
            capacity.insert(
                row.assigned_to.clone(),
                agent_daily_capacity(home_dir, &row.assigned_to),
            );
        }
    }
    if let TaskBoardQueue::Agent(agent) = &options.queue {
        capacity
            .entry(agent.clone())
            .or_insert_with(|| agent_daily_capacity(home_dir, agent));
    }

    let (export, contributing_agents) = build_export(&rows, options, &capacity, Utc::now())?;
    let tasks_db_sha256 = file_sha256(db_path)?;
    Ok(TaskBoardExport {
        queue_id: options.queue.queue_id(),
        source_lineage: format!("{LINEAGE_PREFIX}{tasks_db_sha256}"),
        tasks_db_sha256,
        export,
        contributing_agents,
    })
}

/// Whether a stored pilot's `source_lineage` came from this exporter.
///
/// Anchored `starts_with` on the full `task-board-export@` prefix — the
/// dashboard renders "real data" on the strength of this, so a lineage such as
/// `not-task-board-export@…` must not match (CLAUDE.md coding convention 2).
pub fn is_task_board_lineage(lineage: &str) -> bool {
    lineage.starts_with(LINEAGE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap() + Duration::days(n)
    }

    fn row(id: &str, created: DateTime<Utc>, resolved: Option<DateTime<Utc>>) -> TaskRowForExport {
        TaskRowForExport {
            id: id.into(),
            created_at: created.to_rfc3339(),
            completed_at: resolved.map(|t| t.to_rfc3339()),
            assigned_to: "alice".into(),
        }
    }

    fn opts(horizon: usize) -> TaskBoardExportOptions {
        TaskBoardExportOptions {
            queue: TaskBoardQueue::Agent("alice".into()),
            horizon_days: horizon,
            window_start_utc: Some(day(0)),
            seed: 0,
        }
    }

    fn caps() -> BTreeMap<String, u32> {
        BTreeMap::from([("alice".to_string(), 2_u32)])
    }

    #[test]
    fn export_is_contract_legal_and_round_trips_through_deny_unknown_fields() {
        let rows = vec![
            row("t1", day(0) + Duration::hours(3), Some(day(1))),
            row("t2", day(2) + Duration::hours(1), None),
        ];
        let (export, agents) = build_export(&rows, &opts(14), &caps(), day(20)).unwrap();
        assert_eq!(agents, vec!["alice".to_string()]);
        // `deny_unknown_fields` round trip: serialize then parse back.
        let json = serde_json::to_string(&export).unwrap();
        let back: SupportPilotExport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tickets.len(), 2);
        assert_eq!(back.staffing.len(), 14);
        // And the decision-twin importer accepts it.
        let pilot = crate::decision_ingest::build_support_pilot(&export)
            .expect("task board export must satisfy the pilot contract");
        assert_eq!(pilot.snapshot.queue_id.as_deref(), Some("task-board:alice"));
        assert_eq!(pilot.baseline.agents_by_day, vec![2_u32; 14]);
        assert!(
            pilot
                .baseline
                .fixed_extra_capacity_by_day
                .iter()
                .all(|v| *v == 0)
        );
    }

    #[test]
    fn source_version_hash_matches_the_importers_canonical_digest() {
        let rows = vec![row("t1", day(1), Some(day(2)))];
        let (export, _) = build_export(&rows, &opts(14), &caps(), day(20)).unwrap();
        let expected = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&export.tickets, &export.staffing)).unwrap())
        );
        assert_eq!(
            export.source_version_hashes,
            vec![expected],
            "import_operator_pilot recomputes exactly this digest"
        );
    }

    #[test]
    fn empty_board_still_produces_a_complete_staffing_series() {
        let (export, agents) = build_export(&[], &opts(14), &caps(), day(20)).unwrap();
        assert!(export.tickets.is_empty());
        assert_eq!(export.staffing.len(), 14, "one row per horizon day");
        assert!(agents.is_empty());
        crate::decision_ingest::build_support_pilot(&export)
            .expect("an empty board is a valid zero-arrival pilot");
    }

    #[test]
    fn all_open_tasks_become_unresolved_tickets_and_growing_backlog() {
        let rows: Vec<_> = (0..5)
            .map(|i| row(&format!("t{i}"), day(i) + Duration::hours(2), None))
            .collect();
        let (export, _) = build_export(&rows, &opts(14), &caps(), day(20)).unwrap();
        assert!(export.tickets.iter().all(|t| t.resolved_at_utc.is_none()));
        let pilot = crate::decision_ingest::build_support_pilot(&export).unwrap();
        assert_eq!(pilot.observed_days.last().unwrap().backlog_end, 5);
        assert!(pilot.observed_days.iter().all(|d| d.resolved == 0));
    }

    #[test]
    fn task_created_before_the_window_and_still_open_becomes_initial_backlog() {
        let rows = vec![row("old", day(-3) + Duration::hours(5), None)];
        let (export, _) = build_export(&rows, &opts(14), &caps(), day(20)).unwrap();
        let pilot = crate::decision_ingest::build_support_pilot(&export).unwrap();
        assert_eq!(pilot.snapshot.initial_backlog.len(), 1);
        assert_eq!(pilot.snapshot.initial_backlog[0].age_days, 3);
        assert_eq!(pilot.observed_days[0].backlog_start, 1);
    }

    #[test]
    fn task_fully_resolved_before_the_window_is_dropped() {
        let rows = vec![row("gone", day(-5), Some(day(-4)))];
        let (export, _) = build_export(&rows, &opts(14), &caps(), day(20)).unwrap();
        assert!(export.tickets.is_empty(), "closed before the window starts");
    }

    #[test]
    fn resolution_after_the_cutoff_reads_as_still_open() {
        // Cutoff is day 14; a task closed on day 20 must NOT be dated inside
        // the window, and must not be reported as resolved either.
        let rows = vec![row("late", day(2), Some(day(20)))];
        let (export, _) = build_export(&rows, &opts(14), &caps(), day(25)).unwrap();
        assert_eq!(export.tickets.len(), 1);
        assert!(export.tickets[0].resolved_at_utc.is_none());
    }

    #[test]
    fn creation_at_the_cutoff_boundary_belongs_to_the_next_window() {
        let rows = vec![
            row("inside", day(13) + Duration::hours(23), None),
            row("outside", day(14), None),
        ];
        let (export, _) = build_export(&rows, &opts(14), &caps(), day(25)).unwrap();
        let ids: Vec<_> = export
            .tickets
            .iter()
            .map(|t| t.ticket_id.as_str())
            .collect();
        assert_eq!(ids, vec!["inside"]);
    }

    #[test]
    fn incomplete_window_is_refused_rather_than_reporting_a_fake_quiet_day() {
        let mut options = opts(14);
        options.window_start_utc = Some(day(0));
        let err = build_export(&[], &options, &caps(), day(13) + Duration::hours(12)).unwrap_err();
        assert_eq!(
            err,
            TaskBoardExportError::Invalid("window must end at or before the current time")
        );
    }

    #[test]
    fn non_midnight_window_start_is_refused() {
        let mut options = opts(14);
        options.window_start_utc = Some(day(0) + Duration::hours(6));
        let err = build_export(&[], &options, &caps(), day(30)).unwrap_err();
        assert_eq!(
            err,
            TaskBoardExportError::Invalid("window start must be exactly UTC midnight")
        );
    }

    #[test]
    fn completed_before_created_is_an_error_not_a_silent_repair() {
        let rows = vec![row("broken", day(3), Some(day(2)))];
        let err = build_export(&rows, &opts(14), &caps(), day(20)).unwrap_err();
        assert_eq!(
            err,
            TaskBoardExportError::Invalid("task completed_at precedes created_at")
        );
    }

    #[test]
    fn unparseable_created_at_drops_the_row_instead_of_dating_it_now() {
        let rows = vec![TaskRowForExport {
            id: "junk".into(),
            created_at: "not a timestamp".into(),
            completed_at: None,
            assigned_to: "alice".into(),
        }];
        let (export, _) = build_export(&rows, &opts(14), &caps(), day(20)).unwrap();
        assert!(export.tickets.is_empty());
    }

    #[test]
    fn all_queue_sums_contributing_agent_capacity() {
        let rows = vec![
            TaskRowForExport {
                id: "a1".into(),
                created_at: day(1).to_rfc3339(),
                completed_at: None,
                assigned_to: "alice".into(),
            },
            TaskRowForExport {
                id: "b1".into(),
                created_at: day(2).to_rfc3339(),
                completed_at: None,
                assigned_to: "bob".into(),
            },
        ];
        let options = TaskBoardExportOptions {
            queue: TaskBoardQueue::All,
            horizon_days: 14,
            window_start_utc: Some(day(0)),
            seed: 0,
        };
        let capacity = BTreeMap::from([("alice".to_string(), 2_u32), ("bob".to_string(), 3_u32)]);
        let (export, agents) = build_export(&rows, &options, &capacity, day(20)).unwrap();
        assert_eq!(agents, vec!["alice".to_string(), "bob".to_string()]);
        assert!(export.staffing.iter().all(|s| s.agents == 5));
        assert_eq!(
            export.staffing[0].queue_id.as_deref(),
            Some("task-board:all")
        );
    }

    #[test]
    fn unknown_agent_falls_back_to_capacity_one() {
        let rows = vec![TaskRowForExport {
            id: "x".into(),
            created_at: day(1).to_rfc3339(),
            completed_at: None,
            assigned_to: "nobody".into(),
        }];
        let options = TaskBoardExportOptions {
            queue: TaskBoardQueue::All,
            horizon_days: 14,
            window_start_utc: Some(day(0)),
            seed: 0,
        };
        let (export, _) = build_export(&rows, &options, &BTreeMap::new(), day(20)).unwrap();
        assert!(export.staffing.iter().all(|s| s.agents == 1));
    }

    #[test]
    fn default_window_ends_at_the_last_complete_midnight() {
        let mut options = opts(7);
        options.window_start_utc = None;
        let now = day(10) + Duration::hours(9);
        let (export, _) = build_export(&[], &options, &caps(), now).unwrap();
        assert_eq!(export.window_start_utc, rfc3339_utc(day(3)));
        assert_eq!(export.data_cutoff_utc, rfc3339_utc(day(10)));
    }

    #[test]
    fn lineage_prefix_match_is_anchored() {
        assert!(is_task_board_lineage("task-board-export@abc"));
        assert!(!is_task_board_lineage("not-task-board-export@abc"));
        assert!(!is_task_board_lineage("synthetic-support"));
    }

    #[test]
    fn horizon_bounds_are_enforced() {
        let mut options = opts(0);
        assert!(build_export(&[], &options, &caps(), day(20)).is_err());
        options.horizon_days = 400;
        assert!(build_export(&[], &options, &caps(), day(20)).is_err());
    }

    #[test]
    fn agent_capacity_reads_heartbeat_max_concurrent_runs() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "[heartbeat]\nenabled = true\nmax_concurrent_runs = 4\n",
        )
        .unwrap();
        assert_eq!(agent_daily_capacity(home.path(), "worker"), 4);
        // Missing file, malformed file and a zero value all fall back to 1.
        assert_eq!(agent_daily_capacity(home.path(), "absent"), 1);
        let broken = home.path().join("agents").join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("agent.toml"), "not = [toml").unwrap();
        assert_eq!(agent_daily_capacity(home.path(), "broken"), 1);
        let zero = home.path().join("agents").join("zero");
        std::fs::create_dir_all(&zero).unwrap();
        std::fs::write(
            zero.join("agent.toml"),
            "[heartbeat]\nmax_concurrent_runs = 0\n",
        )
        .unwrap();
        assert_eq!(agent_daily_capacity(home.path(), "zero"), 1);
    }

    #[test]
    fn export_from_db_reads_a_live_task_store() {
        let home = tempfile::tempdir().unwrap();
        let store = crate::task_store::TaskStore::open(home.path()).unwrap();
        drop(store);
        let db = home.path().join("tasks.db");
        // Insert two rows straight into the schema the store just created.
        {
            let conn = Connection::open(&db).unwrap();
            let created = (Utc::now() - Duration::days(3)).to_rfc3339();
            let done = (Utc::now() - Duration::days(2)).to_rfc3339();
            conn.execute(
                "INSERT INTO tasks (id,title,description,status,priority,assigned_to,created_by,created_at,updated_at,completed_at)
                 VALUES ('t1','a','',  'done','medium','alice','system',?1,?1,?2)",
                rusqlite::params![created, done],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks (id,title,description,status,priority,assigned_to,created_by,created_at,updated_at)
                 VALUES ('t2','b','','todo','medium','alice','system',?1,?1)",
                rusqlite::params![created],
            )
            .unwrap();
        }
        let out = export_from_db(
            home.path(),
            &db,
            &TaskBoardExportOptions {
                queue: TaskBoardQueue::Agent("alice".into()),
                horizon_days: 7,
                window_start_utc: None,
                seed: 0,
            },
        )
        .unwrap();
        assert_eq!(out.queue_id, "task-board:alice");
        assert!(is_task_board_lineage(&out.source_lineage));
        assert_eq!(out.source_lineage.len(), LINEAGE_PREFIX.len() + 64);
        assert_eq!(out.export.tickets.len(), 2);
        assert_eq!(out.contributing_agents, vec!["alice".to_string()]);
        crate::decision_ingest::build_support_pilot(&out.export).unwrap();
    }

    #[test]
    fn export_from_db_refuses_a_missing_database() {
        let home = tempfile::tempdir().unwrap();
        let err = export_from_db(
            home.path(),
            &home.path().join("nope.db"),
            &TaskBoardExportOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, TaskBoardExportError::Db(_)));
    }
}
