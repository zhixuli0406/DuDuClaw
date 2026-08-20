//! WP2.3/2.4/2.5 wiring — one AEE round, end to end.
//!
//! This is the module that turns the machinery built in `intent` / `prompt` /
//! `snapshot` / `inner_loop` / `settle` (+ `verifier_gate`, `verifier_measure`,
//! `champion`) from unit-tested parts into the live evolution path:
//!
//! ```text
//!  bump round_seq  ──▶ decide intent (§3.2) ──▶ [Skip → recorded, honest]
//!         │
//!         ├─ champion bootstrap (§2.4.1) — measure the CURRENT playbook once
//!         │
//!         ├─ inner loop ≤3 rounds (§3.1) — generate/gate/shadow/score/revise
//!         │
//!         ├─ full Measure vs champion (§2.3/§2.4) + anti-drift (§2.4.3)
//!         │
//!         ├─ commit deltas through `playbook::store::apply_deltas`
//!         │
//!         └─ enqueue the entry-level settlement (§2.5) for the sweep
//! ```
//!
//! ## What never happens here
//!
//! - **SOUL.md is never written.** WP1.1 closed that surface; AEE's artefact
//!   is a set of playbook entries. The SOUL cap-breach consolidation path
//!   still runs (it is checked by the caller, before the mode branch, and is
//!   shared by both modes) because an over-cap SOUL.md freezes the agent
//!   regardless of which evolution engine is driving.
//! - **A failed eval is never a zero.** Every absent-scorer path degrades the
//!   `cases` dimension to *absent* and says so in the round record.
//! - **The inner loop never persists.** Only the commit step below touches
//!   SQLite; an abandoned round leaves the playbook byte-identical, except for
//!   the `failure_history` notes it deliberately flushes (§3.6 last row).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use duduclaw_memory::SqliteMemoryEngine;
use tracing::{debug, info, warn};

use crate::gvu::champion::{snapshot_hash, Champion, ChampionStore};
use crate::gvu::mistake_notebook::MistakeEntry;
use crate::gvu::stagnation::{stagnation_snapshot, GvuStagnationConfig};
use crate::gvu::text_gradient::TextGradient;
use crate::gvu::verifier::{self, JudgeResult};
use crate::gvu::verifier_measure::{
    anti_drift, commit_verdict, measure, AntiDriftState, MeasureInput, MeasureScorer,
    MeasureVector, NoiseBand, ScoreRequest,
};
use crate::gvu::version_store::{ExperimentLogEntry, VersionStore};
use crate::playbook::delta::PlaybookDelta;
use crate::playbook::entry::{FailureNote, PlaybookMeta, FAILURE_HISTORY_CAP};

use super::eval_scorer::{resolve_eval_suites_root, EvalMeasureScorer};
use super::inner_loop::{run_inner_loop, InnerLoopExit, InnerLoopInput};
use super::intent::{decide_intent, AeeTrigger, EvolutionStrategy, IntentDecision, RoundMaterial};
use super::pending::{PendingSettlement, PendingSettlementStore};
use super::prompt::PromptContext;
use super::snapshot::{PlaybookSnapshot, LOW_STREAK_THRESHOLD};
use super::AeeRoundRecord;

/// How long a committed AEE round is observed before its entries are settled.
/// Mirrors the SOUL observation window's 24 h default.
pub const AEE_SETTLE_HOURS_DEFAULT: f64 = 24.0;

/// `agent.toml [evolution] aee_settle_hours`, clamped to something sane.
///
/// Goes through the shared typed parse point ([`duduclaw_core::agent_toml`]).
/// The field uses `opt_number_lossy`, NOT `opt_float_strict`: this reader
/// deliberately accepted an integer literal (`as_float().or_else(|| …
/// as_integer())`), the opposite of the `[fork]` budget quirk — see
/// `default_direction_settle_hours_accepts_integer_literal`.
fn settle_hours(agent_dir: &Path) -> f64 {
    match duduclaw_core::agent_toml::load(agent_dir)
        .evolution
        .aee_settle_hours
    {
        Some(v) if v > 0.0 => v.min(24.0 * 30.0),
        _ => AEE_SETTLE_HOURS_DEFAULT,
    }
}

/// What one AEE round did, in the terms the caller reports on.
#[derive(Debug, Clone)]
pub enum AeeVerdict {
    /// Deltas were committed to the playbook.
    Committed {
        applied: usize,
        entry_ids: Vec<String>,
        verdict: String,
    },
    /// The round ran but nothing was committed (gate/commit-gate rejection,
    /// nothing proposed, no viable candidate).
    NotCommitted { reason: String, gradient: TextGradient },
    /// The round never started (cooldown-style precondition, no material,
    /// missing infrastructure).
    Skipped { reason: String },
}

