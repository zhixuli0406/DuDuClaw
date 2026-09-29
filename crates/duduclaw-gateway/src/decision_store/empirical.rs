use super::*;

impl DecisionStore {
    /// Persist an empirical fit only after recomputing it from the exact
    /// source export and the requested training prefix.
    pub fn put_empirical_fit(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
        snapshot_id: &str,
        source_bytes: &[u8],
        window_start_utc: &str,
        baseline_scenario_id: &str,
        fit: &EmpiricalParameterFit,
    ) -> Result<String, DecisionStoreError> {
        let (snapshot, snapshot_digest, _, pilot) = self.validated_support_export(
            scope,
            snapshot_id,
            source_bytes,
            window_start_utc,
            baseline_scenario_id,
        )?;
        let actual = fit_empirical_parameters(
            &pilot.observed_days,
            fit.training_days,
            fit.min_saturated_days,
        )?;
        if &actual != fit {
            return Err(DecisionStoreError::Invalid);
        }
        let record = StoredEmpiricalParameterFit {
            id: fit_id.into(),
            snapshot_id: snapshot.id,
            snapshot_sha256: snapshot_digest,
            source_version_hashes: snapshot.source_version_hashes.clone(),
            engine_sha256: engine_code_sha256(),
            fit_engine_sha256: format!(
                "{:x}",
                Sha256::digest(include_str!("../decision_empirical.rs").as_bytes())
            ),
            fit: fit.clone(),
        };
        // Derived source refs carry an explicit input kind, so revocation
        // cannot confuse this fit ID with a snapshot ID in the same scope.
        self.put(
            scope,
            "parameter_fit",
            fit_id,
            &record,
            Some(&snapshot.source_version_hashes),
        )
    }

