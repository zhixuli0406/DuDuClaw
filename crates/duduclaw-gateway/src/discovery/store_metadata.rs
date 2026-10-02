//! Durable provenance and frozen round plans. Missing historical metadata is
//! deliberately ineligible for comparisons or cross-task default promotion.
use super::{DiscoveryStore, PolicyEvalRow, StoreError};
use crate::discovery::contracts::PolicyVersion;
use crate::discovery::online::RunIdentity;
use crate::discovery::policy::GridPlan;
use crate::discovery::policy_runner::CandidateEvaluation;
use crate::discovery::score::BETA_GRID;
use crate::discovery::tree::{Direction, World, WorldTree};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub run_id: String,
    pub goal: String,
    pub agent_id: String,
    pub scorer_name: String,
    pub scorer_hash: String,
    pub direction: Direction,
    pub budget_calls: Option<u32>,
    pub budget_usd: Option<f64>,
    pub budget_secs: Option<u64>,
    pub status: String,
    pub best_cell_id: Option<String>,
    pub created_at: String,
    pub task_id: Option<String>,
    pub creator_id: Option<String>,
    pub creator_origin: Option<String>,
    pub approved_root_id: Option<String>,
    pub has_unconfined: bool,
    pub provenance_verified: bool,
    pub runtime: Option<String>,
    pub configured_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoundMetadata {
    pub policy_params: Option<serde_json::Value>,
    pub plan_reason: Option<String>,
    pub full_grid: bool,
    pub completion: String,
    pub comparison_available: bool,
}

impl DiscoveryStore {
    /// Called with host-created identity, never model- or HTTP-deserialized
    /// authority. Existing attribution cannot be overwritten by a later wake.
    pub fn attach_run_identity(&self, identity: &RunIdentity) -> Result<(), StoreError> {
        if identity.creator_id.is_empty() || identity.creator_origin.is_empty() {
            return Err(StoreError::Corrupt("missing trusted discovery creator".into()));
        }
        let changed = self.conn.execute(
            "UPDATE discovery_runs SET task_id=?2,creator_id=?3,creator_origin=?4,approved_root_id=?5
             WHERE run_id=?1 AND creator_id IS NULL",
            params![identity.run_id, identity.task_id, identity.creator_id,
                identity.creator_origin, identity.approved_root_id],
        )?;
        if changed != 1 {
            return Err(StoreError::Corrupt("discovery identity already bound or run missing".into()));
        }
        Ok(())
    }

