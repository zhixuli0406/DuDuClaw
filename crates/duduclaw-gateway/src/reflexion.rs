//! F2b Reflexion consolidation — bridge MistakeNotebook → semantic memory.
//!
//! When the same `MistakeCategory` accumulates `>= threshold` unresolved entries,
//! distil them into one generalised rule stored in the agent's **semantic**
//! memory layer (via the F1 temporal supersession chain), then mark the source
//! mistakes resolved so they stop re-triggering / re-counting.
//!
//! Rule synthesis is deterministic (zero LLM cost, fully testable): it aggregates
//! the distinct "what went wrong" lessons into a single guard-rail. The semantic
//! rule then becomes a long-lived recall source (F2a) in place of the noisier
//! per-mistake episodic entries.

use std::collections::BTreeMap;
use std::path::Path;

use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::TemporalMeta;

use crate::gvu::mistake_notebook::{
    MistakeCategory, MistakeEntry, MistakeNotebook, MAX_UNRESOLVED_PER_AGENT,
};
use crate::playbook::entry::{PlaybookCategory, PlaybookMeta, PlaybookState};
use crate::prediction::rule_lifecycle::PROBATION_RULE_TAG;

/// Default number of same-category unresolved mistakes that triggers consolidation.
pub const DEFAULT_CONSOLIDATE_THRESHOLD: u32 = 3;

/// GovMem-style (arXiv:2607.02579) promotion verdict for a candidate group of
/// same-category, same-`source_kind` mistakes.
///
/// Deterministic, zero LLM cost — see [`assess_promotion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promotion {
    /// Independent evidence is sufficient — consolidate this group.
    Promote,
    /// All observations trace back to the same session and/or the same
    /// wording — correlated, not independent, evidence. Wait for more
    /// observations rather than consolidating on a single incident.
    NeedsMoreEvidence,
}

/// Decide whether a candidate group of mistakes (already filtered to one
/// `category` + one `source_kind`, already at/above the count threshold)
/// carries enough *independent* evidence to promote into a consolidated rule.
///
/// GovMem's failure mode this guards against: a single incident that just
/// happens to re-trigger the same mistake 3+ times within one session (or
/// gets logged with byte-identical wording) is one correlated observation,
/// not three independent ones. Promotion requires:
/// - distinct `session_id` count >= 2, AND
/// - distinct normalized `what_went_wrong` (trimmed, lowercased, whitespace
///   collapsed) count >= 2.
///
/// B2 (Honest Lying, arXiv:2605.29463) layers a SEPARATE, stricter guard on
/// top of GovMem's independence check: entries with no [`TrajectoryEvidence`]
/// (pure LLM self-report — the paper measured 0% diagnostic accuracy for
/// exactly this) are excluded from the session/lesson tallies entirely. A
/// group of 5 unverified "independent-looking" self-reports must NOT
/// promote just because they happen to carry 2+ distinct session ids and
/// wordings — GovMem's independence axis and B2's evidence axis are
/// orthogonal and both must clear.
///
/// Pure function — no I/O, no LLM call.
///
/// [`TrajectoryEvidence`]: crate::gvu::mistake_notebook::TrajectoryEvidence
pub fn assess_promotion(mistakes: &[MistakeEntry]) -> Promotion {
    let (sessions, lessons) = promotion_counts(mistakes);
    if sessions >= 2 && lessons >= 2 {
        Promotion::Promote
    } else {
        Promotion::NeedsMoreEvidence
    }
}

/// The two independence axes GovMem's promotion gate checks, over the VERIFIED
/// subset only (B2: an unverified self-report contributes nothing toward
/// independence): `(distinct_session_count, distinct_normalized_lesson_count)`.
/// Shared by [`assess_promotion`] (the gate) and the G6 consolidation-failure
/// telemetry (so a `NeedsMoreEvidence` record can report *why* the evidence
/// was judged correlated). Pure — no I/O, no LLM.
pub fn promotion_counts(mistakes: &[MistakeEntry]) -> (usize, usize) {
    let mut sessions: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut lessons: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in mistakes {
        if m.evidence.is_none() {
            continue;
        }
        sessions.insert(m.session_id.as_str());
        let normalized = normalize_lesson(&m.what_went_wrong);
        if !normalized.is_empty() {
            lessons.insert(normalized);
        }
    }
    (sessions.len(), lessons.len())
}

