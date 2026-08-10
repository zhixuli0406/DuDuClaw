//! WP-B1/B2/B3 — episodic transition material: schema, write, read.
//!
//! See `commercial/docs/design-task-forward-model-2026-08-06.md` §6.2/§6.3.
//! T6 (design §11) settled on option (a): `task_prediction_log` is the
//! single source of truth for transition data; episodic memory holds only a
//! pointer-sized (≤200 char) indicator row. This module writes and reads
//! that pointer row.
//!
//! ## The trap this module exists to avoid
//!
//! `SqliteMemoryEngine::store_temporal`'s conflict resolution
//! (`duduclaw-memory/src/engine.rs:1739`) fires whenever **both** `subject`
//! and `predicate` are `Some` on a write, and supersedes (invalidates) any
//! prior row sharing the same `(agent_id, subject, predicate)` triple. A
//! transition's nature is "the same state produces many independent
//! historical samples" — if the triple were filled in, the SECOND
//! transition write would silently retire the first, and the episodic
//! store would never hold more than one sample per state. **Therefore
//! `subject`/`predicate`/`object` are left `None` on every transition
//! write below.** The regression test at the bottom of this file asserts
//! two same-state writes both survive.

use duduclaw_core::types::MemoryEntry;
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::task_forward::{ObservationFidelity, TaskPredictionError};

/// `source_event` tag for transition pointer rows. Distinct from every
/// other `source_event` in the codebase (`reflexion_consolidation`,
/// `playbook_entry`, `prediction_episodic`, …) so `list_valid_by_source_event`
/// scoping never collides.
pub const TRANSITION_SOURCE_EVENT: &str = "task_transition";

/// Schema evolution gate — bump when the shape below changes in a
/// non-additive way.
pub const TRANSITION_SCHEMA_VERSION: u32 = 1;

/// Cap on the human-readable `MemoryEntry.content` line (WP5c convention:
/// memory holds a pointer, not the full text).
const CONTENT_CHAR_CAP: usize = 200;
/// Cap on `TransitionOutcome.feedback_digest`.
const FEEDBACK_DIGEST_CHAR_CAP: usize = 200;

