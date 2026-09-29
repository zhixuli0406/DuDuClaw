use super::*;

impl DecisionStore {
    pub(crate) fn load_shadow_score_revision(
        &self,
        scope: &DecisionScope,
        forecast_id: &str,
        revision_id: &str,
    ) -> Result<(StoredShadowScore, String), DecisionStoreError> {
        let initial: Option<String> = self
            .open()?
            .query_row(
                "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        let score = if initial.as_deref() == Some(revision_id) {
            self.load_shadow_score(scope, revision_id)?
        } else {
            let correction = self.load_shadow_score_correction(scope, revision_id)?;
            if correction.forecast_id != forecast_id {
                return Err(DecisionStoreError::Invalid);
            }
            correction.corrected_score
        };
        if score.forecast_id != forecast_id {
            return Err(DecisionStoreError::Invalid);
        }
        let kind = if initial.as_deref() == Some(revision_id) {
            "shadow_score"
        } else {
            "shadow_score_correction"
        };
        let (_, digest): (serde_json::Value, String) =
            self.get_with_digest(scope, kind, revision_id)?;
        Ok((score, digest))
    }

    /// Validate a separate day-end ticket export before source registration.
    pub fn preview_shadow_sla_score(
        &self,
        scope: &DecisionScope,
        sla_forecast_id: &str,
        aggregate_score_id: &str,
        observation_source: &str,
    ) -> Result<u64, DecisionStoreError> {
        let sla = self.load_shadow_sla_forecast(scope, sla_forecast_id)?;
        let (aggregate, _) =
            self.load_shadow_score_revision(scope, &sla.forecast_id, aggregate_score_id)?;
        let model: QueueModel = self.get(scope, "model", &sla.model_version)?;
        let (_, opening_source) = self.shadow_artifact(
            scope,
            &sla.opening_artifact_id,
            "shadow_sla_opening_export",
            Some(&sla.opening_sha256),
        )?;
        validate_shadow_sla_observation_source(
            observation_source,
            &opening_source,
            &sla,
            &aggregate,
            &model,
        )
    }

    pub fn preview_shadow_sla_score_correction(
        &self,
        scope: &DecisionScope,
        sla_forecast_id: &str,
        previous_revision_id: &str,
        aggregate_revision_id: &str,
        observation_source: &str,
    ) -> Result<u64, DecisionStoreError> {
        let sla = self.load_shadow_sla_forecast(scope, sla_forecast_id)?;
        let (sla_head, _) = self.shadow_sla_score_head(scope, sla_forecast_id)?;
        let (aggregate_head, _) = self.shadow_score_head(scope, &sla.forecast_id)?;
        if sla_head != previous_revision_id || aggregate_head != aggregate_revision_id {
            return Err(DecisionStoreError::VersionConflict);
        }
        self.preview_shadow_sla_score(
            scope,
            sla_forecast_id,
            aggregate_revision_id,
            observation_source,
        )
    }

