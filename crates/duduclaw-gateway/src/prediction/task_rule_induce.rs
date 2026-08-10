//! WP-A4 — task-layer prediction-diff → rule induce/update/prune pipeline
//! (WALL-E arXiv:2410.07484 + WorldEvolver arXiv:2606.30639).
//!
//! See `commercial/docs/design-task-forward-model-2026-08-06.md` §6.5. That
//! section's T8/T9 open questions (does A4 flow through `playbook::Add`,
//! and which "book" settles task-layer rule credit) are resolved by
//! explicit product decision, NOT by picking one of the doc's own listed
//! options: **A4 never touches `playbook/` or `gvu/`/`aee/` at all** — it
//! writes directly into the existing `prediction::rule_lifecycle` ACE/ExpeL
//! machinery that `reflexion.rs` (F2b) already uses for dialogue-layer
//! rules. Concretely:
//!
//! - **Induce**: [`maybe_induce_task_rule`] — mirrors
//!   `reflexion::consolidate_group`'s write path (same B1 novelty gate via
//!   `SqliteMemoryEngine::check_novelty`, same `RuleStats::initial()` +
//!   `PROBATION_RULE_TAG` seeding), but induces from a single settled
//!   [`TaskPredictionError`] rather than N accumulated mistakes. Template
//!   text is built deterministically from the diff's four dimensions by
//!   [`synthesize_task_rule`] — zero LLM cost, and it only ever states
//!   *what* deviated (concrete numbers/classes/outcomes), never *why*
//!   (opus-playbook §5: no evidence ⇒ no guess).
//! - **Inject** ("update" in the WALL-E/A4 sense of feeding the rule back
//!   into the next round): `goal_loop.rs`'s dispatch-prompt assembly calls
//!   [`rule_lifecycle::select_task_rules`] and appends a `## 任務經驗規則`
//!   section after the `<state>` block.
//! - **Prune**: `dispatch_engine.rs`'s settle step calls the SAME, unmodified
//!   `rule_lifecycle::settle_injected_rules` channel-reply's F2a already
//!   uses — task-layer rules ride the identical `rule_stats
//!   {helpful,harmful}` / Janus-probation / net-zero-retirement lifecycle,
//!   just scoped to a different `source_event`
//!   ([`rule_lifecycle::TASK_RULE_SOURCE_EVENT`]) so the two rule
//!   populations never mix in either direction's selection query.
//!
//! Everything in this module is zero-LLM and runtime-neutral — it only
//! reads [`TaskPredictionError`], which is itself built from the
//! runtime-neutral `ToolClass`/`ArtifactShape`/`ObservedOutcome` vocabulary.

use std::path::Path;

use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::TemporalMeta;
use tracing::debug;

use super::engine::ErrorCategory;
use super::rule_gate::HeldOutStats;
use super::rule_lifecycle::{
    RuleStats, PROBATION_RULE_TAG, SHADOW_RULE_TAG, TASK_RULE_SOURCE_EVENT, TASK_RULE_TAG,
};
use super::task_forward::{GoalKind, TaskPredictionError};
use super::task_forward_store::TaskForwardModelConfig;

/// Importance assigned to a freshly induced task rule — same convention as
/// `reflexion.rs`'s F2b consolidated rules (fresh rules start "important",
/// decay only through the rule_lifecycle net-score mechanism, never through
/// the Ebbinghaus recency path since these are semantic-layer entries).
const FRESH_RULE_IMPORTANCE: f64 = 8.0;
/// Confidence stamped on the `TemporalMeta` — task rules are agent-derived,
/// deterministic-template observations, not externally corroborated facts,
/// so this sits below the reflexion consolidation's 0.9 (which benefits
/// from >=2 independent mistake sessions via the GovMem promotion gate — a
/// single settled round here has no equivalent independence check).
const TASK_RULE_CONFIDENCE: f64 = 0.7;

/// The `goal_kind:<kind>` tag a task-layer rule carries so a later
/// task-situation pass can tell whether the rule's trigger matches the current
/// round. Extracted into one function so the write path (this module) and the
/// v1.54 shadow→promotion match path
/// (`rule_lifecycle::score_shadow_candidates_for_task`, via `dispatch_engine`'s
/// settle) share a single definition of the tag format instead of duplicating
/// it — "signal 命中比對重用 task_rule_induce 的既有邏輯".
pub fn goal_kind_tag(goal_kind: GoalKind) -> String {
    format!("goal_kind:{}", goal_kind.as_str())
}