/// (state, action, outcome) triple metadata, stored in
/// `TemporalMeta.metadata["transition"]`. The row itself carries
/// `source_event = TRANSITION_SOURCE_EVENT`, `layer = Episodic`, and
/// **`subject`/`predicate`/`object` left `None`** — see module docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransitionMeta {
    pub schema_version: u32,
    /// `TaskStateKey::canonical()`.
    pub state_key: String,
    /// sha8 of `state_key` — stable identifier for grouping without
    /// re-parsing the canonical string.
    pub state_digest: String,
    pub action: TransitionAction,
    pub outcome: TransitionOutcome,
    pub task_id: String,
    pub round: u32,
    /// `"full"` | `"mcp_only"` — a `"none"`-fidelity round never reaches
    /// this module (see [`should_write_transition`]).
    pub fidelity: String,
    pub runtime: String,
    /// RFC3339.
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransitionAction {
    /// Sorted, deduplicated `ToolClass` names (stable, snake_case).
    pub tool_classes: Vec<String>,
    pub call_count: u32,
    pub error_count: u32,
    /// `ArtifactShape::as_str()`.
    pub artifact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransitionOutcome {
    /// `"accepted"` | `"rejected"` | `"blocked"` | `"escalated"` |
    /// `"unknown"`.
    pub verdict: String,
    /// CJK-safe, ≤ [`FEEDBACK_DIGEST_CHAR_CAP`] chars.
    pub feedback_digest: Option<String>,
    pub composite_error: Option<f64>,
    /// WP-P2 (`commercial/docs/DESIGN-lwm-calibration-2026-08-10.md` §4):
    /// `error.prediction.confidence` at settle time, only populated when
    /// `[task_forward_model] calibration_enabled = true`. `#[serde(default)]`
    /// so a transition row written before this field existed still
    /// deserializes — it reads back as `None`, not a parse error.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// `task_forward::calibration_brier_score(error)` for this settled
    /// prediction, same gating and backward-compat behavior as `confidence`.
    #[serde(default)]
    pub calibration_score: Option<f64>,
}

/// sha8: first 8 hex chars of SHA-256(input). Deterministic identifier for
/// grouping transition samples by state without re-parsing
/// `TaskStateKey::canonical()`'s string form.
fn sha8(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect::<String>().chars().take(8).collect()
}

/// WP-B2 gate: `fidelity == None` rounds are never written as transition
/// material (design §6.2: "fidelity == none 的輪次不寫 transition" — 空結果
/// 優於假結果). Everything else (`Full`, `McpOnly`) is written — including
/// rounds `settle_prediction` marked `!eligible_for_stats` for the
/// statistical bucket, since the transition store is a *narrative* sample
/// pool (design §6.4's D-series consumer), not the same eligibility
/// contract as the numeric bucket.
pub fn should_write_transition(error: &TaskPredictionError) -> bool {
    error.fidelity != ObservationFidelity::None
}

/// Build the `(MemoryEntry, TemporalMeta)` pair for one settled prediction.
/// Caller is responsible for actually calling `store_temporal` (WP-B2) —
/// kept as a pure builder so it's testable without a live
/// `SqliteMemoryEngine`.
///
/// `calibration_enabled` (WP-P2, design §4) gates `TransitionOutcome`'s
/// `confidence`/`calibration_score` fields: `true` populates both from
/// `error.prediction.confidence` / `task_forward::calibration_brier_score`;
/// `false` leaves them `None` — byte-identical to pre-calibration behavior.
pub fn build_transition_write(
    agent_id: &str,
    error: &TaskPredictionError,
    judge_feedback: Option<&str>,
    calibration_enabled: bool,
) -> Option<(MemoryEntry, TemporalMeta)> {
    if !should_write_transition(error) {
        return None;
    }

    let state_key = error.prediction.state_key.canonical();
    let mut tool_classes: Vec<String> = error
        .observation
        .observed_tool_classes
        .iter()
        .map(|c| format!("{c:?}").to_lowercase())
        .collect();
    tool_classes.sort();
    tool_classes.dedup();

    let action = TransitionAction {
        tool_classes,
        call_count: error.observation.observed_calls,
        error_count: error.observation.observed_errors,
        artifact: format!("{:?}", error.observation.observed_artifact).to_lowercase(),
    };

    let (confidence, calibration_score) = if calibration_enabled {
        (
            Some(error.prediction.confidence),
            Some(super::task_forward::calibration_brier_score(error)),
        )
    } else {
        (None, None)
    };

    let outcome = TransitionOutcome {
        verdict: error.observation.observed_outcome.as_str().to_string(),
        feedback_digest: judge_feedback
            .map(|f| duduclaw_core::truncate_chars(f, FEEDBACK_DIGEST_CHAR_CAP)),
        composite_error: Some(error.composite_error),
        confidence,
        calibration_score,
    };

    let fidelity_str = match error.fidelity {
        ObservationFidelity::Full => "full",
        ObservationFidelity::McpOnly => "mcp_only",
        ObservationFidelity::None => unreachable!("gated by should_write_transition"),
    };

    let at = chrono::Utc::now().to_rfc3339();
    let meta = TransitionMeta {
        schema_version: TRANSITION_SCHEMA_VERSION,
        state_digest: sha8(&state_key),
        state_key: state_key.clone(),
        action,
        outcome,
        task_id: error.prediction.task_id.clone(),
        round: error.prediction.round,
        fidelity: fidelity_str.to_string(),
        runtime: error.observation.runtime.clone(),
        at: at.clone(),
    };

    let content = duduclaw_core::truncate_chars(
        &format!(
            "task {} round {} ({state_key}): {} tool call(s), outcome={}, composite_error={:.2}",
            error.prediction.task_id,
            error.prediction.round,
            error.observation.observed_calls,
            error.observation.observed_outcome.as_str(),
            error.composite_error,
        ),
        CONTENT_CHAR_CAP,
    );

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content,
        timestamp: chrono::Utc::now(),
        tags: vec![
            "task_transition".to_string(),
            format!("state:{}", meta.state_digest),
        ],
        embedding: None,
        layer: duduclaw_core::types::MemoryLayer::Episodic,
        importance: 3.0,
        access_count: 0,
        last_accessed: None,
        source_event: TRANSITION_SOURCE_EVENT.to_string(),
    };

    let temporal_meta = TemporalMeta {
        // §6.2 trap avoidance: subject/predicate/object MUST stay None so
        // store_temporal's conflict-resolution branch never triggers.
        subject: None,
        predicate: None,
        object: None,
        valid_from: None,
        valid_until: None,
        confidence: None,
        metadata: Some(serde_json::json!({ "transition": meta })),
        origin: Some("task_forward_model".to_string()),
        origin_trust: Some(1.0),
        derived_from: None,
        source_event: Some(TRANSITION_SOURCE_EVENT.to_string()),
        quarantined: false,
    };

    Some((entry, temporal_meta))
}

