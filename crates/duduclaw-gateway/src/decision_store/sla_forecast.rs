use super::*;

impl DecisionStore {
    /// Reject a changed opening identity when the preceding day has already
    /// produced a valid ticket score. Unscored predecessors are checked later
    /// by the policy-wide assessment once both outcomes exist.
    pub(super) fn validate_shadow_sla_prior_boundary(
        &self,
        scope: &DecisionScope,
        forecast: &StoredShadowForecast,
        opening_source: &str,
        as_of: i64,
    ) -> Result<(), DecisionStoreError> {
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        let conn = self.open()?;
        assert_readable_shadow_days(&conn, scope, &forecast.source_lineage)?;
        let previous_forecast_id: Option<String> = conn
            .query_row(
                "SELECT forecast_id FROM decision_shadow_targets
             WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3
             AND strftime('%s',target_day_utc) IS NOT NULL
             AND CAST(strftime('%s',target_day_utc) AS INTEGER)=?4",
                params![
                    scope.tenant_id,
                    scope.acl,
                    forecast.source_lineage,
                    target - 86_400
                ],
                |row| row.get(0),
            )
            .optional()?;
        drop(conn);
        let Some(previous_forecast_id) = previous_forecast_id else {
            return Ok(());
        };
        let previous_sla_id: Option<String> = self
            .open()?
            .query_row(
                "SELECT sla_id FROM decision_shadow_sla_forecasts
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, previous_forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(previous_sla_id) = previous_sla_id else {
            return Ok(());
        };
        match self.shadow_sla_score_head(scope, &previous_sla_id) {
            Err(DecisionStoreError::NotFound) => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        let score = self.load_current_shadow_sla_score(scope, &previous_sla_id)?;
        if score.scored_at > as_of {
            return Ok(());
        }
        let (_, observation_source) = self.shadow_artifact(
            scope,
            &score.observation_artifact_id,
            "shadow_sla_observation_export",
            Some(&score.observation_sha256),
        )?;
        let previous: ShadowSlaObservationExport = serde_json::from_str(&observation_source)?;
        let opening: ShadowSlaOpeningExport = serde_json::from_str(opening_source)?;
        if !shadow_sla_day_boundary_matches(&previous, &opening)? {
            return Err(DecisionStoreError::Invalid);
        }
        Ok(())
    }

    /// Commit ticket-age-based SLA prediction before the target day's outcome.
    pub fn put_shadow_sla_forecast(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        model_version: &str,
        opening_artifact_id: &str,
    ) -> Result<StoredShadowSlaForecast, DecisionStoreError> {
        self.put_shadow_sla_forecast_at(
            scope,
            id,
            forecast_id,
            model_version,
            opening_artifact_id,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn put_shadow_sla_forecast_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        model_version: &str,
        opening_artifact_id: &str,
        committed_at: i64,
    ) -> Result<StoredShadowSlaForecast, DecisionStoreError> {
        if !scope.valid()
            || id.trim().is_empty()
            || forecast_id.trim().is_empty()
            || model_version.trim().is_empty()
            || opening_artifact_id.trim().is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let forecast = self.load_shadow_forecast(scope, forecast_id)?;
        let (_, forecast_sha256): (StoredShadowForecast, String) =
            self.get_with_digest(scope, "shadow_forecast", forecast_id)?;
        let (model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", model_version)?;
        let policy = self.load_shadow_policy(scope, &forecast.policy_id)?;
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        if committed_at < forecast.committed_at
            || committed_at > target + policy.issue_deadline_seconds as i64
            || committed_at >= target + 86_400
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (_, training_source) = self.shadow_artifact(
            scope,
            &forecast.training_artifact_id,
            "shadow_training_export",
            Some(&forecast.training_sha256),
        )?;
        let training = parse_shadow_training_export(&training_source)?;
        let (metadata, opening_source) = self.shadow_artifact(
            scope,
            opening_artifact_id,
            "shadow_sla_opening_export",
            None,
        )?;
        if metadata.occurred_at != target
            || metadata.ingested_at < target
            || metadata.ingested_at > committed_at
            || metadata.lineage_id != forecast.source_lineage
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (inputs, prediction, baselines) = validate_shadow_sla_opening_source(
            &opening_source,
            &forecast,
            &training.observed_days,
            &model,
        )?;
        self.validate_shadow_sla_prior_boundary(scope, &forecast, &opening_source, committed_at)?;
        let record = StoredShadowSlaForecast {
            id: id.into(),
            forecast_id: forecast_id.into(),
            forecast_sha256,
            opening_artifact_id: opening_artifact_id.into(),
            opening_sha256: metadata.content_sha256.clone(),
            model_version: model.version,
            model_sha256,
            queue_id: inputs.queue_id.clone().ok_or(DecisionStoreError::Invalid)?,
            target_day_utc: forecast.target_day_utc.clone(),
            committed_at,
            inputs,
            prediction,
            baselines,
        };
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::reserve_shadow_sla_in_tx(&tx, scope, forecast_id, id)?;
        Self::put_in_tx(
            &tx,
            scope,
            "shadow_sla_forecast",
            id,
            &record,
            Some(&[forecast.training_sha256, metadata.content_sha256]),
        )?;
        tx.commit()?;
        self.load_shadow_sla_forecast(scope, id)
    }

    /// Recheck the original forecast, model, opening source, and numeric result.
    pub fn load_shadow_sla_forecast(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<StoredShadowSlaForecast, DecisionStoreError> {
        let record: StoredShadowSlaForecast = self.get(scope, "shadow_sla_forecast", id)?;
        let forecast = self.load_shadow_forecast(scope, &record.forecast_id)?;
        let (_, forecast_sha256): (StoredShadowForecast, String) =
            self.get_with_digest(scope, "shadow_forecast", &record.forecast_id)?;
        let (model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", &record.model_version)?;
        let policy = self.load_shadow_policy(scope, &forecast.policy_id)?;
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        let reserved: Option<String> = self
            .open()?
            .query_row(
                "SELECT sla_id FROM decision_shadow_sla_forecasts
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, record.forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        if record.id != id
            || reserved.as_deref() != Some(id)
            || record.forecast_sha256 != forecast_sha256
            || record.model_sha256 != model_sha256
            || record.queue_id != forecast.queue_id.as_deref().unwrap_or("")
            || record.target_day_utc != forecast.target_day_utc
            || record.committed_at < forecast.committed_at
            || record.committed_at > target + policy.issue_deadline_seconds as i64
            || record.committed_at >= target + 86_400
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let (metadata, opening_source) = self.shadow_artifact(
            scope,
            &record.opening_artifact_id,
            "shadow_sla_opening_export",
            Some(&record.opening_sha256),
        )?;
        if metadata.occurred_at != target
            || metadata.ingested_at < target
            || metadata.ingested_at > record.committed_at
            || metadata.lineage_id != forecast.source_lineage
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let (_, training_source) = self.shadow_artifact(
            scope,
            &forecast.training_artifact_id,
            "shadow_training_export",
            Some(&forecast.training_sha256),
        )?;
        let training = parse_shadow_training_export(&training_source)?;
        let (inputs, prediction, baselines) = validate_shadow_sla_opening_source(
            &opening_source,
            &forecast,
            &training.observed_days,
            &model,
        )?;
        if record.inputs != inputs
            || record.prediction != prediction
            || record.baselines != baselines
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    /// Check an operator file before registering it as a source artifact.
    pub fn preview_shadow_sla_forecast(
        &self,
        scope: &DecisionScope,
        forecast_id: &str,
        model_version: &str,
        opening_source: &str,
    ) -> Result<ProspectiveSlaForecast, DecisionStoreError> {
        let forecast = self.load_shadow_forecast(scope, forecast_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let (_, training_source) = self.shadow_artifact(
            scope,
            &forecast.training_artifact_id,
            "shadow_training_export",
            Some(&forecast.training_sha256),
        )?;
        let training = parse_shadow_training_export(&training_source)?;
        let (_, prediction, _) = validate_shadow_sla_opening_source(
            opening_source,
            &forecast,
            &training.observed_days,
            &model,
        )?;
        let policy = self.load_shadow_policy(scope, &forecast.policy_id)?;
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        self.validate_shadow_sla_prior_boundary(
            scope,
            &forecast,
            opening_source,
            target + i64::from(policy.issue_deadline_seconds),
        )?;
        Ok(prediction)
    }

}
