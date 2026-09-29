//! Periodic task-board → decision-twin pilot, **default off**.
//!
//! One period does three things and nothing else:
//!
//! 1. `decision_task_board_export` builds a contract-legal
//!    [`SupportPilotExport`] from the live `tasks.db` for the last complete
//!    `horizon_days` UTC days.
//! 2. That export is fed to the **existing** operator-import path
//!    ([`DecisionStore::import_operator_pilot`]), so the receipt, the source
//!    artifact binding, the replay hashes and the held-out SLA diagnostic all
//!    come from code that was already reviewed — nothing is re-implemented here.
//! 3. A prospective SLA nowcast for the *next* (still in progress) day is
//!    computed from the imported observed prefix with
//!    [`crate::decision_ingest::forecast_next_day_sla`].
//!
//! ## Honest scope of step 3
//!
//! The nowcast is computed and recorded in this module's own run ledger; it is
//! **not** committed as a scored `shadow_sla_forecast` row. That store demands
//! an operator-created shadow *policy*, a prior `shadow_forecast`, and an
//! opening-stock source artifact ingested after the target midnight — a
//! pre-registration chain a background job must not fabricate on an operator's
//! behalf. Scoring this line against outcomes therefore still goes through the
//! `/api/decision/shadow-sla-*` endpoints, with a human choosing the policy.
//! Saying otherwise would be the exact "passing a gate means promoted" claim
//! the three specs forbid.
//!
//! Everything fails open: a bad period logs, writes an audit row and leaves
//! the gateway untouched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::decision_calibration::{KnownDayInputs, ObservedSupportDay};
use crate::decision_ingest::{
    KnownSlaDayInputs, ProspectiveSlaForecast, build_support_pilot, forecast_next_day_sla,
};
use crate::decision_operator_import::{OperatorPilotImportReceipt, OperatorPilotImportRequest};
use crate::decision_sim::{InitialCohort, QueueModel, StaffingScenario};
use crate::decision_store::DecisionStore;
use crate::decision_task_board_export::{TaskBoardExportOptions, TaskBoardQueue, export_from_home};

/// Tenant every task-board pilot is scoped to. The task board is local by
/// definition; there is no multi-tenant task store to disambiguate.
pub const TENANT_ID: &str = "local";
/// ACL of every task-board pilot. Distinct from the operator-upload ACL so an
/// operator can tell machine-exported pilots from uploaded ones at a glance.
pub const ACL: &str = "task-board";
/// Model version minted per period. The capacity number is fitted from the
/// window, so the version has to name the window it was fitted on.
const MODEL_PREFIX: &str = "task-board-fit-v1";
/// Minimum saturated days the capacity fit needs before it means anything.
const MIN_SATURATED_DAYS: usize = 1;
/// SLA definition for the task board, in whole days. Documented, not measured:
/// "a task closed the same or next UTC day" is the only SLA the board's own
/// columns can express.
const SLA_DAYS: u32 = 2;
/// Placeholder staffing cost. The task board has no payroll, so the cost axis
/// of every task-board comparison is a constant and carries no information.
const STAFF_COST_CENTS_PER_AGENT_DAY: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBoardShadowConfig {
    /// `config.toml [decision] task_board_shadow` — **default false**.
    pub enabled: bool,
    /// `task_board_shadow_every_hours` — default 24, floor 1.
    pub every_hours: u64,
    /// `task_board_retention_days` — default 90, floor 1.
    pub retention_days: i64,
    /// `task_board_horizon_days` — default 14 (the SLA holdout's own floor).
    pub horizon_days: usize,
    /// `task_board_queue` — an agent id, or `all` for the whole board.
    pub queue: String,
}

impl Default for TaskBoardShadowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            every_hours: 24,
            retention_days: 90,
            horizon_days: 14,
            queue: crate::decision_task_board_export::ALL_QUEUE_AGENT.to_string(),
        }
    }
}

impl TaskBoardShadowConfig {
    pub fn from_home(home_dir: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        Self::from_table(&table)
    }