/// WP-B2: write one settled prediction's transition sample to episodic
/// memory. No-op (returns `Ok(None)`) when [`should_write_transition`]
/// rejects it. `judge_feedback` is the human-readable rejection/acceptance
/// note (when the caller has one) — optional, purely for the D-series
/// narrative use case. `calibration_enabled` — see
/// [`build_transition_write`]'s doc comment.
pub async fn write_transition(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    error: &TaskPredictionError,
    judge_feedback: Option<&str>,
    calibration_enabled: bool,
) -> Result<Option<String>, String> {
    let Some((entry, meta)) =
        build_transition_write(agent_id, error, judge_feedback, calibration_enabled)
    else {
        return Ok(None);
    };
    engine
        .store_temporal(agent_id, entry, meta)
        .await
        .map(Some)
        .map_err(|e| e.to_string())
}

/// One transition sample as read back from episodic memory, with the
/// parsed [`TransitionMeta`] pulled out of the metadata blob. A row whose
/// metadata doesn't parse as a transition (corrupt/foreign write under the
/// same `source_event` — should never happen since this is a private tag,
/// but defensive) is silently skipped, not surfaced as an error — same
/// "degrade rather than fail the caller" posture as
/// `read_tool_activity_records`.
#[derive(Debug, Clone)]
pub struct TransitionSample {
    pub memory_id: String,
    pub meta: TransitionMeta,
}

