use super::*;

impl DecisionStore {
    /// Score the frozen forecast only after a complete observed UTC day has
    /// been ingested as a separate, exact-scope source artifact.
    pub fn put_shadow_score(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        observation_artifact_id: &str,
    ) -> Result<StoredShadowScore, DecisionStoreError> {
        self.put_shadow_score_at(
            scope,
            id,
            forecast_id,
            observation_artifact_id,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn put_shadow_score_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        observation_artifact_id: &str,
        scored_at: i64,
    ) -> Result<StoredShadowScore, DecisionStoreError> {
        if !scope.valid() || id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let forecast = self.load_shadow_forecast(scope, forecast_id)?;
        let (_, forecast_sha256): (StoredShadowForecast, String) =
            self.get_with_digest(scope, "shadow_forecast", forecast_id)?;
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        let end = target + 86_400;
        if scored_at < end {
            return Err(DecisionStoreError::Invalid);
        }
        let (metadata, source) = self.shadow_artifact(
            scope,
            observation_artifact_id,
            "shadow_observation_export",
            None,
        )?;
        if metadata.lineage_id != forecast.source_lineage
            || metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > scored_at
            || metadata.ingested_at <= forecast.committed_at
        {
            return Err(DecisionStoreError::Invalid);
        }
        let observed = validate_shadow_observation_source(&source, &forecast)?;
        let score = shadow_score(
            id,
            &forecast,
            &forecast_sha256,
            observation_artifact_id,
            &metadata.content_sha256,
            scored_at,
            observed,
        );
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::reserve_shadow_score_in_tx(&tx, scope, forecast_id, id)?;
        let score_sha256 = Self::put_in_tx(
            &tx,
            scope,
            "shadow_score",
            id,
            &score,
            Some(&[forecast.training_sha256, metadata.content_sha256]),
        )?;
        Self::store_shadow_revision_audit_in_tx(
            &tx,
            scope,
            "shadow_score",
            id,
            forecast_id,
            scored_at,
            &score_sha256,
        )?;
        tx.commit()?;
        self.load_shadow_score(scope, id)
    }

    pub fn load_shadow_score(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<StoredShadowScore, DecisionStoreError> {
        let score: StoredShadowScore = self.get(scope, "shadow_score", id)?;
        let reserved: Option<String> = self
            .open()?
            .query_row(
                "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, score.forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        if score.id != id || reserved.as_deref() != Some(id) {
            return Err(DecisionStoreError::Corrupt);
        }
        let forecast = self.load_shadow_forecast(scope, &score.forecast_id)?;
        let (_, forecast_sha256): (StoredShadowForecast, String) =
            self.get_with_digest(scope, "shadow_forecast", &score.forecast_id)?;
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        let end = target + 86_400;
        let (metadata, source) = self.shadow_artifact(
            scope,
            &score.observation_artifact_id,
            "shadow_observation_export",
            Some(&score.observation_sha256),
        )?;
        if metadata.lineage_id != forecast.source_lineage
            || metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > score.scored_at
            || score.scored_at < end
            || metadata.ingested_at <= forecast.committed_at
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let export: ObservedOutcomeExport = serde_json::from_str(&source)?;
        let (start, through) = shadow_export_window(&export)?;
        if start != target
            || through != end
            || export.observed_days.len() != 1
            || export.queue_id != forecast.queue_id
            || export.observed_days[0].backlog_start != forecast.known.opening_backlog
            || export.observed_days[0].agents != forecast.known.planned_agents
            || export.observed_days[0].fixed_extra_capacity
                != forecast.known.planned_fixed_extra_capacity
            || shadow_score(
                id,
                &forecast,
                &forecast_sha256,
                &score.observation_artifact_id,
                &score.observation_sha256,
                score.scored_at,
                export.observed_days[0].clone(),
            ) != score
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(score)
    }

    pub(super) fn store_shadow_revision_audit(
        &self,
        scope: &DecisionScope,
        kind: &str,
        id: &str,
        forecast_id: &str,
        recorded_at: i64,
        digest: &str,
    ) -> Result<(), DecisionStoreError> {
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::store_shadow_revision_audit_in_tx(
            &tx,
            scope,
            kind,
            id,
            forecast_id,
            recorded_at,
            digest,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn store_shadow_revision_audit_in_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: &DecisionScope,
        kind: &str,
        id: &str,
        forecast_id: &str,
        recorded_at: i64,
        digest: &str,
    ) -> Result<(), DecisionStoreError> {
        if !matches!(kind, "shadow_score" | "shadow_score_correction")
            || !scope.valid()
            || [id, forecast_id, digest]
                .iter()
                .any(|value| value.trim().is_empty())
        {
            return Err(DecisionStoreError::Invalid);
        }
        tx.execute(
            "INSERT OR IGNORE INTO decision_shadow_score_revision_audit
             (tenant_id,acl,kind,revision_id,forecast_id,recorded_at,payload_sha256)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                scope.tenant_id,
                scope.acl,
                kind,
                id,
                forecast_id,
                recorded_at,
                digest
            ],
        )?;
        let saved: (String, i64, String) = tx.query_row(
            "SELECT forecast_id,recorded_at,payload_sha256
             FROM decision_shadow_score_revision_audit
             WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND revision_id=?4",
            params![scope.tenant_id, scope.acl, kind, id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if saved != (forecast_id.to_owned(), recorded_at, digest.to_owned()) {
            return Err(DecisionStoreError::VersionConflict);
        }
        Ok(())
    }

    /// Read only immutable audit metadata, even after a source invalidates a
    /// prior score. The original observation payload is never returned here.
    pub(super) fn shadow_score_revision_meta(
        &self,
        scope: &DecisionScope,
        id: &str,
        correction: bool,
    ) -> Result<(String, String, i64), DecisionStoreError> {
        let kind = if correction {
            "shadow_score_correction"
        } else {
            "shadow_score"
        };
        let (schema, digest, payload, invalidated_at): (i64, String, String, Option<i64>) = self.open()?.query_row(
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
                "SELECT forecast_id,recorded_at,payload_sha256
             FROM decision_shadow_score_revision_audit
             WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND revision_id=?4",
                params![scope.tenant_id, scope.acl, kind, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((forecast_id, recorded_at, saved_digest)) = audit {
            if digest != saved_digest
                || forecast_id.trim().is_empty()
                || (invalidated_at.is_none()
                    && digest != format!("{:x}", Sha256::digest(payload.as_bytes())))
            {
                return Err(DecisionStoreError::Corrupt);
            }
            return Ok((digest, forecast_id, recorded_at));
        }
        if invalidated_at.is_some() || digest != format!("{:x}", Sha256::digest(payload.as_bytes()))
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if correction {
            let record: StoredShadowScoreCorrection = serde_json::from_str(&payload)?;
            if record.id != id || record.corrected_score.id != id {
                return Err(DecisionStoreError::Corrupt);
            }
            self.store_shadow_revision_audit(
                scope,
                kind,
                id,
                &record.forecast_id,
                record.corrected_at,
                &digest,
            )?;
            Ok((digest, record.forecast_id, record.corrected_at))
        } else {
            let record: StoredShadowScore = serde_json::from_str(&payload)?;
            if record.id != id {
                return Err(DecisionStoreError::Corrupt);
            }
            self.store_shadow_revision_audit(
                scope,
                kind,
                id,
                &record.forecast_id,
                record.scored_at,
                &digest,
            )?;
            Ok((digest, record.forecast_id, record.scored_at))
        }
    }

    pub(super) fn shadow_score_head(
        &self,
        scope: &DecisionScope,
        forecast_id: &str,
    ) -> Result<(String, bool), DecisionStoreError> {
        let conn = self.open()?;
        let initial: String = conn
            .query_row(
                "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, forecast_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DecisionStoreError::NotFound)?;
        let correction: Option<String> = conn
            .query_row(
                "SELECT correction_id FROM decision_shadow_score_heads
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(correction.map_or((initial, false), |id| (id, true)))
    }

    /// Append a reviewed correction to the current score revision. Requiring
    /// the caller's expected predecessor prevents competing corrections from
    /// silently replacing one another.
    pub fn put_shadow_score_correction(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        previous_revision_id: &str,
        observation_artifact_id: &str,
        reviewer: &str,
        reason: &str,
    ) -> Result<StoredShadowScoreCorrection, DecisionStoreError> {
        self.put_shadow_score_correction_at(
            scope,
            id,
            forecast_id,
            previous_revision_id,
            observation_artifact_id,
            reviewer,
            reason,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn put_shadow_score_correction_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        previous_revision_id: &str,
        observation_artifact_id: &str,
        reviewer: &str,
        reason: &str,
        corrected_at: i64,
    ) -> Result<StoredShadowScoreCorrection, DecisionStoreError> {
        if !scope.valid()
            || [
                id,
                forecast_id,
                previous_revision_id,
                observation_artifact_id,
                reviewer,
                reason,
            ]
            .iter()
            .any(|value| value.trim().is_empty())
            || reason.len() > 2_048
            || reviewer.len() > 256
        {
            return Err(DecisionStoreError::Invalid);
        }
        let mut corrected_at = corrected_at;
        match self.get::<StoredShadowScoreCorrection>(scope, "shadow_score_correction", id) {
            Ok(existing) => {
                if existing.forecast_id == forecast_id
                    && existing.previous_revision_id == previous_revision_id
                    && existing.corrected_score.observation_artifact_id == observation_artifact_id
                    && existing.reviewer == reviewer
                    && existing.reason == reason
                {
                    let linked: bool = self.open()?.query_row(
                        "SELECT EXISTS(SELECT 1 FROM decision_shadow_score_correction_links
                         WHERE tenant_id=?1 AND acl=?2 AND correction_id=?3)",
                        params![scope.tenant_id, scope.acl, id],
                        |row| row.get(0),
                    )?;
                    if linked {
                        return self.load_shadow_score_correction(scope, id);
                    }
                    corrected_at = existing.corrected_at;
                } else {
                    return Err(DecisionStoreError::VersionConflict);
                }
            }
            Err(DecisionStoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let (head_id, head_is_correction) = self.shadow_score_head(scope, forecast_id)?;
        let initial_id: String = self.open()?.query_row(
            "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
            params![scope.tenant_id, scope.acl, forecast_id],
            |row| row.get(0),
        )?;
        if head_id != previous_revision_id || id == previous_revision_id || id == initial_id {
            return Err(DecisionStoreError::VersionConflict);
        }
        let (previous_digest, previous_forecast, previous_at) =
            self.shadow_score_revision_meta(scope, previous_revision_id, head_is_correction)?;
        let forecast = self.load_shadow_forecast(scope, forecast_id)?;
        let (_, forecast_sha256): (StoredShadowForecast, String) =
            self.get_with_digest(scope, "shadow_forecast", forecast_id)?;
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        let end = target + 86_400;
        if previous_forecast != forecast_id || corrected_at <= previous_at || corrected_at < end {
            return Err(DecisionStoreError::Invalid);
        }
        let (metadata, source) = self.shadow_artifact(
            scope,
            observation_artifact_id,
            "shadow_observation_export",
            None,
        )?;
        if metadata.lineage_id != forecast.source_lineage
            || metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > corrected_at
            || metadata.ingested_at <= forecast.committed_at
        {
            return Err(DecisionStoreError::Invalid);
        }
        let observed = validate_shadow_observation_source(&source, &forecast)?;
        let corrected_score = shadow_score(
            id,
            &forecast,
            &forecast_sha256,
            observation_artifact_id,
            &metadata.content_sha256,
            corrected_at,
            observed,
        );
        let record = StoredShadowScoreCorrection {
            id: id.into(),
            forecast_id: forecast_id.into(),
            previous_revision_id: previous_revision_id.into(),
            previous_revision_sha256: previous_digest.clone(),
            reviewer: reviewer.into(),
            reason: reason.into(),
            corrected_at,
            corrected_score,
        };
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let initial: Option<String> = tx
            .query_row(
                "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        let current: Option<String> = tx
            .query_row(
                "SELECT correction_id FROM decision_shadow_score_heads
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.as_deref().or(initial.as_deref()) != Some(previous_revision_id) {
            return Err(DecisionStoreError::VersionConflict);
        }
        let correction_sha256 = Self::put_in_tx(
            &tx,
            scope,
            "shadow_score_correction",
            id,
            &record,
            Some(&[forecast.training_sha256, metadata.content_sha256]),
        )?;
        Self::store_shadow_revision_audit_in_tx(
            &tx,
            scope,
            "shadow_score_correction",
            id,
            forecast_id,
            corrected_at,
            &correction_sha256,
        )?;
        tx.execute(
            "INSERT INTO decision_shadow_score_correction_links
             (tenant_id,acl,forecast_id,correction_id,previous_revision_id,previous_revision_sha256)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                scope.tenant_id,
                scope.acl,
                forecast_id,
                id,
                previous_revision_id,
                previous_digest
            ],
        )?;
        tx.execute(
            "INSERT INTO decision_shadow_score_heads
             (tenant_id,acl,forecast_id,correction_id) VALUES (?1,?2,?3,?4)
             ON CONFLICT(tenant_id,acl,forecast_id) DO UPDATE SET correction_id=excluded.correction_id",
            params![scope.tenant_id, scope.acl, forecast_id, id],
        )?;
        tx.commit()?;
        self.load_shadow_score_correction(scope, id)
    }

    pub fn load_shadow_score_correction(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<StoredShadowScoreCorrection, DecisionStoreError> {
        let record: StoredShadowScoreCorrection = self.get(scope, "shadow_score_correction", id)?;
        let linked: Option<(String, String, String)> = self
            .open()?
            .query_row(
                "SELECT forecast_id,previous_revision_id,previous_revision_sha256
             FROM decision_shadow_score_correction_links
             WHERE tenant_id=?1 AND acl=?2 AND correction_id=?3",
                params![scope.tenant_id, scope.acl, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if record.id != id
            || record.corrected_score.id != id
            || record.corrected_score.forecast_id != record.forecast_id
            || linked
                != Some((
                    record.forecast_id.clone(),
                    record.previous_revision_id.clone(),
                    record.previous_revision_sha256.clone(),
                ))
            || record.reviewer.trim().is_empty()
            || record.reason.trim().is_empty()
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let initial_id: String = self
            .open()?
            .query_row(
                "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, record.forecast_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DecisionStoreError::Corrupt)?;
        let previous_is_correction = record.previous_revision_id != initial_id;
        if previous_is_correction {
            let prior_linked: bool = self.open()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM decision_shadow_score_correction_links
                 WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3 AND correction_id=?4)",
                params![
                    scope.tenant_id,
                    scope.acl,
                    record.forecast_id,
                    record.previous_revision_id
                ],
                |row| row.get(0),
            )?;
            if !prior_linked {
                return Err(DecisionStoreError::Corrupt);
            }
        }
        let (previous_sha, previous_forecast, previous_at) = self.shadow_score_revision_meta(
            scope,
            &record.previous_revision_id,
            previous_is_correction,
        )?;
        if previous_sha != record.previous_revision_sha256
            || previous_forecast != record.forecast_id
            || record.corrected_at <= previous_at
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let forecast = self.load_shadow_forecast(scope, &record.forecast_id)?;
        let (_, forecast_sha256): (StoredShadowForecast, String) =
            self.get_with_digest(scope, "shadow_forecast", &record.forecast_id)?;
        let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
        let end = target + 86_400;
        let score = &record.corrected_score;
        let (metadata, source) = self.shadow_artifact(
            scope,
            &score.observation_artifact_id,
            "shadow_observation_export",
            Some(&score.observation_sha256),
        )?;
        if metadata.lineage_id != forecast.source_lineage
            || metadata.occurred_at != end
            || metadata.ingested_at < end
            || metadata.ingested_at > record.corrected_at
            || metadata.ingested_at <= forecast.committed_at
            || record.corrected_at < end
            || score.scored_at != record.corrected_at
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let export: ObservedOutcomeExport = serde_json::from_str(&source)?;
        let (start, through) = shadow_export_window(&export)?;
        if start != target
            || through != end
            || export.observed_days.len() != 1
            || export.queue_id != forecast.queue_id
            || export.observed_days[0].backlog_start != forecast.known.opening_backlog
            || export.observed_days[0].agents != forecast.known.planned_agents
            || export.observed_days[0].fixed_extra_capacity
                != forecast.known.planned_fixed_extra_capacity
            || shadow_score(
                id,
                &forecast,
                &forecast_sha256,
                &score.observation_artifact_id,
                &score.observation_sha256,
                record.corrected_at,
                export.observed_days[0].clone(),
            ) != *score
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    /// Resolve the latest reviewed, still-valid score for one forecast.
    /// A revoked latest correction never silently falls back to older data.
    pub fn load_current_shadow_score(
        &self,
        scope: &DecisionScope,
        forecast_id: &str,
    ) -> Result<StoredShadowScore, DecisionStoreError> {
        let (id, correction) = self.shadow_score_head(scope, forecast_id)?;
        if correction {
            self.load_shadow_score_correction(scope, &id)
                .map(|record| record.corrected_score)
        } else {
            self.load_shadow_score(scope, &id)
        }
    }

}