/// Normalize `what_went_wrong` for de-duplication: trim, lowercase, collapse
/// internal whitespace runs to a single space.
fn normalize_lesson(s: &str) -> String {
    s.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Consolidate recurring mistakes of `category` into a semantic memory rule.
///
/// WP2 (GovMem 2607.02579): mistakes are first grouped by `source_kind` — an
/// orthogonal axis to `category` recording *how* the failure was detected
/// (e.g. RFC-24 `"decision_gap"` vs. general `"task_failure"`, both of which
/// may land in `MistakeCategory::Capability`). Each group is counted and
/// evaluated independently so unrelated failure modes never pool into one
/// consolidation, and a group below `threshold` never blocks a different
/// group that has reached it. Groups are visited in deterministic
/// (lexicographic `source_kind`) order; the first eligible group — reaching
/// `threshold` AND assessed [`Promotion::Promote`] by [`assess_promotion`] —
/// is consolidated and returned. Remaining eligible groups (if any) will be
/// picked up by a subsequent call (this function already runs after every
/// qualifying mistake record, so no evidence is lost — it's just spread
/// across turns).
///
/// Returns `Ok(Some(semantic_id))` when a consolidation happened, `Ok(None)`
/// when no group reached `threshold`, or every group that did was assessed
/// [`Promotion::NeedsMoreEvidence`] (correlated, not independent, evidence —
/// left unresolved; the notebook's existing FIFO cap bounds accumulation).
pub async fn maybe_consolidate(
    notebook: &MistakeNotebook,
    memory_db_path: &Path,
    home_dir: &Path,
    agent_id: &str,
    category: MistakeCategory,
    threshold: u32,
) -> Result<Option<String>, String> {
    let total = notebook.count_unresolved_by_category(agent_id, category);
    if total < threshold {
        // No sub-group can reach `threshold` if the total doesn't either.
        return Ok(None);
    }

    let mistakes = notebook.query_unresolved_by_category(
        agent_id,
        category,
        MAX_UNRESOLVED_PER_AGENT as usize,
    );

    // Group by source_kind (WP2). Empty string ("" — unattributed / legacy
    // rows) is its own group rather than joining a named one, so it can
    // neither pad out `"decision_gap"`/`"task_failure"` counts nor be
    // silently dropped. BTreeMap gives deterministic iteration order.
    let mut groups: BTreeMap<String, Vec<MistakeEntry>> = BTreeMap::new();
    for m in mistakes {
        groups.entry(m.source_kind.clone()).or_default().push(m);
    }

    for group in groups.into_values() {
        // Captured before the group is consumed by the evidence filter — used
        // by the G6 consolidation-failure telemetry below.
        let source_kind = group.first().map(|m| m.source_kind.clone()).unwrap_or_default();
        let raw_len = group.len();

        // B2 (Honest Lying, arXiv:2605.29463): an unverified mistake — no
        // `TrajectoryEvidence`, i.e. a pure LLM self-report — does not count
        // toward the consolidation threshold at all. This is the
        // fail-closed choice over "count it at half weight": the paper's
        // headline number (0% self-report diagnostic accuracy vs. 86% for
        // programmatic trajectory extraction) means an unverified group
        // provides no reliable signal regardless of how large it grows.
        // Layered ON TOP of the WP2 GovMem independence gate below, not a
        // replacement for it.
        let verified: Vec<MistakeEntry> =
            group.into_iter().filter(|m| m.evidence.is_some()).collect();
        if (verified.len() as u32) < threshold {
            // G6/#7: only a *failure* worth surfacing when the raw group DID
            // reach the threshold but the B2 evidence filter knocked it below
            // — a genuine "these mistakes weren't merged because too few were
            // verified". A group that never reached the threshold is normal
            // accumulation, not a failure, and is deliberately NOT recorded.
            if raw_len as u32 >= threshold {
                crate::consolidation_failures::record_failure(
                    home_dir,
                    &crate::consolidation_failures::ConsolidationFailure::new(
                        agent_id,
                        category.as_str(),
                        &source_kind,
                        crate::consolidation_failures::FailureReason::InsufficientVerifiedEvidence,
                        serde_json::json!({
                            "raw": raw_len,
                            "verified": verified.len(),
                            "threshold": threshold,
                        }),
                    ),
                );
            }
            continue;
        }
        if assess_promotion(&verified) != Promotion::Promote {
            // G6/#7: enough verified mistakes, but GovMem judged them
            // correlated (too few distinct sessions and/or lessons). Record
            // the two independence counts so the drill-down shows *why*.
            let (distinct_sessions, distinct_lessons) = promotion_counts(&verified);
            crate::consolidation_failures::record_failure(
                home_dir,
                &crate::consolidation_failures::ConsolidationFailure::new(
                    agent_id,
                    category.as_str(),
                    &source_kind,
                    crate::consolidation_failures::FailureReason::NeedsMoreEvidence,
                    serde_json::json!({
                        "verified": verified.len(),
                        "distinct_sessions": distinct_sessions,
                        "distinct_lessons": distinct_lessons,
                    }),
                ),
            );
            continue;
        }
        return consolidate_group(notebook, memory_db_path, home_dir, agent_id, category, verified)
            .await;
    }

    Ok(None)
}

/// Synthesize + store a semantic rule from one already-eligible group of
/// mistakes, then mark that group's source mistakes resolved. Split out of
/// `maybe_consolidate` so the grouping/eligibility logic above stays
/// readable.
async fn consolidate_group(
    notebook: &MistakeNotebook,
    memory_db_path: &Path,
    home_dir: &Path,
    agent_id: &str,
    category: MistakeCategory,
    mistakes: Vec<MistakeEntry>,
) -> Result<Option<String>, String> {
    let rule = synthesize_rule(category, &mistakes);
    let source_ids: Vec<String> = mistakes.iter().map(|m| m.id.clone()).collect();

    // B1 anti-false-surprise gate (arXiv:2606.29182): this is exactly the
    // paper's failure mode — a self-evolving loop re-deriving the same
    // "apply extra care" rule for a category it has already learned. Built
    // via the shared R2 factory (`crate::memory_factory::build_memory_engine`)
    // — a fresh local engine instance just for this check, independent of
    // whatever the caller's own engine instance has configured — reflexion's
    // consolidation safety must not depend on the opt-in `w_vec` search
    // feature flag. The factory honors `[memory] novelty_gate` from
    // `<home_dir>/config.toml`: when disabled, no embedder is attached and
    // `check_novelty` below is a guaranteed no-op (see
    // `novelty_gate.rs`'s "Hard invariant") — previously this call was
    // unconditional regardless of the config toggle. `store_temporal` below
    // intentionally skips its own internal B1 check for this write (it
    // carries an explicit `(subject, predicate)` triple — see `engine.rs`'s
    // wiring), so this explicit call is the ONLY place that guards a
    // reflexion consolidation against duplicating an existing rule.
    let engine = crate::memory_factory::build_memory_engine(memory_db_path, home_dir)
        .map_err(|e| format!("open memory engine: {e}"))?;

    if let Some(rejection) = engine.check_novelty(agent_id, MemoryLayer::Semantic, &rule).await {
        tracing::warn!(
            agent_id,
            category = category.as_str(),
            matched_id = %rejection.matched_id,
            similarity = rejection.similarity,
            threshold = rejection.threshold,
            "B1 novelty gate rejected reflexion consolidation: {rejection}"
        );
        // G6/#7: surface the "why not merged" — the synthesized rule was a
        // near-duplicate of an already-known rule.
        let source_kind = mistakes.first().map(|m| m.source_kind.as_str()).unwrap_or("");
        crate::consolidation_failures::record_failure(
            home_dir,
            &crate::consolidation_failures::ConsolidationFailure::new(
                agent_id,
                category.as_str(),
                source_kind,
                crate::consolidation_failures::FailureReason::NoveltyRejected,
                serde_json::json!({
                    "matched_id": rejection.matched_id,
                    "similarity": rejection.similarity,
                    "threshold": rejection.threshold,
                }),
            ),
        );
        // Leave source mistakes unresolved — same conservative posture as
        // `Promotion::NeedsMoreEvidence` above: this group's lesson is
        // already captured by an existing rule, but we don't silently
        // discard the evidence trail. The FIFO cap in MistakeNotebook bounds
        // accumulation; a genuinely new mistake in a later call can still
        // combine with these to promote once it's actually novel.
        return Ok(None);
    }

    // WP-P3 held-out rule gate (design §1.4, §6): read once from
    // `<home>/config.toml`. Defaults `false` ⇒ everything below is
    // byte-identical to the pre-WP3 behavior (no shadow tag, no held-out
    // record seeded). `consolidate_group` already receives `home_dir`, so no
    // signature change to `maybe_consolidate` or its callers is needed.
    let held_out_gate_enabled =
        crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(home_dir)
            .held_out_gate_enabled;
    // Domain-agnostic process/outcome classification (rule_gate::classify_lesson):
    // a lesson grounded in programmatic evidence (tool error / assertion) is
    // Verified and injects on the normal path; an evidence-less inductive
    // lesson is born as a shadow candidate and must earn adoption out-of-sample
    // through the held-out gate. In the current F2b pipeline `maybe_consolidate`
    // has already filtered to evidence-backed (verified) mistakes, so this is
    // Verified today — the shadow branch is the correct, defensive wiring for
    // any future evidence-less consolidation source (design §6 item 1).
    let has_evidence = mistakes.iter().any(|m| m.evidence.is_some());
    let born_as_shadow = held_out_gate_enabled
        && crate::prediction::rule_gate::classify_lesson(has_evidence)
            == crate::prediction::rule_gate::LessonKind::Inductive;

    let mut tags = vec![
        "reflexion".to_string(),
        "consolidated".to_string(),
        format!("category:{}", category.as_str()),
        // WP2 Janus (arXiv:2606.31121): every freshly consolidated rule
        // starts on a trial period — see `prediction::rule_lifecycle`.
        PROBATION_RULE_TAG.to_string(),
    ];
    if born_as_shadow {
        tags.push(crate::prediction::rule_lifecycle::SHADOW_RULE_TAG.to_string());
    }

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: rule.clone(),
        timestamp: chrono::Utc::now(),
        tags,
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 8.0,
        access_count: 0,
        last_accessed: None,
        source_event: "reflexion_consolidation".to_string(),
    };

    // WP1.2: F2b writes go straight through `store_temporal` (this has
    // always been true — reflexion is a direct write path, not routed
    // through `playbook::delta`'s `Add`), so the delta layer's "≥1 eval
    // case" gate (G6) never applies here. `eval_cases` starts empty, same
    // as the M-1 migration default for pre-existing rows — there is no
    // mechanism here that could name a specific eval case for an
    // automatically-synthesized rule.
    let source_kind = mistakes.first().map(|m| m.source_kind.as_str()).unwrap_or("");
    let playbook_meta = PlaybookMeta {
        schema_version: crate::playbook::entry::PLAYBOOK_SCHEMA_VERSION,
        category: PlaybookCategory::Repair,
        signals_match: vec![
            format!("mistake:{}", category.as_str()),
            format!("source_kind:{}", if source_kind.is_empty() { "unattributed" } else { source_kind }),
        ],
        strategy: Vec::new(),
        failure_history: Vec::new(),
        eval_cases: Vec::new(),
        applications: Vec::new(),
        success_streak: 0,
        state: PlaybookState::Probation,
        revision: 0,
        dedup_key: crate::playbook::dedup::dedup_key(&rule, PlaybookCategory::Repair),
        embed_model: None,
        origin: "agent_derived".to_string(),
        derived_from: source_ids.clone(),
        assertions: Default::default(),
    };
    let mut metadata_blob = serde_json::json!({
        "source_mistake_ids": source_ids,
        // `rule_stats` seeds the Janus lifecycle counters (WP2: initial
        // helpful = 1, on probation) settled per-turn by
        // `prediction::rule_lifecycle`.
        "rule_stats": crate::prediction::rule_lifecycle::RuleStats::initial(),
    });
    playbook_meta.merge_into(&mut metadata_blob);

    // G6 (Hindsight #6 parity): record the F1 memory-fact ids this rule was
    // derived from so a later supersession of any of them flags the rule
    // source-stale (`prediction::rule_staleness`). Reflexion consolidates from
    // MistakeNotebook entries, NOT F1 temporal facts, so there is genuinely no
    // fact source to record here — the call is made with an empty list (a
    // no-op that writes no key), keeping the rule fail-open (never wrongly
    // flagged stale). The wiring is explicit so a future consolidation source
    // that DOES read F1 facts only has to pass their ids here.
    crate::prediction::rule_staleness::record_source_facts(&mut metadata_blob, &[]);

    // WP-P3: when the held-out gate is on, seed the rule's out-of-sample
    // record with its birth cursor so the prequential time split
    // (`settle_injected_rules_held_out`) never validates the rule against its
    // own birth batch. No-op when the gate is off (byte-identical).
    if held_out_gate_enabled {
        crate::prediction::rule_gate::HeldOutStats::born(chrono::Utc::now().timestamp().max(0) as u64)
            .merge_into(&mut metadata_blob);
    }

    // Triple ties successive consolidations of the same category into a
    // supersession chain (newer rule supersedes the older one automatically).
    let meta = TemporalMeta {
        subject: Some(format!("category:{}", category.as_str())),
        predicate: Some("requires_care".to_string()),
        object: None,
        valid_from: None,
        valid_until: None,
        confidence: Some(0.9),
        // WP1: a consolidated reflexion rule is agent self-derived content.
        origin: Some("agent_derived".to_string()),
        metadata: Some(metadata_blob),
        ..Default::default()
    };

    let semantic_id = engine
        .store_temporal(agent_id, entry, meta)
        .await
        .map_err(|e| format!("store semantic rule: {e}"))?;

    // Resolve source mistakes so they stop re-triggering and re-counting.
    let id_refs: Vec<&str> = source_ids.iter().map(|s| s.as_str()).collect();
    notebook
        .mark_resolved(&id_refs)
        .map_err(|e| format!("mark resolved: {e}"))?;

    Ok(Some(semantic_id))
}

