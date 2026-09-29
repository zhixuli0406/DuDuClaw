use super::*;

impl DecisionStore {
    /// Commit a source-bound one-day forecast before the target day is over.
    pub fn put_shadow_forecast(
        &self,
        scope: &DecisionScope,
        id: &str,
        training_artifact_id: &str,
        target_day_utc: &str,
        known: KnownDayInputs,
        policy_id: &str,
    ) -> Result<StoredShadowForecast, DecisionStoreError> {
        self.put_shadow_forecast_at(
            scope,
            id,
            training_artifact_id,
            target_day_utc,
            known,
            policy_id,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn put_shadow_forecast_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        training_artifact_id: &str,
        target_day_utc: &str,
        known: KnownDayInputs,
        policy_id: &str,
        committed_at: i64,
    ) -> Result<StoredShadowForecast, DecisionStoreError> {
        if !scope.valid() || id.trim().is_empty() || policy_id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        // `load_shadow_forecast` looks the reservation up by exact string, so
        // the frozen record and the reservation row must carry the same
        // canonical spelling.
        let canonical_day = shadow_utc_day_key(target_day_utc)?;
        let target_day_utc = canonical_day.as_str();
        let target = shadow_utc_midnight(target_day_utc)?.timestamp();
        let policy = self.load_shadow_policy(scope, policy_id)?;
        let (_, policy_sha256): (ShadowPilotPolicy, String) =
            self.get_with_digest(scope, "shadow_policy", policy_id)?;
        let effective_from = shadow_utc_midnight(&policy.effective_from_utc)?.timestamp();
        let effective_until = shadow_utc_midnight(&policy.effective_until_utc)?.timestamp();
        if target < effective_from
            || target >= effective_until
            || committed_at < target
            || committed_at > target + policy.issue_deadline_seconds as i64
            || policy.calibration_engine_sha256 != calibration_engine_sha256()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (metadata, source) =
            self.shadow_artifact(scope, training_artifact_id, "shadow_training_export", None)?;
        if metadata.occurred_at != target
            || metadata.ingested_at < target
            || metadata.ingested_at > committed_at
            || metadata.lineage_id != policy.source_lineage
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (export, forecast) =
            validate_shadow_training_source(&source, target_day_utc, &policy, &known)?;
        let record = StoredShadowForecast {
            id: id.into(),
            policy_id: policy_id.into(),
            policy_sha256,
            training_artifact_id: training_artifact_id.into(),
            training_sha256: metadata.content_sha256.clone(),
            source_lineage: metadata.lineage_id,
            queue_id: export.queue_id,
            training_window_start_utc: export.window_start_utc,
            target_day_utc: target_day_utc.into(),
            committed_at,
            min_saturated_days: policy.min_saturated_days,
            known,
            calibration_engine_sha256: calibration_engine_sha256(),
            forecast,
        };
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::reserve_shadow_target_in_tx(
            &tx,
            scope,
            &record.source_lineage,
            target_day_utc,
            id,
            policy_id,
        )?;
        Self::put_in_tx(
            &tx,
            scope,
            "shadow_forecast",
            id,
            &record,
            Some(&[metadata.content_sha256]),
        )?;
        tx.commit()?;
        self.load_shadow_forecast(scope, id)
    }

    pub fn load_shadow_forecast(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<StoredShadowForecast, DecisionStoreError> {
        let record: StoredShadowForecast = self.get(scope, "shadow_forecast", id)?;
        let target = shadow_utc_midnight(&record.target_day_utc)?.timestamp();
        let policy = self.load_shadow_policy(scope, &record.policy_id)?;
        let (_, policy_sha256): (ShadowPilotPolicy, String) =
            self.get_with_digest(scope, "shadow_policy", &record.policy_id)?;
        let effective_from = shadow_utc_midnight(&policy.effective_from_utc)?.timestamp();
        let effective_until = shadow_utc_midnight(&policy.effective_until_utc)?.timestamp();
        let reserved: Option<String> = self
            .open()?
            .query_row(
                "SELECT forecast_id FROM decision_shadow_targets
             WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3 AND target_day_utc=?4",
                params![
                    scope.tenant_id,
                    scope.acl,
                    record.source_lineage,
                    record.target_day_utc
                ],
                |row| row.get(0),
            )
            .optional()?;
        let active_end: Option<i64> = self
            .open()?
            .query_row(
                "SELECT effective_until FROM decision_shadow_policy_windows
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
                params![scope.tenant_id, scope.acl, record.policy_id],
                |row| row.get(0),
            )
            .optional()?;
        if record.id != id
            || record.min_saturated_days == 0
            || reserved.as_deref() != Some(id)
            || record.policy_sha256 != policy_sha256
            || record.source_lineage != policy.source_lineage
            || policy
                .queue_id
                .as_ref()
                .is_some_and(|expected| record.queue_id.as_ref() != Some(expected))
            || record.min_saturated_days != policy.min_saturated_days
            || record.calibration_engine_sha256 != policy.calibration_engine_sha256
            || target < effective_from
            || target >= effective_until
            || active_end.is_none_or(|end| target >= end)
            || record.committed_at < target
            || record.committed_at > target + policy.issue_deadline_seconds as i64
            || record.calibration_engine_sha256.len() != 64
            || !record
                .calibration_engine_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let (metadata, source) = self.shadow_artifact(
            scope,
            &record.training_artifact_id,
            "shadow_training_export",
            Some(&record.training_sha256),
        )?;
        if metadata.occurred_at != target
            || metadata.ingested_at < target
            || metadata.ingested_at > record.committed_at
            || metadata.lineage_id != record.source_lineage
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let export = parse_shadow_training_export(&source)?;
        let (_, through) = shadow_export_window(&export)?;
        if through != target
            || export.window_start_utc != record.training_window_start_utc
            || export.queue_id != record.queue_id
            || record.forecast.training_days != export.observed_days.len()
            || export.observed_days.len() < policy.min_training_days
            || export.observed_days.last().map(|day| day.backlog_end)
                != Some(record.known.opening_backlog)
            || (record.calibration_engine_sha256 == calibration_engine_sha256()
                && forecast_next_day(
                    &export.observed_days,
                    &record.known,
                    record.min_saturated_days,
                )? != record.forecast)
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

}