/// One round's full result: the machine-readable audit record plus the verdict.
#[derive(Debug, Clone)]
pub struct AeeRoundResult {
    pub record: AeeRoundRecord,
    pub verdict: AeeVerdict,
}

impl AeeRoundResult {
    fn skipped(record: AeeRoundRecord, reason: impl Into<String>) -> Self {
        Self { record, verdict: AeeVerdict::Skipped { reason: reason.into() } }
    }
}

/// Everything a round needs from its caller.
pub struct AeeRoundInput<'a> {
    pub agent_id: &'a str,
    pub agent_dir: &'a Path,
    /// DuDuClaw home (`memory.db`, `config.toml`, `evolution_telemetry.jsonl`).
    pub home_dir: PathBuf,
    pub trigger_context: &'a str,
    pub must_not: &'a [String],
    pub must_always: &'a [String],
    pub relevant_mistakes: &'a [MistakeEntry],
    /// Current SOUL.md — used ONLY as the "already in force" reference for the
    /// anti-sycophancy dimension and the gates. Never written.
    pub current_soul: &'a str,
    pub now: DateTime<Utc>,
}

/// Run one AEE round.
///
/// `call_llm` is the same closure shape `GvuLoop` already hands the legacy
/// path, so the account-rotated CLI/API path is reused verbatim.
pub async fn run_aee_round<F, Fut>(
    input: AeeRoundInput<'_>,
    vs: &VersionStore,
    call_llm: F,
) -> AeeRoundResult
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let scorer = EvalMeasureScorer::from_home(&input.home_dir);
    run_aee_round_with_scorer(input, vs, &scorer, call_llm).await
}

