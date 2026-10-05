use super::*;

impl ApprovalBroker {
    pub(super) fn check_grant_revision(
        tx: &rusqlite::Transaction<'_>,
        spec: &GrantSpec,
    ) -> Result<ApprovedWorkflowRevision, String> {
        let raw: String = tx
            .query_row(
                "SELECT record_json FROM approval_workflow.workflow_revisions WHERE workflow_id=?1 AND revision=?2
                    AND revision_hash=?3",
                params![spec.workflow_id, spec.workflow_revision, spec.revision_hash],
                |r| r.get(0)
            )
            .map_err(|_| "accepted workflow revision missing")?;
        let revision: ApprovedWorkflowRevision =
            serde_json::from_str(&raw).map_err(|_| "invalid stored workflow revision")?;
        if revision.definition.hash() != spec.revision_hash
            || revision.revision_hash != spec.revision_hash
            || revision.definition.skill_revision_hash != spec.skill_hash
            || revision.fixtures_digest != spec.fixtures_digest
            || revision.creator_grant != spec.creator_grant
            || revision.audience != spec.audience
            || revision.owner != spec.actor
        {
            return Err("workflow grant immutable material changed".into());
        }
        for (template_id, template) in &spec.templates {
            let step = revision
                .definition
                .steps
                .iter()
                .find(|s| s.step_id == template.step_id)
                .ok_or("grant step missing")?;
            if !matches!(
                &step.action,
                crate::workflow::StepAction::McpEffect { tool, template_id: id }
                    if tool == &template.tool && id == template_id
            )
                || step.input_schema != template.input_schema
            {
                return Err("grant template does not match accepted step".into());
            }
            crate::workflow::effect_targets::check_pinned_target(&revision.definition, template)?;
        }
        Ok(revision)
    }
    fn check_activation_acceptance(
        tx: &rusqlite::Transaction<'_>,
        accepted_id: &str,
        spec: &GrantSpec,
    ) -> Result<(), String> {
        let (kind, status, payload_raw, binding_raw, created, ttl, decided_by): (
            String,
            String,
            String,
            Option<String>,
            String,
            i64,
            Option<String>,
        ) = tx
            .query_row(
                "SELECT request_kind,status,payload,binding_json,created_at,ttl_seconds,decided_by
                FROM main.approvals WHERE id=?1",
                params![accepted_id],
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
            .map_err(|_| "activation acceptance missing")?;
        if kind != "approval" || status != "approved" {
            return Err("activation not accepted by human".into());
        }
        // U3: only an Admin decision in the dashboard accepts an activation.
        if !decided_by.as_deref().is_some_and(|d| d.starts_with("dashboard:")) {
            return Err("activation must be accepted by an Admin in the dashboard".into());
        }
        let payload: Value =
            serde_json::from_str(&payload_raw).map_err(|_| "invalid activation payload")?;
        let binding: ExecutionBinding = serde_json::from_str(
            binding_raw
                .as_deref()
                .ok_or("legacy acceptance cannot grant workflow")?,
        )
        .map_err(|_| "invalid activation binding")?;
        binding.validate(&payload)?;
        if binding.actor_principal != spec.actor
            || binding.decision_context != spec.operator_context
            || binding.policy_revision != spec.policy_revision
            || payload.get("kind").and_then(Value::as_str) != Some("workflow_activation")
            || payload.get("activation_id").and_then(Value::as_str)
                != Some(spec.activation_id.as_str())
            || payload.get("spec_hash").and_then(Value::as_str) != Some(spec.hash().as_str())
            || payload.get("revision_hash").and_then(Value::as_str)
                != Some(spec.revision_hash.as_str())
            || payload.get("fixtures_digest").and_then(Value::as_str)
                != Some(spec.fixtures_digest.as_str())
        {
            return Err("activation acceptance does not bind grant material".into());
        }
        let created =
            DateTime::parse_from_rfc3339(&created).map_err(|_| "invalid acceptance time")?;
        if Utc::now() >= created + chrono::Duration::seconds(ttl) {
            return Err("activation acceptance expired".into());
        }
        Self::check_task(tx, &binding)
    }
    pub async fn prepare_revision_grant(
        &self,
        accepted: &AcceptedRevisionGrant,
    ) -> Result<GrantRef, String> {
        accepted.spec.validate()?;
        accepted.spec.validate_fresh_fixtures()?;
        if accepted.activation_id != accepted.spec.activation_id
            || accepted.spec_hash != accepted.spec.hash()
            || accepted.acceptance_id != accepted.revision.acceptance_id
        {
            return Err("accepted grant material mismatch".into());
        }
        let home = self
            .home_dir()
            .ok_or("workflow grant requires durable home")?;
        if policy_revision(&home, &accepted.spec.actor)? != accepted.spec.policy_revision {
            return Err("activation policy changed".into());
        }
        let mut conn = self.store.conn.lock().await;
        self.attach_workflow_reader(&conn)?;
        let acceptance_binding: String = conn
            .query_row(
                "SELECT binding_json FROM main.approvals WHERE id=?1",
                params![accepted.acceptance_id],
                |r| r.get(0),
            )
            .map_err(|_| "activation binding unavailable")?;
        let acceptance_binding: ExecutionBinding =
            serde_json::from_str(&acceptance_binding).map_err(|_| "invalid activation binding")?;
        self.attach_tasks(&conn, &acceptance_binding)?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let revision = Self::check_grant_revision(&tx, &accepted.spec)?;
        if revision != accepted.revision {
            return Err("accepted revision changed".into());
        }
        Self::check_activation_acceptance(&tx, &accepted.acceptance_id, &accepted.spec)?;
        let id = uuid::Uuid::new_v4().to_string();
        Self::check_activation_not_revoked(&tx, &accepted.activation_id)?;
        tx.execute(
            "INSERT INTO main.workflow_revision_grants(grant_id,activation_id,workflow_id,workflow_revision,
                revision_hash,skill_hash,fixtures_digest,activation_acceptance_id,grant_spec_json,grant_spec_hash,
                actor_principal,audience_hash,authority_epoch,state,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,
                ?7,?8,?9,?10,?11,?12,1,'prepared',?13,?14) ON CONFLICT(activation_id) DO NOTHING",
            params![
                id,
                accepted.activation_id,
                accepted.spec.workflow_id,
                accepted.spec.workflow_revision,
                accepted.spec.revision_hash,
                accepted.spec.skill_hash,
                accepted.spec.fixtures_digest,
                accepted.acceptance_id,
                serde_json::to_string(&accepted.spec).map_err(|e| e.to_string())?,
                accepted.spec_hash,
                accepted.spec.actor,
                payload_hash(&serde_json::json!(accepted.spec.audience)),
                accepted.spec.expires_at,
                Utc::now().to_rfc3339()
            ]
        )
        .map_err(|e| e.to_string())?;
        let (grant_id, epoch, hash, acceptance): (String, i64, String, String) = tx
            .query_row(
                "SELECT grant_id,authority_epoch,grant_spec_hash,activation_acceptance_id
                    FROM main.workflow_revision_grants WHERE activation_id=?1",
                params![accepted.activation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            )
            .map_err(|e| e.to_string())?;
        if hash != accepted.spec_hash || acceptance != accepted.acceptance_id {
            return Err("activation already has different grant authority".into());
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(GrantRef {
            grant_id,
            epoch,
            spec_hash: hash,
        })
    }
    /// Read current ledger authority for saga recovery; callers must still compare
    /// immutable accepted material. This does not activate or resurrect a grant.
    pub async fn current_revision_grant(
        &self,
        activation_id: &str,
    ) -> Result<Option<(GrantRef, GrantSpec, String)>, String> {
        let conn = self.store.conn.lock().await;
        let row: Option<(String, i64, String, String, String)> = conn
            .query_row(
                "SELECT grant_id,authority_epoch,grant_spec_hash,grant_spec_json,state FROM workflow_revision_grants
                    WHERE activation_id=?1",
                params![activation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            )
            .optional()
            .map_err(|e| e.to_string())?;
        row.map(|(id, epoch, hash, raw, state)| {
            let spec: GrantSpec =
                serde_json::from_str(&raw).map_err(|_| "invalid current grant")?;
            if spec.hash() != hash
                || spec.activation_id != activation_id
                || !matches!(state.as_str(), "prepared" | "active" | "revoked")
            {
                return Err("current grant material corrupt".into());
            }
            Ok((
                GrantRef {
                    grant_id: id,
                    epoch,
                    spec_hash: hash,
                },
                spec,
                state,
            ))
        })
        .transpose()
    }
    pub async fn inspect_revision_grant(
        &self,
        reference: &GrantRef,
    ) -> Result<(GrantSpec, String), String> {
        let conn = self.store.conn.lock().await;
        let (raw, state): (String, String) = conn
            .query_row(
                "SELECT grant_spec_json,state FROM main.workflow_revision_grants WHERE grant_id=?1
                    AND authority_epoch=?2 AND grant_spec_hash=?3",
                params![reference.grant_id, reference.epoch, reference.spec_hash],
                |r| Ok((r.get(0)?, r.get(1)?))
            )
            .map_err(|_| "workflow grant missing or changed")?;
        let spec: GrantSpec = serde_json::from_str(&raw).map_err(|_| "invalid stored grant")?;
        if spec.hash() != reference.spec_hash {
            return Err("grant spec corrupt".into());
        }
        Ok((spec, state))
    }
    pub async fn activate_revision_grant(&self, reference: &GrantRef) -> Result<GrantRef, String> {
        let (spec, state) = self.inspect_revision_grant(reference).await?;
        spec.validate()?;
        if state != "prepared" {
            return Err("grant is not prepared".into());
        }
        let home = self.home_dir().ok_or("workflow grant requires home")?;
        if policy_revision(&home, &spec.actor)? != spec.policy_revision {
            return Err("activation policy changed".into());
        }
        let mut conn = self.store.conn.lock().await;
        self.attach_workflow_reader(&conn)?;
        let acceptance: String = conn
            .query_row(
                "SELECT activation_acceptance_id FROM main.workflow_revision_grants WHERE grant_id=?1",
                params![reference.grant_id],
                |r| r.get(0)
            )
            .map_err(|e| e.to_string())?;
        let binding: String = conn
            .query_row(
                "SELECT binding_json FROM main.approvals WHERE id=?1",
                params![acceptance],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        self.attach_tasks(
            &conn,
            &serde_json::from_str(&binding).map_err(|_| "invalid activation binding")?,
        )?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        Self::check_activation_not_revoked(&tx, &spec.activation_id)?;
        Self::check_grant_revision(&tx, &spec)?;
        Self::check_activation_acceptance(&tx, &acceptance, &spec)?;
        let n = tx
            .execute(
                "UPDATE main.workflow_revision_grants SET state='active',authority_epoch=authority_epoch+1
                    WHERE grant_id=?1 AND authority_epoch=?2 AND grant_spec_hash=?3 AND state='prepared'
                    AND julianday(expires_at)>julianday(?4)",
                params![
                    reference.grant_id,
                    reference.epoch,
                    reference.spec_hash,
                    Utc::now().to_rfc3339()
                ]
            )
            .map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("activation grant revoked or changed".into());
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(GrantRef {
            epoch: reference.epoch + 1,
            ..reference.clone()
        })
    }
    /// Revoke even before grant creation, under the same authoritative ledger lock.
    pub async fn revoke_workflow_activation(
        &self,
        activation_id: &str,
        spec_hash: &str,
        reason: &str,
    ) -> Result<Option<GrantRevocation>, String> {
        if activation_id.is_empty() || spec_hash.is_empty() || reason.trim().is_empty() {
            return Err("activation revoke identity required".into());
        }
        let mut conn = self.store.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let old: Option<String> = tx
            .query_row(
                "SELECT spec_hash FROM workflow_activation_revocations WHERE activation_id=?1",
                params![activation_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if old.as_ref().is_some_and(|hash| hash != spec_hash) {
            return Err("activation revocation material mismatch".into());
        }
        tx.execute(
            "INSERT INTO workflow_activation_revocations VALUES(?1,?2,?3,?4) ON CONFLICT(activation_id) DO NOTHING",
            params![activation_id, spec_hash, Utc::now().to_rfc3339(), reason]
        )
        .map_err(|e| e.to_string())?;
        let current: Option<(String, i64, String, String)> = tx
            .query_row(
                "SELECT grant_id,authority_epoch,grant_spec_hash,state FROM workflow_revision_grants
                    WHERE activation_id=?1",
                params![activation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let revocation = if let Some((id, epoch, hash, state)) = current {
            if hash != spec_hash {
                return Err("activation grant material mismatch".into());
            }
            let next = if state == "revoked" {
                epoch
            } else {
                tx.execute(
                    "UPDATE workflow_revision_grants SET state='revoked',authority_epoch=authority_epoch+1,
                        revoked_at=?1,revoke_reason=?2 WHERE grant_id=?3 AND authority_epoch=?4
                        AND state IN ('prepared','active')",
                    params![Utc::now().to_rfc3339(), reason, id, epoch]
                )
                .map_err(|e| e.to_string())?;
                tx.execute(
                    "INSERT INTO workflow_grant_outbox VALUES(?1,?2,?3,'revoke',?4,0)",
                    params![uuid::Uuid::new_v4().to_string(), id, epoch + 1, reason],
                )
                .map_err(|e| e.to_string())?;
                epoch + 1
            };
            let operations = {
                let mut q = tx
                    .prepare("SELECT operation_id FROM approval_operations WHERE state='executing'
                        AND (revision_grant_id=?1 OR json_extract(run_authority_json,'$[0]')=?2)")
                    .map_err(|e| e.to_string())?;
                q.query_map(params![id, activation_id], |r| r.get(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<String>, _>>()
                    .map_err(|e| e.to_string())?
            };
            Some(GrantRevocation {
                reference: GrantRef {
                    grant_id: id,
                    epoch: next,
                    spec_hash: hash,
                },
                executing_operations: operations,
            })
        } else {
            None
        };
        tx.commit().map_err(|e| e.to_string())?;
        Ok(revocation)
    }
    pub async fn revoke_revision_grant(
        &self,
        reference: &GrantRef,
        reason: &str,
    ) -> Result<GrantRevocation, String> {
        if reason.trim().is_empty() {
            return Err("revocation reason required".into());
        }
        let mut conn = self.store.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let n = tx
            .execute(
                "UPDATE main.workflow_revision_grants SET state='revoked',authority_epoch=authority_epoch+1,
                    revoked_at=?1,revoke_reason=?2 WHERE grant_id=?3 AND authority_epoch=?4 AND grant_spec_hash=?5
                    AND state IN ('prepared','active')",
                params![
                    Utc::now().to_rfc3339(),
                    reason,
                    reference.grant_id,
                    reference.epoch,
                    reference.spec_hash
                ]
            )
            .map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("grant revoke stale or already revoked".into());
        }
        tx.execute(
            "INSERT INTO main.workflow_grant_outbox VALUES(?1,?2,?3,'revoke',?4,0)",
            params![
                uuid::Uuid::new_v4().to_string(),
                reference.grant_id,
                reference.epoch + 1,
                reason
            ],
        )
        .map_err(|e| e.to_string())?;
        let executing_operations = {
            let mut q = tx
                .prepare("SELECT operation_id FROM main.approval_operations WHERE revision_grant_id=?1
                    AND state='executing'")
                .map_err(|e| e.to_string())?;
            q.query_map(params![reference.grant_id], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<String>, _>>()
                .map_err(|e| e.to_string())?
        };
        tx.commit().map_err(|e| e.to_string())?;
        Ok(GrantRevocation {
            reference: GrantRef {
                epoch: reference.epoch + 1,
                ..reference.clone()
            },
            executing_operations,
        })
    }
    /// True when an authoritative revocation tombstone exists for the activation.
    pub async fn activation_revoked(&self, activation_id: &str) -> Result<bool, String> {
        let conn = self.store.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.workflow_activation_revocations WHERE activation_id=?1)",
            params![activation_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
    }
}
