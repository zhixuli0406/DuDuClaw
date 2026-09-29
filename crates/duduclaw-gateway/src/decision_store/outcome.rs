use super::*;

impl DecisionStore {
    /// Append an immutable observation for a prior replay. The source export
    /// is parsed and checked before its digest is bound to the journal entry.
    /// No approval or live action is implied by this diagnostic.
    pub fn record_observed_outcome(
        &self,
        scope: &DecisionScope,
        observation_id: &str,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
        expected_replay_hash: &str,
        recorded_by: &str,
        source_bytes: &[u8],
    ) -> Result<StoredObservedOutcome, DecisionStoreError> {
        self.record_observed_outcome_checked(
            scope,
            observation_id,
            snapshot_id,
            model_version,
            scenario_id,
            expected_replay_hash,
            recorded_by,
            source_bytes,
            None,
        )
    }

    /// Verify aggregate rows and SLA counts against one complete ticket export.
    /// The ticket-source digest is bound to the journal for later revocation.
    pub fn record_observed_outcome_with_ticket_source(
        &self,
        scope: &DecisionScope,
        observation_id: &str,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
        expected_replay_hash: &str,
        recorded_by: &str,
        source_bytes: &[u8],
        ticket_source_bytes: &[u8],
        ticket_source_retention_until_utc: &str,
    ) -> Result<StoredObservedOutcome, DecisionStoreError> {
        let observed: ObservedOutcomeExport = serde_json::from_slice(source_bytes)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        verify_ticket_source_observation(
            ticket_source_bytes,
            &observed,
            snapshot_id,
            scenario_id,
            model.sla_days,
        )?;
        let observed_through = chrono::DateTime::parse_from_rfc3339(&observed.observed_through_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        let retention = chrono::DateTime::parse_from_rfc3339(ticket_source_retention_until_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        if retention.offset().local_minus_utc() != 0
            || retention.timestamp_subsec_nanos() != 0
            || retention.timestamp() <= chrono::Utc::now().timestamp()
            || retention <= observed_through
            || retention > observed_through + chrono::Duration::days(366)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let retention_utc = retention.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let ticket_source_sha256 = format!("{:x}", Sha256::digest(ticket_source_bytes));
        self.record_observed_outcome_checked(
            scope,
            observation_id,
            snapshot_id,
            model_version,
            scenario_id,
            expected_replay_hash,
            recorded_by,
            source_bytes,
            Some((
                ticket_source_sha256,
                ticket_sla_label_engine_sha256(),
                ticket_source_bytes.to_vec(),
                retention.timestamp(),
                retention_utc,
            )),
        )
    }

    pub(super) fn record_observed_outcome_checked(
        &self,
        scope: &DecisionScope,
        observation_id: &str,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
        expected_replay_hash: &str,
        recorded_by: &str,
        source_bytes: &[u8],
        ticket_provenance: Option<(String, String, Vec<u8>, i64, String)>,
    ) -> Result<StoredObservedOutcome, DecisionStoreError> {
        if !scope.valid()
            || observation_id.trim().is_empty()
            || recorded_by.trim().is_empty()
            || expected_replay_hash.trim().is_empty()
            || source_bytes.is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        if source_bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(DecisionStoreError::TooLarge);
        }
        let export: ObservedOutcomeExport = serde_json::from_slice(source_bytes)?;
        let run = self.load_daily_run(scope, expected_replay_hash)?;
        if run.snapshot_id != snapshot_id
            || run.model_version != model_version
            || run.scenario_id != scenario_id
        {
            return Err(DecisionStoreError::Invalid);
        }
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let scenario: StaffingScenario = self.get(scope, "scenario", scenario_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let predicted = &run.result;
        if export
            .queue_id
            .as_deref()
            .is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 128)
            || snapshot
                .queue_id
                .as_ref()
                .is_some_and(|expected| export.queue_id.as_ref() != Some(expected))
            || export.observed_days.len() != predicted.days.len()
            || export.observed_days.is_empty()
            || !valid_observed_sla_labels(
                &export.observed_days,
                export.resolved_within_sla_by_day.as_deref(),
                export.sla_days,
            )
            || export.sla_days.is_some_and(|days| days != model.sla_days)
            || export.observed_days.first().map(|day| day.backlog_start)
                != Some(
                    snapshot
                        .initial_backlog
                        .iter()
                        .map(|cohort| cohort.count as u64)
                        .sum(),
                )
            || export.observed_days.iter().enumerate().any(|(index, day)| {
                day.agents != scenario.agents_by_day[index]
                    || day.fixed_extra_capacity != scenario.fixed_extra_capacity_by_day[index]
            })
        {
            return Err(DecisionStoreError::Invalid);
        }
        let cutoff = chrono::DateTime::parse_from_rfc3339(&snapshot.data_cutoff_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        let window_start = chrono::DateTime::parse_from_rfc3339(&export.window_start_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        let observed_through = chrono::DateTime::parse_from_rfc3339(&export.observed_through_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        let min_end = window_start
            .checked_add_signed(chrono::Duration::days(export.observed_days.len() as i64))
            .ok_or(DecisionStoreError::Invalid)?;
        let expected_start =
            first_full_utc_day_at_or_after(cutoff).ok_or(DecisionStoreError::Invalid)?;
        if window_start.offset().local_minus_utc() != 0
            || observed_through.offset().local_minus_utc() != 0
            || window_start.time() != chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("midnight")
            || window_start != expected_start
            || observed_through != min_end
            || observed_through.timestamp() > chrono::Utc::now().timestamp()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let local_run_created_at = self.daily_run_created_at(scope, expected_replay_hash)?;
        if local_run_created_at <= 0 {
            return Err(DecisionStoreError::Invalid);
        }
        validate_observed_days(&export.observed_days)?;
        let assessment = assess_observed_days(
            predicted,
            &export.observed_days,
            export.resolved_within_sla_by_day.as_deref(),
        );
        let observed_source_sha256 = format!("{:x}", Sha256::digest(source_bytes));
        let record = StoredObservedOutcome {
            id: observation_id.to_owned(),
            snapshot_id: snapshot_id.to_owned(),
            queue_id: export.queue_id,
            model_version: model_version.to_owned(),
            scenario_id: scenario_id.to_owned(),
            replay_hash: run.replay_hash,
            recorded_by: recorded_by.to_owned(),
            local_run_created_at_unix: Some(local_run_created_at),
            local_run_precedes_window: Some(local_run_created_at < window_start.timestamp()),
            observed_source_sha256: observed_source_sha256.clone(),
            window_start_utc: export.window_start_utc,
            observed_through_utc: export.observed_through_utc,
            observed_days: export.observed_days,
            resolved_within_sla_by_day: export.resolved_within_sla_by_day,
            sla_days: export.sla_days,
            ticket_source_sha256: ticket_provenance
                .as_ref()
                .map(|(digest, _, _, _, _)| digest.clone()),
            ticket_label_engine_sha256: ticket_provenance
                .as_ref()
                .map(|(_, engine, _, _, _)| engine.clone()),
            ticket_source_retention_until_utc: ticket_provenance
                .as_ref()
                .map(|(_, _, _, _, retention)| retention.clone()),
            assessment,
        };
        let mut refs = snapshot.source_version_hashes;
        refs.push(observed_source_sha256);
        if let Some((digest, _, bytes, retention_until, _)) = ticket_provenance {
            refs.push(digest.clone());
            let mut conn = self.open()?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            Self::put_ticket_source_blob_in_tx(&tx, scope, &digest, &bytes, retention_until)?;
            Self::put_in_tx(
                &tx,
                scope,
                "observed_outcome",
                observation_id,
                &record,
                Some(&refs),
            )?;
            tx.commit()?;
        } else {
            self.put(
                scope,
                "observed_outcome",
                observation_id,
                &record,
                Some(&refs),
            )?;
        }
        Ok(record)
    }

    pub fn get_observed_outcome(
        &self,
        scope: &DecisionScope,
        observation_id: &str,
    ) -> Result<StoredObservedOutcome, DecisionStoreError> {
        let outcome: StoredObservedOutcome = self.get(scope, "observed_outcome", observation_id)?;
        if outcome.ticket_source_sha256.is_some()
            && outcome.ticket_source_retention_until_utc.is_none()
        {
            self.revoke_source_version(
                scope,
                outcome
                    .ticket_source_sha256
                    .as_deref()
                    .expect("checked Some"),
            )?;
            return Err(DecisionStoreError::Revoked);
        }
        if outcome.ticket_source_sha256.is_some() != outcome.ticket_label_engine_sha256.is_some()
            || outcome.ticket_source_sha256.is_some()
                != outcome.ticket_source_retention_until_utc.is_some()
            || outcome
                .ticket_source_sha256
                .as_deref()
                .is_some_and(|digest| {
                    digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if outcome
            .ticket_label_engine_sha256
            .as_deref()
            .is_some_and(|engine| engine != ticket_sla_label_engine_sha256())
        {
            return Err(DecisionStoreError::ReviewDenied);
        }
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &outcome.snapshot_id)?;
        let run = self.load_daily_run(scope, &outcome.replay_hash)?;
        let local_run_created_at = self.daily_run_created_at(scope, &outcome.replay_hash)?;
        let scenario: StaffingScenario = self.get(scope, "scenario", &outcome.scenario_id)?;
        let model: QueueModel = self.get(scope, "model", &outcome.model_version)?;
        let cutoff = chrono::DateTime::parse_from_rfc3339(&snapshot.data_cutoff_utc)
            .map_err(|_| DecisionStoreError::Corrupt)?;
        let start = chrono::DateTime::parse_from_rfc3339(&outcome.window_start_utc)
            .map_err(|_| DecisionStoreError::Corrupt)?;
        let through = chrono::DateTime::parse_from_rfc3339(&outcome.observed_through_utc)
            .map_err(|_| DecisionStoreError::Corrupt)?;
        let expected_through = start
            .checked_add_signed(chrono::Duration::days(outcome.observed_days.len() as i64))
            .ok_or(DecisionStoreError::Corrupt)?;
        let expected_start =
            first_full_utc_day_at_or_after(cutoff).ok_or(DecisionStoreError::Corrupt)?;
        if outcome.id != observation_id
            || outcome
                .queue_id
                .as_deref()
                .is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 128)
            || snapshot
                .queue_id
                .as_ref()
                .is_some_and(|expected| outcome.queue_id.as_ref() != Some(expected))
            || outcome.observed_days.is_empty()
            || !valid_observed_sla_labels(
                &outcome.observed_days,
                outcome.resolved_within_sla_by_day.as_deref(),
                outcome.sla_days,
            )
            || outcome.sla_days.is_some_and(|days| days != model.sla_days)
            || outcome.observed_days.len() != run.result.days.len()
            || outcome.observed_days.first().map(|day| day.backlog_start)
                != Some(
                    snapshot
                        .initial_backlog
                        .iter()
                        .map(|cohort| cohort.count as u64)
                        .sum(),
                )
            || outcome
                .observed_days
                .iter()
                .enumerate()
                .any(|(index, day)| {
                    scenario.agents_by_day.get(index) != Some(&day.agents)
                        || scenario.fixed_extra_capacity_by_day.get(index)
                            != Some(&day.fixed_extra_capacity)
                })
            || crate::decision_calibration::validate(&outcome.observed_days).is_err()
            || outcome.assessment
                != assess_observed_days(
                    &run.result,
                    &outcome.observed_days,
                    outcome.resolved_within_sla_by_day.as_deref(),
                )
            || outcome
                .local_run_created_at_unix
                .is_some_and(|recorded| recorded <= 0 || recorded != local_run_created_at)
            || outcome.local_run_precedes_window.is_some()
                != outcome.local_run_created_at_unix.is_some()
            || outcome
                .local_run_precedes_window
                .is_some_and(|precedes| precedes != (local_run_created_at < start.timestamp()))
            || start.offset().local_minus_utc() != 0
            || through.offset().local_minus_utc() != 0
            || start.time() != chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("midnight")
            || start != expected_start
            || through != expected_through
            || through.timestamp() > chrono::Utc::now().timestamp()
            || run.snapshot_id != outcome.snapshot_id
            || run.model_version != outcome.model_version
            || run.scenario_id != outcome.scenario_id
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if let Some(digest) = outcome.ticket_source_sha256.as_deref() {
            let (source_bytes, retention_until) = self.load_ticket_source_blob(scope, digest)?;
            let recorded_retention = chrono::DateTime::parse_from_rfc3339(
                outcome
                    .ticket_source_retention_until_utc
                    .as_deref()
                    .ok_or(DecisionStoreError::Corrupt)?,
            )
            .map_err(|_| DecisionStoreError::Corrupt)?;
            if recorded_retention.offset().local_minus_utc() != 0
                || recorded_retention.timestamp() != retention_until
                || recorded_retention <= through
                || recorded_retention > through + chrono::Duration::days(366)
            {
                return Err(DecisionStoreError::Corrupt);
            }
            let observed = ObservedOutcomeExport {
                queue_id: outcome.queue_id.clone(),
                window_start_utc: outcome.window_start_utc.clone(),
                observed_through_utc: outcome.observed_through_utc.clone(),
                observed_days: outcome.observed_days.clone(),
                resolved_within_sla_by_day: outcome.resolved_within_sla_by_day.clone(),
                sla_days: outcome.sla_days,
            };
            verify_ticket_source_observation(
                &source_bytes,
                &observed,
                &outcome.snapshot_id,
                &outcome.scenario_id,
                model.sla_days,
            )
            .map_err(|_| DecisionStoreError::Corrupt)?;
        }
        Ok(outcome)
    }

    /// Create an immutable capacity-fit candidate from a recorded outcome.
    /// This does not mutate the historical model or mark the fit as validated.
    pub fn put_outcome_calibration(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
        outcome_id: &str,
        min_saturated_days: usize,
        training_days: usize,
    ) -> Result<StoredOutcomeCalibration, DecisionStoreError> {
        if !scope.valid()
            || fit_id.trim().is_empty()
            || outcome_id.trim().is_empty()
            || !(3..=366).contains(&min_saturated_days)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let outcome = self.get_observed_outcome(scope, outcome_id)?;
        let (parent_model, parent_model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", &outcome.model_version)?;
        let (fit, holdout) = fit_and_score(
            &outcome.observed_days,
            parent_model.service_capacity_per_agent_day,
            training_days,
            min_saturated_days,
        )?;
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &outcome.snapshot_id)?;
        let record = StoredOutcomeCalibration {
            id: fit_id.to_owned(),
            outcome_id: outcome_id.to_owned(),
            replay_hash: outcome.replay_hash,
            observed_source_sha256: outcome.observed_source_sha256.clone(),
            parent_model_version: parent_model.version,
            parent_model_sha256,
            fit_engine_sha256: outcome_fit_engine_sha256(),
            min_saturated_days,
            training_days,
            holdout: Some(holdout),
            fit,
        };
        let mut refs = snapshot.source_version_hashes;
        refs.push(outcome.observed_source_sha256);
        self.put(scope, "outcome_calibration", fit_id, &record, Some(&refs))?;
        Ok(record)
    }

    pub fn load_outcome_calibration(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
    ) -> Result<StoredOutcomeCalibration, DecisionStoreError> {
        let record: StoredOutcomeCalibration = self.get(scope, "outcome_calibration", fit_id)?;
        if record.id != fit_id {
            return Err(DecisionStoreError::Corrupt);
        }
        let outcome = self.get_observed_outcome(scope, &record.outcome_id)?;
        let (parent_model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", &record.parent_model_version)?;
        if record.replay_hash != outcome.replay_hash
            || record.observed_source_sha256 != outcome.observed_source_sha256
            || record.parent_model_version != outcome.model_version
            || record.parent_model_sha256 != model_sha256
            || record.holdout.as_ref().is_some_and(|holdout| {
                record.training_days != holdout.training_days
                    || record.fit.training_days != holdout.training_days
                    || holdout.training_days.checked_add(holdout.holdout_days)
                        != Some(outcome.observed_days.len())
            })
        {
            return Err(DecisionStoreError::Corrupt);
        }
        if record.fit_engine_sha256 == outcome_fit_engine_sha256() {
            let (fit, holdout) = fit_and_score(
                &outcome.observed_days,
                parent_model.service_capacity_per_agent_day,
                record.training_days,
                record.min_saturated_days,
            )
            .map_err(|_| DecisionStoreError::Corrupt)?;
            if record.fit != fit || record.holdout.as_ref() != Some(&holdout) {
                return Err(DecisionStoreError::Corrupt);
            }
        }
        Ok(record)
    }

}