/// Build a concise generalised rule from recurring mistakes (deterministic).
fn synthesize_rule(category: MistakeCategory, mistakes: &[MistakeEntry]) -> String {
    let mut lessons: Vec<String> = Vec::new();
    for m in mistakes {
        let lesson = m.what_went_wrong.trim();
        if !lesson.is_empty() && !lessons.iter().any(|l| l == lesson) {
            lessons.push(lesson.to_string());
        }
    }
    let bullets = lessons
        .iter()
        .take(5)
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Recurring {} issues consolidated from {} past mistakes. Apply extra care:\n{}",
        category.as_str(),
        mistakes.len(),
        bullets
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gvu::mistake_notebook::{build_mistake_entry, TrajectoryEvidence};
    use duduclaw_core::traits::MemoryEngine; // brings `search` into scope for assertions
    use duduclaw_memory::SqliteMemoryEngine;
    use tempfile::TempDir;

    /// Record `n` **evidence-backed** mistakes with distinct session ids and
    /// distinct wording — independent, verified evidence that should promote
    /// once `n >= threshold`. B2 requires `.with_evidence(...)` here: without
    /// it every group is unverified and can never reach the threshold
    /// (covered separately by `unverified_mistakes_never_consolidate`).
    fn record_n(nb: &MistakeNotebook, agent: &str, cat: MistakeCategory, n: usize, source_kind: &str) {
        for i in 0..n {
            let e = build_mistake_entry(
                agent,
                &format!("sess-{i}"),
                cat,
                &format!("user asked thing {i}"),
                "agent answered wrong",
                &format!("missed validation step {i}"),
                None,
                source_kind,
            )
            .with_evidence(TrajectoryEvidence::from_tool_error(
                "validator",
                &format!("check {i} failed"),
            ));
            nb.record(&e).unwrap();
        }
    }

    /// Record `n` evidence-backed mistakes that all share the same session id
    /// (correlated observations from one incident, not independent evidence —
    /// GovMem should reject this regardless of B2 evidence status).
    fn record_same_session_n(
        nb: &MistakeNotebook,
        agent: &str,
        cat: MistakeCategory,
        n: usize,
        source_kind: &str,
    ) {
        for i in 0..n {
            let e = build_mistake_entry(
                agent,
                "sess-fixed",
                cat,
                &format!("user asked thing {i}"),
                "agent answered wrong",
                &format!("missed validation step {i}"),
                None,
                source_kind,
            )
            .with_evidence(TrajectoryEvidence::from_tool_error(
                "validator",
                &format!("check {i} failed"),
            ));
            nb.record(&e).unwrap();
        }
    }

    #[tokio::test]
    async fn below_threshold_does_not_consolidate() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-a", MistakeCategory::Capability, 2, "");

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-a", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none(), "2 < 3 must not consolidate");
        assert_eq!(nb.count_unresolved_by_category("agent-a", MistakeCategory::Capability), 2);
    }

    #[tokio::test]
    async fn threshold_reached_consolidates_to_semantic() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-b", MistakeCategory::Capability, 3, "");

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-b", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_some(), "3 >= 3 must consolidate");

        // Source mistakes resolved → count drops to zero.
        assert_eq!(
            nb.count_unresolved_by_category("agent-b", MistakeCategory::Capability),
            0,
            "source mistakes must be marked resolved"
        );

        // A semantic memory rule now exists and is searchable.
        let engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let results = engine.search("agent-b", "Recurring", 10).await.unwrap();
        assert_eq!(results.len(), 1, "one consolidated semantic rule");
        assert_eq!(results[0].layer, MemoryLayer::Semantic);
        assert_eq!(results[0].source_event, "reflexion_consolidation");
    }

    #[tokio::test]
    async fn consolidated_rule_seeds_lifecycle_counters() {
        use crate::prediction::rule_lifecycle::RuleStats;

        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-d", MistakeCategory::Factual, 3, "");

        let semantic_id =
            maybe_consolidate(&nb, &mem_path, dir.path(), "agent-d", MistakeCategory::Factual, 3)
                .await
                .unwrap()
                .expect("must consolidate");

        let engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let meta = engine
            .get_metadata("agent-d", &semantic_id)
            .await
            .unwrap()
            .expect("rule metadata present");
        assert_eq!(
            RuleStats::from_metadata(&meta),
            RuleStats::initial(),
            "F2b must seed helpful=1, harmful=0 (WP2 Janus trial-period seed)"
        );
        // Source-mistake provenance still stored alongside the counters.
        assert!(meta["source_mistake_ids"].as_array().is_some_and(|a| a.len() == 3));
        // WP2 Janus: every freshly consolidated rule starts on probation.
        let entry = engine.get_by_id("agent-d", &semantic_id).await.unwrap().unwrap();
        assert!(entry
            .tags
            .iter()
            .any(|t| t == crate::prediction::rule_lifecycle::PROBATION_RULE_TAG));
    }

    #[tokio::test]
    async fn different_categories_counted_separately() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-c", MistakeCategory::Capability, 2, "");
        record_n(&nb, "agent-c", MistakeCategory::Factual, 1, "");

        // Neither category reaches 3 → no consolidation.
        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-c", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn correlated_same_session_mistakes_do_not_consolidate() {
        // GovMem: 3 mistakes that all trace back to the same session are one
        // correlated incident, not three independent observations.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_same_session_n(&nb, "agent-corr", MistakeCategory::Capability, 3, "");

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-corr", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none(), "same-session mistakes are correlated, not independent, evidence");

        // Left unresolved (NeedsMoreEvidence), not silently dropped — still
        // counted as unresolved so a genuinely independent 4th observation
        // can still tip it over.
        assert_eq!(
            nb.count_unresolved_by_category("agent-corr", MistakeCategory::Capability),
            3,
            "NeedsMoreEvidence must not mark_resolved the source mistakes"
        );
    }

    #[tokio::test]
    async fn distinct_sessions_promote() {
        // Mirror of `correlated_same_session_mistakes_do_not_consolidate`:
        // same count and category, but distinct sessions + distinct wording
        // — GovMem's independence bar is met, so this must promote.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-indep", MistakeCategory::Capability, 3, "");

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-indep", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_some(), "distinct sessions + distinct wording must promote");
        assert_eq!(
            nb.count_unresolved_by_category("agent-indep", MistakeCategory::Capability),
            0,
            "promoted group's source mistakes must be resolved"
        );
    }

    #[tokio::test]
    async fn decision_gap_and_task_failure_counted_separately() {
        // WP2: source_kind groups are counted independently — 2 decision_gap
        // + 2 task_failure mistakes total 4 (>= threshold 3 in aggregate),
        // but neither group alone reaches the threshold, so neither promotes.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-split", MistakeCategory::Capability, 2, "decision_gap");
        record_n(&nb, "agent-split", MistakeCategory::Capability, 2, "task_failure");

        assert_eq!(
            nb.count_unresolved_by_category("agent-split", MistakeCategory::Capability),
            4,
            "total unresolved count spans both source_kind groups"
        );

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-split", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(
            r.is_none(),
            "neither source_kind group individually reaches the threshold — must not pool"
        );
        assert_eq!(
            nb.count_unresolved_by_category("agent-split", MistakeCategory::Capability),
            4,
            "nothing resolved — both groups still below threshold"
        );
    }

    // ── B2: unverified self-report mistakes never promote (Honest Lying,
    //    arXiv:2605.29463) — layered ON TOP of the WP2 GovMem gate above ──

    #[tokio::test]
    async fn unverified_mistakes_never_consolidate_regardless_of_diversity() {
        // Same shape as `distinct_sessions_promote` (3 distinct sessions,
        // 3 distinct wordings — GovMem's independence bar is met) but built
        // with plain `build_mistake_entry` (no `.with_evidence(...)`), i.e.
        // pure LLM self-report. B2 must keep this group below the threshold
        // no matter how "independent" it looks.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        for i in 0..3 {
            let e = build_mistake_entry(
                "agent-unverified",
                &format!("sess-{i}"),
                MistakeCategory::Capability,
                &format!("user asked thing {i}"),
                "agent answered wrong",
                &format!("missed validation step {i}"),
                None,
                "",
            );
            assert!(e.evidence.is_none(), "sanity: build_mistake_entry defaults to unverified");
            nb.record(&e).unwrap();
        }

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-unverified", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none(), "B2: unverified mistakes must never reach the consolidation threshold");
        assert_eq!(
            nb.count_unresolved_by_category("agent-unverified", MistakeCategory::Capability),
            3,
            "unverified group left untouched, not silently resolved"
        );
    }

    #[tokio::test]
    async fn mixed_verified_and_unverified_only_counts_verified_toward_threshold() {
        // 2 evidence-backed + 3 unverified = 5 raw entries (>= threshold 3
        // in aggregate), but only 2 are verified — still below threshold 3,
        // so this must NOT consolidate.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");

        for i in 0..2 {
            let e = build_mistake_entry(
                "agent-mixed",
                &format!("verified-sess-{i}"),
                MistakeCategory::Capability,
                &format!("user asked thing {i}"),
                "agent answered wrong",
                &format!("verified issue {i}"),
                None,
                "",
            )
            .with_evidence(TrajectoryEvidence::from_tool_error("bash", "boom"));
            nb.record(&e).unwrap();
        }
        for i in 0..3 {
            let e = build_mistake_entry(
                "agent-mixed",
                &format!("unverified-sess-{i}"),
                MistakeCategory::Capability,
                &format!("user asked other thing {i}"),
                "agent answered wrong",
                &format!("unverified issue {i}"),
                None,
                "",
            );
            nb.record(&e).unwrap();
        }
        assert_eq!(
            nb.count_unresolved_by_category("agent-mixed", MistakeCategory::Capability),
            5,
            "raw total spans both verified and unverified"
        );

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-mixed", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none(), "only 2 of 5 entries are verified — below threshold 3");
        assert_eq!(
            nb.count_unresolved_by_category("agent-mixed", MistakeCategory::Capability),
            5,
            "nothing resolved — verified subset alone didn't reach threshold"
        );
    }

    // ── B1: anti-false-surprise gate on the reflexion write path
    //    (arXiv:2606.29182) ──────────────────────────────────────────────

    #[tokio::test]
    async fn duplicate_consolidation_is_rejected_by_the_b1_gate() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");

        // Round 1: consolidates normally.
        record_n(&nb, "agent-dup", MistakeCategory::Capability, 3, "");
        let r1 = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-dup", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r1.is_some(), "round 1 must consolidate");

        // Round 2: SAME wording (same `what_went_wrong` per index) as round
        // 1 — `synthesize_rule` produces byte-identical text, so the B1 gate
        // must catch it as a near-duplicate of the rule round 1 just wrote,
        // even though GovMem's independence bar (2 distinct sessions, 2
        // distinct wordings within round 2 itself) is satisfied on its own.
        record_n(&nb, "agent-dup", MistakeCategory::Capability, 3, "");
        let r2 = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-dup", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r2.is_none(), "B1 gate must reject a near-duplicate consolidated rule");

        // Round 2's source mistakes are left unresolved (conservative
        // posture, same as `Promotion::NeedsMoreEvidence`).
        assert_eq!(
            nb.count_unresolved_by_category("agent-dup", MistakeCategory::Capability),
            3,
            "B1-rejected group must not be marked resolved"
        );

        // Exactly one semantic rule exists — the duplicate was never written.
        let check_engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let results = check_engine.search("agent-dup", "Recurring", 10).await.unwrap();
        assert_eq!(
            results.len(),
            1,
            "B1 gate must have prevented a second, duplicate consolidated rule from being written"
        );
    }

    /// R2: `[memory] novelty_gate = false` in `<home_dir>/config.toml` must
    /// reach `consolidate_group` via the shared factory
    /// (`crate::memory_factory::build_memory_engine`) and disable the B1
    /// check for this write path too — mirrors
    /// `duplicate_consolidation_is_rejected_by_the_b1_gate` but with the
    /// config toggle off, asserting the consolidation is no longer
    /// short-circuited by the gate. Note the write itself then lands on
    /// `store_temporal`'s F1 REAFFIRM carve-out (identical
    /// `(subject, predicate, object)` + identical content ⇒ the existing
    /// row's id is returned and its access_count bumped, no duplicate row) —
    /// so "let through" here means "reaches the store and resolves its
    /// mistakes", not "creates a second row".
    #[tokio::test]
    async fn novelty_gate_disabled_in_config_lets_duplicate_consolidation_through() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[memory]\nnovelty_gate = false\n").unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");

        record_n(&nb, "agent-dup-off", MistakeCategory::Capability, 3, "");
        let r1 = maybe_consolidate(
            &nb,
            &mem_path,
            dir.path(),
            "agent-dup-off",
            MistakeCategory::Capability,
            3,
        )
        .await
        .unwrap();
        assert!(r1.is_some(), "round 1 must consolidate");

        // Round 2: same wording as round 1 — with the gate on, this is the
        // exact shape `duplicate_consolidation_is_rejected_by_the_b1_gate`
        // asserts gets rejected. With `novelty_gate = false` it must go
        // through instead.
        record_n(&nb, "agent-dup-off", MistakeCategory::Capability, 3, "");
        let r2 = maybe_consolidate(
            &nb,
            &mem_path,
            dir.path(),
            "agent-dup-off",
            MistakeCategory::Capability,
            3,
        )
        .await
        .unwrap();
        assert!(
            r2.is_some(),
            "novelty_gate=false must let a near-duplicate consolidation through"
        );
        // Identical wording + identical (subject, predicate) triple hits the
        // F1 reaffirmation carve-out in `store_temporal`: the surviving row's
        // id comes back (access_count bumped) instead of a duplicate row.
        // That is pre-existing v1.19 semantics, not the novelty gate.
        assert_eq!(
            r1, r2,
            "identical re-consolidation must land on F1 reaffirmation (same row id), not a duplicate row"
        );

        // Round 2's source mistakes ARE resolved this time — the B1 gate
        // never short-circuited `consolidate_group` before `store_temporal`,
        // unlike the gate-enabled test above (where they stay unresolved).
        assert_eq!(
            nb.count_unresolved_by_category("agent-dup-off", MistakeCategory::Capability),
            0,
            "with the gate disabled, round 2 must reach store_temporal and resolve its mistakes"
        );

        // Identical content + same (subject, predicate, object) both rounds →
        // the second write lands on `store_temporal`'s F1 REAFFIRM carve-out:
        // one surviving row (no supersession chain, no duplicate), with the
        // second observation recorded on that same row. The proof that round 2
        // actually REACHED the store (i.e. the gate did not short-circuit it)
        // is the resolved-mistakes assertion above plus r1 == r2.
        let check_engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let history = check_engine
            .get_history("agent-dup-off", "category:capability", "requires_care")
            .await
            .unwrap();
        assert_eq!(
            history.len(),
            1,
            "identical re-consolidation reaffirms the existing row — exactly one row, no chain"
        );
    }

    #[tokio::test]
    async fn distinct_consolidation_after_a_duplicate_still_succeeds() {
        // A genuinely novel lesson in the SAME category must still promote
        // even after a prior duplicate was rejected — the B1 gate targets
        // near-duplicate CONTENT, not the category as a whole.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");

        record_n(&nb, "agent-evolve", MistakeCategory::Capability, 3, "");
        let r1 = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-evolve", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r1.is_some());

        // A genuinely different set of lessons this time.
        for i in 0..3 {
            let e = build_mistake_entry(
                "agent-evolve",
                &format!("sess-new-{i}"),
                MistakeCategory::Capability,
                &format!("user asked a totally different thing {i}"),
                "agent answered wrong",
                &format!("forgot to check the currency conversion rate {i}"),
                None,
                "",
            )
            .with_evidence(TrajectoryEvidence::from_tool_error("fx-lookup", "timeout"));
            nb.record(&e).unwrap();
        }
        let r2 = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-evolve", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r2.is_some(), "a genuinely novel lesson must still consolidate");
        assert_ne!(r1, r2, "the two consolidations produced different semantic ids");
    }

    // ── WP-P3: held-out rule gate wiring at consolidation ─────────────────

    #[tokio::test]
    async fn held_out_gate_on_verified_rule_injects_normally_and_seeds_held_out_record() {
        // record_n attaches TrajectoryEvidence, so the consolidated group is
        // Verified — even with the gate ON it must inject on the normal path
        // (no shadow tag), and the gate seeds a held-out record with a birth
        // cursor for the prequential split.
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nheld_out_gate_enabled = true\n",
        )
        .unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-hog", MistakeCategory::Capability, 3, "");

        let sid =
            maybe_consolidate(&nb, &mem_path, dir.path(), "agent-hog", MistakeCategory::Capability, 3)
                .await
                .unwrap()
                .expect("verified group must consolidate");

        let engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let entry = engine.get_by_id("agent-hog", &sid).await.unwrap().unwrap();
        assert!(
            !entry
                .tags
                .iter()
                .any(|t| t == crate::prediction::rule_lifecycle::SHADOW_RULE_TAG),
            "an evidence-backed (Verified) consolidation must NOT be born as a shadow candidate"
        );
        let meta = engine.get_metadata("agent-hog", &sid).await.unwrap().unwrap();
        let held = crate::prediction::rule_gate::HeldOutStats::from_metadata(&meta);
        assert!(
            held.born_seq > 0,
            "gate-on consolidation seeds a born_seq for the prequential time split"
        );
    }

    #[tokio::test]
    async fn held_out_gate_off_is_byte_identical_no_shadow_no_held_out_record() {
        // Explicit gate OFF → the consolidation metadata/tags must be exactly
        // what the pre-WP3 code produced (no shadow tag, no held_out_stats
        // key). v1.54 flipped the held_out_gate default to ON, so a test that
        // wants the OFF path must now say so explicitly rather than relying on
        // an absent config (absent == ON since v1.54).
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nheld_out_gate_enabled = false\n",
        )
        .unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-off", MistakeCategory::Capability, 3, "");

        let sid =
            maybe_consolidate(&nb, &mem_path, dir.path(), "agent-off", MistakeCategory::Capability, 3)
                .await
                .unwrap()
                .expect("must consolidate");

        let engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let entry = engine.get_by_id("agent-off", &sid).await.unwrap().unwrap();
        assert!(
            !entry
                .tags
                .iter()
                .any(|t| t == crate::prediction::rule_lifecycle::SHADOW_RULE_TAG),
            "gate off must never mint a shadow tag"
        );
        let meta = engine.get_metadata("agent-off", &sid).await.unwrap().unwrap();
        assert!(
            meta.get(crate::prediction::rule_gate::HeldOutStats::METADATA_KEY).is_none(),
            "gate off must not seed a held-out record (byte-identical metadata shape)"
        );
    }

    // ── G6/#7: consolidation-failure telemetry ────────────────────────────

    #[tokio::test]
    async fn needs_more_evidence_records_a_consolidation_failure() {
        use crate::consolidation_failures::{list_failures, FailureReason};

        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        // 3 same-session verified mistakes → GovMem NeedsMoreEvidence.
        record_same_session_n(&nb, "agent-nme", MistakeCategory::Capability, 3, "");

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-nme", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none());

        let fails = list_failures(dir.path(), Some("agent-nme"), 10);
        assert_eq!(fails.len(), 1, "the GovMem rejection must be recorded");
        assert_eq!(fails[0].reason, FailureReason::NeedsMoreEvidence);
        assert_eq!(fails[0].category, "capability");
        assert_eq!(fails[0].detail["distinct_sessions"], serde_json::json!(1));
        assert_eq!(fails[0].detail["distinct_lessons"], serde_json::json!(3));
    }

    #[tokio::test]
    async fn insufficient_verified_evidence_is_recorded_when_raw_reaches_threshold() {
        use crate::consolidation_failures::{list_failures, FailureReason};

        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");

        // 2 verified + 3 unverified = raw 5 (>= threshold 3), verified 2 (< 3).
        for i in 0..2 {
            let e = build_mistake_entry(
                "agent-ive", &format!("v-{i}"), MistakeCategory::Capability,
                "u", "a", &format!("verified issue {i}"), None, "",
            )
            .with_evidence(TrajectoryEvidence::from_tool_error("bash", "boom"));
            nb.record(&e).unwrap();
        }
        for i in 0..3 {
            let e = build_mistake_entry(
                "agent-ive", &format!("u-{i}"), MistakeCategory::Capability,
                "u", "a", &format!("unverified issue {i}"), None, "",
            );
            nb.record(&e).unwrap();
        }

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-ive", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none());

        let fails = list_failures(dir.path(), Some("agent-ive"), 10);
        assert_eq!(fails.len(), 1);
        assert_eq!(fails[0].reason, FailureReason::InsufficientVerifiedEvidence);
        assert_eq!(fails[0].detail["raw"], serde_json::json!(5));
        assert_eq!(fails[0].detail["verified"], serde_json::json!(2));
    }

    #[tokio::test]
    async fn below_threshold_accumulation_is_not_recorded_as_failure() {
        use crate::consolidation_failures::list_failures;

        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        // Only 2 verified mistakes — normal accumulation, NOT a failure.
        record_n(&nb, "agent-acc", MistakeCategory::Capability, 2, "");

        let r = maybe_consolidate(&nb, &mem_path, dir.path(), "agent-acc", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none());
        assert!(
            list_failures(dir.path(), None, 10).is_empty(),
            "below-threshold accumulation must not be logged as a consolidation failure"
        );
    }

    #[tokio::test]
    async fn novelty_rejected_records_a_consolidation_failure() {
        use crate::consolidation_failures::{list_failures, FailureReason};

        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");

        // Round 1 consolidates cleanly (no failure).
        record_n(&nb, "agent-nov", MistakeCategory::Capability, 3, "");
        assert!(maybe_consolidate(&nb, &mem_path, dir.path(), "agent-nov", MistakeCategory::Capability, 3)
            .await
            .unwrap()
            .is_some());
        assert!(list_failures(dir.path(), None, 10).is_empty());

        // Round 2: byte-identical synthesized rule → B1 novelty gate rejects.
        record_n(&nb, "agent-nov", MistakeCategory::Capability, 3, "");
        assert!(maybe_consolidate(&nb, &mem_path, dir.path(), "agent-nov", MistakeCategory::Capability, 3)
            .await
            .unwrap()
            .is_none());

        let fails = list_failures(dir.path(), Some("agent-nov"), 10);
        assert_eq!(fails.len(), 1, "the B1 novelty rejection must be recorded");
        assert_eq!(fails[0].reason, FailureReason::NoveltyRejected);
        assert!(fails[0].detail["matched_id"].is_string());
    }
}