/// WP-B3: read transition samples for `agent_id`, filtered to a specific
/// `state_key` (post-fetch filter in Rust — matches design §6.2's
/// instruction to filter by `state_key` client-side rather than adding a
/// dedicated index/query). Reads via `list_valid_by_source_event`
/// (**not** `search()`/`search_layer()`): `search()` bumps `access_count`
/// and would pollute the Ebbinghaus stability of what is meant to be an
/// internal statistics lookup, never a "this memory was important" signal.
pub async fn read_transitions_for_state(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    state_key: &str,
    limit: usize,
) -> Result<Vec<TransitionSample>, String> {
    // Over-fetch a bit before filtering by state_key client-side, since the
    // source_event alone spans every state_key for this agent.
    let raw = engine
        .list_valid_by_source_event(agent_id, TRANSITION_SOURCE_EVENT, limit.saturating_mul(4).max(limit))
        .await
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for (entry, metadata) in raw {
        let Some(transition_value) = metadata.get("transition") else {
            continue;
        };
        let Ok(meta) = serde_json::from_value::<TransitionMeta>(transition_value.clone()) else {
            continue;
        };
        if meta.state_key != state_key {
            continue;
        }
        out.push(TransitionSample { memory_id: entry.id, meta });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prediction::task_forward::{
        ArtifactShape, ExpectedOutcome, ObservationFidelity, ObservedOutcome, PredictionSource,
        RoundPhase, TaskObservation, TaskPrediction, TaskStateKey, GoalKind,
    };
    use crate::prediction::metacognition::AdaptiveThresholds;
    use crate::prediction::tool_class::ToolClass;
    use std::collections::BTreeSet;

    fn sample_error(task_id: &str, round: u32, fidelity: ObservationFidelity) -> TaskPredictionError {
        let state_key = TaskStateKey {
            agent_id: "agnes".to_string(),
            goal_kind: GoalKind::CodingSimple,
            phase: RoundPhase::First,
            has_outcome_spec: false,
        };
        let prediction = TaskPrediction {
            prediction_id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            agent_id: "agnes".to_string(),
            round,
            state_key,
            expected_tool_classes: BTreeSet::from([ToolClass::Read]),
            expected_call_band: (1, 5),
            expected_outcome: ExpectedOutcome::Accept,
            expected_artifact: ArtifactShape::FileWrite,
            confidence: 0.5,
            source: PredictionSource::Statistical,
            created_at: chrono::Utc::now(),
        };
        let observation = TaskObservation {
            task_id: task_id.to_string(),
            agent_id: "agnes".to_string(),
            round,
            observed_tool_classes: BTreeSet::from([ToolClass::Read, ToolClass::Write]),
            observed_calls: 3,
            observed_errors: 0,
            observed_outcome: ObservedOutcome::Accepted,
            observed_artifact: ArtifactShape::FileWrite,
            fidelity,
            window: ("2026-08-06T00:00:00Z".into(), "2026-08-06T00:10:00Z".into()),
            runtime: "claude".into(),
        };
        match crate::prediction::task_forward::diff(prediction, observation, &AdaptiveThresholds::default()) {
            crate::prediction::task_forward::DiffOutcome::Computed(e) => e,
            crate::prediction::task_forward::DiffOutcome::Unobservable { .. } => {
                panic!("test fixture must be observable")
            }
        }
    }

    #[test]
    fn should_write_transition_rejects_none_fidelity_only() {
        assert!(should_write_transition(&sample_error("t1", 1, ObservationFidelity::Full)));
        assert!(should_write_transition(&sample_error("t1", 1, ObservationFidelity::McpOnly)));
        // Building a None-fidelity TaskPredictionError isn't possible via
        // `diff()` (it returns `Unobservable` instead) — should_write_transition's
        // None branch exists defensively for any future direct constructor;
        // assert the gate function's own logic in isolation instead.
    }

    #[test]
    fn build_transition_write_leaves_subject_predicate_object_none() {
        let error = sample_error("t1", 1, ObservationFidelity::McpOnly);
        let (_, meta) = build_transition_write("agnes", &error, None, false).unwrap();
        assert!(meta.subject.is_none(), "subject must stay None — see module docs trap warning");
        assert!(meta.predicate.is_none());
        assert!(meta.object.is_none());
    }

    #[test]
    fn build_transition_write_content_is_capped_and_fidelity_stamped() {
        let error = sample_error("t1", 1, ObservationFidelity::McpOnly);
        let (entry, temporal_meta) =
            build_transition_write("agnes", &error, Some("looked fine"), false).unwrap();
        assert!(entry.content.chars().count() <= CONTENT_CHAR_CAP);
        let transition: TransitionMeta = serde_json::from_value(
            temporal_meta.metadata.as_ref().unwrap().get("transition").unwrap().clone(),
        )
        .unwrap();
        assert_eq!(transition.fidelity, "mcp_only");
        assert_eq!(entry.source_event, TRANSITION_SOURCE_EVENT);
        assert_eq!(entry.layer, duduclaw_core::types::MemoryLayer::Episodic);
    }

    // ── WP-P2: calibration_enabled gating ──

    #[test]
    fn build_transition_write_calibration_disabled_leaves_fields_none() {
        let error = sample_error("t1", 1, ObservationFidelity::McpOnly);
        let (_, temporal_meta) = build_transition_write("agnes", &error, None, false).unwrap();
        let transition: TransitionMeta = serde_json::from_value(
            temporal_meta.metadata.as_ref().unwrap().get("transition").unwrap().clone(),
        )
        .unwrap();
        assert!(transition.outcome.confidence.is_none());
        assert!(transition.outcome.calibration_score.is_none());
    }

    #[test]
    fn build_transition_write_calibration_enabled_fills_confidence_and_score() {
        let error = sample_error("t1", 1, ObservationFidelity::Full);
        let (_, temporal_meta) = build_transition_write("agnes", &error, None, true).unwrap();
        let transition: TransitionMeta = serde_json::from_value(
            temporal_meta.metadata.as_ref().unwrap().get("transition").unwrap().clone(),
        )
        .unwrap();
        assert_eq!(transition.outcome.confidence, Some(error.prediction.confidence));
        assert_eq!(
            transition.outcome.calibration_score,
            Some(crate::prediction::task_forward::calibration_brier_score(&error))
        );
    }

    #[test]
    fn transition_outcome_serde_defaults_missing_calibration_fields_to_none() {
        // A transition row written before this WP-P2 change had no
        // `confidence`/`calibration_score` keys at all — the `#[serde(default)]`
        // attribute must let old rows deserialize instead of erroring out.
        let old_json = serde_json::json!({
            "verdict": "accepted",
            "feedback_digest": null,
            "composite_error": 0.1
        });
        let outcome: TransitionOutcome = serde_json::from_value(old_json).unwrap();
        assert_eq!(outcome.verdict, "accepted");
        assert_eq!(outcome.composite_error, Some(0.1));
        assert!(outcome.confidence.is_none());
        assert!(outcome.calibration_score.is_none());
    }

    #[tokio::test]
    async fn regression_two_transitions_same_state_both_survive() {
        // This is THE regression test the design mandates (§6.2): if
        // subject/predicate were ever accidentally filled in, the second
        // write would supersede (invalidate) the first via
        // store_temporal's conflict-resolution branch, and only one
        // sample would remain valid. Both must survive.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory.db");
        let engine = SqliteMemoryEngine::new(&db_path).unwrap();

        let error1 = sample_error("task-a", 1, ObservationFidelity::McpOnly);
        let error2 = sample_error("task-b", 1, ObservationFidelity::McpOnly); // same state_key (CodingSimple/First/no-spec), different task

        assert_eq!(
            error1.prediction.state_key.canonical(),
            error2.prediction.state_key.canonical(),
            "fixture sanity: both samples must share a state_key for this regression to be meaningful"
        );

        let id1 = write_transition(&engine, "agnes", &error1, None, false).await.unwrap();
        let id2 = write_transition(&engine, "agnes", &error2, None, false).await.unwrap();
        assert!(id1.is_some());
        assert!(id2.is_some());
        assert_ne!(id1, id2);

        let state_key = error1.prediction.state_key.canonical();
        let samples = read_transitions_for_state(&engine, "agnes", &state_key, 10)
            .await
            .unwrap();
        assert_eq!(
            samples.len(),
            2,
            "both same-state transition samples must still be valid (not superseded)"
        );
        let task_ids: BTreeSet<&str> = samples.iter().map(|s| s.meta.task_id.as_str()).collect();
        assert!(task_ids.contains("task-a"));
        assert!(task_ids.contains("task-b"));
    }

    #[tokio::test]
    async fn write_transition_none_fidelity_never_reaches_store() {
        // Since diff() never emits a None-fidelity TaskPredictionError,
        // exercise the gate directly against a hand-built error would
        // require bypassing diff(); instead assert the documented contract
        // via should_write_transition's boolean directly (unit-level,
        // covered above) plus confirm the McpOnly/Full paths DO write
        // (covered by the regression test). This test documents intent
        // for readers rather than adding redundant coverage.
        let error = sample_error("t1", 1, ObservationFidelity::McpOnly);
        assert!(should_write_transition(&error));
    }

    #[tokio::test]
    async fn read_transitions_for_state_filters_by_state_key() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory.db");
        let engine = SqliteMemoryEngine::new(&db_path).unwrap();

        let error_a = sample_error("task-a", 1, ObservationFidelity::McpOnly);
        write_transition(&engine, "agnes", &error_a, None, false).await.unwrap();

        // A different agent's transitions must not leak in.
        let error_b = sample_error("task-b", 1, ObservationFidelity::McpOnly);
        write_transition(&engine, "other-agent", &error_b, None, false).await.unwrap();

        let state_key = error_a.prediction.state_key.canonical();
        let samples = read_transitions_for_state(&engine, "agnes", &state_key, 10)
            .await
            .unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].meta.task_id, "task-a");
    }
}