/// Deterministically synthesize a rule sentence from one settled diff's four
/// dimensions (design §3.1: `tool_set_error` / `volume_error` /
/// `outcome_error` / `artifact_error`). Only dimensions with a nonzero error
/// contribute a clause — this states *what* deviated, in concrete terms
/// pulled directly from the prediction/observation pair, and never
/// speculates about *why*.
///
/// Pure function — no I/O, no LLM call, fully unit-testable.
pub fn synthesize_task_rule(error: &TaskPredictionError) -> String {
    let pred = &error.prediction;
    let obs = &error.observation;
    let mut clauses: Vec<String> = Vec::new();

    if error.tool_set_error > 0.0 {
        let missing: Vec<String> = pred
            .expected_tool_classes
            .difference(&obs.observed_tool_classes)
            .map(|c| format!("{c:?}").to_lowercase())
            .collect();
        let extra: Vec<String> = obs
            .observed_tool_classes
            .difference(&pred.expected_tool_classes)
            .map(|c| format!("{c:?}").to_lowercase())
            .collect();
        if !missing.is_empty() {
            clauses.push(format!("預期使用工具類別「{}」但本輪未使用", missing.join("、")));
        }
        if !extra.is_empty() {
            clauses.push(format!("額外使用了預期外的工具類別「{}」", extra.join("、")));
        }
    }

    if error.volume_error > 0.0 {
        let (lo, hi) = pred.expected_call_band;
        if obs.observed_calls > hi {
            clauses.push(format!(
                "呼叫量 {} 次超出預期上限 {} 次",
                obs.observed_calls, hi
            ));
        } else if obs.observed_calls < lo {
            clauses.push(format!(
                "呼叫量 {} 次低於預期下限 {} 次",
                obs.observed_calls, lo
            ));
        }
    }

    if error.outcome_error_applicable && error.outcome_error > 0.0 {
        clauses.push(format!(
            "預期結果為「{}」但實際為「{}」",
            pred.expected_outcome.as_str(),
            obs.observed_outcome.as_str()
        ));
    }

    if error.artifact_error > 0.0 {
        clauses.push(format!(
            "預期產出形態為「{}」但實際為「{}」",
            pred.expected_artifact.as_str(),
            obs.observed_artifact.as_str()
        ));
    }

    if clauses.is_empty() {
        // The caller only reaches induction when `category` is
        // Significant/Critical, so composite_error is high even though no
        // single dimension individually cleared > 0.0 here (shouldn't
        // happen given the §3.2 weight table, but this branch exists so a
        // future weight-table change can never silently produce an empty
        // rule body) — fall back to a composite-only statement rather than
        // fabricate a specific claim.
        clauses.push(format!(
            "整體預測誤差偏高(composite_error={:.2})但無單一維度可歸因",
            error.composite_error
        ));
    }

    format!(
        "agent {} 在 goal_kind {}(phase {})的任務中:{}。",
        pred.agent_id,
        pred.state_key.goal_kind.as_str(),
        pred.state_key.phase.as_str(),
        clauses.join(";"),
    )
}

