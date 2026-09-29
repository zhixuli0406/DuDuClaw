use super::*;

impl DecisionStore {
    /// File a time-limited human review for one exact run. A failed local
    /// link write leaves an unusable broker request, never an authorization.
    pub async fn request_pilot_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        replay_hash: &str,
        agent_id: &str,
        summary: &str,
        ttl_seconds: i64,
    ) -> Result<PilotReviewLink, DecisionStoreError> {
        if !scope.valid()
            || agent_id.trim().is_empty()
            || summary.trim().is_empty()
            || summary.len() > 500
            || !(1..=86_400).contains(&ttl_seconds)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let run = self.current_daily_run_for_review(scope, replay_hash)?;
        let payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "snapshot_id": run.snapshot_id,
            "model_version": run.model_version,
            "scenario_id": run.scenario_id,
        });
        let approval_id = broker
            .request(
                agent_id,
                "support_pilot_review",
                summary,
                payload,
                ttl_seconds,
            )
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        let link = PilotReviewLink {
            approval_id: approval_id.to_string(),
            agent_id: agent_id.to_owned(),
            replay_hash: replay_hash.to_owned(),
            snapshot_id: run.snapshot_id,
            scenario_id: run.scenario_id,
        };
        self.put(
            scope,
            "pilot_review",
            &link.approval_id,
            &link,
            Some(&run.source_version_hashes),
        )?;
        Ok(link)
    }

    /// Fail closed unless the broker approved this exact persisted request,
    /// its time window is still open, and the linked run remains accessible.
    pub async fn require_pilot_review(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
    ) -> Result<PilotReviewLink, DecisionStoreError> {
        if !scope.valid() || approval_id.trim().is_empty() || replay_hash.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let link: PilotReviewLink = self.get(scope, "pilot_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let run = self.current_daily_run_for_review(scope, replay_hash)?;
        if link.snapshot_id != run.snapshot_id || link.scenario_id != run.scenario_id {
            return Err(DecisionStoreError::Corrupt);
        }
        let id = ApprovalId::from(approval_id.to_owned());
        let status = broker
            .poll(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        if status != ApprovalStatus::Approved {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let rec = broker
            .get(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let deadline = rec
            .deadline_rfc3339()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(&time).ok())
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let expected_payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "snapshot_id": run.snapshot_id,
            "model_version": run.model_version,
            "scenario_id": run.scenario_id,
        });
        let legacy_payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "snapshot_id": run.snapshot_id,
            "scenario_id": run.scenario_id,
        });
        let legacy_synthetic =
            run.snapshot_id.starts_with("synthetic-support-") && rec.payload == legacy_payload;
        if rec.id.as_str() != approval_id
            || rec.action_kind != "support_pilot_review"
            || rec.agent_id != link.agent_id
            || (rec.payload != expected_payload && !legacy_synthetic)
            || rec.decided_by.as_deref().is_none_or(str::is_empty)
            || chrono::Utc::now() >= deadline
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        // Broker polling can overlap a source revocation or engine change.
        self.current_daily_run_for_review(scope, replay_hash)?;
        Ok(link)
    }

    /// Read the broker's pending/terminal state without treating pending as
    /// approval. The persisted link, broker payload, exact run and source
    /// bindings are all checked before any state is returned.
    pub async fn pilot_review_status(
        &self,
        broker: &ApprovalBroker,
        scope: &DecisionScope,
        approval_id: &str,
        replay_hash: &str,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
    ) -> Result<PilotReviewStatus, DecisionStoreError> {
        if !scope.valid()
            || [
                approval_id,
                replay_hash,
                snapshot_id,
                model_version,
                scenario_id,
            ]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err(DecisionStoreError::Invalid);
        }
        let link: PilotReviewLink = self.get(scope, "pilot_review", approval_id)?;
        if link.approval_id != approval_id || link.replay_hash != replay_hash {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let run = self.current_daily_run_for_review(scope, replay_hash)?;
        if run.snapshot_id != snapshot_id
            || run.model_version != model_version
            || run.scenario_id != scenario_id
            || link.snapshot_id != run.snapshot_id
            || link.scenario_id != run.scenario_id
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let id = ApprovalId::from(approval_id.to_owned());
        let polled = broker
            .poll(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?;
        let rec = broker
            .get(&id)
            .await
            .map_err(DecisionStoreError::ReviewBroker)?
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let expires_at_utc = rec
            .deadline_rfc3339()
            .ok_or(DecisionStoreError::ReviewDenied)?;
        let expires_at = chrono::DateTime::parse_from_rfc3339(&expires_at_utc)
            .map_err(|_| DecisionStoreError::ReviewDenied)?;
        let expected_payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "snapshot_id": run.snapshot_id,
            "model_version": run.model_version,
            "scenario_id": run.scenario_id,
        });
        let legacy_payload = serde_json::json!({
            "tenant_id": scope.tenant_id,
            "acl": scope.acl,
            "replay_hash": replay_hash,
            "snapshot_id": run.snapshot_id,
            "scenario_id": run.scenario_id,
        });
        let legacy_synthetic =
            run.snapshot_id.starts_with("synthetic-support-") && rec.payload == legacy_payload;
        if rec.id.as_str() != approval_id
            || rec.action_kind != "support_pilot_review"
            || rec.agent_id != link.agent_id
            || (rec.payload != expected_payload && !legacy_synthetic)
            || rec.status != polled
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let status = if chrono::Utc::now() >= expires_at {
            ApprovalStatus::Expired
        } else {
            rec.status
        };
        if status == ApprovalStatus::Approved {
            self.require_pilot_review(broker, scope, approval_id, replay_hash)
                .await?;
        }
        // A source can be revoked during the broker read. Fail closed on a
        // final scoped, digest-checked read before exposing the status.
        self.current_daily_run_for_review(scope, replay_hash)?;
        Ok(PilotReviewStatus {
            link,
            status,
            expires_at_utc,
            decided_by: rec.decided_by,
        })
    }

}
