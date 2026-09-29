use super::*;

impl DecisionStore {
    pub fn replay(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
    ) -> Result<SimulationResult, DecisionStoreError> {
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let scenario: StaffingScenario = self.get(scope, "scenario", scenario_id)?;
        let result = simulate(&snapshot, &model, &scenario)?;
        self.verify_simulation_inputs_still_current(scope, &snapshot, &model, &[&scenario])?;
        Ok(result)
    }

    /// A source may be invalidated while a simulation is running. Recheck
    /// immediately before returning the computed result, including every
    /// immutable input and the snapshot's causal bindings.
    pub(crate) fn verify_simulation_inputs_still_current(
        &self,
        scope: &DecisionScope,
        snapshot: &DecisionSnapshot,
        model: &QueueModel,
        scenarios: &[&StaffingScenario],
    ) -> Result<(), DecisionStoreError> {
        if self.get::<DecisionSnapshot>(scope, "snapshot", &snapshot.id)? != *snapshot
            || self.get::<QueueModel>(scope, "model", &model.version)? != *model
        {
            return Err(DecisionStoreError::VersionConflict);
        }
        for scenario in scenarios {
            if self.get::<StaffingScenario>(scope, "scenario", &scenario.id)? != **scenario {
                return Err(DecisionStoreError::VersionConflict);
            }
        }
        Ok(())
    }

    /// Reconstruct an event scenario from exact stored inputs and the original
    /// ticket/staffing bytes. Snapshot reads recheck any bound causal source.
    pub fn replay_ticket_events(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        baseline_scenario_id: &str,
        config: &EventQueueConfig,
    ) -> Result<EventSimulationResult, DecisionStoreError> {
        let (_, _, export, _) = self.validated_support_export(
            scope,
            snapshot_id,
            source_bytes,
            window_start_utc,
            baseline_scenario_id,
        )?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let scenario: StaffingScenario = self.get(scope, "scenario", scenario_id)?;
        Ok(simulate_ticket_events(&export, &model, &scenario, config)?)
    }

    pub fn put_event_run(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        baseline_scenario_id: &str,
        config: &EventQueueConfig,
    ) -> Result<StoredEventRun, DecisionStoreError> {
        let result = self.replay_ticket_events(
            scope,
            snapshot_id,
            model_version,
            scenario_id,
            source_bytes,
            window_start_utc,
            baseline_scenario_id,
            config,
        )?;
        let (snapshot, snapshot_sha256): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", snapshot_id)?;
        let (_, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", model_version)?;
        let (_, scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", scenario_id)?;
        let (_, baseline_scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", baseline_scenario_id)?;
        let record = StoredEventRun {
            replay_hash: result.replay_hash.clone(),
            daily_engine_sha256: engine_code_sha256(),
            snapshot_id: snapshot_id.into(),
            snapshot_sha256,
            source_version_hashes: snapshot.source_version_hashes.clone(),
            source_sha256: format!("{:x}", Sha256::digest(source_bytes)),
            model_version: model_version.into(),
            model_sha256,
            scenario_id: scenario_id.into(),
            scenario_sha256,
            baseline_scenario_id: baseline_scenario_id.into(),
            baseline_scenario_sha256,
            window_start_utc: window_start_utc.into(),
            config: config.clone(),
            result,
        };
        self.put(
            scope,
            "event_run",
            &record.replay_hash,
            &record,
            Some(&snapshot.source_version_hashes),
        )?;
        Ok(record)
    }

    pub fn load_event_run(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
        source_bytes: &[u8],
    ) -> Result<StoredEventRun, DecisionStoreError> {
        if source_bytes.is_empty() || source_bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(DecisionStoreError::Invalid);
        }
        let run: StoredEventRun = self.get(scope, "event_run", replay_hash)?;
        if run.replay_hash != replay_hash
            || run.result.replay_hash != replay_hash
            || run.source_sha256 != format!("{:x}", Sha256::digest(source_bytes))
            || run.result.snapshot_id != run.snapshot_id
            || run.result.model_version != run.model_version
            || run.result.scenario_id != run.scenario_id
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let (snapshot, snapshot_sha256): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", &run.snapshot_id)?;
        let (_, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", &run.model_version)?;
        let (_, scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", &run.scenario_id)?;
        let (_, baseline_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", &run.baseline_scenario_id)?;
        if run.snapshot_sha256 != snapshot_sha256
            || run.source_version_hashes != snapshot.source_version_hashes
            || !snapshot.source_version_hashes.contains(&run.source_sha256)
            || run.model_sha256 != model_sha256
            || run.scenario_sha256 != scenario_sha256
            || run.baseline_scenario_sha256 != baseline_sha256
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(run)
    }

}