/// WP-A4 induce: for a Significant/Critical settled diff (design §3.4's T4
/// static 0.5/0.8 boundary — already what `error.category` carries, since
/// `dispatch_engine.rs`'s settle hook diffs against a fresh
/// `AdaptiveThresholds::default()` instance, never a self-calibrating one),
/// synthesize + B1-novelty-gate + write one task-layer rule to semantic
/// memory.
///
/// Returns `Ok(None)` when: the category doesn't warrant induction, the B1
/// novelty gate rejects a near-duplicate of an already-known rule, or
/// memory access fails. All three are non-fatal by design — this is an
/// enhancement layered on top of an already-committed review verdict, same
/// best-effort posture as `settle_forward_model`'s transition-write
/// side-effect.
pub async fn maybe_induce_task_rule(
    memory_db_path: &Path,
    error: &TaskPredictionError,
) -> Result<Option<String>, String> {
    if !matches!(error.category, ErrorCategory::Significant | ErrorCategory::Critical) {
        return Ok(None);
    }

    let rule = synthesize_task_rule(error);
    let agent_id = error.prediction.agent_id.as_str();

    // R2: fresh engine handle via the shared factory — mirrors
    // `reflexion::consolidate_group`'s independent-of-caller instance
    // (that module's own comment explains why: consolidation safety must
    // not depend on whatever the caller's own engine instance has
    // configured, e.g. the opt-in `w_vec` search feature flag). The factory
    // honors `[memory] novelty_gate` from `<home_dir>/config.toml` instead
    // of unconditionally attaching an embedder (previous behavior: this
    // write's B1 gate was ALWAYS active regardless of the config toggle).
    // `memory_db_path` is always `<home_dir>/memory.db` — dispatch_engine.rs
    // (this function's only caller) builds it as `home.join("memory.db")`
    // — so the parent directory IS home_dir; no separate parameter needed,
    // which avoids changing this function's signature (its caller is out of
    // R2's scope — see the R2 handoff notes for dispatch_engine.rs).
    let home_dir = memory_db_path.parent().unwrap_or(memory_db_path);
    let engine = crate::memory_factory::build_memory_engine(memory_db_path, home_dir)
        .map_err(|e| format!("open memory engine: {e}"))?;

    if let Some(rejection) = engine.check_novelty(agent_id, MemoryLayer::Semantic, &rule).await {
        debug!(
            agent_id,
            matched_id = %rejection.matched_id,
            similarity = rejection.similarity,
            threshold = rejection.threshold,
            "A4 B1 novelty gate rejected task-rule induction: {rejection}"
        );
        return Ok(None);
    }

    // v1.54 (DESIGN-lwm-calibration §6): a task-induced rule is an inductive
    // lesson generalized from a SINGLE settled diff — exactly the "outcome /
    // N≈30 全雜訊候選, shadow-only, 禁注入" class. When the held-out gate is on
    // it is therefore born as a **shadow candidate** (excluded from injection)
    // and must earn adoption out-of-sample via
    // `rule_lifecycle::score_shadow_candidates_for_task` before it can be
    // injected. Additive: gate OFF ⇒ no shadow tag, no held-out record, so the
    // pre-v1.54 born-probation behavior is byte-identical. `home_dir` is
    // `memory_db_path.parent()` (same derivation the engine handle above uses),
    // so this reuses the exact config location the caller already targets.
    let held_out_gate_enabled = TaskForwardModelConfig::from_home(home_dir).held_out_gate_enabled;

    let mut tags = vec![
        TASK_RULE_TAG.to_string(),
        // WP2 Janus (arXiv:2606.31121): same trial-period convention as
        // `reflexion.rs` — every freshly induced task rule starts on
        // probation, settled by the shared `rule_lifecycle` machinery.
        PROBATION_RULE_TAG.to_string(),
        goal_kind_tag(error.prediction.state_key.goal_kind),
    ];
    if held_out_gate_enabled {
        tags.push(SHADOW_RULE_TAG.to_string());
    }

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: rule,
        timestamp: chrono::Utc::now(),
        tags,
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: FRESH_RULE_IMPORTANCE,
        access_count: 0,
        last_accessed: None,
        source_event: TASK_RULE_SOURCE_EVENT.to_string(),
    };

    let mut metadata = serde_json::json!({
        "rule_stats": RuleStats::initial(),
        "source_task_id": error.prediction.task_id,
        "source_round": error.prediction.round,
    });
    if held_out_gate_enabled {
        // Seed the prequential birth cursor so the shadow→promotion pass never
        // validates the rule against its own birth batch (train != test).
        HeldOutStats::born(chrono::Utc::now().timestamp().max(0) as u64)
            .merge_into(&mut metadata);
    }

    let meta = TemporalMeta {
        // No `(subject, predicate)` triple — deliberately, same reasoning
        // as `transition.rs`'s trap-avoidance note:
        // `SqliteMemoryEngine::store_temporal`'s conflict resolution fires
        // whenever both are `Some` and supersedes any prior row sharing the
        // triple. Two task-layer rules induced for the SAME `TaskStateKey`
        // but describing two genuinely different deviations (e.g. one about
        // `tool_set_error`, a later one about `outcome_error`) must both
        // stay valid — the B1 novelty gate above is the only intended dedup
        // mechanism here, matching `reflexion.rs`'s own convention.
        confidence: Some(TASK_RULE_CONFIDENCE),
        origin: Some("task_forward_model".to_string()),
        origin_trust: Some(TASK_RULE_CONFIDENCE),
        metadata: Some(metadata),
        ..Default::default()
    };

    engine
        .store_temporal(agent_id, entry, meta)
        .await
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prediction::metacognition::AdaptiveThresholds;
    use crate::prediction::task_forward::{
        diff, ArtifactShape, DiffOutcome, ExpectedOutcome, GoalKind, ObservationFidelity,
        ObservedOutcome, PredictionSource, RoundPhase, TaskObservation, TaskPrediction,
        TaskStateKey,
    };
    use crate::prediction::tool_class::ToolClass;
    use duduclaw_memory::SqliteMemoryEngine;
    use std::collections::BTreeSet;

    fn base_prediction(state_key: TaskStateKey) -> TaskPrediction {
        TaskPrediction {
            prediction_id: "p1".into(),
            task_id: "t1".into(),
            agent_id: "agnes".into(),
            round: 1,
            expected_tool_classes: BTreeSet::from([ToolClass::Read, ToolClass::Write]),
            expected_call_band: (1, 5),
            expected_outcome: ExpectedOutcome::Accept,
            expected_artifact: ArtifactShape::FileWrite,
            confidence: 0.5,
            source: PredictionSource::Statistical,
            created_at: chrono::Utc::now(),
            state_key,
        }
    }

    fn base_observation(fidelity: ObservationFidelity) -> TaskObservation {
        TaskObservation {
            task_id: "t1".into(),
            agent_id: "agnes".into(),
            round: 1,
            observed_tool_classes: BTreeSet::from([ToolClass::Read, ToolClass::Write]),
            observed_calls: 3,
            observed_errors: 0,
            observed_outcome: ObservedOutcome::Accepted,
            observed_artifact: ArtifactShape::FileWrite,
            fidelity,
            window: ("2026-08-06T00:00:00Z".into(), "2026-08-06T00:10:00Z".into()),
            runtime: "claude".into(),
        }
    }

    fn key() -> TaskStateKey {
        TaskStateKey {
            agent_id: "agnes".into(),
            goal_kind: GoalKind::CodingSimple,
            phase: RoundPhase::First,
            has_outcome_spec: false,
        }
    }

    fn computed(pred: TaskPrediction, obs: TaskObservation) -> TaskPredictionError {
        match diff(pred, obs, &AdaptiveThresholds::default()) {
            DiffOutcome::Computed(e) => e,
            DiffOutcome::Unobservable { .. } => panic!("fixture must be observable"),
        }
    }

    // ── synthesize_task_rule: per-dimension coverage ──

    #[test]
    fn synthesize_covers_missing_and_extra_tool_classes() {
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        // expected {Read, Write}; observed {Exec} -> Read/Write missing, Exec extra
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec]);
        let error = computed(pred, obs);
        let rule = synthesize_task_rule(&error);
        assert!(rule.contains("但本輪未使用"), "rule: {rule}");
        assert!(rule.contains("額外使用了預期外的工具類別"), "rule: {rule}");
        assert!(rule.contains("read"), "rule: {rule}");
        assert!(rule.contains("write"), "rule: {rule}");
        assert!(rule.contains("exec"), "rule: {rule}");
    }

    #[test]
    fn synthesize_covers_volume_above_band() {
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_calls = 50;
        let error = computed(pred, obs);
        let rule = synthesize_task_rule(&error);
        assert!(rule.contains("超出預期上限"), "rule: {rule}");
        assert!(rule.contains("50"), "rule: {rule}");
    }

    #[test]
    fn synthesize_covers_volume_below_band() {
        let mut pred = base_prediction(key());
        pred.expected_call_band = (5, 10);
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_calls = 1;
        let error = computed(pred, obs);
        let rule = synthesize_task_rule(&error);
        assert!(rule.contains("低於預期下限"), "rule: {rule}");
    }

    #[test]
    fn synthesize_covers_outcome_mismatch() {
        let mut pred = base_prediction(key());
        pred.expected_outcome = ExpectedOutcome::Accept;
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_outcome = ObservedOutcome::Rejected;
        let error = computed(pred, obs);
        let rule = synthesize_task_rule(&error);
        assert!(rule.contains("預期結果為「accept」但實際為「rejected」"), "rule: {rule}");
    }

    #[test]
    fn synthesize_covers_artifact_mismatch() {
        let mut pred = base_prediction(key());
        pred.expected_artifact = ArtifactShape::TextOnly;
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_artifact = ArtifactShape::ExternalEffect;
        let error = computed(pred, obs);
        let rule = synthesize_task_rule(&error);
        assert!(
            rule.contains("預期產出形態為「text_only」但實際為「external_effect」"),
            "rule: {rule}"
        );
    }

    #[test]
    fn synthesize_names_agent_goal_kind_and_phase() {
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_calls = 999;
        let error = computed(pred, obs);
        let rule = synthesize_task_rule(&error);
        assert!(rule.starts_with("agent agnes 在 goal_kind coding_simple(phase first)"), "rule: {rule}");
    }

    // ── maybe_induce_task_rule: gating + write + B1 novelty ──

    #[tokio::test]
    async fn induce_skips_negligible_and_moderate_categories() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory.db");
        let pred = base_prediction(key());
        let obs = base_observation(ObservationFidelity::Full); // perfect match -> Negligible
        let error = computed(pred, obs);
        assert_eq!(error.category, ErrorCategory::Negligible);
        let result = maybe_induce_task_rule(&db_path, &error).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn induce_writes_rule_for_significant_category() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory.db");
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec, ToolClass::Net]);
        obs.observed_outcome = ObservedOutcome::Rejected; // forces high composite
        let error = computed(pred, obs);
        assert!(matches!(error.category, ErrorCategory::Significant | ErrorCategory::Critical));

        let id = maybe_induce_task_rule(&db_path, &error).await.unwrap();
        assert!(id.is_some());

        let engine = SqliteMemoryEngine::new(&db_path).unwrap();
        let rows = engine
            .list_valid_by_source_event("agnes", TASK_RULE_SOURCE_EVENT, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let (entry, metadata) = &rows[0];
        assert!(entry.tags.iter().any(|t| t == TASK_RULE_TAG));
        assert!(entry.tags.iter().any(|t| t == PROBATION_RULE_TAG));
        assert_eq!(RuleStats::from_metadata(metadata), RuleStats::initial());
    }

    #[tokio::test]
    async fn induce_b1_gate_rejects_near_duplicate_second_induction() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory.db");
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec]);
        obs.observed_outcome = ObservedOutcome::Rejected;
        let error1 = computed(pred.clone(), obs.clone());
        let first = maybe_induce_task_rule(&db_path, &error1).await.unwrap();
        assert!(first.is_some());

        // Same prediction/observation shape (different round) -> byte-identical
        // synthesized rule text -> B1 must reject the second write.
        let mut pred2 = pred;
        pred2.round = 2;
        let mut obs2 = obs;
        obs2.round = 2;
        let error2 = computed(pred2, obs2);
        let second = maybe_induce_task_rule(&db_path, &error2).await.unwrap();
        assert!(second.is_none(), "B1 novelty gate must reject a near-duplicate induction");

        let engine = SqliteMemoryEngine::new(&db_path).unwrap();
        let rows = engine
            .list_valid_by_source_event("agnes", TASK_RULE_SOURCE_EVENT, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "only the first induction should have been written");
    }

    /// R2: `[memory] novelty_gate = false` in `<home_dir>/config.toml` must
    /// reach `maybe_induce_task_rule` via the shared factory
    /// (`crate::memory_factory::build_memory_engine`, no embedder attached
    /// when disabled) and disable the B1 check — mirrors
    /// `induce_b1_gate_rejects_near_duplicate_second_induction` but with the
    /// config toggle off. `memory_db_path` is `<home_dir>/memory.db` in this
    /// test (same as every other test in this module), so writing
    /// `config.toml` at `dir.path()` is exactly the home_dir the function
    /// derives via `memory_db_path.parent()`.
    #[tokio::test]
    async fn induce_novelty_gate_disabled_in_config_lets_duplicate_through() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[memory]\nnovelty_gate = false\n").unwrap();
        let db_path = dir.path().join("memory.db");
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec]);
        obs.observed_outcome = ObservedOutcome::Rejected;
        let error1 = computed(pred.clone(), obs.clone());
        let first = maybe_induce_task_rule(&db_path, &error1).await.unwrap();
        assert!(first.is_some());

        // Same byte-identical synthesized rule text as the B1-gate test above —
        // with the gate disabled it must be written, not rejected.
        let mut pred2 = pred;
        pred2.round = 2;
        let mut obs2 = obs;
        obs2.round = 2;
        let error2 = computed(pred2, obs2);
        let second = maybe_induce_task_rule(&db_path, &error2).await.unwrap();
        assert!(second.is_some(), "novelty_gate=false must let a near-duplicate induction through");

        let engine = SqliteMemoryEngine::new(&db_path).unwrap();
        let rows = engine
            .list_valid_by_source_event("agnes", TASK_RULE_SOURCE_EVENT, 10)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "with the gate disabled both inductions must be written (no subject/predicate \
             triple here, so there's no supersession to collapse the count back to 1)"
        );
    }

    #[tokio::test]
    async fn induce_allows_distinct_deviation_for_same_state_key() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory.db");
        let pred = base_prediction(key());

        let mut obs_tool = base_observation(ObservationFidelity::Full);
        obs_tool.observed_tool_classes = BTreeSet::from([ToolClass::Exec, ToolClass::Net]);
        obs_tool.observed_outcome = ObservedOutcome::Rejected;
        let error_tool = computed(pred.clone(), obs_tool);
        let first = maybe_induce_task_rule(&db_path, &error_tool).await.unwrap();
        assert!(first.is_some());

        let mut pred2 = pred;
        pred2.round = 2;
        let mut obs_artifact = base_observation(ObservationFidelity::Full);
        obs_artifact.round = 2;
        obs_artifact.observed_artifact = ArtifactShape::ExternalEffect;
        obs_artifact.observed_outcome = ObservedOutcome::Escalated;
        let error_artifact = computed(pred2, obs_artifact);
        let second = maybe_induce_task_rule(&db_path, &error_artifact).await.unwrap();
        assert!(
            second.is_some(),
            "a genuinely different deviation for the same state_key must not be blocked \
             by supersession (no subject/predicate triple is set) — B1 only rejects \
             near-duplicate TEXT, and these two rules describe different dimensions"
        );

        let engine = SqliteMemoryEngine::new(&db_path).unwrap();
        let rows = engine
            .list_valid_by_source_event("agnes", TASK_RULE_SOURCE_EVENT, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "both distinct-deviation rules must remain valid");
    }

    // ── v1.54: shadow birth under the held-out gate ──

    #[tokio::test]
    async fn induce_gate_on_by_default_births_shadow_with_born_seq() {
        // No config.toml ⇒ held_out_gate_enabled defaults ON (v1.54), so a
        // task-induced (inductive) rule is born as a shadow candidate carrying
        // a prequential birth cursor — it must earn adoption out-of-sample
        // before it can be injected.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory.db");
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec, ToolClass::Net]);
        obs.observed_outcome = ObservedOutcome::Rejected;
        let error = computed(pred, obs);

        let id = maybe_induce_task_rule(&db_path, &error).await.unwrap().unwrap();
        let engine = SqliteMemoryEngine::new(&db_path).unwrap();
        let entry = engine.get_by_id("agnes", &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == SHADOW_RULE_TAG), "born shadow under the gate");
        assert!(entry.tags.iter().any(|t| t == PROBATION_RULE_TAG), "still on the Janus probation");
        assert!(
            entry.tags.iter().any(|t| *t == goal_kind_tag(GoalKind::CodingSimple)),
            "carries the goal_kind signal used for later shadow→promotion matching"
        );
        let meta = engine.get_metadata("agnes", &id).await.unwrap().unwrap();
        let held = HeldOutStats::from_metadata(&meta);
        assert!(held.born_seq > 0, "gate-on birth seeds a born_seq for the prequential split");

        // Shadow ⇒ excluded from injection until promoted.
        let sel = super::super::rule_lifecycle::select_task_rules(&engine, "agnes", 5).await;
        assert!(sel.is_empty(), "a freshly born shadow rule is not injectable");
    }

    #[tokio::test]
    async fn induce_gate_off_births_plain_probation_rule_byte_identical() {
        // Explicit gate OFF ⇒ pre-v1.54 behavior: born probation (not shadow),
        // no held-out record. select_task_rules injects it immediately.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nheld_out_gate_enabled = false\n",
        )
        .unwrap();
        let db_path = dir.path().join("memory.db");
        let pred = base_prediction(key());
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec, ToolClass::Net]);
        obs.observed_outcome = ObservedOutcome::Rejected;
        let error = computed(pred, obs);

        let id = maybe_induce_task_rule(&db_path, &error).await.unwrap().unwrap();
        let engine = SqliteMemoryEngine::new(&db_path).unwrap();
        let entry = engine.get_by_id("agnes", &id).await.unwrap().unwrap();
        assert!(!entry.tags.iter().any(|t| t == SHADOW_RULE_TAG), "gate off never mints a shadow tag");
        let meta = engine.get_metadata("agnes", &id).await.unwrap().unwrap();
        assert!(
            meta.get(HeldOutStats::METADATA_KEY).is_none(),
            "gate off must not seed a held-out record (byte-identical metadata shape)"
        );
        let sel = super::super::rule_lifecycle::select_task_rules(&engine, "agnes", 5).await;
        assert_eq!(sel.len(), 1, "a non-shadow task rule is immediately injectable");
    }
}