    pub fn from_table(table: &toml::Table) -> Self {
        let defaults = Self::default();
        let Some(section) = table.get("decision").and_then(|v| v.as_table()) else {
            return defaults;
        };
        Self {
            enabled: section
                .get("task_board_shadow")
                .and_then(|v| v.as_bool())
                .unwrap_or(defaults.enabled),
            every_hours: section
                .get("task_board_shadow_every_hours")
                .and_then(|v| v.as_integer())
                .and_then(|v| u64::try_from(v).ok())
                .map(|v| v.max(1))
                .unwrap_or(defaults.every_hours),
            retention_days: section
                .get("task_board_retention_days")
                .and_then(|v| v.as_integer())
                .filter(|v| *v > 0)
                .unwrap_or(defaults.retention_days),
            horizon_days: section
                .get("task_board_horizon_days")
                .and_then(|v| v.as_integer())
                .and_then(|v| usize::try_from(v).ok())
                .filter(|v| (1..=366).contains(v))
                .unwrap_or(defaults.horizon_days),
            queue: section
                .get("task_board_queue")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .unwrap_or(defaults.queue),
        }
    }

    pub fn queue(&self) -> TaskBoardQueue {
        if self.queue == crate::decision_task_board_export::ALL_QUEUE_AGENT {
            TaskBoardQueue::All
        } else {
            TaskBoardQueue::Agent(self.queue.clone())
        }
    }
}

/// What one period produced. Serialized into the run ledger and returned by
/// the admin route, so an operator can see the exact ids without opening the
/// decision store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBoardShadowRun {
    pub run_at_utc: String,
    pub queue_id: String,
    pub tasks_db_sha256: String,
    pub source_lineage: String,
    pub horizon_days: usize,
    pub ticket_count: usize,
    pub contributing_agents: Vec<String>,
    pub receipt: OperatorPilotImportReceipt,
    /// `None` when the window does not identify a service capacity (too few
    /// saturated days). An unidentified fit is reported, never guessed.
    pub nowcast: Option<ProspectiveSlaForecast>,
    pub nowcast_unavailable_reason: Option<String>,
    /// Always present. The staffing series is a proxy (see
    /// `decision_task_board_export`'s module doc) and the cost axis is inert.
    pub limitations: Vec<String>,
}

fn limitations() -> Vec<String> {
    vec![
        "staffing[].agents is agent.toml [heartbeat] max_concurrent_runs — a static proxy for staffed capacity, not a measured daily staffing series".into(),
        "staff_cost_cents_per_agent_day is 0: the task board carries no payroll, so every cost comparison on this pilot is inert".into(),
        "sla_days = 2 is a stipulated definition (closed the same or next UTC day), not an agreed service target".into(),
        "The prospective nowcast is computed, not pre-registered: scoring it against outcomes requires an operator-created shadow policy".into(),
    ]
}

/// Fit the model this period's comparison uses from the window's own observed
/// days, the same way `decision_synthetic` does — never a hard-coded capacity.
fn fit_model(observed: &[ObservedSupportDay], window_tag: &str) -> Result<QueueModel, String> {
    let fit = crate::decision_calibration::fit_capacity(observed, MIN_SATURATED_DAYS)
        .map_err(|e| format!("capacity fit: {e}"))?;
    Ok(QueueModel {
        version: format!("{MODEL_PREFIX}:{window_tag}"),
        service_capacity_per_agent_day: fit.service_per_agent_day,
        sla_days: SLA_DAYS,
        staff_cost_cents_per_agent_day: STAFF_COST_CENTS_PER_AGENT_DAY,
    })
}

/// The alternative this line always compares against: the same schedule with
/// one more agent every day. Deliberately fixed — a background job choosing
/// its own counterfactual would be a policy decision, not a measurement.
fn plus_one_agent(baseline: &StaffingScenario, window_tag: &str) -> StaffingScenario {
    StaffingScenario {
        id: format!("task-board-plus-one-agent-{window_tag}"),
        agents_by_day: baseline
            .agents_by_day
            .iter()
            .map(|v| v.saturating_add(1))
            .collect(),
        fixed_extra_capacity_by_day: baseline.fixed_extra_capacity_by_day.clone(),
    }
}

