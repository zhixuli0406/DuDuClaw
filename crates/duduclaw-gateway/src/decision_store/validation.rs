use super::*;

impl DecisionStore {
    pub(super) fn validated_support_export(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        baseline_scenario_id: &str,
    ) -> Result<
        (
            DecisionSnapshot,
            String,
            SupportPilotExport,
            ImportedSupportPilot,
        ),
        DecisionStoreError,
    > {
        if source_bytes.is_empty()
            || source_bytes.len() > MAX_PAYLOAD_BYTES
            || window_start_utc.trim().is_empty()
            || baseline_scenario_id.trim().is_empty()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (snapshot, snapshot_digest): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", snapshot_id)?;
        let source_hash = format!("{:x}", Sha256::digest(source_bytes));
        if !snapshot.source_version_hashes.contains(&source_hash) {
            return Err(DecisionStoreError::Invalid);
        }
        let (tickets, staffing): (Vec<TicketEvent>, Vec<DailyStaffing>) =
            serde_json::from_slice(source_bytes)?;
        let export = SupportPilotExport {
            snapshot_id: snapshot.id.clone(),
            baseline_scenario_id: baseline_scenario_id.into(),
            window_start_utc: window_start_utc.into(),
            data_cutoff_utc: snapshot.data_cutoff_utc.clone(),
            source_version_hashes: snapshot.source_version_hashes.clone(),
            seed: snapshot.seed,
            horizon_days: snapshot.arrivals_by_day.len(),
            tickets,
            staffing,
        };
        let reconstructed = build_support_pilot(&export).map_err(EventSimulationError::from)?;
        if reconstructed.snapshot != snapshot {
            return Err(DecisionStoreError::Invalid);
        }
        Ok((snapshot, snapshot_digest, export, reconstructed))
    }