    pub fn put_shadow_sla_score(
        &self,
        scope: &DecisionScope,
        id: &str,
        sla_forecast_id: &str,
        aggregate_score_id: &str,
        observation_artifact_id: &str,
    ) -> Result<StoredShadowSlaScore, DecisionStoreError> {
        self.put_shadow_sla_score_at(
            scope,
            id,
            sla_forecast_id,
            aggregate_score_id,
            observation_artifact_id,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn put_shadow_sla_score_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        sla_forecast_id: &str,
        aggregate_score_id: &str,
        observation_artifact_id: &str,
        scored_at: i64,
    ) -> Result<StoredShadowSlaScore, DecisionStoreError> {
        if !scope.valid() || id.trim().is_empty() || observation_artifact_id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let sla = self.load_shadow_sla_forecast(scope, sla_forecast_id)?;
        let forecast = self.load_shadow_forecast(scope, &sla.forecast_id)?;
        let (aggregate, aggregate_sha256) =
            self.load_shadow_score_revision(scope, &sla.forecast_id, aggregate_score_id)?;
        let (_, sla_sha256): (StoredShadowSlaForecast, String) =
            self.get_with_digest(scope, "shadow_sla_forecast", sla_forecast_id)?;
        let (aggregate_head, _) = self.shadow_score_head(scope, &sla.forecast_id)?;
        if aggregate_head != aggregate_score_id || aggregate.scored_at > scored_at {
            return Err(DecisionStoreError::Invalid);
        }
        let end = shadow_utc_midnight(&sla.target_day_utc)?.timestamp() + 86_400;
        let (metadata, source) = self.shadow_artifact(
            scope,
            observation_artifact_id,
            "shadow_sla_observation_export",
            None,
        )?;
        if scored_at < end
            || metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > scored_at
            || metadata.lineage_id != forecast.source_lineage
        {
            return Err(DecisionStoreError::Invalid);
        }
        let observed =
            self.preview_shadow_sla_score(scope, sla_forecast_id, aggregate_score_id, &source)?;
        let record = StoredShadowSlaScore {
            id: id.into(),
            sla_forecast_id: sla_forecast_id.into(),
            sla_forecast_sha256: sla_sha256,
            aggregate_score_id: aggregate_score_id.into(),
            aggregate_score_sha256: aggregate_sha256,
            observation_artifact_id: observation_artifact_id.into(),
            observation_sha256: metadata.content_sha256.clone(),
            scored_at,
            predicted_resolved_within_sla: u64::from(sla.prediction.predicted_resolved_within_sla),
            observed_resolved_within_sla: observed,
            abs_error: u64::from(sla.prediction.predicted_resolved_within_sla).abs_diff(observed),
            no_change_abs_error: u64::from(sla.baselines.no_change).abs_diff(observed),
            seasonal_naive_abs_error: u64::from(sla.baselines.seasonal_naive).abs_diff(observed),
            seven_day_mean_abs_error: u64::from(sla.baselines.seven_day_mean).abs_diff(observed),
        };
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let aggregate_initial: String = tx.query_row(
            "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
            params![scope.tenant_id, scope.acl, sla.forecast_id],
            |row| row.get(0),
        )?;
        let aggregate_current: Option<String> = tx
            .query_row(
                "SELECT correction_id FROM decision_shadow_score_heads
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, sla.forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        if aggregate_current.as_deref().unwrap_or(&aggregate_initial) != aggregate_score_id {
            return Err(DecisionStoreError::VersionConflict);
        }
        tx.execute(
            "INSERT OR IGNORE INTO decision_shadow_sla_scores
            (tenant_id,acl,sla_forecast_id,score_id) VALUES (?1,?2,?3,?4)",
            params![scope.tenant_id, scope.acl, sla_forecast_id, id],
        )?;
        let reserved: Option<String> = tx
            .query_row(
                "SELECT score_id FROM decision_shadow_sla_scores
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
                params![scope.tenant_id, scope.acl, sla_forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        if reserved.as_deref() != Some(id) {
            return Err(DecisionStoreError::VersionConflict);
        }
        let digest = Self::put_in_tx(
            &tx,
            scope,
            "shadow_sla_score",
            id,
            &record,
            Some(&[
                sla.opening_sha256,
                aggregate.observation_sha256,
                metadata.content_sha256,
            ]),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO decision_shadow_sla_score_revision_audit
            (tenant_id,acl,kind,revision_id,sla_forecast_id,recorded_at,payload_sha256)
            VALUES (?1,?2,'shadow_sla_score',?3,?4,?5,?6)",
            params![
                scope.tenant_id,
                scope.acl,
                id,
                sla_forecast_id,
                scored_at,
                digest
            ],
        )?;
        tx.commit()?;
        self.load_shadow_sla_score(scope, id)
    }

    pub fn load_shadow_sla_score(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<StoredShadowSlaScore, DecisionStoreError> {
        let record: StoredShadowSlaScore = self.get(scope, "shadow_sla_score", id)?;
        let reserved: Option<String> = self
            .open()?
            .query_row(
                "SELECT score_id FROM decision_shadow_sla_scores
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
                params![scope.tenant_id, scope.acl, record.sla_forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        let sla = self.load_shadow_sla_forecast(scope, &record.sla_forecast_id)?;
        let forecast = self.load_shadow_forecast(scope, &sla.forecast_id)?;
        let (aggregate, aggregate_sha256) =
            self.load_shadow_score_revision(scope, &sla.forecast_id, &record.aggregate_score_id)?;
        let (_, sla_sha256): (StoredShadowSlaForecast, String) =
            self.get_with_digest(scope, "shadow_sla_forecast", &record.sla_forecast_id)?;
        let end = shadow_utc_midnight(&sla.target_day_utc)?.timestamp() + 86_400;
        let (metadata, source) = self.shadow_artifact(
            scope,
            &record.observation_artifact_id,
            "shadow_sla_observation_export",
            Some(&record.observation_sha256),
        )?;
        if record.id != id
            || reserved.as_deref() != Some(id)
            || record.sla_forecast_sha256 != sla_sha256
            || record.aggregate_score_sha256 != aggregate_sha256
            || aggregate.forecast_id != sla.forecast_id
            || aggregate.scored_at > record.scored_at
            || record.scored_at < end
            || metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > record.scored_at
            || metadata.lineage_id != forecast.source_lineage
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let observed = self.preview_shadow_sla_score(
            scope,
            &record.sla_forecast_id,
            &record.aggregate_score_id,
            &source,
        )?;
        if record.predicted_resolved_within_sla
            != u64::from(sla.prediction.predicted_resolved_within_sla)
            || record.observed_resolved_within_sla != observed
            || record.abs_error != record.predicted_resolved_within_sla.abs_diff(observed)
            || record.no_change_abs_error != u64::from(sla.baselines.no_change).abs_diff(observed)
            || record.seasonal_naive_abs_error
                != u64::from(sla.baselines.seasonal_naive).abs_diff(observed)
            || record.seven_day_mean_abs_error
                != u64::from(sla.baselines.seven_day_mean).abs_diff(observed)
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let (_, stored_digest): (StoredShadowSlaScore, String) =
            self.get_with_digest(scope, "shadow_sla_score", id)?;
        let (audit_digest, audit_sla_id, audit_at) =
            self.shadow_sla_score_revision_meta(scope, id, false)?;
        if audit_digest != stored_digest
            || audit_sla_id != record.sla_forecast_id
            || audit_at != record.scored_at
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    pub(super) fn shadow_sla_score_head(
        &self,
        scope: &DecisionScope,
        sla_forecast_id: &str,
    ) -> Result<(String, bool), DecisionStoreError> {
        let initial: String = self
            .open()?
            .query_row(
                "SELECT score_id FROM decision_shadow_sla_scores
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
                params![scope.tenant_id, scope.acl, sla_forecast_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DecisionStoreError::NotFound)?;
        let correction: Option<String> = self
            .open()?
            .query_row(
                "SELECT correction_id FROM decision_shadow_sla_score_heads
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
                params![scope.tenant_id, scope.acl, sla_forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(correction.map_or((initial, false), |id| (id, true)))
    }

    pub(super) fn shadow_sla_score_revision_meta(
        &self,
        scope: &DecisionScope,
        id: &str,
        correction: bool,
    ) -> Result<(String, String, i64), DecisionStoreError> {
        let kind = if correction {
            "shadow_sla_score_correction"
        } else {
            "shadow_sla_score"
        };
        let (schema, digest, payload, invalidated): (i64, String, String, Option<i64>) = self.open()?.query_row(
            "SELECT schema_version,payload_sha256,payload_json,invalidated_at FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND input_id=?4",
            params![scope.tenant_id, scope.acl, kind, id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?.ok_or(DecisionStoreError::NotFound)?;
        if schema != STORE_SCHEMA_VERSION {
            return Err(DecisionStoreError::Corrupt);
        }
        let audit: Option<(String, i64, String)> = self
            .open()?
            .query_row(
                "SELECT sla_forecast_id,recorded_at,payload_sha256
             FROM decision_shadow_sla_score_revision_audit
             WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND revision_id=?4",
                params![scope.tenant_id, scope.acl, kind, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((sla_id, at, saved)) = audit {
            if saved != digest
                || sla_id.trim().is_empty()
                || (invalidated.is_none()
                    && digest != format!("{:x}", Sha256::digest(payload.as_bytes())))
            {
                return Err(DecisionStoreError::Corrupt);
            }
            return Ok((digest, sla_id, at));
        }
        if invalidated.is_some() || digest != format!("{:x}", Sha256::digest(payload.as_bytes())) {
            return Err(DecisionStoreError::Corrupt);
        }
        if correction {
            let record: StoredShadowSlaScoreCorrection = serde_json::from_str(&payload)?;
            if record.id != id || record.corrected_score.id != id {
                return Err(DecisionStoreError::Corrupt);
            }
            Ok((digest, record.sla_forecast_id, record.corrected_at))
        } else {
            let record: StoredShadowSlaScore = serde_json::from_str(&payload)?;
            if record.id != id {
                return Err(DecisionStoreError::Corrupt);
            }
            Ok((digest, record.sla_forecast_id, record.scored_at))
        }
    }

    /// Append a reviewed SLA correction against the exact current revisions.
    pub fn put_shadow_sla_score_correction(
        &self,
        scope: &DecisionScope,
        id: &str,
        sla_forecast_id: &str,
        previous_revision_id: &str,
        aggregate_revision_id: &str,
        observation_artifact_id: &str,
        reviewer: &str,
        reason: &str,
    ) -> Result<StoredShadowSlaScoreCorrection, DecisionStoreError> {
        self.put_shadow_sla_score_correction_at(
            scope,
            id,
            sla_forecast_id,
            previous_revision_id,
            aggregate_revision_id,
            observation_artifact_id,
            reviewer,
            reason,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn put_shadow_sla_score_correction_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        sla_forecast_id: &str,
        previous_revision_id: &str,
        aggregate_revision_id: &str,
        observation_artifact_id: &str,
        reviewer: &str,
        reason: &str,
        corrected_at: i64,
    ) -> Result<StoredShadowSlaScoreCorrection, DecisionStoreError> {
        if !scope.valid()
            || [
                id,
                sla_forecast_id,
                previous_revision_id,
                aggregate_revision_id,
                observation_artifact_id,
                reviewer,
                reason,
            ]
            .iter()
            .any(|value| value.trim().is_empty())
            || reviewer.len() > 256
            || reason.len() > 2_048
        {
            return Err(DecisionStoreError::Invalid);
        }
        match self.get::<StoredShadowSlaScoreCorrection>(scope, "shadow_sla_score_correction", id) {
            Ok(existing) => {
                if existing.sla_forecast_id == sla_forecast_id
                    && existing.previous_revision_id == previous_revision_id
                    && existing.corrected_score.aggregate_score_id == aggregate_revision_id
                    && existing.corrected_score.observation_artifact_id == observation_artifact_id
                    && existing.reviewer == reviewer
                    && existing.reason == reason
                {
                    return self.load_shadow_sla_score_correction(scope, id);
                }
                return Err(DecisionStoreError::VersionConflict);
            }
            Err(DecisionStoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let (head, previous_is_correction) = self.shadow_sla_score_head(scope, sla_forecast_id)?;
        let initial_id: String = self.open()?.query_row(
            "SELECT score_id FROM decision_shadow_sla_scores
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
            params![scope.tenant_id, scope.acl, sla_forecast_id],
            |row| row.get(0),
        )?;
        let sla = self.load_shadow_sla_forecast(scope, sla_forecast_id)?;
        let (aggregate_head, _) = self.shadow_score_head(scope, &sla.forecast_id)?;
        if head != previous_revision_id
            || aggregate_head != aggregate_revision_id
            || id == previous_revision_id
            || id == initial_id
        {
            return Err(DecisionStoreError::VersionConflict);
        }
        let (previous_digest, previous_sla_id, previous_at) = self.shadow_sla_score_revision_meta(
            scope,
            previous_revision_id,
            previous_is_correction,
        )?;
        let (aggregate, aggregate_digest) =
            self.load_shadow_score_revision(scope, &sla.forecast_id, aggregate_revision_id)?;
        let (_, sla_digest): (StoredShadowSlaForecast, String) =
            self.get_with_digest(scope, "shadow_sla_forecast", sla_forecast_id)?;
        let forecast = self.load_shadow_forecast(scope, &sla.forecast_id)?;
        let end = shadow_utc_midnight(&sla.target_day_utc)?.timestamp() + 86_400;
        if previous_sla_id != sla_forecast_id
            || corrected_at <= previous_at
            || corrected_at < end
            || aggregate.scored_at > corrected_at
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (metadata, source) = self.shadow_artifact(
            scope,
            observation_artifact_id,
            "shadow_sla_observation_export",
            None,
        )?;
        if metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > corrected_at
            || metadata.lineage_id != forecast.source_lineage
        {
            return Err(DecisionStoreError::Invalid);
        }
        let observed =
            self.preview_shadow_sla_score(scope, sla_forecast_id, aggregate_revision_id, &source)?;
        let predicted = u64::from(sla.prediction.predicted_resolved_within_sla);
        let corrected_score = StoredShadowSlaScore {
            id: id.into(),
            sla_forecast_id: sla_forecast_id.into(),
            sla_forecast_sha256: sla_digest,
            aggregate_score_id: aggregate_revision_id.into(),
            aggregate_score_sha256: aggregate_digest,
            observation_artifact_id: observation_artifact_id.into(),
            observation_sha256: metadata.content_sha256.clone(),
            scored_at: corrected_at,
            predicted_resolved_within_sla: predicted,
            observed_resolved_within_sla: observed,
            abs_error: predicted.abs_diff(observed),
            no_change_abs_error: u64::from(sla.baselines.no_change).abs_diff(observed),
            seasonal_naive_abs_error: u64::from(sla.baselines.seasonal_naive).abs_diff(observed),
            seven_day_mean_abs_error: u64::from(sla.baselines.seven_day_mean).abs_diff(observed),
        };
        let record = StoredShadowSlaScoreCorrection {
            id: id.into(),
            sla_forecast_id: sla_forecast_id.into(),
            previous_revision_id: previous_revision_id.into(),
            previous_revision_sha256: previous_digest.clone(),
            reviewer: reviewer.into(),
            reason: reason.into(),
            corrected_at,
            corrected_score,
        };
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let initial: String = tx.query_row(
            "SELECT score_id FROM decision_shadow_sla_scores
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
            params![scope.tenant_id, scope.acl, sla_forecast_id],
            |row| row.get(0),
        )?;
        let current: Option<String> = tx
            .query_row(
                "SELECT correction_id FROM decision_shadow_sla_score_heads
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
                params![scope.tenant_id, scope.acl, sla_forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        let aggregate_current: Option<String> = tx
            .query_row(
                "SELECT correction_id FROM decision_shadow_score_heads
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, sla.forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        let aggregate_initial: String = tx.query_row(
            "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
            params![scope.tenant_id, scope.acl, sla.forecast_id],
            |row| row.get(0),
        )?;
        if current.as_deref().unwrap_or(&initial) != previous_revision_id
            || aggregate_current.as_deref().unwrap_or(&aggregate_initial) != aggregate_revision_id
        {
            return Err(DecisionStoreError::VersionConflict);
        }
        let digest = Self::put_in_tx(
            &tx,
            scope,
            "shadow_sla_score_correction",
            id,
            &record,
            Some(&[
                sla.opening_sha256,
                aggregate.observation_sha256,
                metadata.content_sha256,
            ]),
        )?;
        tx.execute(
            "INSERT INTO decision_shadow_sla_score_revision_audit
            (tenant_id,acl,kind,revision_id,sla_forecast_id,recorded_at,payload_sha256)
            VALUES (?1,?2,'shadow_sla_score_correction',?3,?4,?5,?6)",
            params![
                scope.tenant_id,
                scope.acl,
                id,
                sla_forecast_id,
                corrected_at,
                digest
            ],
        )?;
        tx.execute("INSERT INTO decision_shadow_sla_score_correction_links
            (tenant_id,acl,sla_forecast_id,correction_id,previous_revision_id,previous_revision_sha256)
            VALUES (?1,?2,?3,?4,?5,?6)",
            params![scope.tenant_id, scope.acl, sla_forecast_id, id,
                previous_revision_id, previous_digest])?;
        tx.execute("INSERT INTO decision_shadow_sla_score_heads
            (tenant_id,acl,sla_forecast_id,correction_id) VALUES (?1,?2,?3,?4)
            ON CONFLICT(tenant_id,acl,sla_forecast_id) DO UPDATE SET correction_id=excluded.correction_id",
            params![scope.tenant_id, scope.acl, sla_forecast_id, id])?;
        tx.commit()?;
        self.load_shadow_sla_score_correction(scope, id)
    }

    pub fn load_shadow_sla_score_correction(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<StoredShadowSlaScoreCorrection, DecisionStoreError> {
        let record: StoredShadowSlaScoreCorrection =
            self.get(scope, "shadow_sla_score_correction", id)?;
        let linked: Option<(String, String, String)> = self
            .open()?
            .query_row(
                "SELECT sla_forecast_id,previous_revision_id,previous_revision_sha256
             FROM decision_shadow_sla_score_correction_links
             WHERE tenant_id=?1 AND acl=?2 AND correction_id=?3",
                params![scope.tenant_id, scope.acl, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let initial: String = self
            .open()?
            .query_row(
                "SELECT score_id FROM decision_shadow_sla_scores
             WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3",
                params![scope.tenant_id, scope.acl, record.sla_forecast_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DecisionStoreError::Corrupt)?;
        if record.id == initial {
            return Err(DecisionStoreError::Corrupt);
        }
        if record.previous_revision_id != initial {
            let prior_linked: bool = self.open()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM decision_shadow_sla_score_correction_links
                 WHERE tenant_id=?1 AND acl=?2 AND sla_forecast_id=?3 AND correction_id=?4)",
                params![
                    scope.tenant_id,
                    scope.acl,
                    record.sla_forecast_id,
                    record.previous_revision_id
                ],
                |row| row.get(0),
            )?;
            if !prior_linked {
                return Err(DecisionStoreError::Corrupt);
            }
        }
        let (own_digest, own_sla_id, own_at) =
            self.shadow_sla_score_revision_meta(scope, id, true)?;
        let (_, stored_digest): (StoredShadowSlaScoreCorrection, String) =
            self.get_with_digest(scope, "shadow_sla_score_correction", id)?;
        if own_digest != stored_digest
            || own_sla_id != record.sla_forecast_id
            || own_at != record.corrected_at
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let (previous_digest, previous_sla_id, previous_at) = self.shadow_sla_score_revision_meta(
            scope,
            &record.previous_revision_id,
            record.previous_revision_id != initial,
        )?;
        let score = &record.corrected_score;
        let sla = self.load_shadow_sla_forecast(scope, &record.sla_forecast_id)?;
        let forecast = self.load_shadow_forecast(scope, &sla.forecast_id)?;
        let (aggregate, aggregate_digest) =
            self.load_shadow_score_revision(scope, &sla.forecast_id, &score.aggregate_score_id)?;
        let (_, sla_digest): (StoredShadowSlaForecast, String) =
            self.get_with_digest(scope, "shadow_sla_forecast", &record.sla_forecast_id)?;
        let end = shadow_utc_midnight(&sla.target_day_utc)?.timestamp() + 86_400;
        let (metadata, source) = self.shadow_artifact(
            scope,
            &score.observation_artifact_id,
            "shadow_sla_observation_export",
            Some(&score.observation_sha256),
        )?;
        if record.id != id
            || record.sla_forecast_id != score.sla_forecast_id
            || score.id != id
            || record.previous_revision_id == id
            || linked
                != Some((
                    record.sla_forecast_id.clone(),
                    record.previous_revision_id.clone(),
                    record.previous_revision_sha256.clone(),
                ))
            || previous_digest != record.previous_revision_sha256
            || previous_sla_id != record.sla_forecast_id
            || record.corrected_at <= previous_at
            || record.corrected_at < end
            || aggregate.scored_at > record.corrected_at
            || score.sla_forecast_sha256 != sla_digest
            || score.aggregate_score_sha256 != aggregate_digest
            || score.scored_at != record.corrected_at
            || metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > record.corrected_at
            || metadata.lineage_id != forecast.source_lineage
            || record.reviewer.trim().is_empty()
            || record.reviewer.len() > 256
            || record.reason.trim().is_empty()
            || record.reason.len() > 2_048
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let observed = self.preview_shadow_sla_score(
            scope,
            &record.sla_forecast_id,
            &score.aggregate_score_id,
            &source,
        )?;
        let predicted = u64::from(sla.prediction.predicted_resolved_within_sla);
        if score.predicted_resolved_within_sla != predicted
            || score.observed_resolved_within_sla != observed
            || score.abs_error != predicted.abs_diff(observed)
            || score.no_change_abs_error != u64::from(sla.baselines.no_change).abs_diff(observed)
            || score.seasonal_naive_abs_error
                != u64::from(sla.baselines.seasonal_naive).abs_diff(observed)
            || score.seven_day_mean_abs_error
                != u64::from(sla.baselines.seven_day_mean).abs_diff(observed)
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    /// A corrected aggregate score makes an older SLA revision stale.
    pub fn load_current_shadow_sla_score(
        &self,
        scope: &DecisionScope,
        sla_forecast_id: &str,
    ) -> Result<StoredShadowSlaScore, DecisionStoreError> {
        let (id, correction) = self.shadow_sla_score_head(scope, sla_forecast_id)?;
        let score = if correction {
            self.load_shadow_sla_score_correction(scope, &id)?
                .corrected_score
        } else {
            self.load_shadow_sla_score(scope, &id)?
        };
        let sla = self.load_shadow_sla_forecast(scope, sla_forecast_id)?;
        let (aggregate_head, _) = self.shadow_score_head(scope, &sla.forecast_id)?;
        if score.sla_forecast_id != sla_forecast_id || score.aggregate_score_id != aggregate_head {
            return Err(DecisionStoreError::VersionConflict);
        }
        Ok(score)
    }

}
