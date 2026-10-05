//! Gates every step of a run re-checks against current authority.
use super::super::*;
use super::WorkflowRunner;
use crate::approval::policy_revision;
use chrono::{DateTime, Utc};
use rusqlite::params;

/// What a particular check point needs beyond the always-on gates.
#[derive(Default)]
pub(super) struct GuardPoint<'a> {
    /// Enforce the run input's age (run start and before effects).
    pub input_freshness: bool,
    /// `observed_at` of read steps whose outputs feed this step.
    pub read_observations: Vec<&'a str>,
    /// This dispatch's own time limit, separate from the waiting window.
    pub segment_deadline: Option<DateTime<Utc>>,
    /// Re-read the stored status so a cancellation stops the next step.
    pub live_status: bool,
}

/// Outcome of reading the employee's authority snapshot against a run's.
enum PolicyRead {
    Same,
    /// Two clean reads in a row agree, and differ from the run's.
    Changed,
    /// Not readable cleanly after a few short retries.
    Unreadable,
}

const POLICY_READ_ATTEMPTS: usize = 4;
const POLICY_READ_DELAY_MS: u64 = 250;

/// One read: `Some(revision)` when every policy file parsed, else `None`.
fn clean_policy_revision(home: &std::path::Path, actor: &str) -> Option<String> {
    use crate::approval::policy_snapshot as snapshot;
    let digests = snapshot::policy_digests(home, actor).ok()?;
    if snapshot::has_unparseable(&digests) {
        return None;
    }
    Some(snapshot::revision_of(&digests))
}

async fn confirm_policy(home: &std::path::Path, actor: &str, expected: &str) -> PolicyRead {
    if policy_revision(home, actor).ok().as_deref() == Some(expected) {
        return PolicyRead::Same;
    }
    let mut previous: Option<String> = None;
    for attempt in 0..POLICY_READ_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(POLICY_READ_DELAY_MS)).await;
        }
        match clean_policy_revision(home, actor) {
            Some(current) if current == expected => return PolicyRead::Same,
            Some(current) => {
                if previous.as_deref() == Some(current.as_str()) {
                    return PolicyRead::Changed;
                }
                previous = Some(current);
            }
            None => previous = None,
        }
    }
    PolicyRead::Unreadable
}

impl WorkflowRunner {
    /// Every gate, as checked when a run is enqueued or handed off.
    pub async fn guard(&self, run: &WorkflowRun) -> Result<(), String> {
        self.guard_at(
            run,
            &GuardPoint {
                input_freshness: true,
                ..GuardPoint::default()
            },
        )
        .await
    }

    pub(super) async fn guard_at(
        &self,
        run: &WorkflowRun,
        point: &GuardPoint<'_>,
    ) -> Result<(), String> {
        if point.live_status {
            let stored = self
                .store
                .get_run(&run.run_id)
                .await?
                .ok_or("workflow run missing")?;
            if stored.status == RunStatus::Cancelled {
                return Err(super::super::run_control::RUN_CANCELLED.into());
            }
        }
        if point.segment_deadline.is_some_and(|d| d <= Utc::now()) {
            return Err(super::WORKFLOW_SEGMENT_EXPIRED.into());
        }
        if DateTime::parse_from_rfc3339(&run.deadline_at)
            .map_err(|_| "invalid workflow deadline")?
            <= Utc::now()
        {
            return Err("workflow_deadline_expired".into());
        }
        // The ledger refuses any charge that would pass the cap, so reaching
        // it exactly is allowed; only a run already over it stops here.
        if run.cost.total()? > run.budget.per_run_micros {
            return Err("workflow_budget_exhausted".into());
        }
        if let Some(activation_id) = &run.activation_id {
            let state = super::super::suspension::activation_state(&self.store, activation_id)
                .await?
                .map(|r| r.state);
            match state {
                Some(ActivationState::Active) => {}
                Some(ActivationState::Suspended) => {
                    return Err(super::super::suspension::SUSPENDED_ERROR.into());
                }
                Some(ActivationState::Expired) => {
                    return Err(super::super::suspension::EXPIRED_ERROR.into());
                }
                _ => return Err("workflow_authority_revoked".into()),
            }
        }
        // R-M6: a differing snapshot suspends only when confirmed (two clean
        // reads that agree); a read caught mid-write is retried as transient.
        match Box::pin(confirm_policy(&self.home, &run.actor, &run.policy_revision)).await {
            PolicyRead::Same => {}
            PolicyRead::Changed => {
                if let Some(activation_id) = &run.activation_id {
                    Box::pin(super::super::suspension::suspend_for_policy(
                        &self.store,
                        &self.broker,
                        &self.home,
                        activation_id,
                    ))
                    .await?;
                }
                return Err("workflow_policy_changed".into());
            }
            PolicyRead::Unreadable => {
                return Err(super::super::run_control::POLICY_UNREADABLE.into());
            }
        }
        let draft = self
            .store
            .draft_for_workflow(&run.workflow_id, run.revision)
            .await?
            .ok_or("workflow source draft missing")?;
        if draft.owner != run.actor
            || draft.audience != run.audience
            || draft.revision_hash != run.workflow_hash
        {
            return Err("workflow source draft authority changed".into());
        }
        let context = run
            .decision_context
            .as_ref()
            .ok_or("workflow decision context missing")?;
        let principal = crate::review_evidence::audience::trusted_dashboard_principal(
            &self.home,
            &context.principal_id,
        )?;
        crate::review_evidence::audience::authorize_workflow_audience(
            &self.home,
            &principal,
            &draft.source_task,
            &draft.audience,
        )
        .await?;
        let (_, skill) = crate::workflow_draft_context::installed_skill_revision(
            &self.home,
            &draft.owner,
            &draft.skill_id,
        )?;
        if skill != run.skill_hash {
            return Err("workflow_skill_changed".into());
        }
        // E-H5c: the source task and its artifacts are the evidence a human
        // reviewed before accepting; after activation the accepted revision
        // stands on its own and later edits to the task do not stop runs.
        if run.activation_id.is_none() {
            self.guard_source_evidence(&draft).await?;
        }
        if let Some(grant) = &run.grant {
            self.guard_grant(run, grant, point).await?;
        }
        if let Some(task) = &run.task {
            let store = crate::task_store::TaskStore::open(&self.home)?;
            let current = store
                .authority_snapshot(&task.task_id)
                .await?
                .ok_or("workflow task missing")?;
            if current.revision != task.revision
                || current.hash != task.snapshot_hash
                || !current.eligible
            {
                return Err("workflow_task_authority_changed".into());
            }
        }
        Ok(())
    }