/// [`run_aee_round`] with an explicit scorer — the seam the wiring tests drive
/// (and the hook a future live-mode scorer plugs into).
pub async fn run_aee_round_with_scorer<F, Fut>(
    input: AeeRoundInput<'_>,
    vs: &VersionStore,
    scorer: &dyn MeasureScorer,
    call_llm: F,
) -> AeeRoundResult
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let started = std::time::Instant::now();
    let agent_id = input.agent_id;
    let strategy = EvolutionStrategy::from_agent_dir(input.agent_dir);
    let db_path = vs.db_path_ref().to_path_buf();
    let champions = ChampionStore::new(&db_path);
    let pending = PendingSettlementStore::new(&db_path);
    let suites_root = resolve_eval_suites_root(&input.home_dir);

    // ── §2.4.3 companion 2: a committed round owns its observation window ──
    // A tie-commit must not be able to skip real-traffic observation by
    // starting another round immediately.
    if let Some(p) = pending.get(agent_id) {
        if input.now < p.settle_after {
            let record = AeeRoundRecord::skipped(
                agent_id,
                strategy,
                AeeTrigger::default(),
                champions.round_seq(agent_id),
                &super::intent::SkipReason::Cooldown,
            );
            let reason = format!(
                "AEE settlement window active until {} ({} entries awaiting verdict)",
                p.settle_after.to_rfc3339(),
                p.entry_ids.len()
            );
            debug!(agent = %agent_id, "{reason}");
            return AeeRoundResult::skipped(record, reason);
        }
    }

    // ── Storage ────────────────────────────────────────────────────────────
    let engine = match SqliteMemoryEngine::new(&input.home_dir.join("memory.db")) {
        Ok(e) => e,
        Err(e) => {
            // Loud degradation: without the memory engine there IS no
            // playbook, so the round cannot run. It is skipped, not faked.
            warn!(
                agent = %agent_id,
                "AEE: memory engine unavailable — round skipped (no playbook to evolve): {e}"
            );
            let record = AeeRoundRecord::skipped(
                agent_id,
                strategy,
                AeeTrigger::default(),
                0,
                &super::intent::SkipReason::PlaybookAtCapacity,
            );
            return AeeRoundResult::skipped(record, format!("memory engine unavailable: {e}"));
        }
    };

    // ── Round counter (persisted; drives the mix and the sampling seed) ────
    let round_seq = champions.bump_round(agent_id).unwrap_or_else(|e| {
        warn!(agent = %agent_id, "AEE: round counter bump failed ({e}) — using 0");
        0
    });

    let rows = crate::playbook::store::list_active(&engine, agent_id).await;
    let base = PlaybookSnapshot::from_rows(&rows);

    // ── WP0.5 stagnation (absent ⇒ not stagnant, loudly) ──────────────────
    let stag_cfg = GvuStagnationConfig::from_agent_dir(input.agent_dir);
    let stagnation = Some(stagnation_snapshot(vs, agent_id, &stag_cfg));
    let is_stagnant = stagnation.as_ref().is_some_and(|s| s.is_stagnant());

    // The typed trigger AEE reasons about. `GvuLoop` only hands us a free-text
    // trigger context, so it is derived from the evidence at hand rather than
    // parsed out of prose: stagnation beats everything (AVO P8), a concrete
    // unresolved mistake means there is a real failure to consume, and
    // otherwise this is the neutral exploration window.
    let trigger = if is_stagnant {
        AeeTrigger::Stagnation
    } else if !input.relevant_mistakes.is_empty() {
        AeeTrigger::ChannelReply
    } else {
        AeeTrigger::ForcedReflection
    };

    let low_streak_ids: Vec<String> = base
        .low_streak(LOW_STREAK_THRESHOLD)
        .iter()
        .map(|e| e.id.clone())
        .collect();
    let material = RoundMaterial {
        unresolved_mistakes: input.relevant_mistakes.iter().filter(|m| !m.resolved).count(),
        low_streak_entries: low_streak_ids.len(),
        active_entries: base.active_count(),
    };

    let intent = match decide_intent(strategy, trigger, round_seq, material) {
        IntentDecision::Run(i) => i,
        IntentDecision::Skip(reason) => {
            let record =
                AeeRoundRecord::skipped(agent_id, strategy, trigger, round_seq, &reason);
            vs.record_experiment(&ExperimentLogEntry::new(
                agent_id,
                0,
                super::MAX_INNER_ROUNDS,
                started.elapsed(),
                "skipped",
                &format!("AEE round skipped: {}", reason.as_str()),
            ));
            info!(agent = %agent_id, reason = reason.as_str(), "AEE round skipped");
            return AeeRoundResult::skipped(record, format!("AEE: {}", reason.as_str()));
        }
    };

    let mut record = AeeRoundRecord {
        agent_id: agent_id.to_string(),
        intent: Some(intent.as_str().to_string()),
        strategy: strategy.as_str().to_string(),
        trigger: trigger.as_str().to_string(),
        round_seq,
        inner_rounds: 0,
        llm_calls: 0,
        deltas_proposed: 0,
        deltas_applied: 0,
        gate_rejections: Vec::new(),
        verdict: None,
        skipped: None,
        exit: "not_started".to_string(),
        case_dimension_available: false,
        // WP-6A / A2 — read-only harness-knob snapshot, carried into
        // telemetry only. See `crate::gvu::knob_snapshot` module docs.
        knobs: Some(crate::gvu::knob_snapshot::capture(&input.home_dir, input.agent_dir)),
    };

    // ── Measure inputs shared by the bootstrap and the candidate ───────────
    let rolled_back_summaries: Vec<String> = vs
        .get_history(agent_id, 20)
        .into_iter()
        .filter(|v| matches!(v.status, crate::gvu::version_store::VersionStatus::RolledBack))
        .map(|v| v.soul_summary)
        .collect();
    let mistake_descriptions: Vec<String> = input
        .relevant_mistakes
        .iter()
        .filter(|m| !m.resolved)
        .map(|m| m.what_went_wrong.clone())
        .collect();
    let band = NoiseBand::from_agent_dir(input.agent_dir);

    // ── §2.4.1 champion bootstrap ─────────────────────────────────────────
    //
    // A champion is measured the SAME way a candidate is (same `measure`
    // call, same dimensions) — a hand-assembled baseline vector would make
    // dimensions that are not actually comparable look comparable, and the
    // first real candidate would "regress" against numbers nothing produced.
    //
    // **WP-5A — `include_holdout: true`, matching the candidate below.**
    // This measurement used to exclude held-out cases while the candidate
    // measurement included them, which made the very first comparison of an
    // agent's life an apples-to-oranges one: the champion's `cases` dimension
    // was a visible-only mean and the candidate's was a mixed mean, so the
    // same suite could read as an improvement or a regression purely from the
    // held-out share — and `cases_holdout` (WP-4E's anti-gaming fence) was
    // skipped outright, because a dimension present on one side only is not
    // comparable. Aligning the two costs one extra replay of the held-out
    // subset, once per agent, with zero LLM calls (this call passes
    // `judge: None`, and the scorer runs `duduclaw eval --replay --no-judge`);
    // leaving it misaligned would mean every agent's first evolution round is
    // permanently unfenced. Held-out case *names* still never reach a prompt:
    // what leaves this block is a scalar vector, and the inner loop keeps its
    // own `include_holdout: false` (§3.5).
    let mut champion = live_champion(&champions, agent_id);
    if champion.is_none() {
        let base_contents: Vec<String> = live_contents(&base);
        let base_measure = measure(
            &MeasureInput {
                agent_id: agent_id.to_string(),
                contents: base_contents,
                current_reference: input.current_soul.to_string(),
                rolled_back_summaries: rolled_back_summaries.clone(),
                mistake_descriptions: mistake_descriptions.clone(),
                judge: None,
                post_hoc_must_not_hit: false,
            },
            scorer,
            &ScoreRequest {
                agent_id: agent_id.to_string(),
                cases: Vec::new(),
                // Same shape as the candidate's measurement below — see the
                // WP-5A note above.
                include_holdout: true,
            },
        )
        .await;

        if base_measure.case_dimension_available {
            let c = Champion {
                agent_id: agent_id.to_string(),
                snapshot_hash: snapshot_hash(&base.dedup_keys()),
                measure: base_measure.vector,
                established_at: input.now,
                anti_drift: AntiDriftState::default(),
                round_seq,
                holdout_rotation_due: false,
            };
            match champions.put(&c) {
                Ok(()) => {
                    info!(
                        agent = %agent_id,
                        cases = c.measure.cases.len(),
                        "AEE: baseline champion established from the current playbook"
                    );
                    champion = Some(c);
                }
                Err(e) => warn!(agent = %agent_id, "AEE: champion bootstrap write failed: {e}"),
            }
        } else {
            // Honest deferral: with no eval signal the baseline would be a
            // vector of content-only dimensions, which is not a champion in
            // any meaningful sense. `commit_verdict(_, None, _)` then treats
            // the first candidate as a first submission (§2.4), which is
            // exactly the designed bootstrap semantics.
            warn!(
                agent = %agent_id,
                scorer = scorer.name(),
                error = ?base_measure.scorer_error,
                "AEE: no eval score available — champion bootstrap DEFERRED; \
                 this round is judged as a first submission (gates only)"
            );
        }
    }

    // ── §3.1 inner loop ───────────────────────────────────────────────────
    let canaries = verifier::default_canary_tests();
    let ctx = PromptContext {
        agent_id: agent_id.to_string(),
        strategy,
        trigger,
        intent,
        round_seq,
        inner_round: 0,
        must_not: input.must_not.to_vec(),
        must_always: input.must_always.to_vec(),
        versions: vs.get_history(agent_id, 5),
        experiments: vs.get_experiment_summary(agent_id),
        telemetry: Some(crate::gvu::telemetry::telemetry_summary(&input.home_dir, agent_id, 30)),
        champion_headline: champion.as_ref().map(|c| c.measure.headline()),
        stagnation: stagnation.clone(),
        mistakes: input.relevant_mistakes.to_vec(),
        low_streak_ids,
        uncovered_signals: Vec::new(),
        previous_gradients: Vec::new(),
        previous_case_failures: Vec::new(),
    };

    let inner = run_inner_loop(
        InnerLoopInput {
            agent_id,
            base: base.clone(),
            ctx,
            must_not: input.must_not,
            must_always: input.must_always,
            canary_tests: &canaries,
            eval_cases_root: &suites_root,
            stagnation,
            round_seq,
            now: input.now,
        },
        scorer,
        // By reference: the same closure is needed again for the outer judge
        // call below, and requiring `F: Clone` here would push a `Clone`
        // bound all the way out to every existing `GvuLoop` caller.
        &call_llm,
    )
    .await;

    record.inner_rounds = inner.rounds_used;
    record.llm_calls = inner.llm_calls;
    record.deltas_proposed = inner.deltas.len() + inner.rejected.len();
    record.gate_rejections = inner.gate_rejections.clone();
    record.exit = inner.exit.as_str().to_string();

    // §3.6 last row: failure notes are flushed whether or not the round
    // commits — an abandoned round's notes are what stop the next one walking
    // into the same wall.
    let notes_written =
        persist_failure_notes(&engine, agent_id, &inner.pending_notes, input.now).await;
    if notes_written > 0 {
        debug!(agent = %agent_id, notes_written, "AEE: failure notes flushed");
    }

    // Gate rejections feed the existing WP0.6 telemetry (same stage/layer
    // fields the SOUL path writes — no second JSONL for AEE).
    for (delta, reason) in &inner.rejected {
        crate::gvu::telemetry::record_rejection_from_store(
            vs,
            agent_id,
            "gate",
            "G-Schema",
            reason,
            &delta_content(delta),
            inner.rounds_used,
        );
    }

    if !inner.has_candidate() {
        let (outcome_label, reason) = match &inner.exit {
            InnerLoopExit::NothingProposed => (
                "skipped",
                "AEE inner loop proposed no change (a legitimate empty answer)".to_string(),
            ),
            InnerLoopExit::GeneratorUnavailable { error } => {
                ("skipped", format!("AEE generator unavailable: {error}"))
            }
            InnerLoopExit::EscalateToHuman { reason } => (
                // Distinct outcome label + shared prefix: the stagnation
                // detector must be able to tell this meta-record apart from a
                // real rejected attempt, or the record feeds the very signal
                // that produced it (the 2026-08 self-feeding deadlock).
                crate::gvu::stagnation::ESCALATED_OUTCOME,
                format!(
                    "{}: {reason}",
                    crate::gvu::stagnation::ESCALATION_DESCRIPTION_PREFIX
                ),
            ),
            _ => (
                "abandoned",
                "AEE inner loop produced no gate-clearing candidate".to_string(),
            ),
        };
        vs.record_experiment(&ExperimentLogEntry::new(
            agent_id,
            inner.rounds_used,
            super::MAX_INNER_ROUNDS,
            started.elapsed(),
            outcome_label,
            &reason,
        ));
        info!(agent = %agent_id, exit = inner.exit.as_str(), "{reason}");
        return AeeRoundResult {
            record,
            verdict: if outcome_label == "skipped" {
                AeeVerdict::Skipped { reason }
            } else {
                AeeVerdict::NotCommitted {
                    gradient: TextGradient::blocking(
                        "AEE",
                        "inner_loop",
                        &reason,
                        "Review the gate rejections in evolution_telemetry.jsonl",
                    ),
                    reason,
                }
            },
        };
    }

    // ── §2.3 full Measure of the candidate ────────────────────────────────
    let contents = candidate_contents(&inner.deltas);
    let judge = judge_candidate(agent_id, &contents, input.must_not, &call_llm).await;
    let measured = measure(
        &MeasureInput {
            agent_id: agent_id.to_string(),
            contents: contents.clone(),
            current_reference: input.current_soul.to_string(),
            rolled_back_summaries,
            mistake_descriptions,
            judge,
            post_hoc_must_not_hit: false,
        },
        scorer,
        &ScoreRequest {
            agent_id: agent_id.to_string(),
            cases: Vec::new(),
            // The single full measurement of the round — held-out included.
            // This is the ONLY place held-out cases are scored, and their
            // names never enter a prompt (§3.5).
            include_holdout: true,
        },
    )
    .await;
    record.case_dimension_available = measured.case_dimension_available;

    // ── §2.4 commit gate ──────────────────────────────────────────────────
    let verdict = commit_verdict(&measured.vector, champion.as_ref().map(|c| &c.measure), &band);
    let decision = anti_drift(
        &verdict,
        champion.as_ref().map(|c| c.anti_drift).unwrap_or_default(),
    );
    record.verdict = Some(verdict.label().to_string());

    if !decision.commit {
        let reason = decision
            .blocked_reason
            .unwrap_or_else(|| format!("commit gate returned {}", verdict.label()));
        crate::gvu::telemetry::record_rejection_from_store(
            vs,
            agent_id,
            "measure",
            verdict.label(),
            &reason,
            &contents.join("\n"),
            inner.rounds_used,
        );
        vs.record_experiment(&ExperimentLogEntry::new(
            agent_id,
            inner.rounds_used,
            super::MAX_INNER_ROUNDS,
            started.elapsed(),
            "abandoned",
            &format!("AEE commit gate rejected: {reason}"),
        ));
        info!(agent = %agent_id, verdict = verdict.label(), "AEE candidate not committed: {reason}");
        return AeeRoundResult {
            record,
            verdict: AeeVerdict::NotCommitted {
                gradient: TextGradient::blocking(
                    "AEE-Commit",
                    "candidate",
                    &reason,
                    "Improve at least one measured dimension, or wait for the champion to be re-measured",
                ),
                reason,
            },
        };
    }

    // ── Commit ────────────────────────────────────────────────────────────
    let merge = crate::playbook::store::apply_deltas(
        &engine,
        agent_id,
        inner.deltas.clone(),
        input.must_not,
        &suites_root,
        input.now,
    )
    .await;
    record.deltas_applied = merge.applied.len();

    if merge.applied.is_empty() {
        // Everything was rejected at the persistence boundary — report it as
        // a non-commit rather than claiming an evolution that never landed.
        let reason = merge
            .rejected
            .first()
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| "no delta survived the store's validation".to_string());
        vs.record_experiment(&ExperimentLogEntry::new(
            agent_id,
            inner.rounds_used,
            super::MAX_INNER_ROUNDS,
            started.elapsed(),
            "abandoned",
            &format!("AEE commit produced no applied op: {reason}"),
        ));
        warn!(agent = %agent_id, "AEE commit applied nothing: {reason}");
        return AeeRoundResult {
            record,
            verdict: AeeVerdict::NotCommitted {
                gradient: TextGradient::blocking("AEE-Store", "delta", &reason, "Fix the delta"),
                reason,
            },
        };
    }

    // Re-read so the champion hash and the settlement id list describe what is
    // actually on disk, not what we expected to write.
    let after_rows = crate::playbook::store::list_active(&engine, agent_id).await;
    let after = PlaybookSnapshot::from_rows(&after_rows);
    let entry_ids = committed_entry_ids(&merge, &after);

    let next_champion = Champion {
        agent_id: agent_id.to_string(),
        snapshot_hash: snapshot_hash(&after.dedup_keys()),
        measure: measured.vector.clone(),
        established_at: input.now,
        anti_drift: AntiDriftState { consecutive_matches: decision.next_consecutive_matches },
        round_seq,
        holdout_rotation_due: decision.holdout_rotation_due
            || champion.as_ref().is_some_and(|c| c.holdout_rotation_due),
    };
    if let Err(e) = champions.put(&next_champion) {
        warn!(agent = %agent_id, "AEE: champion update failed after a successful commit: {e}");
    }

    // ── §2.5 enqueue the entry-level settlement ───────────────────────────
    let before_cases = champion
        .as_ref()
        .map(|c| c.measure.cases.clone())
        .unwrap_or_else(|| measured.vector.cases.clone());
    let settle_at = input.now
        + ChronoDuration::milliseconds((settle_hours(input.agent_dir) * 3_600_000.0) as i64);
    if let Err(e) = pending.put(&PendingSettlement {
        agent_id: agent_id.to_string(),
        applied_at: input.now,
        settle_after: settle_at,
        before: before_cases,
        entry_ids: entry_ids.clone(),
        band_cases: band.cases,
        band_holdout: Some(band.holdout),
    }) {
        warn!(agent = %agent_id, "AEE: settlement enqueue failed: {e}");
    }

    vs.record_experiment(&ExperimentLogEntry::new(
        agent_id,
        inner.rounds_used,
        super::MAX_INNER_ROUNDS,
        started.elapsed(),
        "applied",
        &format!(
            "AEE {} round committed {} playbook delta(s) [{}], settling at {}",
            intent.as_str(),
            merge.applied.len(),
            verdict.label(),
            settle_at.to_rfc3339(),
        ),
    ));
    info!(
        agent = %agent_id,
        intent = intent.as_str(),
        verdict = verdict.label(),
        applied = merge.applied.len(),
        cases = measured.case_dimension_available,
        "AEE round committed"
    );

    AeeRoundResult {
        record,
        verdict: AeeVerdict::Committed {
            applied: merge.applied.len(),
            entry_ids,
            verdict: verdict.label().to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A champion row with an empty `snapshot_hash` is the counter-only row
/// [`ChampionStore::bump_round`] creates for an agent that has never had one.
/// Treating it as a real champion would mean comparing every candidate against
/// an all-zero vector — everything would "improve", and the anti-drift
/// counters would never engage.
fn live_champion(store: &ChampionStore, agent_id: &str) -> Option<Champion> {
    store.get(agent_id).filter(|c| !c.snapshot_hash.is_empty())
}

fn live_contents(snapshot: &PlaybookSnapshot) -> Vec<String> {
    use crate::playbook::entry::PlaybookState;
    snapshot
        .entries
        .iter()
        .filter(|e| matches!(e.meta.state, PlaybookState::Active | PlaybookState::Probation))
        .map(|e| e.content.clone())
        .collect()
}

fn candidate_contents(deltas: &[PlaybookDelta]) -> Vec<String> {
    deltas
        .iter()
        .filter_map(|d| match d {
            PlaybookDelta::Add { content, .. } | PlaybookDelta::Revise { content, .. } => {
                Some(content.clone())
            }
            _ => None,
        })
        .collect()
}

fn delta_content(d: &PlaybookDelta) -> String {
    match d {
        PlaybookDelta::Add { content, .. } | PlaybookDelta::Revise { content, .. } => content.clone(),
        PlaybookDelta::Link { id, .. }
        | PlaybookDelta::Record { id, .. }
        | PlaybookDelta::Retire { id, .. } => id.clone(),
    }
}

/// Storage ids of everything the commit touched. `Added` ops carry a dedup key
/// (no id exists until persistence mints one), so they are resolved against
/// the re-read playbook.
fn committed_entry_ids(
    merge: &crate::playbook::delta::MergeOutcome,
    after: &PlaybookSnapshot,
) -> Vec<String> {
    use crate::playbook::delta::AppliedOp;
    let mut ids = Vec::new();
    for op in &merge.applied {
        match op {
            AppliedOp::Added { dedup_key, .. } => {
                if let Some(e) = after.entries.iter().find(|e| &e.meta.dedup_key == dedup_key) {
                    ids.push(e.id.clone());
                }
            }
            AppliedOp::Revised { id, .. }
            | AppliedOp::Linked { id, .. }
            | AppliedOp::Recorded { id, .. }
            | AppliedOp::Retired { id, .. } => ids.push(id.clone()),
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// The AEE judge: a compact quality read on the delta bodies themselves.
///
/// Deliberately NOT `verifier::build_judge_prompt` — that prompt frames the
/// artefact as a SOUL.md patch and hands the model the whole SOUL document,
/// which would have the judge scoring the wrong thing entirely. A failed or
/// unparseable call is `None` (absent dimension), never a low score.
async fn judge_candidate<F, Fut>(
    agent_id: &str,
    contents: &[String],
    must_not: &[String],
    call_llm: &F,
) -> Option<JudgeResult>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    if contents.is_empty() {
        // Link/Record/Retire-only batches carry no prose to judge. Absent, not
        // zero.
        return None;
    }
    let body = contents
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}", i + 1, duduclaw_core::truncate_chars(c, 600)))
        .collect::<Vec<_>>()
        .join("\n");
    let boundaries = if must_not.is_empty() {
        "(none declared)".to_string()
    } else {
        must_not.join("; ")
    };
    let prompt = format!(
        "You are an evolution quality judge for a DuDuClaw agent's playbook.\n\
         A playbook entry is one concrete, actionable behavioural rule the agent \
         will be shown when a matching situation occurs.\n\n\
         ## Agent\n{agent_id}\n\n\
         ## Contract boundaries the entry must never cross\n{boundaries}\n\n\
         ## Proposed entries\n{body}\n\n\
         ## Score each on\n\
         - concreteness (a rule, not a slogan or a document)\n\
         - actionability (the agent can tell whether it complied)\n\
         - non-sycophancy (it does not tell the agent to please the user)\n\n\
         Reply with ONLY this JSON object:\n\
         {{\"approved\": true|false, \"score\": 0.0-1.0, \"feedback\": \"one sentence\"}}"
    );
    match call_llm(prompt).await {
        Ok(r) => Some(verifier::parse_judge_response(&r)),
        Err(e) => {
            warn!(agent = %agent_id, "AEE judge call failed: {e} — judge dimension ABSENT (not zero)");
            None
        }
    }
}

/// Append the inner loop's buffered notes to the real entries' metadata.
///
/// Goes through the engine's existing metadata API rather than any bespoke
/// SQL, and silently ignores notes whose target is not a persisted entry (an
/// `Add`'s dedup key when the add never landed).
pub(crate) async fn persist_failure_notes(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    notes: &[super::snapshot::PendingFailureNote],
    now: DateTime<Utc>,
) -> usize {
    let mut written = 0usize;
    for note in notes {
        if note.target.is_empty() {
            continue;
        }
        let Ok(Some(mut metadata)) = engine.get_metadata(agent_id, &note.target).await else {
            continue;
        };
        let Some(mut meta) = PlaybookMeta::from_metadata(&metadata) else {
            continue;
        };
        meta.failure_history.insert(
            0,
            FailureNote {
                at: now.to_rfc3339(),
                what: note.what.clone(),
                source: note.source.clone(),
            },
        );
        meta.failure_history.truncate(FAILURE_HISTORY_CAP);
        meta.merge_into(&mut metadata);
        if engine
            .update_metadata(agent_id, &note.target, &metadata)
            .await
            .is_ok()
        {
            written += 1;
        }
    }
    written
}

/// Persist the `success_streak` bookkeeping [`super::settle::apply_streaks`]
/// computes in memory. `settlement_deltas` deliberately does not encode it
/// (the merge is a pure function and must not carry AEE policy), so it lands
/// here, beside the settlement that decided it.
pub(crate) async fn persist_streaks(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    observations: &[super::settle::EntryObservation],
) -> usize {
    use super::settle::EntryVerdict;
    let mut written = 0usize;
    for obs in observations {
        let delta: Option<u32> = match obs.verdict {
            EntryVerdict::Confirm { .. } => None, // increment, handled below
            EntryVerdict::Rollback { .. } => Some(0),
            EntryVerdict::Insufficient { .. } => continue,
        };
        let Ok(Some(mut metadata)) = engine.get_metadata(agent_id, &obs.entry_id).await else {
            continue;
        };
        let Some(mut meta) = PlaybookMeta::from_metadata(&metadata) else {
            continue;
        };
        meta.success_streak = match delta {
            Some(v) => v,
            None => meta.success_streak.saturating_add(1),
        };
        meta.merge_into(&mut metadata);
        if engine
            .update_metadata(agent_id, &obs.entry_id, &metadata)
            .await
            .is_ok()
        {
            written += 1;
        }
    }
    written
}

/// Judge one agent's due settlement: re-score the suite, compare against the
/// stored `before`, and persist the entry-level verdicts.
///
/// Returns the settle report when a verdict was reachable, `None` when the
/// scorer could not produce an `after` side (reported, never rendered as
/// "checked and fine").
pub async fn settle_pending(
    p: &PendingSettlement,
    db_path: &Path,
    home_dir: &Path,
    scorer: &dyn MeasureScorer,
) -> Option<super::settle::SettleReport> {
    let store = PendingSettlementStore::new(db_path);
    let agent_id = p.agent_id.as_str();

    let after = match scorer
        .score(&ScoreRequest {
            agent_id: agent_id.to_string(),
            cases: Vec::new(),
            include_holdout: true,
        })
        .await
    {
        Ok(Some(scores)) if !scores.is_empty() => scores,
        Ok(_) => {
            warn!(
                agent = %agent_id,
                "AEE settle: no eval score available — entries left exactly as they are \
                 (INSUFFICIENT, not confirmed)"
            );
            let _ = store.remove(agent_id);
            return None;
        }
        Err(e) => {
            warn!(agent = %agent_id, "AEE settle: scorer failed ({e}) — entries left untouched");
            let _ = store.remove(agent_id);
            return None;
        }
    };

    let engine = match SqliteMemoryEngine::new(&home_dir.join("memory.db")) {
        Ok(e) => e,
        Err(e) => {
            warn!(agent = %agent_id, "AEE settle: memory engine unavailable ({e}) — retrying next tick");
            return None;
        }
    };
    let rows = crate::playbook::store::list_active(&engine, agent_id).await;
    let snapshot = PlaybookSnapshot::from_rows(&rows);

    let observations = super::settle::observe_entries(&snapshot, &p.before, &after, p.band_cases);
    let suite = super::settle::suite_verdict(
        &MeasureVector { cases: p.before.clone(), ..Default::default() },
        &MeasureVector { cases: after.clone(), ..Default::default() },
        p.case_bands(),
    );
    let (deltas, report) = super::settle::finalise(&observations, &suite, agent_id);

    if !deltas.is_empty() {
        let suites_root = resolve_eval_suites_root(home_dir);
        super::settle::persist(&engine, agent_id, deltas, &[], &suites_root, Utc::now()).await;
        persist_streaks(&engine, agent_id, &observations).await;
    }
    let _ = store.remove(agent_id);

    info!(
        agent = %agent_id,
        confirmed = report.confirmed,
        rolled_back = report.rolled_back.len(),
        insufficient = report.insufficient,
        suite_regressed = report.suite_regressed,
        "AEE settle: entry-level verdicts applied"
    );
    Some(report)
}

/// R5: `[evolution] aee_settle_hours` direction, pinned.
///
/// absent / malformed / wrong-typed / non-positive ⇒ [`AEE_SETTLE_HOURS_DEFAULT`],
/// and any value is capped at 30 days.
///
/// The quirk worth pinning is the **opposite** of `[fork]`'s budget keys and
/// of `[evolution.noise_band]`, both of which used `as_float()` alone and so
/// ignored an integer literal. This reader wrote
/// `as_float().or_else(|| as_integer().map(|i| i as f64))`, so
/// `aee_settle_hours = 24` was — and remains — honoured. The typed field
/// therefore uses `lenient::opt_number_lossy`, not `opt_float_strict`.
/// Converging the two would be a real behavior change smuggled in by a schema
/// refactor.
#[cfg(test)]
mod settle_hours_tests {
    use super::{settle_hours, AEE_SETTLE_HOURS_DEFAULT};

    fn dir_with(body: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("agent.toml"), body).unwrap();
        dir
    }

    #[test]
    fn default_direction_settle_hours_falls_back_on_anything_unusable() {
        for body in [
            "",                                            // empty file
            "[evolution]\n",                               // section, no key
            "[evolution]\naee_settle_hours = 0\n",         // non-positive
            "[evolution]\naee_settle_hours = -1.0\n",      // negative
            "[evolution]\naee_settle_hours = \"24\"\n",    // wrong type
            "[evolution]\naee_settle_hours = true\n",      // wrong type
            "evolution = \"scalar\"\n",                    // wrong-typed section
            "not toml [[[",                                // malformed file
        ] {
            let dir = dir_with(body);
            assert_eq!(
                settle_hours(dir.path()),
                AEE_SETTLE_HOURS_DEFAULT,
                "for {body:?}"
            );
        }

        // Missing file entirely — same direction.
        let empty = tempfile::TempDir::new().unwrap();
        assert_eq!(settle_hours(empty.path()), AEE_SETTLE_HOURS_DEFAULT);
    }

    #[test]
    fn default_direction_settle_hours_accepts_integer_literal() {
        // The deliberate opposite of `[fork] default_budget_usd` and
        // `[evolution.noise_band]`, both of which ignore an integer.
        let dir = dir_with("[evolution]\naee_settle_hours = 48\n");
        assert_eq!(settle_hours(dir.path()), 48.0);

        let dir = dir_with("[evolution]\naee_settle_hours = 1.5\n");
        assert_eq!(settle_hours(dir.path()), 1.5);
    }

    #[test]
    fn default_direction_settle_hours_is_capped_at_thirty_days() {
        let dir = dir_with("[evolution]\naee_settle_hours = 100000.0\n");
        assert_eq!(settle_hours(dir.path()), 24.0 * 30.0);
    }
}