    pub(super) fn compute_forecast_validation(
        &self,
        scope: &DecisionScope,
        id: &str,
        snapshot_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        baseline_scenario_id: &str,
        min_training_days: usize,
        min_saturated_days: usize,
        calibration_points: usize,
    ) -> Result<StoredForecastValidation, DecisionStoreError> {
        if id.trim().is_empty()
            || min_training_days < 7
            || min_saturated_days == 0
            || calibration_points < 14
        {
            return Err(DecisionStoreError::Invalid);
        }
        let (snapshot, snapshot_sha256, _, pilot) = self.validated_support_export(
            scope,
            snapshot_id,
            source_bytes,
            window_start_utc,
            baseline_scenario_id,
        )?;
        let (baseline, baseline_scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", baseline_scenario_id)?;
        if baseline != pilot.baseline {
            return Err(DecisionStoreError::Invalid);
        }
        let forecast = backtest_one_step_forecast(
            &pilot.observed_days,
            min_training_days,
            min_saturated_days,
        )?;
        let (rolling_interval, fixed_interval) = if forecast.points.len() > calibration_points {
            (
                Some(diagnose_forecast_intervals(&forecast, calibration_points)?),
                Some(diagnose_fixed_forecast_intervals(
                    &forecast,
                    calibration_points,
                )?),
            )
        } else {
            (None, None)
        };
        Ok(StoredForecastValidation {
            id: id.into(),
            snapshot_id: snapshot.id,
            snapshot_sha256,
            source_version_hashes: snapshot.source_version_hashes,
            source_sha256: format!("{:x}", Sha256::digest(source_bytes)),
            window_start_utc: window_start_utc.into(),
            baseline_scenario_id: baseline_scenario_id.into(),
            baseline_scenario_sha256,
            calibration_engine_sha256: calibration_engine_sha256(),
            min_training_days,
            min_saturated_days,
            calibration_points,
            forecast,
            rolling_interval,
            fixed_interval,
        })
    }

    /// Save a reproducible, source-bound historical forecast assessment.
    pub fn put_forecast_validation(
        &self,
        scope: &DecisionScope,
        id: &str,
        snapshot_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        baseline_scenario_id: &str,
        min_training_days: usize,
        min_saturated_days: usize,
        calibration_points: usize,
    ) -> Result<(StoredForecastValidation, String), DecisionStoreError> {
        let record = self.compute_forecast_validation(
            scope,
            id,
            snapshot_id,
            source_bytes,
            window_start_utc,
            baseline_scenario_id,
            min_training_days,
            min_saturated_days,
            calibration_points,
        )?;
        let digest = self.put(
            scope,
            "forecast_validation",
            id,
            &record,
            Some(&record.source_version_hashes),
        )?;
        Ok((record, digest))
    }

    /// Recompute from the exact export before serving a historical assessment.
    pub fn load_forecast_validation(
        &self,
        scope: &DecisionScope,
        id: &str,
        source_bytes: &[u8],
    ) -> Result<StoredForecastValidation, DecisionStoreError> {
        let saved: StoredForecastValidation = self.get(scope, "forecast_validation", id)?;
        let actual = self.compute_forecast_validation(
            scope,
            id,
            &saved.snapshot_id,
            source_bytes,
            &saved.window_start_utc,
            &saved.baseline_scenario_id,
            saved.min_training_days,
            saved.min_saturated_days,
            saved.calibration_points,
        )?;
        if saved != actual {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(saved)
    }

    pub(super) fn compute_sla_holdout(
        &self,
        scope: &DecisionScope,
        id: &str,
        snapshot_id: &str,
        model_version: &str,
        baseline_scenario_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        training_days: usize,
        min_saturated_days: usize,
    ) -> Result<StoredSlaHoldout, DecisionStoreError> {
        if id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let (snapshot, snapshot_sha256, export, pilot) = self.validated_support_export(
            scope,
            snapshot_id,
            source_bytes,
            window_start_utc,
            baseline_scenario_id,
        )?;
        let (model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", model_version)?;
        let (baseline, baseline_scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", baseline_scenario_id)?;
        if baseline != pilot.baseline {
            return Err(DecisionStoreError::Invalid);
        }
        let diagnostic =
            evaluate_ticket_sla_holdout(&export, &model, training_days, min_saturated_days)?;
        let mut engine = Sha256::new();
        engine.update(include_str!("../decision_ingest.rs").as_bytes());
        engine.update(include_str!("../decision_calibration.rs").as_bytes());
        engine.update(include_str!("../decision_sim.rs").as_bytes());
        Ok(StoredSlaHoldout {
            id: id.into(),
            snapshot_id: snapshot.id,
            snapshot_sha256,
            model_version: model.version,
            model_sha256,
            baseline_scenario_id: baseline_scenario_id.into(),
            baseline_scenario_sha256,
            source_version_hashes: snapshot.source_version_hashes,
            source_sha256: format!("{:x}", Sha256::digest(source_bytes)),
            window_start_utc: window_start_utc.into(),
            training_days,
            min_saturated_days,
            engine_sha256: format!("{:x}", engine.finalize()),
            diagnostic,
        })
    }

    /// Persist one exact-source SLA assessment without changing the model.
    pub fn put_sla_holdout(
        &self,
        scope: &DecisionScope,
        id: &str,
        snapshot_id: &str,
        model_version: &str,
        baseline_scenario_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        training_days: usize,
        min_saturated_days: usize,
    ) -> Result<(StoredSlaHoldout, String), DecisionStoreError> {
        let record = self.compute_sla_holdout(
            scope,
            id,
            snapshot_id,
            model_version,
            baseline_scenario_id,
            source_bytes,
            window_start_utc,
            training_days,
            min_saturated_days,
        )?;
        let digest = self.put(
            scope,
            "sla_holdout",
            id,
            &record,
            Some(&record.source_version_hashes),
        )?;
        Ok((record, digest))
    }

    /// Reload from the exact source and refuse changed inputs or calculations.
    pub fn load_sla_holdout(
        &self,
        scope: &DecisionScope,
        id: &str,
        source_bytes: &[u8],
    ) -> Result<StoredSlaHoldout, DecisionStoreError> {
        let saved: StoredSlaHoldout = self.get(scope, "sla_holdout", id)?;
        let actual = self.compute_sla_holdout(
            scope,
            id,
            &saved.snapshot_id,
            &saved.model_version,
            &saved.baseline_scenario_id,
            source_bytes,
            &saved.window_start_utc,
            saved.training_days,
            saved.min_saturated_days,
        )?;
        if saved != actual {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(saved)
    }

}