    async fn guard_source_evidence(&self, draft: &crate::workflow_drafts::WorkflowDraft) -> Result<(), String> {
        let snapshot = self
            .store
            .review_snapshot(&draft.source_snapshot_id)
            .await?
            .ok_or("workflow source evidence missing")?;
        let current = crate::task_store::TaskStore::open(&self.home)?
            .authority_snapshot(&draft.source_task)
            .await?
            .ok_or("workflow source task missing")?;
        if snapshot.snapshot_hash != draft.source_evidence_hash
            || snapshot.authority_revision != current.revision
            || snapshot.authority_snapshot_hash != current.hash
            || snapshot.artifacts.is_empty()
            || snapshot
                .current_artifacts(&self.home, &current)
                .iter()
                .any(|a| a.integrity != crate::review_evidence::IntegrityStatus::Current)
        {
            return Err("workflow_source_evidence_changed".into());
        }
        Ok(())
    }

    async fn guard_grant(
        &self,
        run: &WorkflowRun,
        grant: &crate::approval::GrantRef,
        point: &GuardPoint<'_>,
    ) -> Result<(), String> {
        let (spec, state) = self.broker.inspect_revision_grant(grant).await?;
        spec.validate()?;
        if state != "active" {
            return Err("workflow_grant_revoked".into());
        }
        let max_age = chrono::Duration::seconds(spec.input_max_age_seconds as i64);
        let stale = |raw: &str| -> Result<bool, String> {
            let observed = DateTime::parse_from_rfc3339(raw)
                .map_err(|_| "invalid workflow input timestamp")?
                .with_timezone(&Utc);
            Ok(observed > Utc::now() || Utc::now() - observed > max_age)
        };
        if point.input_freshness && stale(&run.input_observed_at)? {
            return Err("workflow_input_expired".into());
        }
        for observed in &point.read_observations {
            if stale(observed)? {
                return Err("workflow_read_data_expired".into());
            }
        }
        let failure_sql = super::super::consecutive_failure_sql("");
        let (monthly, statuses): (u64, Vec<String>) = self
            .store
            .with_connection(|c| {
                // E-H3: the ledger is the only authority for money spent.
                let monthly = super::super::cost_ledger::month_total_in(
                    c,
                    "",
                    &run.workflow_id,
                    &super::super::cost_ledger::current_month(),
                )?;
                let mut q = c.prepare(&failure_sql).map_err(|e| e.to_string())?;
                let statuses = q
                    .query_map(
                        params![
                            run.workflow_id,
                            run.run_id,
                            run.activation_id,
                            spec.budget.max_consecutive_failures
                        ],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?;
                Ok((monthly, statuses))
            })
            .await?;
        if monthly > spec.budget.monthly_micros {
            return Err("workflow_monthly_budget_exhausted".into());
        }
        if statuses.len() >= spec.budget.max_consecutive_failures as usize
            && statuses.iter().all(|s| s != "succeeded")
        {
            return Err("workflow_consecutive_failure_limit".into());
        }
        Ok(())
    }
}
