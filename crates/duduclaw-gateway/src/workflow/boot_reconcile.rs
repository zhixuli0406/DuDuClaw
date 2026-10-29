//! Boot-time reconciliation of workflow projections against their authorities.
//!
//! Runs once from the gateway boot owner. A proven missing, revoked, or
//! materially invalid grant fails closed that one projection and its routine.
//! A ledger or other store failure does not rewrite projections or disable
//! routines: that phase stops, the other phases still run, and the reconcile
//! itself fails. Run leases of the previous process are dropped, and then the
//! resident sweep runs once (undelivered handoffs, decided or expired waits,
//! unowned pending/running runs). It never enables a routine and never moves
//! a projection forward.
use super::*;
use crate::approval::RevisionGrantReadError;

const MAX_BOOT_RECONCILE_ERRORS: usize = 200;
const BOOT_TOOL_NAME_CHARS: usize = 120;

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
    /// How many errors were dropped after the report reached its cap (200).
    pub errors_omitted: usize,
}

fn push_boot_error(report: &mut BootReconcileReport, error: impl Into<String>) {
    if report.errors.len() < MAX_BOOT_RECONCILE_ERRORS {
        report.errors.push(error.into());
    } else {
        report.errors_omitted = report.errors_omitted.saturating_add(1);
    }
}

fn sanitize_tool_name(tool: &str) -> String {
    let cleaned: String = tool
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{2028}' && *c != '\u{2029}')
        .collect();
    duduclaw_core::truncate_chars(&cleaned, BOOT_TOOL_NAME_CHARS)
}

/// `Some` is an authority-material problem and may fail closed one item.
/// `None` is infrastructure: do not rewrite, fail the phase.
fn authority_boot_label(error: &RevisionGrantReadError) -> Option<String> {
    match error {
        RevisionGrantReadError::StandingToolRefused { tool } => Some(format!(
            "standing_tool_refused tool={}",
            sanitize_tool_name(tool)
        )),
        RevisionGrantReadError::InvalidCurrentGrant => Some("invalid current grant".to_string()),
        RevisionGrantReadError::MaterialCorrupt => {
            Some("current grant material corrupt".to_string())
        }
        RevisionGrantReadError::Store(_) => None,
    }
}

fn infrastructure_text(error: RevisionGrantReadError) -> String {
    match error {
        RevisionGrantReadError::Store(message) => message,
        other => other.to_string(),
    }
}