    pub fn load_empirical_fit(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
    ) -> Result<StoredEmpiricalParameterFit, DecisionStoreError> {
        let record: StoredEmpiricalParameterFit = self.get(scope, "parameter_fit", fit_id)?;
        let (snapshot, digest): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", &record.snapshot_id)?;
        if record.id != fit_id
            || record.snapshot_sha256 != digest
            || record.source_version_hashes != snapshot.source_version_hashes
            || record.engine_sha256 != engine_code_sha256()
            || record.fit_engine_sha256
                != format!(
                    "{:x}",
                    Sha256::digest(include_str!("../decision_empirical.rs").as_bytes())
                )
            || record.fit.training_days > snapshot.arrivals_by_day.len()
            || record.fit.arrival_samples != snapshot.arrivals_by_day[..record.fit.training_days]
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    pub fn simulate_stored_empirical(
        &self,
        scope: &DecisionScope,
        fit_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        plan: &EmpiricalSensitivityPlan,
    ) -> Result<EmpiricalSensitivityReport, DecisionStoreError> {
        let record = self.load_empirical_fit(scope, fit_id)?;
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", &record.snapshot_id)?;
        let model: QueueModel = self.get(scope, "model", model_version)?;
        let baseline: StaffingScenario = self.get(scope, "scenario", baseline_id)?;
        let alternative: StaffingScenario = self.get(scope, "scenario", alternative_id)?;
        Ok(simulate_empirical_sensitivity(
            &snapshot,
            &model,
            &baseline,
            &alternative,
            &record.fit,
            plan,
        )?)
    }

    pub(super) fn build_empirical_run(
        &self,
        scope: &DecisionScope,
        run_id: &str,
        fit_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        plan: &EmpiricalSensitivityPlan,
    ) -> Result<StoredEmpiricalRun, DecisionStoreError> {
        if run_id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let fit = self.load_empirical_fit(scope, fit_id)?;
        let (_, fit_sha256): (StoredEmpiricalParameterFit, String) =
            self.get_with_digest(scope, "parameter_fit", fit_id)?;
        let (snapshot, snapshot_sha256): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", &fit.snapshot_id)?;
        let (model, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", model_version)?;
        let (baseline, baseline_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", baseline_id)?;
        let (alternative, alternative_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", alternative_id)?;
        let report = simulate_empirical_sensitivity(
            &snapshot,
            &model,
            &baseline,
            &alternative,
            &fit.fit,
            plan,
        )?;
        Ok(StoredEmpiricalRun {
            id: run_id.into(),
            fit_id: fit_id.into(),
            fit_sha256,
            snapshot_id: snapshot.id,
            snapshot_sha256,
            source_version_hashes: snapshot.source_version_hashes,
            data_cutoff_utc: snapshot.data_cutoff_utc,
            seed: snapshot.seed,
            horizon_days: snapshot.arrivals_by_day.len(),
            initial_backlog_sha256: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&snapshot.initial_backlog)?)
            ),
            engine_sha256: engine_code_sha256(),
            fit_engine_sha256: fit.fit_engine_sha256,
            model_version: model.version,
            model_sha256,
            baseline_id: baseline.id,
            baseline_sha256,
            alternative_id: alternative.id,
            alternative_sha256,
            plan: plan.clone(),
            report,
        })
    }

    /// Store a run manifest only after recomputing its result from active,
    /// exact-scope immutable inputs. Source revocation scrubs this record.
    pub fn put_empirical_run(
        &self,
        scope: &DecisionScope,
        run_id: &str,
        fit_id: &str,
        model_version: &str,
        baseline_id: &str,
        alternative_id: &str,
        plan: &EmpiricalSensitivityPlan,
    ) -> Result<String, DecisionStoreError> {
        let record = self.build_empirical_run(
            scope,
            run_id,
            fit_id,
            model_version,
            baseline_id,
            alternative_id,
            plan,
        )?;
        self.put(
            scope,
            "empirical_run",
            run_id,
            &record,
            Some(&record.source_version_hashes),
        )
    }

    /// Revalidate all input digests, active sources, engine code, and result.
    pub fn load_empirical_run(
        &self,
        scope: &DecisionScope,
        run_id: &str,
    ) -> Result<StoredEmpiricalRun, DecisionStoreError> {
        let record: StoredEmpiricalRun = self.get(scope, "empirical_run", run_id)?;
        let actual = self.build_empirical_run(
            scope,
            run_id,
            &record.fit_id,
            &record.model_version,
            &record.baseline_id,
            &record.alternative_id,
            &record.plan,
        )?;
        if record != actual {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    /// Recompute with current code before committing the screen. The source
    /// reference makes revocation scrub this derived record atomically.
    pub fn put_policy_screen(
        &self,
        scope: &DecisionScope,
        run_id: &str,
        resource_plan: &StaffingResourcePlan,
        criteria: &JointRiskScreenCriteria,
    ) -> Result<StoredPolicyScreen, DecisionStoreError> {
        let report = self.screen_empirical_policy(scope, run_id, resource_plan, criteria)?;
        let run = self.load_empirical_run(scope, run_id)?;
        let (_, empirical_run_sha256): (StoredEmpiricalRun, String) =
            self.get_with_digest(scope, "empirical_run", run_id)?;
        if report.empirical_run_id != run.id || report.empirical_run_sha256 != empirical_run_sha256
        {
            return Err(DecisionStoreError::Corrupt);
        }
        let record = StoredPolicyScreen {
            replay_hash: report.replay_hash.clone(),
            empirical_run_id: run.id,
            empirical_run_sha256,
            source_version_hashes: run.source_version_hashes.clone(),
            report,
        };
        self.put(
            scope,
            "policy_screen",
            &record.replay_hash,
            &record,
            Some(&record.source_version_hashes),
        )?;
        Ok(record)
    }

    /// Read the exact historical screen without substituting a later policy
    /// engine. Verify its immutable input digests and active source binding.
    pub fn load_policy_screen(
        &self,
        scope: &DecisionScope,
        replay_hash: &str,
    ) -> Result<StoredPolicyScreen, DecisionStoreError> {
        let record: StoredPolicyScreen = self.get(scope, "policy_screen", replay_hash)?;
        let (run, run_sha256): (StoredEmpiricalRun, String) =
            self.get_with_digest(scope, "empirical_run", &record.empirical_run_id)?;
        let (snapshot, snapshot_sha256): (DecisionSnapshot, String) =
            self.get_with_digest(scope, "snapshot", &run.snapshot_id)?;
        let (_, model_sha256): (QueueModel, String) =
            self.get_with_digest(scope, "model", &run.model_version)?;
        let (_, baseline_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", &run.baseline_id)?;
        let (_, alternative_sha256): (StaffingScenario, String) =
            self.get_with_digest(scope, "scenario", &run.alternative_id)?;
        if record.replay_hash != replay_hash
            || record.report.replay_hash != replay_hash
            || !policy_screen_hash_matches(&record.report)?
            || record.empirical_run_id != run.id
            || record.empirical_run_sha256 != run_sha256
            || record.report.empirical_run_id != run.id
            || record.report.empirical_run_sha256 != run_sha256
            || record.report.empirical_report_hash != run.report.replay_hash
            || record.source_version_hashes != run.source_version_hashes
            || run.snapshot_sha256 != snapshot_sha256
            || run.source_version_hashes != snapshot.source_version_hashes
            || run.model_sha256 != model_sha256
            || run.baseline_sha256 != baseline_sha256
            || run.alternative_sha256 != alternative_sha256
            || record.report.policy_sweep.snapshot_id != run.snapshot_id
            || record.report.policy_sweep.model_version != run.model_version
            || record.report.policy_sweep.baseline_scenario_id != run.baseline_id
            || record.report.policy_sweep.alternative_scenario_id != run.alternative_id
            || record.report.resource_plan.max_final_backlog != run.plan.max_final_backlog
            || record.report.resource_plan.max_staff_cost_cents != run.plan.max_staff_cost_cents
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }
}
