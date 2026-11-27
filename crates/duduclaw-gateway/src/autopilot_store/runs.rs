//! Durable multi-step storage only; execution and authorization live in the gateway.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{in_immediate_tx, AutopilotHistoryRow, AutopilotStore, DISPATCH_EXECUTING, DISPATCH_SETTLED};
use crate::autopilot_approval::RESULT_SUCCESS;

/// Keep byte-identical to the final statements in autopilot-v186.sql.
pub(crate) const RUNS_SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS autopilot_runs (
    run_id TEXT PRIMARY KEY,
    rule_id TEXT NOT NULL,
    rule_name TEXT NOT NULL,
    rule_hash TEXT NOT NULL,
    event_name TEXT NOT NULL,
    event_ref TEXT NOT NULL,
    dedupe_key TEXT NOT NULL UNIQUE,
    fields_json TEXT NOT NULL,
    state TEXT NOT NULL,
    step_index INTEGER NOT NULL DEFAULT 0,
    step_count INTEGER NOT NULL,
    next_at TEXT,
    lease_owner TEXT,
    lease_until TEXT,
    owner_department TEXT,
    outcome TEXT,
    detail TEXT,
    notice_pending INTEGER NOT NULL DEFAULT 0,
    notice_push_state TEXT,
    notice_push_attempts INTEGER NOT NULL DEFAULT 0,
    notice_push_next_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ap_runs_due ON autopilot_runs(state, next_at);
CREATE INDEX IF NOT EXISTS idx_ap_runs_rule ON autopilot_runs(rule_id, state);
CREATE TABLE IF NOT EXISTS autopilot_run_steps (
    run_id TEXT NOT NULL,
    step_index INTEGER NOT NULL,
    step_type TEXT NOT NULL,
    state TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    dispatch_id TEXT,
    queue_message_id TEXT,
    started_at TEXT,
    finished_at TEXT,
    detail TEXT,
    PRIMARY KEY (run_id, step_index)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_ap_dispatch_run_step ON autopilot_approval_dispatch(run_id, step_index) WHERE run_id IS NOT NULL;";

pub const RUN_PENDING: &str = "pending";
pub const RUN_RUNNING: &str = "running";
pub const RUN_WAITING: &str = "waiting";
pub const RUN_AWAITING_APPROVAL: &str = "awaiting_approval";
/// Reserved for a future executor; this release never writes it.
pub const RUN_DEFERRED: &str = "deferred";
pub const RUN_SETTLED: &str = "settled";
pub const STEP_PENDING: &str = "pending";
pub const STEP_RUNNING: &str = "running";
pub const STEP_DONE: &str = "done";
pub const STEP_FAILED: &str = "failed";
pub const STEP_SKIPPED: &str = "skipped";
pub const MAX_UNSETTLED_RUNS_PER_RULE: i64 = 20;
pub const MAX_UNSETTLED_RUNS_TOTAL: i64 = 1000;
pub const RUN_LEASE_SECS: i64 = 60;

pub fn is_known_run_state(state: &str) -> bool {
    matches!(state, RUN_PENDING | RUN_RUNNING | RUN_WAITING | RUN_AWAITING_APPROVAL | RUN_DEFERRED | RUN_SETTLED)
}

/// Fixed-width UTC timestamps keep SQLite TEXT comparisons chronological.
pub fn run_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn normalize_ts(value: Option<&str>) -> Result<Option<String>, String> {
    value.map(|s| DateTime::parse_from_rfc3339(s)
        .map(|t| run_ts(t.with_timezone(&Utc)))
        .map_err(|e| format!("invalid run timestamp: {e}"))).transpose()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutopilotRunRow {
    pub run_id: String,
    pub rule_id: String,
    pub rule_name: String,
    pub rule_hash: String,
    pub event_name: String,
    pub event_ref: String,
    pub dedupe_key: String,
    pub fields_json: String,
    pub state: String,
    pub step_index: i64,
    pub step_count: i64,
    pub next_at: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_until: Option<String>,
    pub owner_department: Option<String>,
    #[serde(default)]
    pub gateway_origin: bool,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub notice_pending: bool,
    pub notice_push_state: Option<String>,
    pub notice_push_attempts: i64,
    pub notice_push_next_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl AutopilotRunRow {
    pub(crate) fn event_origin(&self) -> crate::autopilot_engine::EventOrigin {
        if self.gateway_origin { crate::autopilot_engine::EventOrigin::gateway() }
        else { crate::autopilot_engine::EventOrigin::external() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutopilotRunStepRow {
    pub run_id: String,
    pub step_index: i64,
    pub step_type: String,
    pub state: String,
    pub idempotency_key: String,
    pub dispatch_id: Option<String>,
    pub queue_message_id: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunInsertOutcome {
    Inserted,
    Duplicate,
    RuleLimit,
    GlobalLimit,
    FieldsTooLarge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunNext {
    Continue { step_index: i64 },
    Wait { step_index: i64, next_at: DateTime<Utc> },
    Settle { outcome: String, detail: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepApprovalBindMode {
    Filing,
    Recovery,
}

pub fn step_idempotency_key(run_id: &str, step_index: i64) -> String {
    hex::encode(Sha256::digest(format!("autopilot-step:v1:{run_id}:{step_index}").as_bytes()))
}

const RUN_COLUMNS: &str = "run_id, rule_id, rule_name, rule_hash, event_name, event_ref,
    dedupe_key, fields_json, state, step_index, step_count, next_at, lease_owner,
    lease_until, owner_department, outcome, detail, created_at, updated_at,
    notice_pending, notice_push_state, notice_push_attempts, notice_push_next_at, gateway_origin";
const STEP_COLUMNS: &str = "run_id, step_index, step_type, state, idempotency_key,
    dispatch_id, queue_message_id, started_at, finished_at, detail";

impl AutopilotStore {
    /// Publish an approvable dispatch and its run/step binding in one transaction.
    /// Filing requires this owner's live lease. Recovery may also bind an expired
    /// lease, but never a live lease belonging to another process.
    pub async fn bind_step_dispatch_awaiting(
        &self, dispatch_id: &str, approval_id: &str, run_id: &str, step_index: i64,
        lease_owner: &str, now: DateTime<Utc>, mode: StepApprovalBindMode,
    ) -> Result<bool, String> {
        if approval_id.is_empty() || lease_owner.is_empty() { return Ok(false); }
        let conn = self.conn.lock().await;
        let now = run_ts(now);
        in_immediate_tx(&conn, "bind step approval", |conn| {
            let eligible: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM autopilot_approval_dispatch d
                 JOIN autopilot_runs r ON r.run_id=d.run_id
                 JOIN autopilot_run_steps s ON s.run_id=r.run_id AND s.step_index=r.step_index
                 WHERE d.dispatch_id=?1 AND d.state='requesting' AND d.approval_id IS NULL
                   AND d.run_id=?2 AND d.step_index=?3 AND r.step_index=?3
                   AND d.rule_id=r.rule_id AND d.rule_hash=r.rule_hash
                   AND r.state='running' AND s.state='running' AND s.dispatch_id IS NULL
                   AND ((?6=0 AND r.lease_owner=?4 AND julianday(r.lease_until)>=julianday(?5))
                     OR (?6=1 AND julianday(r.lease_until) IS NOT NULL
                         AND (r.lease_owner=?4 OR julianday(r.lease_until)<julianday(?5)))))",
                params![dispatch_id, run_id, step_index, lease_owner, now, mode == StepApprovalBindMode::Recovery],
                |r| r.get(0),
            ).map_err(|e| format!("check step approval binding: {e}"))?;
            if !eligible { return Ok(false); }
            let n = conn.execute("UPDATE autopilot_approval_dispatch
                SET state='awaiting', approval_id=?1, updated_at=?2
                WHERE dispatch_id=?3 AND state='requesting'",
                params![approval_id, now, dispatch_id]).map_err(|e| format!("bind approval dispatch: {e}"))?;
            if n != 1 { return Err("approval dispatch changed during binding".into()); }
            let n = conn.execute("UPDATE autopilot_run_steps SET dispatch_id=?1
                WHERE run_id=?2 AND step_index=?3 AND state='running' AND dispatch_id IS NULL",
                params![dispatch_id, run_id, step_index]).map_err(|e| format!("bind approval step: {e}"))?;
            if n != 1 { return Err("approval step changed during binding".into()); }
            let n = conn.execute("UPDATE autopilot_runs SET state='awaiting_approval',
                next_at=NULL, lease_owner=NULL, lease_until=NULL, updated_at=?1
                WHERE run_id=?2 AND step_index=?3 AND state='running'",
                params![now, run_id, step_index]).map_err(|e| format!("bind approval run: {e}"))?;
            if n != 1 { return Err("approval run changed during binding".into()); }
            conn.execute("INSERT OR IGNORE INTO autopilot_history
                (id, rule_id, rule_name, triggered_at, result, details)
                SELECT ?1, rule_id, rule_name, ?2, 'awaiting_approval', ?3
                FROM autopilot_runs WHERE run_id=?4",
                params![format!("run:{run_id}:step:{step_index}:approval"), now,
                    format!("等待管理員核准（步驟 {}，核准紀錄 {dispatch_id}）", step_index + 1), run_id])
                .map_err(|e| format!("record step approval: {e}"))?;
            Ok(true)
        })
    }

    pub async fn list_awaiting_approval_runs(&self) -> Result<Vec<AutopilotRunRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!("SELECT {RUN_COLUMNS} FROM autopilot_runs
            WHERE state='awaiting_approval' ORDER BY created_at, run_id"))
            .map_err(|e| format!("prepare approval runs: {e}"))?;
        let rows = stmt.query_map([], row_to_run)
            .map_err(|e| format!("query approval runs: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("read approval runs: {e}"))?;
        Ok(rows)
    }

    pub async fn get_step_dispatch(&self, run_id: &str, step_index: i64) -> Result<Option<super::AutopilotDispatchRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(&format!("SELECT {} FROM autopilot_approval_dispatch WHERE run_id=?1 AND step_index=?2",
            super::DISPATCH_COLUMNS), params![run_id, step_index], super::row_to_dispatch)
            .optional().map_err(|e| format!("read step dispatch: {e}"))
    }

    /// Atomically enforce capacity and dedupe, then insert the run and all steps.
    /// Creation/audit timestamps use `now`; supplied optional timestamps are normalized.
    pub async fn insert_run(
        &self, run: &AutopilotRunRow, steps: &[AutopilotRunStepRow], now: DateTime<Utc>,
    ) -> Result<RunInsertOutcome, String> {
        let conn = self.conn.lock().await;
        let mut duplicate = false;
        let result = in_immediate_tx(&conn, "insert run", |conn| {
            if run.fields_json.len() > crate::autopilot_approval::MAX_FROZEN_PAYLOAD_BYTES {
                return Ok(RunInsertOutcome::FieldsTooLarge);
            }
            if i64::try_from(steps.len()).ok() != Some(run.step_count)
                || steps.iter().enumerate().any(|(i, s)| s.step_index != i as i64 || s.run_id != run.run_id)
            {
                return Err("run steps must match step_count and have contiguous indexes from zero".into());
            }
            if !is_known_run_state(&run.state) || run.state == RUN_DEFERRED {
                return Err("unsupported initial run state".into());
            }
            if steps.iter().any(|s| !matches!(s.state.as_str(), STEP_PENDING | STEP_RUNNING | STEP_DONE | STEP_FAILED | STEP_SKIPPED)) {
                return Err("unsupported initial step state".into());
            }
            // A replay consumes no capacity, even when the rule is already at
            // its limit. Keep this read inside the same immediate transaction.
            let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM autopilot_runs WHERE dedupe_key=?1)",
                params![run.dedupe_key], |r| r.get(0)).map_err(|e| format!("check run dedupe: {e}"))?;
            if exists { return Ok(RunInsertOutcome::Duplicate); }
            if count_unsettled(conn, Some(&run.rule_id))? >= MAX_UNSETTLED_RUNS_PER_RULE {
                return Ok(RunInsertOutcome::RuleLimit);
            }
            if count_unsettled(conn, None)? >= MAX_UNSETTLED_RUNS_TOTAL {
                return Ok(RunInsertOutcome::GlobalLimit);
            }
            let now = run_ts(now);
            // Target only the dedupe constraint: other uniqueness errors roll back.
            // A conflict rolls the transaction back before returning Duplicate.
            let n = conn.execute(
                &format!("INSERT INTO autopilot_runs ({RUN_COLUMNS})
                    VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)
                    ON CONFLICT(dedupe_key) DO NOTHING"),
                params![run.run_id, run.rule_id, run.rule_name, run.rule_hash, run.event_name,
                    run.event_ref, run.dedupe_key, run.fields_json, run.state, run.step_index,
                    run.step_count, normalize_ts(run.next_at.as_deref())?, run.lease_owner,
                    normalize_ts(run.lease_until.as_deref())?, run.owner_department,
                    run.outcome, run.detail, now, now, run.notice_pending, run.notice_push_state,
                    run.notice_push_attempts, normalize_ts(run.notice_push_next_at.as_deref())?, run.gateway_origin],
            ).map_err(|e| format!("insert run: {e}"))?;
            if n == 0 {
                duplicate = true;
                return Err("duplicate run dedupe key".into());
            }
            for step in steps {
                conn.execute(
                    &format!("INSERT INTO autopilot_run_steps ({STEP_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"),
                    params![step.run_id, step.step_index, step.step_type, step.state, step.idempotency_key,
                        step.dispatch_id, step.queue_message_id, normalize_ts(step.started_at.as_deref())?,
                        normalize_ts(step.finished_at.as_deref())?, step.detail],
                ).map_err(|e| format!("insert run step: {e}"))?;
            }
            Ok(RunInsertOutcome::Inserted)
        });
        if duplicate { Ok(RunInsertOutcome::Duplicate) } else { result }
    }

    pub async fn get_run(&self, run_id: &str) -> Result<Option<AutopilotRunRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(&format!("SELECT {RUN_COLUMNS} FROM autopilot_runs WHERE run_id=?1"),
            params![run_id], row_to_run).optional().map_err(|e| format!("get run: {e}"))
    }

    pub async fn list_run_steps(&self, run_id: &str) -> Result<Vec<AutopilotRunStepRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!("SELECT {STEP_COLUMNS} FROM autopilot_run_steps WHERE run_id=?1 ORDER BY step_index"))
            .map_err(|e| format!("prepare run steps: {e}"))?;
        let rows = stmt.query_map(params![run_id], row_to_step)
            .map_err(|e| format!("query run steps: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("read run steps: {e}"))?;
        Ok(rows)
    }

    pub async fn list_runs_for_rule(&self, rule_id: &str, limit: i64) -> Result<Vec<AutopilotRunRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!("SELECT {RUN_COLUMNS} FROM autopilot_runs WHERE rule_id=?1 ORDER BY created_at DESC, run_id DESC LIMIT ?2"))
            .map_err(|e| format!("prepare rule runs: {e}"))?;
        let rows = stmt.query_map(params![rule_id, limit.max(0)], row_to_run)
            .map_err(|e| format!("query rule runs: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("read rule runs: {e}"))?;
        Ok(rows)
    }

    pub async fn count_unsettled_runs(&self, rule_id: Option<&str>) -> Result<i64, String> {
        let conn = self.conn.lock().await;
        count_unsettled(&conn, rule_id)
    }

    pub async fn list_due_runs(&self, now: DateTime<Utc>, limit: i64) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().await;
        list_run_ids(&conn, "SELECT run_id FROM autopilot_runs WHERE state IN ('pending','waiting')
            AND (next_at IS NULL OR next_at<=?1) ORDER BY next_at ASC, created_at ASC, run_id ASC LIMIT ?2", now, limit)
    }

    pub async fn list_expired_running_runs(&self, now: DateTime<Utc>, limit: i64) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().await;
        list_run_ids(&conn, "SELECT run_id FROM autopilot_runs WHERE state='running' AND lease_until<?1
            ORDER BY lease_until ASC, run_id ASC LIMIT ?2", now, limit)
    }

    /// Unknown states are not due/expired candidates, but must not remain stranded.
    pub async fn list_unsupported_runs(&self, limit: i64) -> Result<Vec<AutopilotRunRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!("SELECT {RUN_COLUMNS} FROM autopilot_runs
            WHERE state NOT IN ('pending','running','waiting','awaiting_approval','deferred','settled')
            ORDER BY created_at, run_id LIMIT ?1"))
            .map_err(|e| format!("prepare unsupported runs: {e}"))?;
        let rows = stmt.query_map(params![limit.max(0)], row_to_run)
            .map_err(|e| format!("query unsupported runs: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("read unsupported runs: {e}"))?;
        Ok(rows)
    }

    pub async fn claim_run(&self, run_id: &str, lease_owner: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn.execute("UPDATE autopilot_runs SET state='running', lease_owner=?1, lease_until=?2, updated_at=?3
            WHERE run_id=?4 AND state IN ('pending','waiting') AND (next_at IS NULL OR next_at<=?3)",
            params![lease_owner, run_ts(now + Duration::seconds(RUN_LEASE_SECS)), run_ts(now), run_id])
            .map_err(|e| format!("claim run: {e}"))?;
        Ok(n == 1)
    }

    pub async fn reclaim_expired_run(&self, run_id: &str, lease_owner: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn.execute("UPDATE autopilot_runs SET lease_owner=?1, lease_until=?2, updated_at=?3
            WHERE run_id=?4 AND state='running' AND lease_until<?3",
            params![lease_owner, run_ts(now + Duration::seconds(RUN_LEASE_SECS)), run_ts(now), run_id])
            .map_err(|e| format!("reclaim run: {e}"))?;
        Ok(n == 1)
    }

    /// Start only the current pending step while the caller holds a live lease.
    pub async fn mark_step_running(&self, run_id: &str, step_index: i64, lease_owner: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        mark_step_running(&conn, run_id, step_index, lease_owner, now)
    }

    /// Retry only the current running step under a live, reclaimed lease.
    pub async fn reset_running_step(&self, run_id: &str, step_index: i64, lease_owner: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn.execute("UPDATE autopilot_run_steps SET state='pending', started_at=NULL
            WHERE run_id=?1 AND step_index=?2 AND state='running'
            AND EXISTS (SELECT 1 FROM autopilot_runs WHERE run_id=?1 AND step_index=?2
                AND state='running' AND lease_owner=?3 AND lease_until>=?4)",
            params![run_id, step_index, lease_owner, run_ts(now)])
            .map_err(|e| format!("reset running step: {e}"))?;
        Ok(n == 1)
    }

    /// Finish a running step and advance its run atomically. Approval binding
    /// uses bind_step_dispatch_awaiting instead, while the step stays running.
    pub async fn complete_step(
        &self, run_id: &str, lease_owner: &str, step_index: i64, step_state: &str,
        detail: Option<&str>, queue_message_id: Option<&str>, next: RunNext, now: DateTime<Utc>,
    ) -> Result<bool, String> {
        if !matches!(step_state, STEP_DONE | STEP_FAILED | STEP_SKIPPED) {
            return Err("invalid completed step state".into());
        }
        let conn = self.conn.lock().await;
        in_immediate_tx(&conn, "complete step", |conn| {
            let count: Option<i64> = conn.query_row("SELECT step_count FROM autopilot_runs
                WHERE run_id=?1 AND step_index=?2 AND state='running' AND lease_owner=?3 AND lease_until>=?4",
                params![run_id, step_index, lease_owner, run_ts(now)], |r| r.get(0))
                .optional().map_err(|e| format!("check step lease: {e}"))?;
            let Some(count) = count else { return Ok(false); };
            let (state, index, next_at, outcome, run_detail) = match next {
                RunNext::Continue { step_index } => (RUN_PENDING, step_index, None, None, None),
                RunNext::Wait { step_index, next_at } => (RUN_WAITING, step_index, Some(run_ts(next_at)), None, None),
                RunNext::Settle { outcome, detail } => (RUN_SETTLED, step_index, None, Some(outcome), detail),
            };
            if index < 0 || index >= count { return Err("next step index out of range".into()); }
            let now = run_ts(now);
            let n = conn.execute("UPDATE autopilot_run_steps SET state=?1, finished_at=?2, detail=?3,
                queue_message_id=?4 WHERE run_id=?5 AND step_index=?6 AND state='running'",
                params![step_state, now, detail, queue_message_id, run_id, step_index])
                .map_err(|e| format!("complete run step: {e}"))?;
            if n == 0 { return Ok(false); }
            let n = conn.execute("UPDATE autopilot_runs SET state=?1, step_index=?2, next_at=?3,
                outcome=?4, detail=?5, lease_owner=NULL, lease_until=NULL, updated_at=?6,
                notice_pending=CASE WHEN ?1='settled' THEN 1 ELSE notice_pending END,
                notice_push_state=CASE WHEN ?1='settled' AND ?4 IN ('missed_late','execution_uncertain')
                    THEN 'pending' ELSE notice_push_state END
                WHERE run_id=?7 AND state='running' AND lease_owner=?8 AND step_index=?9 AND lease_until>=?6",
                params![state, index, next_at, outcome, run_detail, now, run_id, lease_owner, step_index])
                .map_err(|e| format!("advance run: {e}"))?;
            if n != 1 { return Err("run transition lost after completing step".into()); }
            Ok(true)
        })
    }

    pub async fn resume_after_approval(&self, run_id: &str, step_index: i64, dispatch_id: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        in_immediate_tx(&conn, "resume approved run", |conn| {
            let count: Option<i64> = conn.query_row("SELECT step_count FROM autopilot_runs
                WHERE run_id=?1 AND state='awaiting_approval' AND step_index=?2
                AND EXISTS (SELECT 1 FROM autopilot_run_steps
                    WHERE run_id=?1 AND step_index=?2 AND state='running' AND dispatch_id=?3)
                AND EXISTS (SELECT 1 FROM autopilot_approval_dispatch
                    WHERE dispatch_id=?3 AND run_id=?1 AND step_index=?2
                    AND (state=?4 OR (state=?5 AND outcome=?6)))",
                params![run_id, step_index, dispatch_id, DISPATCH_EXECUTING, DISPATCH_SETTLED, RESULT_SUCCESS], |r| r.get(0)).optional()
                .map_err(|e| format!("read approved run: {e}"))?;
            let Some(count) = count else { return Ok(false); };
            if step_index < 0 || step_index >= count { return Err("approved step index out of range".into()); }
            let now = run_ts(now);
            let n = conn.execute("UPDATE autopilot_run_steps SET state='done', finished_at=?1
                WHERE run_id=?2 AND step_index=?3 AND state='running' AND dispatch_id=?4",
                params![now, run_id, step_index, dispatch_id]).map_err(|e| format!("complete approved step: {e}"))?;
            if n == 0 { return Ok(false); }
            let last = step_index == count - 1;
            let n = conn.execute("UPDATE autopilot_runs SET state=?1, step_index=?2, outcome=?3,
                next_at=NULL, lease_owner=NULL, lease_until=NULL, updated_at=?4,
                notice_pending=CASE WHEN ?1='settled' THEN 1 ELSE notice_pending END
                WHERE run_id=?5 AND state='awaiting_approval' AND step_index=?6",
                params![if last { RUN_SETTLED } else { RUN_PENDING }, if last { step_index } else { step_index + 1 },
                    last.then_some("completed"), now, run_id, step_index])
                .map_err(|e| format!("resume approved run: {e}"))?;
            if n != 1 { return Err("run transition lost after approving step".into()); }
            Ok(true)
        })
    }

    /// Only the owner of a live lease may settle a running run.
    pub async fn settle_claimed_run(
        &self, run_id: &str, lease_owner: &str, outcome: &str, detail: Option<&str>, now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn.execute("UPDATE autopilot_runs SET state='settled', outcome=?1, detail=?2,
                notice_pending=1,
                notice_push_state=CASE WHEN ?1 IN ('missed_late','execution_uncertain') THEN 'pending' ELSE NULL END,
            next_at=NULL, lease_owner=NULL, lease_until=NULL, updated_at=?3
            WHERE run_id=?4 AND state='running' AND lease_owner=?5 AND lease_until>=?3",
            params![outcome, detail, run_ts(now), run_id, lease_owner])
            .map_err(|e| format!("settle claimed run: {e}"))?;
        Ok(n == 1)
    }

    /// Unknown persisted states can only be settled as unsupported_state.
    /// Running and settled states must never be included in expected_states.
    pub async fn settle_unclaimed_run(
        &self, run_id: &str, expected_states: &[&str], outcome: &str, detail: Option<&str>, now: DateTime<Utc>,
    ) -> Result<bool, String> {
        if expected_states.iter().any(|state| matches!(*state, RUN_RUNNING | RUN_SETTLED)) {
            return Err("unclaimed settlement cannot include running or settled states".into());
        }
        let conn = self.conn.lock().await;
        in_immediate_tx(&conn, "settle unclaimed run", |conn| {
            let state: Option<String> = conn.query_row("SELECT state FROM autopilot_runs WHERE run_id=?1",
                params![run_id], |r| r.get(0)).optional().map_err(|e| format!("read run state: {e}"))?;
            let Some(state) = state else { return Ok(false); };
            if matches!(state.as_str(), RUN_RUNNING | RUN_SETTLED) || !expected_states.contains(&state.as_str()) { return Ok(false); }
            if !is_known_run_state(&state) && outcome != "unsupported_state" {
                return Err("unknown run states must be settled as unsupported_state".into());
            }
            if state == RUN_AWAITING_APPROVAL {
                conn.execute("UPDATE autopilot_run_steps SET state='failed', finished_at=?1, detail=?2
                    WHERE run_id=?3 AND state='running'
                    AND step_index=(SELECT step_index FROM autopilot_runs WHERE run_id=?3)",
                    params![run_ts(now), detail, run_id]).map_err(|e| format!("fail approval step: {e}"))?;
            }
            let n = conn.execute("UPDATE autopilot_runs SET state='settled', outcome=?1, detail=?2,
                notice_pending=1,
                notice_push_state=CASE WHEN ?1 IN ('missed_late','execution_uncertain') THEN 'pending' ELSE NULL END,
                next_at=NULL, lease_owner=NULL, lease_until=NULL, updated_at=?3
                WHERE run_id=?4 AND state=?5 AND state NOT IN ('running','settled')",
                params![outcome, detail, run_ts(now), run_id, state]).map_err(|e| format!("settle unclaimed run: {e}"))?;
            Ok(n == 1)
        })
    }

    pub async fn list_run_notices_due(&self, now: DateTime<Utc>, limit: i64) -> Result<Vec<AutopilotRunRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!("SELECT {RUN_COLUMNS} FROM autopilot_runs
            WHERE state='settled' AND (notice_pending=1 OR (notice_push_state='pending'
                AND (notice_push_next_at IS NULL OR notice_push_next_at<=?1)))
            ORDER BY updated_at, run_id LIMIT ?2"))
            .map_err(|e| format!("prepare run notices: {e}"))?;
        let rows = stmt.query_map(params![run_ts(now), limit.max(0)], row_to_run)
            .map_err(|e| format!("query run notices: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("read run notices: {e}"))?;
        Ok(rows)
    }

    /// Atomically retire both owed channels without publishing event-derived data.
    pub async fn deny_run_notice(&self, run_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE autopilot_runs SET notice_pending=0,
            notice_push_state='scope_denied', notice_push_next_at=NULL
            WHERE run_id=?1 AND state='settled'", params![run_id])
            .map_err(|_| "settle scope-denied run notice".to_string())?;
        Ok(())
    }

    pub async fn clear_run_notice_pending(&self, run_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE autopilot_runs SET notice_pending=0 WHERE run_id=?1 AND state='settled'",
            params![run_id]).map_err(|e| format!("clear run notice: {e}"))?;
        Ok(())
    }

    /// A crashed final attempt is retired when its in-flight lease expires.
    pub async fn lease_run_notice_push(&self, run_id: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn.execute("UPDATE autopilot_runs
            SET notice_push_attempts=notice_push_attempts+1, notice_push_next_at=?1
            WHERE run_id=?2 AND state='settled' AND notice_push_state='pending'
                AND notice_push_attempts<?3 AND (notice_push_next_at IS NULL OR notice_push_next_at<=?4)",
            params![run_ts(now + Duration::seconds(60)), run_id,
                crate::autopilot_approval::MAX_NOTICE_PUSH_ATTEMPTS, run_ts(now)])
            .map_err(|e| format!("lease run notice push: {e}"))?;
        if n == 1 { return Ok(true); }
        conn.execute("UPDATE autopilot_runs SET notice_push_state='gave_up', notice_push_next_at=NULL
            WHERE run_id=?1 AND state='settled' AND notice_push_state='pending'
                AND notice_push_attempts>=?2 AND (notice_push_next_at IS NULL OR notice_push_next_at<=?3)",
            params![run_id, crate::autopilot_approval::MAX_NOTICE_PUSH_ATTEMPTS, run_ts(now)])
            .map_err(|e| format!("retire run notice push: {e}"))?;
        Ok(false)
    }

    pub async fn record_run_notice_push(
        &self, run_id: &str, attempt: i64, result: crate::autopilot_approval::NoticePush, now: DateTime<Utc>,
    ) -> Result<bool, String> {
        use crate::autopilot_approval::{NoticePush, MAX_NOTICE_PUSH_ATTEMPTS, notice_push_backoff};
        let (state, next_at) = match result {
            NoticePush::Sent | NoticePush::Queued => ("sent", None),
            NoticePush::ScopeDenied => ("scope_denied", None),
            NoticePush::NoTarget => ("no_target", None),
            NoticePush::Failed if attempt >= MAX_NOTICE_PUSH_ATTEMPTS => ("gave_up", None),
            NoticePush::Failed => ("pending", Some(run_ts(now + notice_push_backoff(attempt)))),
        };
        let conn = self.conn.lock().await;
        let n = conn.execute("UPDATE autopilot_runs SET notice_push_state=?1, notice_push_next_at=?2
            WHERE run_id=?3 AND state='settled' AND notice_push_state='pending' AND notice_push_attempts=?4",
            params![state, next_at, run_id, attempt]).map_err(|e| format!("record run notice push: {e}"))?;
        Ok(n == 1)
    }

    /// Retryable per-step history without changing the rule's fire counter.
    pub async fn append_step_history(&self, row: &AutopilotHistoryRow) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute("INSERT OR IGNORE INTO autopilot_history (id, rule_id, rule_name, triggered_at, result, details)
            VALUES (?1,?2,?3,?4,?5,?6)",
            params![row.id, row.rule_id, row.rule_name, row.triggered_at, row.result, row.details])
            .map_err(|e| format!("append step history: {e}"))?;
        Ok(())
    }
}

fn count_unsettled(conn: &Connection, rule_id: Option<&str>) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM autopilot_runs WHERE state!='settled' AND (?1 IS NULL OR rule_id=?1)",
        params![rule_id], |r| r.get(0)).map_err(|e| format!("count unsettled runs: {e}"))
}

fn list_run_ids(conn: &Connection, sql: &str, now: DateTime<Utc>, limit: i64) -> Result<Vec<String>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| format!("prepare run ids: {e}"))?;
    let rows = stmt.query_map(params![run_ts(now), limit.max(0)], |r| r.get(0))
        .map_err(|e| format!("query run ids: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("read run ids: {e}"))?;
    Ok(rows)
}

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<AutopilotRunRow> {
    Ok(AutopilotRunRow {
        run_id: r.get(0)?, rule_id: r.get(1)?, rule_name: r.get(2)?, rule_hash: r.get(3)?,
        event_name: r.get(4)?, event_ref: r.get(5)?, dedupe_key: r.get(6)?, fields_json: r.get(7)?,
        state: r.get(8)?, step_index: r.get(9)?, step_count: r.get(10)?, next_at: r.get(11)?,
        lease_owner: r.get(12)?, lease_until: r.get(13)?, owner_department: r.get(14)?,
        outcome: r.get(15)?, detail: r.get(16)?, created_at: r.get(17)?, updated_at: r.get(18)?,
        notice_pending: r.get::<_, i64>(19)? != 0, notice_push_state: r.get(20)?,
        notice_push_attempts: r.get(21)?, notice_push_next_at: r.get(22)?, gateway_origin: r.get(23)?,
    })
}

fn row_to_step(r: &rusqlite::Row<'_>) -> rusqlite::Result<AutopilotRunStepRow> {
    Ok(AutopilotRunStepRow {
        run_id: r.get(0)?, step_index: r.get(1)?, step_type: r.get(2)?, state: r.get(3)?,
        idempotency_key: r.get(4)?, dispatch_id: r.get(5)?, queue_message_id: r.get(6)?,
        started_at: r.get(7)?, finished_at: r.get(8)?, detail: r.get(9)?,
    })
}

#[cfg(test)]
mod tests;

fn mark_step_running(
    conn: &Connection, run_id: &str, step_index: i64, lease_owner: &str, now: DateTime<Utc>,
) -> Result<bool, String> {
    let n = conn.execute("UPDATE autopilot_run_steps SET state='running', started_at=?1
        WHERE run_id=?2 AND step_index=?3 AND state='pending'
        AND EXISTS (SELECT 1 FROM autopilot_runs WHERE run_id=?2 AND step_index=?3
            AND state='running' AND lease_owner=?4 AND lease_until>=?1)",
        params![run_ts(now), run_id, step_index, lease_owner])
        .map_err(|e| format!("mark step running: {e}"))?;
    Ok(n == 1)
}

impl super::RuleFence<'_> {
    /// Read the exact run/step binding without recursively locking the store.
    pub fn approval_run(&self, run_id: &str, step_index: i64, dispatch_id: &str) -> Result<Option<AutopilotRunRow>, String> {
        self.conn.query_row(&format!("SELECT {RUN_COLUMNS} FROM autopilot_runs
            WHERE run_id=?1 AND state='awaiting_approval' AND step_index=?2
            AND EXISTS (SELECT 1 FROM autopilot_run_steps WHERE run_id=?1
                AND step_index=?2 AND state='running' AND dispatch_id=?3)"),
            params![run_id, step_index, dispatch_id], row_to_run)
            .optional().map_err(|e| format!("read fenced approval run: {e}"))
    }

    /// Record the point of no return on this fence's already locked connection.
    pub fn mark_step_running(&self, run_id: &str, step_index: i64, lease_owner: &str, now: DateTime<Utc>) -> Result<bool, String> {
        mark_step_running(&self.conn, run_id, step_index, lease_owner, now)
    }

    /// Complete a pending (or recovered running) wait without exposing a running wait.
    /// The caller must drop this fence on any error, or release it on success.
    pub fn complete_wait_step(
        &self, run_id: &str, step_index: i64, lease_owner: &str, next_at: DateTime<Utc>, now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let n = self.conn.execute("UPDATE autopilot_run_steps SET state='done',
            started_at=COALESCE(started_at,?1), finished_at=?1
            WHERE run_id=?2 AND step_index=?3 AND step_type='wait' AND state IN ('pending','running')
            AND EXISTS (SELECT 1 FROM autopilot_runs WHERE run_id=?2 AND step_index=?3
                AND step_index+1<step_count AND state='running' AND lease_owner=?4 AND lease_until>=?1)",
            params![run_ts(now), run_id, step_index, lease_owner])
            .map_err(|e| format!("complete fenced wait: {e}"))?;
        if n == 0 { return Ok(false); }
        let n = self.conn.execute("UPDATE autopilot_runs SET state='waiting', step_index=step_index+1,
            next_at=?1, lease_owner=NULL, lease_until=NULL, updated_at=?2
            WHERE run_id=?3 AND step_index=?4 AND state='running' AND lease_owner=?5 AND lease_until>=?2",
            params![run_ts(next_at), run_ts(now), run_id, step_index, lease_owner])
            .map_err(|e| format!("advance fenced wait: {e}"))?;
        if n != 1 { return Err("run transition lost after completing wait".into()); }
        Ok(true)
    }
}
