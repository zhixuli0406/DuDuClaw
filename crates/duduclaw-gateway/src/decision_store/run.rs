use super::*;

impl DecisionStore {
    /// Persist a daily prediction while this engine version is installed.
    /// Later observation ingest can use this historical result without
    /// running changed engine code against old inputs.
    pub fn put_daily_run(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        model_version: &str,
        scenario_id: &str,
    ) -> Result<StoredDailyRun, DecisionStoreError> {
        let (snapshot, snapshot_sha256): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", snapshot_id)?;
        let (model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", model_version)?;
        let (scenario, scenario_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", scenario_id)?;
        let result = simulate(&snapshot, &model, &scenario)?;
        self.verify_simulation_inputs_still_current(scope, &snapshot, &model, &[&scenario])?;
        let record = StoredDailyRun {
            replay_hash: result.replay_hash.clone(),
            snapshot_id: snapshot_id.to_owned(),
            snapshot_sha256,
            source_version_hashes: snapshot.source_version_hashes.clone(),
            model_version: model_version.to_owned(),
            model_sha256,
            scenario_id: scenario_id.to_owned(),
            scenario_sha256,
            result,
        };
        self.put(
            scope,
            "daily_run",
            &record.replay_hash,
            &record,
            Some(&snapshot.source_version_hashes),
        )?;
        self.load_daily_run(scope, &record.replay_hash)?;
        Ok(record)
    }

    /// Load a stored run and report whether current engine code still carries
    /// the engine identity recorded with it. The result is never recomputed;
    /// "historical" and "reproducible by current code" stay two facts.
    pub fn load_daily_run_with_engine_state(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<LoadedRun<StoredDailyRun>, DecisionStoreError> {
        let run = self.load_daily_run(scope, replay_hash)?;
        let engine_matches_current = run.result.engine_sha256 == engine_code_sha256();
        Ok(LoadedRun {
            run,
            engine_matches_current,
        })
    }

    /// Ticket-event twin of [`Self::load_daily_run_with_engine_state`]. Both
    /// engine identities (daily stock-flow and ticket-event) must match for
    /// the run to count as reproducible by current code.
    pub fn load_event_run_with_engine_state(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
        source_bytes: &[u8],
    ) -> Result<LoadedRun<StoredEventRun>, DecisionStoreError> {
        let run = self.load_event_run(scope, replay_hash, source_bytes)?;
        let engine_matches_current = run.daily_engine_sha256 == engine_code_sha256()
            && run.result.event_engine_sha256 == event_engine_sha256();
        Ok(LoadedRun {
            run,
            engine_matches_current,
        })
    }

    pub fn load_daily_run(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredDailyRun, DecisionStoreError> {
        let run: StoredDailyRun = self.get(scope, "daily_run", replay_hash)?;
        if run.replay_hash != replay_hash
            || run.result.replay_hash != replay_hash
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
        if run.snapshot_sha256 != snapshot_sha256
            || run.source_version_hashes != snapshot.source_version_hashes
            || run.model_sha256 != model_sha256
            || run.scenario_sha256 != scenario_sha256
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(run)
    }

    /// A review receipt expires when current simulator code no longer
    /// reproduces every field of the stored run, even if its SQLite payload
    /// digest and immutable input digests still validate.
    pub(super) fn current_daily_run_for_review(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredDailyRun, DecisionStoreError> {
        let run = self.load_daily_run(scope, replay_hash)?;
        let current = self.replay(
            scope,
            &run.snapshot_id,
            &run.model_version,
            &run.scenario_id,
        )?;
        if current.replay_hash != replay_hash || current != run.result {
            return Err(DecisionStoreError::VersionConflict);
        }
        Ok(run)
    }

    pub(super) fn daily_run_created_at(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<i64, DecisionStoreError> {
        self.decision_input_created_at(scope, "daily_run", replay_hash)
    }

    pub(crate) fn decision_input_created_at(
        &self,
        scope: &DecisionScope,
        kind: &str,
        id: &str,
    ) -> Result<i64, DecisionStoreError> {
        self.open()?
            .query_row(
                "SELECT created_at FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND input_id=?4",
                params![scope.tenant_id, scope.acl, kind, id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DecisionStoreError::NotFound)
    }

}