/// Reconstruct the opening ticket ages at the first day AFTER the window, so
/// the nowcast's opening cohorts are derived from the same source rows the
/// import bound, not from a second read of the board.
fn opening_cohorts_after_window(
    export: &crate::decision_ingest::SupportPilotExport,
) -> Result<(Vec<InitialCohort>, u64), String> {
    let cutoff = DateTime::parse_from_rfc3339(&export.data_cutoff_utc)
        .map_err(|_| "cutoff is not RFC3339".to_string())?;
    let mut by_age: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
    let mut total = 0_u64;
    for ticket in &export.tickets {
        if ticket.resolved_at_utc.is_some() {
            continue;
        }
        let created = DateTime::parse_from_rfc3339(&ticket.created_at_utc)
            .map_err(|_| "ticket creation is not RFC3339".to_string())?;
        let age_secs = cutoff.timestamp() - created.timestamp();
        // `div_euclid` matches `decision_ingest::day_index`; an age of 0 days
        // (created during the final day) is one whole day old at the cutoff.
        let age = u32::try_from(age_secs.div_euclid(86_400).max(0))
            .map_err(|_| "ticket age overflow".to_string())?
            .saturating_add(1);
        *by_age.entry(age).or_default() = by_age
            .get(&age)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("cohort overflow")?;
        total = total.checked_add(1).ok_or("backlog overflow")?;
    }
    Ok((
        by_age
            .into_iter()
            .map(|(age_days, count)| InitialCohort { age_days, count })
            .collect(),
        total,
    ))
}

/// Run one period end to end. Returns the run record; every failure is an
/// `Err(String)` the caller logs — nothing here panics or aborts the gateway.
pub fn run_once(
    home_dir: &Path,
    config: &TaskBoardShadowConfig,
) -> Result<TaskBoardShadowRun, String> {
    let options = TaskBoardExportOptions {
        queue: config.queue(),
        horizon_days: config.horizon_days,
        window_start_utc: None,
        seed: 0,
    };
    let exported = export_from_home(home_dir, &options).map_err(|e| e.to_string())?;
    let export = exported.export.clone();
    let window_tag = export
        .window_start_utc
        .split('T')
        .next()
        .unwrap_or("window")
        .replace('-', "");
    let pilot = build_support_pilot(&export).map_err(|e| e.to_string())?;
    let model = fit_model(&pilot.observed_days, &window_tag)?;
    let alternative = plus_one_agent(&pilot.baseline, &window_tag);

    let retention = Utc::now()
        .checked_add_signed(ChronoDuration::days(config.retention_days))
        .ok_or("retention overflow")?;
    let request = OperatorPilotImportRequest {
        tenant_id: TENANT_ID.into(),
        acl: ACL.into(),
        expected_queue_id: exported.queue_id.clone(),
        source_lineage: exported.source_lineage.clone(),
        retention_until_utc: retention.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        export: export.clone(),
        model: model.clone(),
        alternative_scenario: alternative,
    };

    let store = DecisionStore::with_causal_store(
        home_dir.join("decisions.db"),
        duduclaw_memory::causal::CausalStore::new(home_dir.join("memory.db")),
    );
    let receipt = store
        .import_operator_pilot(&request)
        .map_err(|e| format!("import: {e}"))?;

    // Prospective nowcast for the day that started at the cutoff.
    let (nowcast, nowcast_unavailable_reason) = match opening_cohorts_after_window(&export) {
        Ok((cohorts, opening)) => {
            let inputs = KnownSlaDayInputs {
                target_day_utc: export.data_cutoff_utc.clone(),
                queue_id: Some(exported.queue_id.clone()),
                opening_cohorts: cohorts,
                known: KnownDayInputs {
                    opening_backlog: opening,
                    planned_agents: pilot.baseline.agents_by_day.last().copied().unwrap_or(1),
                    planned_fixed_extra_capacity: 0,
                },
            };
            match forecast_next_day_sla(&pilot.observed_days, &inputs, &model, MIN_SATURATED_DAYS) {
                Ok(forecast) => (Some(forecast), None),
                Err(e) => (None, Some(format!("nowcast unavailable: {e}"))),
            }
        }
        Err(e) => (None, Some(format!("opening stock unavailable: {e}"))),
    };

    let run = TaskBoardShadowRun {
        run_at_utc: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        queue_id: exported.queue_id,
        tasks_db_sha256: exported.tasks_db_sha256,
        source_lineage: exported.source_lineage,
        horizon_days: export.horizon_days,
        ticket_count: export.tickets.len(),
        contributing_agents: exported.contributing_agents,
        receipt,
        nowcast,
        nowcast_unavailable_reason,
        limitations: limitations(),
    };
    append_run_ledger(home_dir, &run);
    Ok(run)
}

/// Where each period's record lands. Append-only JSONL, the same shape every
/// other ledger in this tree uses.
pub fn ledger_path(home_dir: &Path) -> PathBuf {
    home_dir.join("decision_task_board_runs.jsonl")
}

fn append_run_ledger(home_dir: &Path, run: &TaskBoardShadowRun) {
    let Ok(line) = serde_json::to_string(run) else {
        return;
    };
    let path = ledger_path(home_dir);
    // Cross-process advisory lock: the background task and the admin route
    // can both append (CLAUDE.md coding convention 3).
    let _ = duduclaw_core::with_file_lock(&path, || {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{line}")?;
        Ok(())
    });
}

