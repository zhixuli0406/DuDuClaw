use super::*;

impl DecisionStore {
    pub(crate) fn shadow_test_policy_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        lineage: &str,
        queue_id: &str,
        start: &str,
        end: &str,
        deadline: u32,
        min_training: usize,
        min_saturated: usize,
        registered_at: i64,
    ) -> Result<ShadowPilotPolicy, DecisionStoreError> {
        self.put_shadow_policy_at(
            scope,
            id,
            lineage,
            queue_id,
            start,
            end,
            deadline,
            min_training,
            min_saturated,
            registered_at,
        )
    }

    pub(crate) fn shadow_test_forecast_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        artifact_id: &str,
        target: &str,
        known: KnownDayInputs,
        policy_id: &str,
        committed_at: i64,
    ) -> Result<StoredShadowForecast, DecisionStoreError> {
        self.put_shadow_forecast_at(
            scope,
            id,
            artifact_id,
            target,
            known,
            policy_id,
            committed_at,
        )
    }

    pub(crate) fn shadow_test_score_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        forecast_id: &str,
        artifact_id: &str,
        scored_at: i64,
    ) -> Result<StoredShadowScore, DecisionStoreError> {
        self.put_shadow_score_at(scope, id, forecast_id, artifact_id, scored_at)
    }
}