enum BootAuthority {
    Proven { revoked: bool, live: bool },
    Material(String),
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
                    Err(error) => push_boot_error(&mut report, format!("queue: {error}")),
                },
                Err(error) => push_boot_error(&mut report, format!("queue: {error}")),
            }
        }
        // Store failures stop only their phase. Later phases still run, and
        // the reconcile returns Err so boot logs a failure.
        let mut phase_errors = Vec::new();
        if let Err(error) = self.reconcile_activations_on_boot(&mut report).await {
            phase_errors.push(format!("activations: {error}"));
        }
        if let Err(error) = self.disable_unbacked_routines(&mut report).await {
            phase_errors.push(format!("routines: {error}"));
        }
        let sweep = self.sweep().await;
        report.outbox_delivered += sweep.outbox_delivered;
        report.runs_blocked += sweep.runs_blocked;
        report.resumed += sweep.resumed;
        report.requeued += sweep.requeued;
        for error in sweep.errors {
            push_boot_error(&mut report, error);
        }
        for error in &phase_errors {
            push_boot_error(&mut report, error.clone());
        }
        if !phase_errors.is_empty() {
            let mut message = phase_errors.join("; ");
            if report.errors_omitted > 0 {
                message.push_str(&format!("; errors omitted: {}", report.errors_omitted));
            }
            return Err(message);
        }
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
    async fn activation_authority_live(
        &self,
        record: &ActivationRecord,
    ) -> Result<bool, RevisionGrantReadError> {
        let id = &record.request.activation_id;
        if self.broker.activation_revoked(id).await? {
            return Ok(false);
        }
        Ok(matches!(
            self.broker.current_revision_grant(id).await?,
            Some((_, spec, state)) if state == "active" && spec == record.request.spec
        ))
    }

    /// A tombstone short-circuits before the grant is parsed. Any failure of
    /// that read is a store error. A grant read then either proves the row or
    /// yields one authority label; store errors stay `Err`.
    async fn read_boot_authority(
        &self,
        record: &ActivationRecord,
    ) -> Result<BootAuthority, String> {
        let id = &record.request.activation_id;
        let tombstone = self
            .broker
            .activation_revoked(id)
            .await
            .map_err(infrastructure_text)?;
        if tombstone {
            return Ok(BootAuthority::Proven {
                revoked: true,
                live: false,
            });
        }
        let grant = match self.broker.current_revision_grant(id).await {
            Ok(grant) => grant,
            Err(error) => {
                // Bind the label first. The borrow of `error` must end before
                // the store message is moved into the phase error.
                let label = authority_boot_label(&error);
                return match label {
                    Some(label) => Ok(BootAuthority::Material(label)),
                    None => Err(infrastructure_text(error)),
                };
            }
        };
        let revoked = grant
            .as_ref()
            .is_some_and(|(_, _, state)| state == "revoked");
        let live = grant.as_ref().is_some_and(|(_, spec, state)| {
            state == "active" && spec == &record.request.spec
        });
        Ok(BootAuthority::Proven { revoked, live })
    }

    async fn reconcile_activations_on_boot(
        &self,
        report: &mut BootReconcileReport,
    ) -> Result<(), String> {
        for mut record in self.all_activations().await? {
            let id = record.request.activation_id.clone();
            // Authority material fails closed this row. A store error stops
            // the phase before this row or any later row is rewritten.
            let (revoked, live) = match self.read_boot_authority(&record).await {
                Ok(BootAuthority::Proven { revoked, live }) => (revoked, live),
                Ok(BootAuthority::Material(label)) => {
                    push_boot_error(report, format!("activation {id}: {label}"));
                    (false, false)
                }
                Err(error) => return Err(error),
            };
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
            match target {
                Some(ActivationState::Revoked) => {
                    let mut stored = true;
                    if record.state != ActivationState::Revoking {
                        record.state = ActivationState::Revoking;
                        if let Err(error) = self.store_activation(&record).await {
                            push_boot_error(report, format!("activation {id}: {error}"));
                            stored = false;
                        }
                    }
                    if stored {
                        record.state = ActivationState::Revoked;
                        if let Err(error) = self.store_activation(&record).await {
                            push_boot_error(report, format!("activation {id}: {error}"));
                        } else {
                            report.activations_revoked += 1;
                        }
                    }
                }
                Some(state) => {
                    record.state = state;
                    record.error_code = Some("authority_missing_on_boot".into());
                    if let Err(error) = self.store_activation(&record).await {
                        push_boot_error(report, format!("activation {id}: {error}"));
                    } else {
                        report.activations_failed += 1;
                    }
                }
                None => {}
            }
            // The projection write is already attempted. A disable failure
            // stays on this routine and the loop continues.
            if let (Some(cron), false) = (&record.request.cron, routine_live) {
                if let Err(error) = self.disable_routine(&cron.cron_id, report).await {
                    push_boot_error(report, format!("routine {}: {error}", cron.cron_id));
                }
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
            let cron_id = row.id.as_str();
            // A store read stops the phase and leaves this routine enabled.
            // An authority defect disables only this routine.
            let backed = match store.workflow_binding(cron_id).await {
                Ok(None) => continue,
                Err(error) => return Err(error),
                Ok(Some((activation_id, material_hash))) => {
                    match self.activation(&activation_id).await {
                        Ok(Some(record)) => {
                            if record.state != ActivationState::Active
                                || record.material_hash != material_hash
                            {
                                false
                            } else {
                                match self.activation_authority_live(&record).await {
                                    Ok(live) => live,
                                    Err(error) => {
                                        let label = authority_boot_label(&error);
                                        if let Some(label) = label {
                                            push_boot_error(
                                                report,
                                                format!("routine {cron_id}: {label}"),
                                            );
                                            false
                                        } else {
                                            return Err(infrastructure_text(error));
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None) => false,
                        Err(error) => return Err(error),
                    }
                }
            };
            if !backed {
                if let Err(error) = self.disable_routine(cron_id, report).await {
                    push_boot_error(report, format!("routine {cron_id}: {error}"));
                }
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

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::{BOOT_TOOL_NAME_CHARS, authority_boot_label, push_boot_error};
    use crate::approval::{DecisionContext, EffectTemplate, GrantRef, GrantSpec, payload_hash};
    use chrono::Utc;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    const LEDGER_MARKER: &str = "ledger-body-marker";

    fn activation(id: &str, tool: &str, with_grant: bool) -> ActivationRecord {
        let expires = (Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let spec = GrantSpec {
            schema_version: 1,
            workflow_id: "weekly".into(),
            workflow_revision: 1,
            revision_hash: "revision".into(),
            skill_hash: "skill".into(),
            fixtures_digest: "fixtures".into(),
            activation_id: id.into(),
            actor: "alice".into(),
            operator_context: DecisionContext {
                channel: "dashboard".into(),
                account_id: "acct".into(),
                conversation_id: "thread".into(),
                principal_id: "owner".into(),
            },
            creator_grant: CreatorGrantSnapshot {
                actor: "alice".into(),
                allowed_tools: BTreeSet::from([tool.into()]),
                policy_revision: "policy".into(),
            },
            audience: vec!["owner".into()],
            templates: BTreeMap::from([(
                "step".into(),
                EffectTemplate {
                    step_id: "step".into(),
                    tool: tool.into(),
                    input_schema: TypedSchema::Integer,
                    resource_scope: BTreeMap::from([("marker".into(), json!(LEDGER_MARKER))]),
                    receipt_adapter_version: 1,
                },
            )]),
            input_max_age_seconds: 600,
            fixtures_expires_at: expires.clone(),
            budget: CostBudget {
                per_run_micros: 1000,
                monthly_micros: 10_000,
                max_consecutive_failures: 3,
            },
            expires_at: expires.clone(),
            policy_revision: "policy".into(),
        };
        let spec_hash = spec.hash();
        let definition = WorkflowDefinition {
            schema_version: 1,
            workflow_id: "weekly".into(),
            revision: 1,
            skill_revision_hash: "skill".into(),
            input_schema: TypedSchema::Integer,
            output_schema: TypedSchema::Integer,
            required_capabilities: BTreeSet::new(),
            steps: Vec::new(),
        };
        ActivationRecord {
            request: ActivationRequest {
                activation_id: id.into(),
                draft_id: "draft".into(),
                draft_hash: "draft-hash".into(),
                revision: ApprovedWorkflowRevision {
                    definition,
                    revision_hash: "revision".into(),
                    owner: "alice".into(),
                    creator_grant: spec.creator_grant.clone(),
                    audience: spec.audience.clone(),
                    fixtures_digest: "fixtures".into(),
                    acceptance_id: format!("acceptance-{id}"),
                    accepted_at: expires.clone(),
                    expires_at: expires,
                },
                spec,
                fixture_results: Vec::new(),
                fixture_assertions: Vec::new(),
                cron: None,
            },
            acceptance_id: format!("acceptance-{id}"),
            material_hash: format!("material-{id}"),
            state: ActivationState::Active,
            grant: with_grant.then(|| GrantRef {
                grant_id: format!("grant-{id}"),
                epoch: 1,
                spec_hash,
            }),
            error_code: None,
            policy_digests: None,
            suspension: None,
            expiry_notice_at: None,
        }
    }

    /// Schema first, then the grant rows, then the broker the service keeps.
    /// Closing a second connection while the broker is open drops its locks.
    fn insert_grants(home: &std::path::Path, records: &[ActivationRecord]) {
        drop(crate::approval::ApprovalBroker::open(home).unwrap());
        let conn = rusqlite::Connection::open(home.join("approvals.db")).unwrap();
        for record in records.iter().filter(|record| record.grant.is_some()) {
            let spec = &record.request.spec;
            let grant = record.grant.as_ref().unwrap();
            conn.execute(
                "INSERT INTO workflow_revision_grants(grant_id,activation_id,workflow_id,workflow_revision,
                    revision_hash,skill_hash,fixtures_digest,activation_acceptance_id,grant_spec_json,
                    grant_spec_hash,actor_principal,audience_hash,authority_epoch,state,expires_at,created_at)
                    VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,'active',?13,?14)",
                rusqlite::params![
                    grant.grant_id,
                    spec.activation_id,
                    spec.workflow_id,
                    spec.workflow_revision,
                    spec.revision_hash,
                    spec.skill_hash,
                    spec.fixtures_digest,
                    record.acceptance_id,
                    serde_json::to_string(spec).unwrap(),
                    grant.spec_hash,
                    spec.actor,
                    payload_hash(&json!(spec.audience)),
                    spec.expires_at,
                    Utc::now().to_rfc3339(),
                ],
            )
            .unwrap();
        }
    }

    async fn harness(records: Vec<ActivationRecord>) -> (tempfile::TempDir, WorkflowService) {
        let home = tempfile::tempdir().unwrap();
        insert_grants(home.path(), &records);
        let service = WorkflowService::new(
            home.path().to_path_buf(),
            std::env::current_exe().unwrap(),
            Arc::new(WorkflowStore::open(home.path()).unwrap()),
            Arc::new(crate::approval::ApprovalBroker::open(home.path()).unwrap()),
        )
        .unwrap();
        for record in &records {
            service.store_activation(record).await.unwrap();
        }
        (home, service)
    }

    async fn stored(service: &WorkflowService, id: &str) -> (ActivationState, Option<String>) {
        let record = service.activation(id).await.unwrap().unwrap();
        (record.state, record.error_code)
    }

    /// The first activation's stored grant names a delete tool, so its ledger
    /// read fails. The next one has no authority and must still be failed.
    #[tokio::test]
    async fn unreadable_grant_does_not_skip_later_activations() {
        let records = vec![
            activation("act-a-poison", "shared_wiki_delete", true),
            activation("act-b-missing", "tasks_update", false),
            activation("act-c-healthy", "tasks_update", true),
        ];
        let (_home, service) = harness(records).await;
        let report = service.reconcile_on_boot().await.unwrap();
        assert_eq!(stored(&service, "act-a-poison").await.0, ActivationState::Failed);
        assert_eq!(
            stored(&service, "act-a-poison").await.1.as_deref(),
            Some("authority_missing_on_boot")
        );
        assert_eq!(stored(&service, "act-b-missing").await.0, ActivationState::Failed);
        assert_eq!(
            stored(&service, "act-b-missing").await.1.as_deref(),
            Some("authority_missing_on_boot")
        );
        assert_eq!(stored(&service, "act-c-healthy").await, (ActivationState::Active, None));
        assert_eq!(report.activations_failed, 2, "{report:?}");
        assert_eq!(report.activations_revoked, 0, "{report:?}");
        let own: Vec<_> = report
            .errors
            .iter()
            .filter(|error| error.starts_with("activation "))
            .collect();
        assert_eq!(own.len(), 1, "{report:?}");
        assert!(own[0].contains("act-a-poison"), "{own:?}");
        assert!(own[0].contains("standing_tool_refused tool=shared_wiki_delete"), "{own:?}");
        assert!(!own[0].contains("single-call approval"), "{own:?}");
        assert!(!own[0].contains(LEDGER_MARKER), "{own:?}");
        assert!(!own[0].contains('\n'), "{own:?}");
    }

    /// A projection write that fails for the first activation is recorded.
    /// Its routine is still disabled, and later activations are reconciled.
    #[tokio::test]
    async fn projection_write_error_does_not_skip_later_activations() {
        let mut failing = activation("act-a-write-fail", "tasks_update", false);
        failing.request.cron = Some(RoutineSchedule {
            cron_id: "cron-write-fail".into(),
            expression: "0 9 * * 1".into(),
            timezone: "UTC".into(),
        });
        let records = vec![
            failing,
            activation("act-b-missing", "tasks_update", false),
            activation("act-c-healthy", "tasks_update", true),
        ];
        let (home, service) = harness(records).await;
        enable_plain_routine(home.path(), "cron-write-fail", "2026-01-01T00:00:00Z").await;
        service
            .store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE workflow_activations SET material_hash='tampered' WHERE activation_id=?1",
                    ["act-a-write-fail"],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
        let report = service.reconcile_on_boot().await.unwrap();
        assert_eq!(stored(&service, "act-b-missing").await.0, ActivationState::Failed);
        assert_eq!(
            stored(&service, "act-b-missing").await.1.as_deref(),
            Some("authority_missing_on_boot")
        );
        assert_eq!(stored(&service, "act-c-healthy").await, (ActivationState::Active, None));
        assert!(!routine_enabled(home.path(), "cron-write-fail").await);
        assert_eq!(report.activations_failed, 1, "{report:?}");
        assert_eq!(report.routines_disabled, 1, "{report:?}");
        let own: Vec<_> = report
            .errors
            .iter()
            .filter(|error| error.starts_with("activation "))
            .collect();
        assert_eq!(own.len(), 1, "{report:?}");
        assert!(own[0].contains("act-a-write-fail"), "{own:?}");
        assert!(own[0].contains("activation projection terminal or changed"), "{own:?}");
        assert!(!report.errors.iter().any(|error| error.starts_with("activations:")), "{report:?}");
    }

    /// Dropping the grant table is a store failure. No projection is rewritten
    /// and no routine is disabled, but the reconcile itself fails after both
    /// phases have run.
    #[tokio::test]
    async fn ledger_store_error_fails_reconcile_without_rewriting() {
        let records = vec![
            activation("act-store-healthy", "tasks_update", true),
            activation("act-store-missing", "tasks_update", false),
        ];
        let (home, service) = harness(records).await;
        enable_bound_routine(
            home.path(),
            "cron-store-healthy",
            "2026-01-01T00:00:00Z",
            "act-store-healthy",
            "material-act-store-healthy",
        )
        .await;
        enable_bound_routine(
            home.path(),
            "cron-store-missing",
            "2026-01-02T00:00:00Z",
            "act-store-missing",
            "material-act-store-missing",
        )
        .await;
        let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
        conn.execute_batch(
            "ALTER TABLE workflow_revision_grants RENAME TO workflow_revision_grants_hidden;",
        )
        .unwrap();
        drop(conn);
        let error = service.reconcile_on_boot().await.unwrap_err();
        assert!(error.contains("activations:"), "{error}");
        assert!(error.contains("routines:"), "{error}");
        assert!(error.contains("no such table"), "{error}");
        assert!(!error.contains("authority_missing_on_boot"), "{error}");
        assert_eq!(
            stored(&service, "act-store-healthy").await,
            (ActivationState::Active, None)
        );
        assert_eq!(
            stored(&service, "act-store-missing").await,
            (ActivationState::Active, None)
        );
        assert!(routine_enabled(home.path(), "cron-store-healthy").await);
        assert!(routine_enabled(home.path(), "cron-store-missing").await);
    }

    /// The first activation's routine cannot be disabled. Its projection is
    /// still marked Failed, and the next activation is still reconciled.
    #[tokio::test]
    async fn routine_disable_error_keeps_failed_projection_and_continues() {
        let mut failing = activation("act-a-disable-fail", "tasks_update", false);
        failing.request.cron = Some(RoutineSchedule {
            cron_id: "cron-disable-fail".into(),
            expression: "0 9 * * 1".into(),
            timezone: "UTC".into(),
        });
        let records = vec![
            failing,
            activation("act-b-missing", "tasks_update", false),
            activation("act-c-healthy", "tasks_update", true),
        ];
        let (home, service) = harness(records).await;
        enable_plain_routine(home.path(), "cron-disable-fail", "2026-01-01T00:00:00Z").await;
        let conn = rusqlite::Connection::open(home.path().join("cron_tasks.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_disable_for_test BEFORE UPDATE OF enabled ON cron_tasks
             WHEN OLD.id = 'cron-disable-fail'
             BEGIN SELECT RAISE(ABORT, 'disable blocked for test'); END;",
        )
        .unwrap();
        drop(conn);
        let report = service.reconcile_on_boot().await.unwrap();
        assert_eq!(stored(&service, "act-a-disable-fail").await.0, ActivationState::Failed);
        assert_eq!(
            stored(&service, "act-a-disable-fail").await.1.as_deref(),
            Some("authority_missing_on_boot")
        );
        assert_eq!(stored(&service, "act-b-missing").await.0, ActivationState::Failed);
        assert_eq!(stored(&service, "act-c-healthy").await, (ActivationState::Active, None));
        assert!(routine_enabled(home.path(), "cron-disable-fail").await);
        assert_eq!(report.activations_failed, 2, "{report:?}");
        assert_eq!(report.routines_disabled, 0, "{report:?}");
        let disable_errors: Vec<_> = report
            .errors
            .iter()
            .filter(|error| error.starts_with("routine cron-disable-fail:"))
            .collect();
        assert_eq!(disable_errors.len(), 1, "{report:?}");
        assert!(disable_errors[0].contains("disable blocked for test"), "{disable_errors:?}");
        assert!(!report.errors.iter().any(|error| error.starts_with("activations:")), "{report:?}");
    }

    async fn enable_bound_routine(
        home: &std::path::Path,
        cron_id: &str,
        created_at: &str,
        activation_id: &str,
        material_hash: &str,
    ) {
        let store = crate::cron_store::CronStore::open(home).unwrap();
        let mut row = crate::cron_store::CronTaskRow::new(
            cron_id.to_string(),
            cron_id.to_string(),
            "alice".into(),
            "0 9 * * 1".into(),
            "run".into(),
        );
        row.enabled = false;
        row.created_at = created_at.to_string();
        row.updated_at = created_at.to_string();
        store.insert(&row).await.unwrap();
        store.bind_workflow(cron_id, activation_id, material_hash).await.unwrap();
        store.set_enabled(cron_id, true).await.unwrap();
    }

    async fn routine_enabled(home: &std::path::Path, cron_id: &str) -> bool {
        crate::cron_store::CronStore::open(home)
            .unwrap()
            .get(cron_id)
            .await
            .unwrap()
            .unwrap()
            .enabled
    }

    /// The first routine's activation is still Active, but its stored grant
    /// names a delete tool, so the authority read fails. The next routine has
    /// lost its backing and must still be disabled.
    #[tokio::test]
    async fn unreadable_grant_does_not_skip_later_routines() {
        let records = vec![
            activation("act-a-poison", "shared_wiki_delete", true),
            activation("act-b-missing", "tasks_update", false),
            activation("act-c-healthy", "tasks_update", true),
        ];
        let (home, service) = harness(records).await;
        enable_bound_routine(
            home.path(),
            "cron-poison",
            "2026-01-01T00:00:00Z",
            "act-a-poison",
            "material-act-a-poison",
        )
        .await;
        enable_bound_routine(
            home.path(),
            "cron-missing",
            "2026-01-02T00:00:00Z",
            "act-b-missing",
            "material-act-b-missing",
        )
        .await;
        enable_bound_routine(
            home.path(),
            "cron-healthy",
            "2026-01-03T00:00:00Z",
            "act-c-healthy",
            "material-act-c-healthy",
        )
        .await;
        let mut report = BootReconcileReport::default();
        service.disable_unbacked_routines(&mut report).await.unwrap();
        assert!(!routine_enabled(home.path(), "cron-poison").await);
        assert!(!routine_enabled(home.path(), "cron-missing").await);
        assert!(routine_enabled(home.path(), "cron-healthy").await);
        assert_eq!(report.routines_disabled, 2, "{report:?}");
        let own: Vec<_> = report.errors.iter().filter(|error| error.starts_with("routine ")).collect();
        assert_eq!(own.len(), 1, "{report:?}");
        assert!(own[0].starts_with("routine cron-poison:"), "{own:?}");
        assert!(own[0].contains("standing_tool_refused tool=shared_wiki_delete"), "{own:?}");
        assert!(!own[0].contains("single-call approval"), "{own:?}");
        assert!(!own[0].contains(LEDGER_MARKER), "{own:?}");
        assert!(!own[0].contains('\n'), "{own:?}");
    }

    async fn enable_plain_routine(home: &std::path::Path, cron_id: &str, created_at: &str) {
        let store = crate::cron_store::CronStore::open(home).unwrap();
        let mut row = crate::cron_store::CronTaskRow::new(
            cron_id.to_string(),
            cron_id.to_string(),
            "alice".into(),
            "0 9 * * 1".into(),
            "run".into(),
        );
        row.enabled = false;
        row.created_at = created_at.to_string();
        row.updated_at = created_at.to_string();
        store.insert(&row).await.unwrap();
        store.set_enabled(cron_id, true).await.unwrap();
    }

    #[test]
    fn boot_report_caps_errors_and_tool_names() {
        let mut report = BootReconcileReport::default();
        for i in 0..205 {
            push_boot_error(&mut report, format!("activation {i}: x"));
        }
        assert_eq!(report.errors.len(), 200);
        assert_eq!(report.errors_omitted, 5);
        let noisy = format!("shared_wiki_delete\n{}\u{0001}", "工具".repeat(150));
        let label = authority_boot_label(&crate::approval::RevisionGrantReadError::StandingToolRefused {
            tool: noisy,
        })
        .unwrap();
        assert!(label.starts_with("standing_tool_refused tool=shared_wiki_delete"));
        assert!(!label.contains('\n'));
        assert!(!label.contains('\u{0001}'));
        let prefix = "standing_tool_refused tool=";
        assert_eq!(label.chars().count(), prefix.chars().count() + BOOT_TOOL_NAME_CHARS);
        assert!(authority_boot_label(&crate::approval::RevisionGrantReadError::Store(
            "database is locked".into()
        ))
        .is_none());
    }
}
