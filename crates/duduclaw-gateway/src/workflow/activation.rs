//! Activation DTOs. Effect authority lives solely in the approval ledger.
use super::schema::*;
use crate::approval::{GrantRef, GrantSpec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationRequest {
    pub activation_id: String,
    pub draft_id: String,
    pub draft_hash: String,
    pub revision: ApprovedWorkflowRevision,
    pub spec: GrantSpec,
    pub fixture_results: Vec<FixtureRunEvidence>,
    pub fixture_assertions: Vec<FixtureAssertionResult>,
    pub cron: Option<RoutineSchedule>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineSchedule {
    pub cron_id: String,
    pub expression: String,
    pub timezone: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationState {
    Prepared,
    GrantPrepared,
    Authorized,
    Arming,
    Active,
    Revoking,
    Revoked,
    Failed,
    /// The employee's authority changed after acceptance, or runs kept ending
    /// on a limit; no run starts until an Admin accepts a new request (see
    /// `suspension`). Only `revoking`/`revoked` may follow.
    Suspended,
    /// The activation's validity (`[workflow] activation_days`) ended; renew
    /// by requesting and approving it again. Only `revoking`/`revoked` may
    /// follow.
    Expired,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationRecord {
    pub request: ActivationRequest,
    pub acceptance_id: String,
    pub material_hash: String,
    pub state: ActivationState,
    pub grant: Option<GrantRef>,
    pub error_code: Option<String>,
    /// Authority snapshot per category when the activation was requested;
    /// compared on drift to name what changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_digests: Option<crate::approval::policy_snapshot::PolicyDigests>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspension: Option<super::suspension::ActivationSuspension>,
    /// When Admins were told this activation is about to expire (once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_notice_at: Option<String>,
}

/// Default and bounds of `config.toml [workflow] activation_days` (U8).
pub const DEFAULT_ACTIVATION_DAYS: i64 = 30;
pub const MAX_ACTIVATION_DAYS: i64 = 365;

/// How many days an activation stays valid. A missing key means the
/// default; a value that is not an integer in 1–365, or an unreadable
/// `config.toml`, refuses the activation (a configuration error).
pub fn activation_days(home: &std::path::Path) -> Result<i64, String> {
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DEFAULT_ACTIVATION_DAYS),
        Err(_) => return Err("config.toml unreadable: [workflow] activation_days unknown".into()),
    };
    let table: toml::Table = raw
        .parse()
        .map_err(|_| "config.toml unparseable: [workflow] activation_days unknown")?;
    match table
        .get("workflow")
        .and_then(toml::Value::as_table)
        .and_then(|w| w.get("activation_days"))
    {
        None => Ok(DEFAULT_ACTIVATION_DAYS),
        Some(toml::Value::Integer(days)) if (1..=MAX_ACTIVATION_DAYS).contains(days) => Ok(*days),
        Some(_) => Err("[workflow] activation_days must be an integer from 1 to 365".into()),
    }
}

pub fn activation_payload(request: &ActivationRequest) -> serde_json::Value {
    serde_json::json!({
        "kind": "workflow_activation",
        "activation_id": request.activation_id,
        "draft_id": request.draft_id,
        "revision_hash": request.revision.revision_hash,
        "fixtures_digest": request.revision.fixtures_digest,
        "spec_hash": request.spec.hash(),
        "material_hash": crate::approval::payload_hash(
            &serde_json::to_value(request).expect("activation serializable")
        ),
        "request": request
    })
}

impl super::WorkflowService {
    pub async fn activation_for_draft(
        &self,
        draft_id: &str,
        revision: i64,
    ) -> Result<Option<ActivationRecord>, String> {
        self.store
            .with_connection(|c| {
                use rusqlite::OptionalExtension;
                let raw: Option<String> = c
                    .query_row(
                        "SELECT record_json FROM workflow_activations WHERE json_extract(record_json,
                            '$.request.draft_id')=?1 AND revision=?2 ORDER BY rowid DESC LIMIT 1",
                        rusqlite::params![draft_id, revision],
                        |r| r.get(0)
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;
                raw.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                    .transpose()
            })
            .await
    }
    pub async fn activation(&self, id: &str) -> Result<Option<ActivationRecord>, String> {
        self.store
            .with_connection(|c| {
                use rusqlite::OptionalExtension;
                let raw: Option<String> = c
                    .query_row(
                        "SELECT record_json FROM workflow_activations WHERE activation_id=?1",
                        rusqlite::params![id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;
                raw.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                    .transpose()
            })
            .await
    }
    async fn validate_activation(&self, request: &ActivationRequest) -> Result<(), String> {
        use super::*;
        use chrono::{DateTime, Utc};
        let draft = self
            .store
            .draft(&request.draft_id, request.revision.definition.revision)
            .await?
            .ok_or("workflow activation draft missing")?;
        if request.activation_id != request.spec.activation_id
            || draft.definition != request.revision.definition
            || draft.revision_hash != request.spec.revision_hash
            || draft.creator_grant != request.spec.creator_grant
            || draft.audience != request.spec.audience
            || draft.owner != request.spec.actor
            || draft.budget != request.spec.budget
        {
            return Err("workflow activation fixed material differs".into());
        }
        if request.draft_hash != draft.draft_hash
            || request.spec.templates != draft.effect_templates
            || request.cron != draft.routine
            || request.spec.input_max_age_seconds != draft.input_max_age_seconds
        {
            return Err("workflow activation fixed draft changed".into());
        }
        // A-H-1: each effect may only change the one record the card names.
        for template in draft.effect_templates.values() {
            super::effect_targets::check_pinned_target(&draft.definition, template)?;
        }
        // R-M3: a definition that needs more than the limits allow can never
        // finish a run; refuse it now instead of failing every slot.
        if let Some(code) = super::cost_ledger::structural_limit_error(
            &draft.definition,
            draft.budget.per_run_micros,
            &super::cost_ledger::load_pricing(&self.home),
        ) {
            return Err(format!("workflow activation exceeds limits: {code}"));
        }
        let (_, skill_hash) = crate::workflow_draft_context::installed_skill_revision(
            &self.home,
            &draft.owner,
            &draft.skill_id,
        )?;
        if skill_hash != draft.definition.skill_revision_hash {
            return Err("workflow activation skill changed".into());
        }
        let snapshot = self
            .store
            .review_snapshot(&draft.source_snapshot_id)
            .await?
            .ok_or("workflow source review missing")?;
        let tasks = crate::task_store::TaskStore::open(&self.home)?;
        let current = tasks
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
            return Err("workflow activation source evidence stale".into());
        }
        let principal = crate::review_evidence::audience::trusted_dashboard_principal(
            &self.home,
            &request.spec.operator_context.principal_id,
        )?;
        crate::review_evidence::audience::authorize_workflow_audience(
            &self.home,
            &principal,
            &draft.source_task,
            &draft.audience,
        )
        .await?;
        request.spec.validate()?;
        if crate::approval::policy_revision(&self.home, &request.spec.actor)?
            != request.spec.policy_revision
        {
            return Err("workflow activation policy changed".into());
        }
        let mut kinds = std::collections::BTreeSet::new();
        let mut complete = Vec::new();
        for evidence in &request.fixture_results {
            let stored = self.fixture_evidence(&evidence.run_id).await?;
            if stored != *evidence
                || DateTime::parse_from_rfc3339(&evidence.expires_at)
                    .map_err(|_| "invalid fixture expiry")?
                    <= Utc::now()
                || !kinds.insert(serde_json::to_string(&evidence.kind).map_err(|e| e.to_string())?)
            {
                return Err("activation fixture stale, replaced or duplicated".into());
            }
            let fixture = draft
                .fixtures
                .iter()
                .find(|f| f.fixture_id == evidence.fixture_id)
                .ok_or("activation fixture unknown")?;
            if evidence.workflow_hash != draft.revision_hash
                || evidence.skill_hash != draft.definition.skill_revision_hash
                || evidence.input_hash != fixture.input_hash
                || evidence.assertion_hash != fixture.assertion_hash
                || evidence.policy_revision != draft.creator_grant.policy_revision
            {
                return Err("activation fixture material changed".into());
            }
            complete.extend(crate::workflow_drafts::evaluate_fixture(
                &fixture.assertions,
                evidence,
            ));
        }
        if kinds.len() != 5
            || complete != request.fixture_assertions
            || complete
                .iter()
                .any(|r| r.outcome != AssertionOutcome::Matched)
        {
            return Err("activation requires all five verified fixture assertions".into());
        }
        let digest = crate::approval::payload_hash(
            &serde_json::json!({"results":request.fixture_results,"assertions":complete}),
        );
        if digest != request.spec.fixtures_digest || digest != request.revision.fixtures_digest {
            return Err("activation fixture digest mismatch".into());
        }
        if let Some(cron) = &request.cron {
            cron.timezone
                .parse::<chrono_tz::Tz>()
                .map_err(|_| "invalid routine timezone")?;
            cron.expression
                .parse::<cron::Schedule>()
                .map_err(|_| "invalid routine cron")?;
        }
        Ok(())
    }
    pub async fn request_activation(
        &self,
        request: ActivationRequest,
        binding: crate::approval::ExecutionBinding,
    ) -> Result<String, String> {
        self.validate_activation(&request).await?;
        if binding.actor_principal != request.spec.actor
            || binding.decision_context != request.spec.operator_context
            || binding.payload_hash != crate::approval::payload_hash(&activation_payload(&request))
        {
            return Err("activation trusted binding does not match request".into());
        }
        if let Some(existing) = self.activation(&request.activation_id).await? {
            if existing.request != request {
                return Err("activation ID reused for changed material".into());
            }
            if !existing.acceptance_id.is_empty() {
                return Ok(existing.acceptance_id);
            }
        }
        let material_hash = crate::approval::payload_hash(
            &serde_json::to_value(&request).map_err(|e| e.to_string())?,
        );
        let mut record = ActivationRecord {
            request,
            acceptance_id: String::new(),
            material_hash,
            state: ActivationState::Prepared,
            grant: None,
            error_code: None,
            policy_digests: None,
            suspension: None,
            expiry_notice_at: None,
        };
        // The snapshot that `validate_activation` just matched against the
        // revision; suspension later names the categories that drifted.
        let digests =
            crate::approval::policy_snapshot::policy_digests(&self.home, &record.request.spec.actor)?;
        if crate::approval::policy_snapshot::revision_of(&digests) != record.request.spec.policy_revision {
            return Err("workflow activation policy changed during request".into());
        }
        record.policy_digests = Some(digests);
        self.store_activation(&record).await?;
        #[cfg(test)]
        super::security_race_tests::checkpoint("request_prepared").await;
        let id = self
            .broker
            .request_bound(
                crate::approval::RequestKind::Approval,
                &record.request.spec.actor,
                "接受固定工作流版本與排程範圍",
                activation_payload(&record.request),
                binding,
            )
            .await?;
        record.acceptance_id = id.to_string();
        self.store_activation(&record).await?;
        // F5-A: a content-free notice to Admins' verified chats; the card
        // itself is decided in the dashboard only.
        if let Ok(Some(card)) = self.broker.get(&id).await {
            let text = crate::approval_notify::activation_notice_body(&card, false);
            super::workflow_notify::send(
                &self.home,
                &record.request.spec.actor,
                super::workflow_notify::admin_links(&self.home),
                &text,
            )
            .await;
        }
        Ok(id.to_string())
    }
    pub(super) async fn store_activation(&self, record: &ActivationRecord) -> Result<(), String> {
        self.store
            .with_transaction(|tx| {
                let n = tx
                    .execute(
                        "INSERT INTO workflow_activations VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                            ON CONFLICT(activation_id) DO UPDATE SET acceptance_id=excluded.acceptance_id,
                            grant_id=excluded.grant_id,grant_epoch=excluded.grant_epoch,state=excluded.state,
                            record_json=excluded.record_json
                            WHERE workflow_activations.material_hash=excluded.material_hash
                            AND (workflow_activations.state NOT IN ('revoked','revoking','suspended','expired')
                            OR workflow_activations.state=excluded.state OR workflow_activations.state='revoking'
                            AND excluded.state='revoked' OR workflow_activations.state IN ('suspended','expired')
                            AND excluded.state IN ('revoking','revoked'))",
                        rusqlite::params![
                            record.request.activation_id,
                            record.request.revision.definition.workflow_id,
                            record.request.revision.definition.revision,
                            record.material_hash,
                            record.acceptance_id,
                            record.grant.as_ref().map(|g| &g.grant_id),
                            record.grant.as_ref().map(|g| g.epoch),
                            serde_json::to_value(record.state)
                                .map_err(|e| e.to_string())?
                                .as_str()
                                .unwrap(),
                            serde_json::to_string(record).map_err(|e| e.to_string())?
                        ]
                    )
                    .map_err(|e| e.to_string())?;
                if n != 1 {
                    return Err("activation projection terminal or changed".into());
                }
                Ok(())
            })
            .await
    }
    pub async fn commit_activation(&self, id: &str) -> Result<ActivationRecord, String> {
        use crate::approval::{AcceptedRevisionGrant, ApprovalId, ApprovalStatus};
        let mut record = self.activation(id).await?.ok_or("activation not found")?;
        #[cfg(test)]
        super::security_race_tests::checkpoint("prepared_read").await;
        if record.state == ActivationState::Active {
            self.validate_activation(&record.request).await?;
            let grant = record
                .grant
                .as_ref()
                .ok_or("active projection lacks ledger authority")?;
            let (spec, state) = self.broker.inspect_revision_grant(grant).await?;
            if state != "active" || spec.hash() != record.request.spec.hash() {
                return Err("active workflow authority changed".into());
            }
            return Ok(record);
        }
        // R-H1: a suspended or expired activation never returns to active;
        // only a new request with a new Admin approval does.
        if matches!(
            record.state,
            ActivationState::Revoked
                | ActivationState::Revoking
                | ActivationState::Failed
                | ActivationState::Suspended
                | ActivationState::Expired
        ) {
            return Err("activation no longer eligible".into());
        }
        self.validate_activation(&record.request).await?;
        let acceptance = self
            .broker
            .get(&ApprovalId::from(record.acceptance_id.clone()))
            .await?
            .ok_or("activation acceptance missing")?;
        if acceptance.status != ApprovalStatus::Approved {
            return Err("activation requires real human acceptance".into());
        }
        let mut revision = record.request.revision.clone();
        revision.acceptance_id = record.acceptance_id.clone();
        revision.accepted_at = acceptance
            .decided_at
            .ok_or("activation has no human decision timestamp")?;
        self.store
            .with_transaction(|tx| {
                tx.execute(
                    "INSERT INTO workflow_revisions VALUES(?1,?2,?3,?4) ON CONFLICT(workflow_id,revision) DO NOTHING",
                    rusqlite::params![
                        revision.definition.workflow_id,
                        revision.definition.revision,
                        revision.revision_hash,
                        serde_json::to_string(&revision).map_err(|e| e.to_string())?
                    ]
                )
                .map_err(|e| e.to_string())?;
                let raw: String = tx
                    .query_row(
                        "SELECT record_json FROM workflow_revisions WHERE workflow_id=?1 AND revision=?2",
                        rusqlite::params![
                            revision.definition.workflow_id,
                            revision.definition.revision
                        ],
                        |r| r.get(0)
                    )
                    .map_err(|e| e.to_string())?;
                if serde_json::from_str::<super::ApprovedWorkflowRevision>(&raw)
                    .map_err(|e| e.to_string())?
                    != revision
                {
                    return Err("accepted workflow revision already differs".into());
                }
                Ok(())
            })
            .await?;
        let accepted = AcceptedRevisionGrant {
            activation_id: record.request.activation_id.clone(),
            acceptance_id: record.acceptance_id.clone(),
            revision,
            spec: record.request.spec.clone(),
            spec_hash: record.request.spec.hash(),
        };
        // Ledger activation may have committed before a projection write failed.
        let current = self
            .broker
            .current_revision_grant(&record.request.activation_id)
            .await?;
        if let Some((reference, spec, state)) = current {
            if spec != record.request.spec {
                return Err("activation ledger immutable material differs".into());
            }
            if state == "revoked" {
                return Err("activation ledger already revoked".into());
            }
            record.grant = Some(reference);
            record.state = if state == "active" {
                ActivationState::Authorized
            } else {
                ActivationState::GrantPrepared
            };
        } else {
            #[cfg(test)]
            super::security_race_tests::checkpoint("current_none").await;
            #[cfg(test)]
            super::security_race_tests::checkpoint("before_prepare").await;
            record.grant = Some(self.broker.prepare_revision_grant(&accepted).await?);
            record.state = ActivationState::GrantPrepared;
        }
        self.store_activation(&record).await?;
        if record.state == ActivationState::GrantPrepared {
            record.grant = Some(
                self.broker
                    .activate_revision_grant(record.grant.as_ref().unwrap())
                    .await?,
            );
            record.state = ActivationState::Authorized;
            self.store_activation(&record).await?;
        }
        if let Some(cron) = &record.request.cron {
            let store = crate::cron_store::CronStore::open(&self.home)?;
            if store.get(&cron.cron_id).await?.is_none() {
                let mut row = crate::cron_store::CronTaskRow::new(
                    cron.cron_id.clone(),
                    record.request.revision.definition.workflow_id.clone(),
                    record.request.spec.actor.clone(),
                    cron.expression.clone(),
                    String::new(),
                );
                row.enabled = false;
                row.cron_timezone = Some(cron.timezone.clone());
                store.insert(&row).await?;
            }
            let existing = store
                .get(&cron.cron_id)
                .await?
                .ok_or("routine row unavailable")?;
            if existing.agent_id != record.request.spec.actor
                || existing.cron != cron.expression
                || existing.cron_timezone.as_deref() != Some(cron.timezone.as_str())
                || existing.trigger_kind != "time"
                || !existing.task.is_empty()
            {
                return Err("routine row is not immutable activation projection".into());
            }
            store
                .bind_workflow(
                    &cron.cron_id,
                    &record.request.activation_id,
                    &record.material_hash,
                )
                .await?;
            record.state = ActivationState::Arming;
            self.store_activation(&record).await?;
            #[cfg(test)]
            super::security_race_tests::checkpoint("before_cron_enable").await;
            store.set_enabled(&cron.cron_id, true).await?;
            #[cfg(test)]
            super::security_race_tests::checkpoint("after_cron_enable").await;
        }
        // Arming is a cross-store saga: a stale committer must compensate its
        // own enable after a revoker has already disabled the routine.
        let final_result = async {
            let reference = record.grant.as_ref().ok_or("activation grant missing")?;
            let (spec, state) = self.broker.inspect_revision_grant(reference).await?;
            if state != "active" || spec != record.request.spec {
                return Err("activation authority revoked during arming".to_string());
            }
            record.state = ActivationState::Active;
            self.store_activation(&record).await
        }
        .await;
        if let Err(error) = final_result {
            if let Some(cron) = &record.request.cron {
                crate::cron_store::CronStore::open(&self.home)?
                    .set_enabled(&cron.cron_id, false)
                    .await?;
            }
            return Err(error);
        }
        Ok(record)
    }
    pub async fn revoke_activation(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<ActivationRecord, String> {
        let mut record = self.activation(id).await?.ok_or("activation missing")?;
        if record.state == ActivationState::Revoked {
            return Ok(record);
        }
        if let Some(revoked) = self
            .broker
            .revoke_workflow_activation(id, &record.request.spec.hash(), reason)
            .await?
        {
            record.grant = Some(revoked.reference);
            // Revocation does not stop an effect that already began; say so
            // on the projection instead of dropping the list.
            if !revoked.executing_operations.is_empty() {
                record.error_code = Some(format!(
                    "revoked_with_executing_operations:{}",
                    revoked.executing_operations.join(",")
                ));
            }
        }
        #[cfg(test)]
        super::security_race_tests::checkpoint("after_revoke_ledger").await;
        record.state = ActivationState::Revoking;
        self.store_activation(&record).await?;
        if let Some(cron) = &record.request.cron {
            crate::cron_store::CronStore::open(&self.home)?
                .set_enabled(&cron.cron_id, false)
                .await?;
        }
        record.state = ActivationState::Revoked;
        self.store_activation(&record).await?;
        Ok(record)
    }
}