    pub fn load_run(&self, run_id: &str) -> Result<Option<RunRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT run_id,goal,agent_id,scorer_name,scorer_hash,direction,budget_calls,budget_usd,
             budget_secs,status,best_cell_id,created_at,task_id,creator_id,creator_origin,
             approved_root_id,has_unconfined,provenance_verified,runtime,configured_model
             FROM discovery_runs WHERE run_id=?1",
        )?;
        let mut rows = statement.query(params![run_id])?;
        let Some(row) = rows.next()? else { return Ok(None); };
        let direction: String = row.get(5)?;
        let calls: Option<i64> = row.get(6)?;
        let seconds: Option<i64> = row.get(8)?;
        Ok(Some(RunRecord {
            run_id: row.get(0)?, goal: row.get(1)?, agent_id: row.get(2)?,
            scorer_name: row.get(3)?, scorer_hash: row.get(4)?,
            direction: Direction::parse(&direction)
                .ok_or_else(|| StoreError::Corrupt("invalid stored direction".into()))?,
            budget_calls: calls.map(|value| super::to_u32(value, "budget_calls")).transpose()?,
            budget_usd: row.get(7)?,
            budget_secs: seconds.map(|value| super::to_u64(value, "budget_secs")).transpose()?,
            status: row.get(9)?, best_cell_id: row.get(10)?, created_at: row.get(11)?,
            task_id: row.get(12)?, creator_id: row.get(13)?, creator_origin: row.get(14)?,
            approved_root_id: row.get(15)?, has_unconfined: row.get(16)?,
            provenance_verified: row.get(17)?,
            runtime: row.get(18)?, configured_model: row.get(19)?,
        }))
    }

    pub fn list_run_worlds(&self, run_id: &str) -> Result<Vec<World>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT round FROM discovery_rounds WHERE run_id=?1 ORDER BY round",
        )?;
        let rounds = statement.query_map(params![run_id], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rounds.into_iter().map(|round| {
            self.load_world(run_id, super::to_u32(round, "round")?)?
                .ok_or_else(|| StoreError::Corrupt("world disappeared during query".into()))
        }).collect()
    }

    pub fn mark_unconfined(&self, run_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE discovery_runs SET has_unconfined=1,provenance_verified=0 WHERE run_id=?1",
            params![run_id],
        )?;
        Ok(())
    }

    pub fn set_run_runtime(&self, run_id: &str, runtime: &str, model: &str)
        -> Result<(), StoreError> {
        if runtime.trim().is_empty() || model.trim().is_empty() {
            return Err(StoreError::Corrupt("missing discovery runtime configuration".into()));
        }
        self.conn.execute(
            "UPDATE discovery_runs SET runtime=?2,configured_model=?3
             WHERE run_id=?1 AND runtime IS NULL AND configured_model IS NULL",
            params![run_id, runtime, model],
        )?;
        Ok(())
    }

    /// A positive attestation is allowed only after the host completed the
    /// entire confined run; imported and interrupted runs remain unverified.
    pub fn verify_run_provenance(&self, run_id: &str) -> Result<(), StoreError> {
        let tx=self.conn.unchecked_transaction()?;
        self.conn.execute(
            "UPDATE discovery_runs SET provenance_verified=(
             has_unconfined=0 AND status='complete' AND creator_id IS NOT NULL
             AND EXISTS(SELECT 1 FROM discovery_nodes WHERE run_id=?1)
             AND NOT EXISTS(SELECT 1 FROM discovery_nodes WHERE run_id=?1
                 AND (isolation_backend IS NULL OR isolation_backend!='container'))
             AND NOT EXISTS(SELECT 1 FROM discovery_rounds WHERE run_id=?1
                 AND (completion!='complete' OR full_grid=0 OR policy_params IS NULL)))
             WHERE run_id=?1",
            params![run_id],
        )?;
        // Dream writes while the run is still active. Recompute the ledger's
        // qualification only after the host's terminal attestation, in the
        // same transaction. Unknown historical source provenance stays false.
        self.conn.execute(
            "UPDATE discovery_policy_evals AS e SET comparison_available=(
                e.valid=1 AND e.violation IS NULL AND e.out_of_support=0 AND e.context_mismatch_rate=0
                AND e.pareto_auc IS NOT NULL AND e.pareto_reward IS NOT NULL
                AND e.evaluation_origin IN ('builtin','incumbent','task_development','night_replay')
                AND EXISTS(SELECT 1 FROM discovery_rounds r JOIN discovery_runs u USING(run_id)
                    WHERE r.run_id=e.run_id AND r.round=e.round AND u.status='complete'
                    AND u.provenance_verified=1 AND u.has_unconfined=0
                    AND r.full_grid=1 AND r.completion='complete' AND r.policy_params IS NOT NULL))
             WHERE e.run_id=?1",
            params![run_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_configured_model(&self, run_id: &str, cell_id: &str, model: &str)
        -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE discovery_nodes SET configured_model=?3 WHERE run_id=?1 AND cell_id=?2",
            params![run_id, cell_id, model],
        )?;
        Ok(())
    }

    pub fn freeze_round_plan(&self, world: &World, version: &PolicyVersion,
        source: Option<&str>, plan: &GridPlan) -> Result<(), StoreError> {
        self.freeze_round_plan_with_origin(world, version, source, plan, &[])
    }

    pub fn freeze_round_plan_with_origin(&self, world: &World, version: &PolicyVersion,
        source: Option<&str>, plan: &GridPlan, origins: &[String]) -> Result<(), StoreError> {
        if version.policy_id != world.policy_id || plan.branch_count != world.branch_count
            || plan.refine_count != world.refine_count {
            return Err(StoreError::Corrupt("round plan/version mismatch".into()));
        }
        match (source, &version.source_sha256) {
            (Some(code), Some(expected)) => {
                use sha2::{Digest, Sha256};
                if format!("{:x}", Sha256::digest(code.as_bytes())) != *expected {
                    return Err(StoreError::Corrupt("frozen policy source hash mismatch".into()));
                }
            },
            (None, None) if version.policy_id == crate::discovery::policy::BASELINE_POLICY_ID => {},
            _ => return Err(StoreError::Corrupt("missing frozen policy source".into())),
        }
        let frozen = serde_json::to_string(&serde_json::json!({
            "schema":"duduclaw.discovery.round_plan.v1", "version":version,
            "source":source, "beta":world.beta, "plan":plan, "origin_task_ids":origins,
        }))?;
        let changed = self.conn.execute(
            "UPDATE discovery_rounds SET policy_params=?3,plan_reason=?4,completion='running'
             WHERE run_id=?1 AND round=?2 AND policy_params IS NULL",
            params![world.run_id, world.round, frozen, plan.reason],
        )?;
        if changed != 1 {
            return Err(StoreError::Corrupt("round plan already frozen or missing".into()));
        }
        Ok(())
    }

    /// Completion is a statement about all planned cells, independent of the
    /// policy's normal Done event. A pruned round is partial, even if valid.
    pub fn finish_round(&self, world: &World) -> Result<(), StoreError> {
        let expected = u64::from(world.branch_count) * (u64::from(world.refine_count) + 1);
        let (count, evaluated): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),COALESCE(SUM(evaluated),0) FROM discovery_nodes
             WHERE run_id=?1 AND round=?2", params![world.run_id, world.round],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let full = super::to_u64(count, "nodes")? == expected && count == evaluated;
        self.conn.execute(
            "UPDATE discovery_rounds SET full_grid=?3,completion=?4 WHERE run_id=?1 AND round=?2",
            params![world.run_id, world.round, full, if full { "complete" } else { "partial" }],
        )?;
        Ok(())
    }

    pub fn load_round_metadata(&self, run_id: &str, round: u32)
        -> Result<Option<RoundMetadata>, StoreError> {
        let row = self.conn.query_row(
            "SELECT r.policy_params,r.plan_reason,r.full_grid,r.completion,
             (r.full_grid=1 AND r.completion='complete' AND r.policy_params IS NOT NULL
              AND u.provenance_verified=1 AND u.has_unconfined=0)
             FROM discovery_rounds r JOIN discovery_runs u USING(run_id)
             WHERE r.run_id=?1 AND r.round=?2", params![run_id, round],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get(1)?, row.get(2)?,
                row.get(3)?, row.get(4)?)),
        ).optional()?;
        row.map(|(frozen, reason, full_grid, completion, comparison_available)| {
            Ok(RoundMetadata { policy_params: frozen.map(|json| serde_json::from_str(&json)).transpose()?,
                plan_reason: reason, full_grid, completion, comparison_available })
        }).transpose()
    }

    /// Preserve invalid candidates at every input (world,beta), rather than
    /// omitting their failed evaluations from the production ledger.
    pub fn record_candidate_evaluation(&self, policy_id: &str, policy_params: &str,
        worlds: &[WorldTree], evaluation: &CandidateEvaluation) -> Result<(), StoreError> {
        self.record_candidate_evaluation_with_origin(policy_id, policy_params, worlds, evaluation, &[], "unknown")
    }

    pub fn record_candidate_evaluation_with_origin(&self, policy_id: &str, policy_params: &str,
        worlds: &[WorldTree], evaluation: &CandidateEvaluation, origins: &[String], origin: &str) -> Result<(), StoreError> {
        self.record_candidate_evaluation_with_occurrence(policy_id, None, policy_params, worlds, evaluation, origins, origin)
    }

    /// Canonical frozen policy identity and a development occurrence are
    /// separate authority fields. The occurrence never becomes policy_id.
    pub fn record_candidate_evaluation_with_occurrence(&self, policy_id: &str, occurrence: Option<&str>, policy_params: &str,
        worlds: &[WorldTree], evaluation: &CandidateEvaluation, origins: &[String], origin: &str) -> Result<(), StoreError> {
        if let Some(occurrence)=occurrence {
            if occurrence.is_empty() || occurrence.len()>512 || occurrence.chars().any(char::is_control) {
                return Err(StoreError::Corrupt("invalid policy development occurrence".into()));
            }
            let params:serde_json::Value=serde_json::from_str(policy_params)?;
            if params.get("policy_id").and_then(|value|value.as_str())!=Some(policy_id)
                || params.get("occurrence_id").and_then(|value|value.as_str())!=Some(occurrence) {
                return Err(StoreError::Corrupt("policy identity/occurrence params mismatch".into()));
            }
        }
        let tx = self.conn.unchecked_transaction()?;
        for tree in worlds {
            let world = tree.world();
            let score = evaluation.worlds.iter().find(|score|
                score.run_id == world.run_id && score.round == world.round);
            for beta in BETA_GRID {
                let point = score.and_then(|score| score.points.iter().find(|point| point.beta == beta));
                let valid = evaluation.valid && point.is_some();
                let id = self.insert_policy_eval(&PolicyEvalRow {
                    policy_id: policy_id.into(), policy_params: policy_params.into(),
                    run_id: world.run_id.clone(), round: world.round, beta,
                    attainment: point.map_or(0.0, |point| point.attainment),
                    work: point.map_or(0.0, |point| point.work),
                    probes: point.map_or(0, |point| point.probes),
                    decision_rounds: point.map_or(0, |point| point.decision_rounds),
                    effective_sequential_rounds: point.map_or(0, |point| point.effective_sequential_rounds),
                    parallel_penalty: point.map_or(0.0, |point| point.parallel_penalty),
                    context_mismatch_rate: point.map_or(0.0, |point| point.context_mismatch_rate),
                    pareto_auc: score.filter(|_| valid).map(|score| score.pareto_auc),
                    pareto_reward: score.filter(|_| valid).map(|score| score.pareto_reward),
                    out_of_support: score.is_none_or(|score| score.out_of_support),
                })?;
                let comparable = valid && evaluation.violation.is_none()
                    && matches!(origin,"builtin"|"incumbent"|"task_development"|"night_replay")
                    && self.load_round_metadata(&world.run_id, world.round)?
                    .is_some_and(|metadata| metadata.comparison_available)
                    && score.is_some_and(|score| !score.out_of_support)
                    && point.is_some_and(|point| point.context_mismatch_rate == 0.0);
                self.set_policy_eval_origin(id, origins, origin)?;
                self.conn.execute(
                    "UPDATE discovery_policy_evals SET valid=?2,violation=?3,comparison_available=?4,occurrence_id=?5 WHERE id=?1",
                    params![id, valid, evaluation.violation, comparable, occurrence],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn record_verified_artifact(&self, run_id: &str, cell_id: &str, hash: &str)
        -> Result<(), StoreError> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StoreError::Corrupt("invalid verified artifact digest".into()));
        }
        self.conn.execute(
            "INSERT INTO discovery_artifacts(run_id,cell_id,sha256,created_at) VALUES(?1,?2,?3,?4)",
            params![run_id, cell_id, hash, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn load_artifact_hash(&self, run_id: &str, cell_id: &str)
        -> Result<Option<String>, StoreError> {
        Ok(self.conn.query_row(
            "SELECT sha256 FROM discovery_artifacts WHERE run_id=?1 AND cell_id=?2",
            params![run_id, cell_id], |row| row.get(0),
        ).optional()?)
    }
}