/// Background loop. Self-gating: reads `[decision]` on every tick and returns
/// immediately while `task_board_shadow` is off (the default), so a deployment
/// that never opts in pays one `config.toml` read per tick and writes nothing.
pub async fn run(home_dir: PathBuf, tick: Duration) {
    let mut interval = tokio::time::interval(tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_run: Option<std::time::Instant> = None;
    loop {
        interval.tick().await;
        let config = TaskBoardShadowConfig::from_home(&home_dir);
        if !config.enabled {
            continue;
        }
        // The master `[decision] enabled` kill switch (P5) governs this line
        // too: an operator who turned the decision surface off must not keep
        // getting background writes into its stores.
        if !crate::decision_gate::DecisionConfig::from_home(&home_dir).enabled {
            debug!("task-board shadow skipped: [decision] enabled = false");
            continue;
        }
        let due = last_run.is_none_or(|at| {
            at.elapsed() >= Duration::from_secs(config.every_hours.saturating_mul(3_600))
        });
        if !due {
            continue;
        }
        last_run = Some(std::time::Instant::now());
        let home = home_dir.clone();
        let cfg = config.clone();
        match tokio::task::spawn_blocking(move || run_once(&home, &cfg)).await {
            Ok(Ok(run)) => info!(
                queue = %run.queue_id,
                tickets = run.ticket_count,
                snapshot = %run.receipt.snapshot_id,
                "任務板→決策孿生：本期匯出匯入完成"
            ),
            Ok(Err(e)) => {
                warn!(error = %e, "任務板→決策孿生本期失敗（不影響 gateway）");
                audit_failure(&home_dir, &e);
            }
            Err(e) => {
                warn!(error = %e, "任務板→決策孿生本期 panic（不影響 gateway）");
                audit_failure(&home_dir, &e.to_string());
            }
        }
    }
}

fn audit_failure(home_dir: &Path, reason: &str) {
    let event = duduclaw_security::audit::AuditEvent::new(
        "decision_task_board_shadow_failed",
        "gateway",
        duduclaw_security::audit::Severity::Warning,
        serde_json::json!({
            "reason": duduclaw_core::truncate_bytes(reason, 400),
        }),
    );
    duduclaw_security::audit::append_audit_event(home_dir, &event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_off_and_daily() {
        let cfg = TaskBoardShadowConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.every_hours, 24);
        assert_eq!(cfg.retention_days, 90);
        assert_eq!(cfg.horizon_days, 14);
        assert_eq!(cfg.queue, "all");
    }

    #[test]
    fn missing_or_malformed_config_resolves_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            TaskBoardShadowConfig::from_home(dir.path()),
            TaskBoardShadowConfig::default()
        );
        std::fs::write(dir.path().join("config.toml"), "not = [toml").unwrap();
        assert_eq!(
            TaskBoardShadowConfig::from_home(dir.path()),
            TaskBoardShadowConfig::default()
        );
        std::fs::write(dir.path().join("config.toml"), "[other]\nx = 1\n").unwrap();
        assert_eq!(
            TaskBoardShadowConfig::from_home(dir.path()),
            TaskBoardShadowConfig::default()
        );
    }

    #[test]
    fn config_reads_every_key_and_clamps_nonsense() {
        let table = "[decision]\ntask_board_shadow = true\ntask_board_shadow_every_hours = 0\n\
                     task_board_retention_days = 30\ntask_board_horizon_days = 900\n\
                     task_board_queue = '  alice  '\n"
            .parse::<toml::Table>()
            .unwrap();
        let cfg = TaskBoardShadowConfig::from_table(&table);
        assert!(cfg.enabled);
        assert_eq!(cfg.every_hours, 1, "0 hours is clamped to the 1 h floor");
        assert_eq!(cfg.retention_days, 30);
        assert_eq!(
            cfg.horizon_days, 14,
            "out-of-range horizon keeps the default"
        );
        assert_eq!(cfg.queue, "alice");
        assert_eq!(cfg.queue(), TaskBoardQueue::Agent("alice".into()));
    }

    #[test]
    fn all_queue_maps_to_the_whole_board() {
        let cfg = TaskBoardShadowConfig::default();
        assert_eq!(cfg.queue(), TaskBoardQueue::All);
    }

    #[test]
    fn plus_one_agent_adds_exactly_one_per_day() {
        let baseline = StaffingScenario {
            id: "b".into(),
            agents_by_day: vec![1, 2, 3],
            fixed_extra_capacity_by_day: vec![0, 0, 0],
        };
        let alt = plus_one_agent(&baseline, "20260901");
        assert_eq!(alt.agents_by_day, vec![2, 3, 4]);
        assert_ne!(alt.id, baseline.id, "baseline and alternative must differ");
    }

    #[test]
    fn opening_cohorts_count_only_still_open_tickets() {
        use crate::decision_ingest::{DailyStaffing, SupportPilotExport, TicketEvent};
        let export = SupportPilotExport {
            snapshot_id: "s".into(),
            baseline_scenario_id: "b".into(),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            data_cutoff_utc: "2026-09-15T00:00:00Z".into(),
            source_version_hashes: vec!["x".into()],
            seed: 0,
            horizon_days: 14,
            tickets: vec![
                TicketEvent {
                    queue_id: None,
                    ticket_id: "open-old".into(),
                    created_at_utc: "2026-09-01T00:00:00Z".into(),
                    resolved_at_utc: None,
                },
                TicketEvent {
                    queue_id: None,
                    ticket_id: "open-new".into(),
                    created_at_utc: "2026-09-14T06:00:00Z".into(),
                    resolved_at_utc: None,
                },
                TicketEvent {
                    queue_id: None,
                    ticket_id: "closed".into(),
                    created_at_utc: "2026-09-02T00:00:00Z".into(),
                    resolved_at_utc: Some("2026-09-03T00:00:00Z".into()),
                },
            ],
            staffing: vec![DailyStaffing {
                queue_id: None,
                day_utc: "2026-09-01T00:00:00Z".into(),
                agents: 1,
                fixed_extra_capacity: 0,
            }],
        };
        let (cohorts, total) = opening_cohorts_after_window(&export).unwrap();
        assert_eq!(total, 2, "the closed ticket contributes nothing");
        // Ages are strictly positive and unique, as `forecast_next_day_sla`
        // requires.
        assert!(cohorts.iter().all(|c| c.age_days > 0 && c.count > 0));
        let ages: Vec<_> = cohorts.iter().map(|c| c.age_days).collect();
        assert_eq!(ages, vec![1, 15]);
        assert_eq!(cohorts.iter().map(|c| c.count as u64).sum::<u64>(), total);
    }

    #[test]
    fn limitations_are_never_empty() {
        assert!(limitations().len() >= 4);
    }

    #[tokio::test]
    async fn disabled_loop_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let handle = tokio::spawn(run(home.clone(), Duration::from_millis(10)));
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();
        assert!(
            !ledger_path(&home).exists(),
            "the default-off loop must never create a ledger"
        );
    }

    #[tokio::test]
    async fn enabled_loop_runs_one_period_against_a_live_task_store() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // A board with enough closed work for the capacity fit to identify.
        let store = crate::task_store::TaskStore::open(&home).unwrap();
        drop(store);
        {
            let conn = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
            for day in 1..14_i64 {
                let created = (Utc::now() - ChronoDuration::days(15 - day)).to_rfc3339();
                let done = (Utc::now() - ChronoDuration::days(14 - day)).to_rfc3339();
                conn.execute(
                    "INSERT INTO tasks (id,title,description,status,priority,assigned_to,created_by,created_at,updated_at,completed_at)
                     VALUES (?1,'t','','done','medium','alice','system',?2,?2,?3)",
                    rusqlite::params![format!("t{day}"), created, done],
                )
                .unwrap();
            }
        }
        std::fs::write(
            home.join("config.toml"),
            "[decision]\ntask_board_shadow = true\ntask_board_queue = 'alice'\n",
        )
        .unwrap();
        let handle = tokio::spawn(run(home.clone(), Duration::from_millis(10)));
        for _ in 0..200 {
            if ledger_path(&home).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        handle.abort();
        let ledger = std::fs::read_to_string(ledger_path(&home)).unwrap_or_default();
        assert!(
            !ledger.trim().is_empty(),
            "an enabled period must append exactly one ledger line; got {ledger:?}"
        );
        let run: TaskBoardShadowRun = serde_json::from_str(ledger.lines().next().unwrap()).unwrap();
        assert_eq!(run.queue_id, "task-board:alice");
        assert_eq!(run.receipt.queue_id, "task-board:alice");
        assert!(!run.limitations.is_empty());
    }
}
