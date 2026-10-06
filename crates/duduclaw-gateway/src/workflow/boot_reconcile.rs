//! Boot-time reconciliation of workflow projections against their authorities.
//!
//! Runs once from the gateway boot owner. It only fails closed: projections
//! whose ledger authority is revoked or missing become terminal, routines
//! without a live authority are disabled, run leases of the previous process
//! are dropped, and then the resident sweep runs once (undelivered handoffs,
//! decided or expired waits, unowned pending/running runs). It never enables
//! a routine and never moves a projection forward.
use super::*;

/// Counts of what one boot pass changed, for the boot log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BootReconcileReport {
    pub activations_revoked: usize,
    pub activations_failed: usize,
    pub routines_disabled: usize,
    pub outbox_delivered: usize,
    pub runs_blocked: usize,
    pub leases_cleared: usize,
    pub resumed: usize,
    pub requeued: usize,
    pub errors: Vec<String>,
}

impl WorkflowService {
    pub async fn reconcile_on_boot(&self) -> Result<BootReconcileReport, String> {
        let mut report = BootReconcileReport::default();
        // The gateway is the only run executor for this home, so every lease
        // left in the table belongs to a process that no longer exists.
        report.leases_cleared = self
            .store
            .with_transaction(|tx| {
                tx.execute("DELETE FROM workflow_run_leases", [])
                    .map_err(|e| e.to_string())
            })
            .await?;
        // Likewise every workflow wake-up the previous process had picked up
        // but not settled: nothing in this process is executing it.
        if self.home.join("message_queue.db").exists() {
            match crate::message_queue::MessageQueue::open(&self.home) {
                Ok(queue) => match queue.requeue_unsettled_with_prefix("workflow:").await {
                    Ok(n) => report.requeued += n,
                    Err(error) => report.errors.push(format!("queue: {error}")),
                },
                Err(error) => report.errors.push(format!("queue: {error}")),
            }
        }
        // Each phase only fails closed; one failing phase must not skip the
        // others (a bad projection used to stop routine disabling).
        if let Err(error) = self.reconcile_activations_on_boot(&mut report).await {
            report.errors.push(format!("activations: {error}"));
        }
        if let Err(error) = self.disable_unbacked_routines(&mut report).await {
            report.errors.push(format!("routines: {error}"));
        }
        let sweep = self.sweep().await;
        report.outbox_delivered += sweep.outbox_delivered;
        report.runs_blocked += sweep.runs_blocked;
        report.resumed += sweep.resumed;
        report.requeued += sweep.requeued;
        report.errors.extend(sweep.errors);
        Ok(report)
    }

    async fn all_activations(&self) -> Result<Vec<ActivationRecord>, String> {
        let raw: Vec<String> = self
            .store
            .with_connection(|c| {
                let mut q = c
                    .prepare("SELECT record_json FROM workflow_activations ORDER BY activation_id")
                    .map_err(|e| e.to_string())?;
                q.query_map([], |r| r.get(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())
            })
            .await?;
        raw.iter()
            .map(|s| serde_json::from_str(s).map_err(|_| "invalid activation projection".to_string()))
            .collect()
    }

    /// Live authority: an active ledger grant for exactly this spec and no
    /// revocation tombstone.
    async fn activation_authority_live(&self, record: &ActivationRecord) -> Result<bool, String> {
        let id = &record.request.activation_id;
        if self.broker.activation_revoked(id).await? {
            return Ok(false);
        }
        Ok(matches!(
            self.broker.current_revision_grant(id).await?,
            Some((_, spec, state)) if state == "active" && spec == record.request.spec
        ))
    }

    async fn reconcile_activations_on_boot(
        &self,
        report: &mut BootReconcileReport,
    ) -> Result<(), String> {
        for mut record in self.all_activations().await? {
            let id = record.request.activation_id.clone();
            let revoked = self.broker.activation_revoked(&id).await?
                || matches!(
                    self.broker.current_revision_grant(&id).await?,
                    Some((_, _, state)) if state == "revoked"
                );
            let live = self.activation_authority_live(&record).await?;
            let claims_grant = matches!(
                record.state,
                ActivationState::Authorized | ActivationState::Arming | ActivationState::Active
            );
            let target = match record.state {
                ActivationState::Revoked | ActivationState::Failed => None,
                // A suspended or expired activation keeps its reason; its
                // ledger grant is revoked on purpose (R-H1).
                ActivationState::Suspended | ActivationState::Expired => None,
                ActivationState::Revoking => Some(ActivationState::Revoked),
                _ if revoked => Some(ActivationState::Revoked),
                _ if claims_grant && !live => Some(ActivationState::Failed),
                _ => None,
            };
            let routine_live = live && record.state == ActivationState::Active && target.is_none();
            if let (Some(cron), false) = (&record.request.cron, routine_live) {
                self.disable_routine(&cron.cron_id, report).await?;
            }
            match target {
                Some(ActivationState::Revoked) => {
                    if record.state != ActivationState::Revoking {
                        record.state = ActivationState::Revoking;
                        self.store_activation(&record).await?;
                    }
                    record.state = ActivationState::Revoked;
                    self.store_activation(&record).await?;
                    report.activations_revoked += 1;
                }
                Some(state) => {
                    record.state = state;
                    record.error_code = Some("authority_missing_on_boot".into());
                    self.store_activation(&record).await?;
                    report.activations_failed += 1;
                }
                None => {}
            }
        }
        Ok(())
    }

    async fn disable_routine(
        &self,
        cron_id: &str,
        report: &mut BootReconcileReport,
    ) -> Result<(), String> {
        let store = crate::cron_store::CronStore::open(&self.home)?;
        if store.get(cron_id).await?.is_some_and(|row| row.enabled) {
            store.set_enabled(cron_id, false).await?;
            report.routines_disabled += 1;
        }
        Ok(())
    }

    /// A bound routine stays enabled only behind an active projection with the
    /// same material and a live ledger authority.
    async fn disable_unbacked_routines(&self, report: &mut BootReconcileReport) -> Result<(), String> {
        let store = crate::cron_store::CronStore::open(&self.home)?;
        for row in store.list_enabled().await? {
            let Some((activation_id, material_hash)) = store.workflow_binding(&row.id).await? else {
                continue;
            };
            let backed = match self.activation(&activation_id).await? {
                Some(record) => {
                    record.state == ActivationState::Active
                        && record.material_hash == material_hash
                        && self.activation_authority_live(&record).await?
                }
                None => false,
            };
            if !backed {
                self.disable_routine(&row.id, report).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn run_still_authorized(&self, run: &WorkflowRun) -> Result<(), String> {
        if let Some(activation_id) = &run.activation_id {
            let record = self
                .activation(activation_id)
                .await?
                .ok_or("workflow activation missing")?;
            if record.state != ActivationState::Active || !self.activation_authority_live(&record).await? {
                return Err("workflow_authority_revoked".into());
            }
            let grant = run.grant.as_ref().ok_or("active workflow grant missing")?;
            let (spec, state) = self.broker.inspect_revision_grant(grant).await?;
            if state != "active" || spec.hash() != record.request.spec.hash() {
                return Err("workflow_authority_revoked".into());
            }
        }
        self.runner.guard(run).await
    }
}
