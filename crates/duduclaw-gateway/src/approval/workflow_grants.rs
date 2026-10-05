//! Fixed-revision authorization is distinct from a single human decision.
use super::*;
use crate::workflow::{ApprovedWorkflowRevision, CostBudget, CreatorGrantSnapshot, TypedSchema};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationAuthority {
    BoundHumanApproval {
        approval_id: String,
    },
    ActiveWorkflowRevisionGrant {
        grant_id: String,
        epoch: i64,
        spec_hash: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRef {
    pub grant_id: String,
    pub epoch: i64,
    pub spec_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectTemplate {
    pub step_id: String,
    pub tool: String,
    pub input_schema: TypedSchema,
    /// Exact resource fields are immutable accepted constraints.
    pub resource_scope: BTreeMap<String, Value>,
    pub receipt_adapter_version: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantSpec {
    pub schema_version: u32,
    pub workflow_id: String,
    pub workflow_revision: i64,
    pub revision_hash: String,
    pub skill_hash: String,
    pub fixtures_digest: String,
    pub activation_id: String,
    pub actor: String,
    pub operator_context: DecisionContext,
    pub creator_grant: CreatorGrantSnapshot,
    pub audience: Vec<String>,
    pub templates: BTreeMap<String, EffectTemplate>,
    pub input_max_age_seconds: u32,
    pub fixtures_expires_at: String,
    pub budget: CostBudget,
    pub expires_at: String,
    pub policy_revision: String,
}
impl GrantSpec {
    pub fn hash(&self) -> String {
        payload_hash(&serde_json::to_value(self).expect("grant serializable"))
    }
    /// Acceptance-time check: the fixture evidence behind this grant is
    /// still fresh.
    pub fn validate_fresh_fixtures(&self) -> Result<(), String> {
        let fixtures = DateTime::parse_from_rfc3339(&self.fixtures_expires_at)
            .map_err(|_| "invalid fixture expiry")?;
        if fixtures <= Utc::now() {
            return Err("grant fixtures expired".into());
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        self.operator_context.validate()?;
        if self.schema_version != 1
            || self.workflow_revision < 1
            || self.actor != self.creator_grant.actor
            || self.actor.is_empty()
            || self.revision_hash.is_empty()
            || self.fixtures_digest.is_empty()
            || self.skill_hash.is_empty()
            || self.policy_revision != self.creator_grant.policy_revision
            || self.budget.per_run_micros == 0
            || self.budget.monthly_micros < self.budget.per_run_micros
            || self.budget.max_consecutive_failures == 0
            || self.input_max_age_seconds == 0
        {
            return Err("invalid workflow revision grant".into());
        }
        let expires =
            DateTime::parse_from_rfc3339(&self.expires_at).map_err(|_| "invalid grant expiry")?;
        let fixtures = DateTime::parse_from_rfc3339(&self.fixtures_expires_at)
            .map_err(|_| "invalid fixture expiry")?;
        // U8 (F5-A): the grant lives for its own validity
        // (`[workflow] activation_days`). The fixture evidence only has to be
        // fresh when the activation is accepted (`validate_fresh_fixtures`):
        // after that every run re-checks the skill, the policy snapshot and
        // the binary itself.
        let _ = fixtures;
        if expires <= Utc::now() {
            return Err("grant expired".into());
        }
        for (id, template) in &self.templates {
            if id.is_empty()
                || template.step_id.is_empty()
                || template.tool.is_empty()
                || template.receipt_adapter_version != 1
                || !self.creator_grant.allowed_tools.contains(&template.tool)
            {
                return Err("grant template exceeds creator authority".into());
            }
            // A-H-1: every effect names its one fixed target record.
            crate::workflow::effect_targets::template_target(template)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedRevisionGrant {
    pub activation_id: String,
    pub acceptance_id: String,
    pub revision: ApprovedWorkflowRevision,
    pub spec: GrantSpec,
    pub spec_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRevocation {
    pub reference: GrantRef,
    pub executing_operations: Vec<String>,
}
#[cfg(test)]
mod tests;

mod migration;

impl ApprovalBroker {
    fn check_activation_not_revoked(
        tx: &rusqlite::Transaction<'_>,
        activation_id: &str,
    ) -> Result<(), String> {
        let revoked: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM main.workflow_activation_revocations WHERE activation_id=?1)",
                params![activation_id],
                |r| r.get(0)
            )
            .map_err(|e| e.to_string())?;
        if revoked {
            Err("workflow activation authoritatively revoked".into())
        } else {
            Ok(())
        }
    }
    fn check_granted_material(
        tx: &rusqlite::Transaction<'_>,
        reference: &GrantRef,
        step_key: &str,
        payload: &Value,
        binding: &ExecutionBinding,
    ) -> Result<(), String> {
        let raw: String = tx
            .query_row(
                "SELECT grant_spec_json FROM main.workflow_revision_grants WHERE grant_id=?1 AND authority_epoch=?2
                    AND grant_spec_hash=?3 AND state='active' AND julianday(expires_at)>julianday(?4)",
                params![
                    reference.grant_id,
                    reference.epoch,
                    reference.spec_hash,
                    Utc::now().to_rfc3339()
                ],
                |r| r.get(0)
            )
            .map_err(|_| "workflow grant revoked, changed or expired")?;
        let spec: GrantSpec =
            serde_json::from_str(&raw).map_err(|_| "invalid stored workflow grant")?;
        if spec.hash() != reference.spec_hash {
            return Err("workflow grant corrupt".into());
        }
        spec.validate()?;
        Self::check_activation_not_revoked(tx, &spec.activation_id)?;
        let revision = Self::check_grant_revision(tx, &spec)?;
        let raw: String = tx
            .query_row(
                "SELECT record_json FROM approval_workflow.workflow_runs WHERE run_id=?1",
                params![binding.run_id],
                |r| r.get(0),
            )
            .map_err(|_| "workflow run authority missing")?;
        let run: crate::workflow::WorkflowRun =
            serde_json::from_str(&raw).map_err(|_| "invalid workflow run authority")?;
        if run.grant.as_ref() != Some(reference)
            || run.activation_id.as_ref() != Some(&spec.activation_id)
            || run.actor != spec.actor
            || run.workflow_id != spec.workflow_id
            || run.revision != spec.workflow_revision
            || run.workflow_hash != spec.revision_hash
            || run.skill_hash != spec.skill_hash
            || run.creator_grant != spec.creator_grant
            || run.audience != spec.audience
            || run.policy_revision != spec.policy_revision
            || run.environment_hash != binding.environment_hash
            || run.input_hash != payload_hash(&run.input)
            || run.budget != spec.budget
            || !matches!(
                run.status,
                crate::workflow::RunStatus::Pending
                    | crate::workflow::RunStatus::Running
                    | crate::workflow::RunStatus::WaitingApproval
            )
        {
            return Err("workflow run does not match active grant".into());
        }
        let now = Utc::now();
        let observed = DateTime::parse_from_rfc3339(&run.input_observed_at)
            .map_err(|_| "invalid run freshness")?;
        let deadline =
            DateTime::parse_from_rfc3339(&run.deadline_at).map_err(|_| "invalid run deadline")?;
        let expiry = DateTime::parse_from_rfc3339(&binding.expires_at)
            .map_err(|_| "invalid binding expiry")?;
        let grant_expiry =
            DateTime::parse_from_rfc3339(&spec.expires_at).map_err(|_| "invalid grant expiry")?;
        // R-M5: the run input's age binds only an effect that reads the run
        // input; waiting for a person must not expire a literal effect.
        let input_age_binds =
            crate::workflow::effect_targets::step_uses_run_input(&revision.definition, step_key);
        if observed > now
            || input_age_binds
                && now - observed.with_timezone(&Utc)
                    > chrono::Duration::seconds(spec.input_max_age_seconds as i64)
            || deadline <= now
            || expiry > deadline
            || expiry > grant_expiry
        {
            return Err("workflow input stale or deadline exceeded".into());
        }
        // E-H3: budgets are read from the cost ledger, never from the run's
        // JSON projection. The effect's own dispatch must already be
        // reserved there (the runner charges it with the running
        // checkpoint), and the reservation kept the totals within the caps.
        use crate::workflow::cost_ledger as ledger;
        if !ledger::effect_reserved_in(tx, "approval_workflow.", &run.run_id, step_key)? {
            return Err("workflow effect not reserved in the cost ledger".into());
        }
        let cost = ledger::run_cost_in(tx, "approval_workflow.", &run.run_id)?.total()?;
        if cost > spec.budget.per_run_micros {
            return Err("workflow per-run budget exhausted".into());
        }
        let monthly = ledger::month_total_in(
            tx,
            "approval_workflow.",
            &spec.workflow_id,
            &now.format("%Y-%m").to_string(),
        )?;
        if monthly > spec.budget.monthly_micros {
            return Err("workflow monthly budget exhausted".into());
        }
        let statuses: Vec<String> = {
            let mut q = tx
                .prepare(&crate::workflow::consecutive_failure_sql("approval_workflow."))
                .map_err(|e| e.to_string())?;
            q.query_map(
                params![
                    spec.workflow_id,
                    binding.run_id,
                    spec.activation_id,
                    spec.budget.max_consecutive_failures
                ],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
        };
        if statuses.len() >= spec.budget.max_consecutive_failures as usize
            && statuses.iter().all(|s| s != "succeeded")
        {
            return Err("workflow consecutive failure limit".into());
        }
        if binding.actor_principal != spec.actor
            || binding.decision_context != spec.operator_context
            || binding.policy_revision != spec.policy_revision
            || binding.run_origin_kind != "workflow"
            || binding.resume_handler != "workflow_v1"
            || run.task.as_ref().map(|t| t.task_id.clone()) != binding.task_id
            || run.task.as_ref().map(|t| t.revision) != binding.task_revision
            || run.task.as_ref().map(|t| t.snapshot_hash.clone()) != binding.task_snapshot_hash
        {
            return Err("granted operation binding mismatch".into());
        }
        let template = spec
            .templates
            .values()
            .find(|t| t.step_id == step_key)
            .ok_or("step not accepted as effect template")?;
        if payload.get("name").and_then(Value::as_str) != Some(template.tool.as_str()) {
            return Err("effective tool exceeds template".into());
        }
        let arguments = payload
            .get("arguments")
            .ok_or("effective arguments missing")?;
        template.input_schema.validate(arguments)?;
        for (key, expected) in &template.resource_scope {
            if arguments.get(key) != Some(expected) {
                return Err("effective resource outside accepted scope".into());
            }
        }
        crate::workflow::effect_targets::check_effect_arguments(template, arguments)?;
        Ok(())
    }
    pub async fn prepare_granted_operation(
        &self,
        reference: &GrantRef,
        step_key: &str,
        payload: &Value,
        binding: &ExecutionBinding,
        provider_key: Option<&str>,
    ) -> Result<String, String> {
        if step_key.is_empty() || step_key.len() > 256 {
            return Err("invalid granted operation step".into());
        }
        binding.validate(payload)?;
        binding.validate_host_files()?;
        let home = self
            .home_dir()
            .ok_or("workflow effect requires durable home")?;
        if policy_revision(&home, &binding.actor_principal)? != binding.policy_revision {
            return Err("workflow operation policy changed".into());
        }
        let mut conn = self.store.conn.lock().await;
        self.attach_workflow_reader(&conn)?;
        self.attach_tasks(&conn, binding)?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        Self::check_granted_material(&tx, reference, step_key, payload, binding)?;
        Self::check_task(&tx, binding)?;
        let id = uuid::Uuid::new_v4().to_string();
        let frozen = serde_json::to_string(binding).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO main.approval_operations(operation_id,run_id,step_key,approval_id,binding_json,payload_json,
                provider_key,authority_source,revision_grant_id,revision_grant_epoch,revision_grant_hash,
                run_authority_json) VALUES(?1,?2,?3,'',?4,?5,?6,'active_workflow_revision_grant',?7,?8,?9,?10)
                ON CONFLICT(run_id,step_key) DO NOTHING",
            params![
                id,
                binding.run_id,
                step_key,
                frozen,
                payload.to_string(),
                provider_key,
                reference.grant_id,
                reference.epoch,
                reference.spec_hash,
                Self::workflow_run_authority(&tx, binding)?
            ]
        )
        .map_err(|e| e.to_string())?;
        let existing: (
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<i64>,
            Option<String>
        ) = tx
            .query_row(
                "SELECT operation_id,binding_json,payload_json,provider_key,authority_source,revision_grant_id,
                    revision_grant_epoch,revision_grant_hash FROM main.approval_operations WHERE run_id=?1
                    AND step_key=?2",
                params![binding.run_id, step_key],
                |r|
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?
                    ))
            )
            .map_err(|e| e.to_string())?;
        if existing.1 != frozen
            || payload_hash(
                &serde_json::from_str(&existing.2)
                    .map_err(|_| "invalid stored operation payload")?,
            ) != payload_hash(payload)
            || existing.3.as_deref() != provider_key
            || existing.4 != "active_workflow_revision_grant"
            || existing.5.as_deref() != Some(reference.grant_id.as_str())
            || existing.6 != Some(reference.epoch)
            || existing.7.as_deref() != Some(reference.spec_hash.as_str())
        {
            return Err("step already has different effect authority or contract".into());
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(existing.0)
    }
    /// Capture server-owned run authority; human approval never replaces this fence.
    pub(super) fn workflow_run_authority(
        tx: &rusqlite::Transaction<'_>,
        binding: &ExecutionBinding,
    ) -> Result<Option<String>, String> {
        if binding.run_origin_kind != "workflow" || binding.resume_handler != "workflow_v1" {
            return Ok(None);
        }
        let raw: String = tx
            .query_row(
                "SELECT record_json FROM approval_workflow.workflow_runs WHERE run_id=?1",
                params![binding.run_id],
                |r| r.get(0),
            )
            .map_err(|_| "workflow run authority unavailable")?;
        let run: crate::workflow::WorkflowRun =
            serde_json::from_str(&raw).map_err(|_| "workflow run authority corrupt")?;
        match (&run.activation_id, &run.grant) {
            (Some(activation), Some(reference)) => {
                Self::check_activation_not_revoked(tx, activation)?;
                Ok(Some(
                    serde_json::to_string(&(activation, reference)).map_err(|e| e.to_string())?,
                ))
            }
            (None, None) if matches!(run.trigger, crate::workflow::Trigger::Fixture { .. }) => {
                Ok(None)
            }
            _ => Err("workflow run activation authority missing".into()),
        }
    }
    pub(super) fn check_operation_authority(
        tx: &rusqlite::Transaction<'_>,
        id: &str,
        binding: &ExecutionBinding,
    ) -> Result<(), String> {
        let (source, approval, grant, epoch, hash, payload, step): (
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            String,
            String
        ) = tx
            .query_row(
                "SELECT authority_source,approval_id,revision_grant_id,revision_grant_epoch,revision_grant_hash,
                    payload_json,step_key FROM main.approval_operations WHERE operation_id=?1",
                params![id],
                |r|
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?
                    ))
            )
            .map_err(|_| "operation authority missing")?;
        if binding.run_origin_kind == "workflow" && binding.resume_handler == "workflow_v1" {
            let frozen: Option<String> = tx
                .query_row(
                    "SELECT run_authority_json FROM main.approval_operations WHERE operation_id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if frozen != Self::workflow_run_authority(tx, binding)? {
                return Err("operation workflow authority changed or missing".into());
            }
        }
        match source.as_str() {
            "bound_human_approval"
                if !approval.is_empty() && grant.is_none() && epoch.is_none() && hash.is_none() =>
            {
                let valid: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM main.approvals WHERE id=?1 AND request_kind='approval'
                            AND status='approved' AND binding_json=?2
                            AND julianday(?3)<julianday(json_extract(binding_json,'$.expires_at'))
                            AND julianday(?3)<julianday(created_at)+ttl_seconds/86400.0)",
                        params![
                            approval,
                            serde_json::to_string(binding).map_err(|e| e.to_string())?,
                            Utc::now().to_rfc3339()
                        ],
                        |r| r.get(0)
                    )
                    .map_err(|e| e.to_string())?;
                if !valid {
                    return Err("human operation authority revoked or expired".into());
                }
                if binding.run_origin_kind == "workflow" && binding.resume_handler == "workflow_v1"
                {
                    let raw: String = tx
                        .query_row(
                            "SELECT record_json FROM approval_workflow.workflow_runs WHERE run_id=?1",
                            params![binding.run_id],
                            |r| r.get(0)
                        )
                        .map_err(|_| "human workflow run authority unavailable")?;
                    let run: crate::workflow::WorkflowRun = serde_json::from_str(&raw)
                        .map_err(|_| "human workflow run authority corrupt")?;
                    match (&run.activation_id, &run.grant) {
                        (Some(activation), Some(reference)) => {
                            // A scripted fixture decision never authorizes a formal run.
                            let scripted: bool = tx
                                .query_row(
                                    "SELECT action_kind=?2 FROM main.approvals WHERE id=?1",
                                    params![approval, super::FIXTURE_DECISION_KIND],
                                    |r| r.get(0),
                                )
                                .map_err(|e| e.to_string())?;
                            if scripted {
                                return Err("fixture decision cannot authorize a formal run".into());
                            }
                            Self::check_activation_not_revoked(tx, activation)?;
                            Self::check_granted_material(
                                tx,
                                reference,
                                &step,
                                &serde_json::from_str(&payload)
                                    .map_err(|_| "invalid human workflow payload")?,
                                binding,
                            )?;
                        }
                        (None, None)
                            if matches!(run.trigger, crate::workflow::Trigger::Fixture { .. }) =>
                        {
                            ()
                        }
                        _ => return Err("human workflow activation authority missing".into()),
                    }
                }
                Ok(())
            }
            "active_workflow_revision_grant" if approval.is_empty() => {
                let reference = GrantRef {
                    grant_id: grant.ok_or("missing revision grant")?,
                    epoch: epoch.ok_or("missing revision grant epoch")?,
                    spec_hash: hash.ok_or("missing revision grant hash")?,
                };
                Self::check_granted_material(
                    tx,
                    &reference,
                    &step,
                    &serde_json::from_str(&payload).map_err(|_| "invalid effect payload")?,
                    binding,
                )
            }
            _ => Err("unknown or malformed operation authority".into()),
        }
    }
    pub(super) fn attach_workflow_reader(&self, conn: &Connection) -> Result<(), String> {
        let home = std::fs::canonicalize(
            self.home_dir()
                .ok_or("workflow grant requires durable home")?,
        )
        .map_err(|e| e.to_string())?;
        let path = home.join("workflow.db");
        for suffix in ["", "-wal", "-shm"] {
            let p = PathBuf::from(format!("{}{suffix}", path.display()));
            if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err("workflow authority symlink refused".into());
            }
        }
        if !path.is_file() {
            return Err("workflow authority store missing".into());
        }
        let attached: Option<String> = conn
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name='approval_workflow'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(existing) = attached {
            if Path::new(&existing) != path {
                return Err("workflow authority alias mismatch".into());
            }
        } else {
            conn.execute(
                "ATTACH DATABASE ?1 AS approval_workflow",
                params![path.to_string_lossy().as_ref()],
            )
            .map_err(|e| e.to_string())?;
        }
        let version: i64 = conn
            .query_row(
                "SELECT version FROM approval_workflow.workflow_schema_meta WHERE owner='foundation'",
                [],
                |r| r.get(0)
            )
            .map_err(|_| "workflow authority schema missing")?;
        if version != 1 {
            return Err("unsupported workflow authority schema".into());
        }
        Ok(())
    }
}

mod revision_grants;
