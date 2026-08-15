//! AEE — the Agentic Evolution Engine (WP2.3 / WP2.5).
//!
//! `commercial/docs/DESIGN-evolution-v3-aee.md` chapter 3. AEE is what GVU
//! becomes once the evolution target moves off SOUL.md: the outer loop still
//! belongs to [`crate::gvu::loop_::GvuLoop`], but a single generation is now
//! an *agentic* step rather than one blind shot —
//!
//! ```text
//! decide intent (§3.2, deterministic)
//!   → assemble lineage prompt (§3.3, three cache blocks)
//!     → inner loop ≤3 rounds: generate → gate → shadow → score → revise (§3.1)
//!       → full measure + commit_verdict vs champion (§2.4)
//!         → commit deltas + entry-level settle (§2.5)
//!           → telemetry every step (WP0.6)
//! ```
//!
//! Module map:
//! - [`intent`] — GEP G4 strategy mix, deterministic per-round intent
//! - [`prompt`] — three-block prompt assembly + the held-out firewall
//! - [`snapshot`] — the in-memory shadow playbook (nothing persists in-loop)
//! - [`inner_loop`] — the ≤3-round generate/gate/score cycle
//! - [`settle`] — WP2.5 entry-level accept/rollback
//!
//! ### Placement note
//!
//! The design checklist filed these under `src/aee/`. They live under
//! `src/gvu/aee/` instead: AEE *is* the GVU loop's inner machinery, every
//! collaborator it needs (`champion`, `verifier_gate`, `verifier_measure`,
//! `stagnation`, `telemetry`, `version_store`) is a `gvu` sibling, and
//! keeping the whole wave inside one module means `lib.rs` — which several
//! concurrent sessions are editing — needs no change at all.
//!
//! ### Degradation contract (§3.7.2)
//!
//! Every external dependency is optional and degrades **loudly**:
//! cooldown absent ⇒ no cooldown; `stagnation_snapshot` failure ⇒ treated as
//! not stagnant; `telemetry_summary` failure ⇒ the lineage block says so in
//! words; scorer absent ⇒ the `cases` dimension is *absent*, never zero.
//! Not one of those paths is allowed to be silent.

pub mod eval_scorer;
pub mod inner_loop;
pub mod intent;
pub mod pending;
pub mod prompt;
pub mod run;
pub mod settle;
pub mod snapshot;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_wiring;

pub use eval_scorer::{resolve_eval_suites_root, EvalMeasureScorer};
pub use inner_loop::{run_inner_loop, InnerLoopExit, InnerLoopInput, InnerLoopOutcome, MAX_INNER_ROUNDS};
pub use pending::{PendingSettlement, PendingSettlementStore};
pub use run::{run_aee_round, settle_pending, AeeRoundInput, AeeRoundResult, AeeVerdict};
pub use intent::{
    decide_intent, AeeTrigger, EvolutionStrategy, IntentDecision, RoundIntent, RoundMaterial,
    SkipReason,
};
pub use prompt::{assemble, is_holdout, reject_holdout_links, PromptContext};
pub use settle::{
    entry_verdict, finalise, observe_entries, settlement_deltas, suite_verdict, CaseBands,
    EntryVerdict, SettleReport, SuiteVerdict,
};
pub use snapshot::{PendingFailureNote, PlaybookSnapshot};

use serde::Serialize;

use crate::gvu::knob_snapshot::KnobSnapshot;
use crate::gvu::verifier_measure::CommitVerdict;

/// §3.2.4 — one AEE round's audit record.
///
/// Serialised into an evolution event so a later reader can reconstruct *why*
/// a round did what it did: which intent, under which strategy, from which
/// trigger, how many inner rounds it took, and what the commit gate said.
/// Without the intent recorded, a mix misconfiguration is invisible in
/// hindsight — which is precisely how root cause R3 stayed unnoticed for
/// three months.
#[derive(Debug, Clone, Serialize)]
pub struct AeeRoundRecord {
    pub agent_id: String,
    pub intent: Option<String>,
    pub strategy: String,
    pub trigger: String,
    pub round_seq: u64,
    pub inner_rounds: u32,
    pub llm_calls: u32,
    pub deltas_proposed: usize,
    pub deltas_applied: usize,
    pub gate_rejections: Vec<String>,
    pub verdict: Option<String>,
    /// Present only when the round produced no work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// How the inner loop terminated.
    pub exit: String,
    /// Degradation flags — an absent eval dimension must be visible in the
    /// audit trail, not inferred from a missing field.
    pub case_dimension_available: bool,
    /// WP-6A / A2 (`commercial/docs/DESIGN-evolution-harness-knobs-2026-08.md`
    /// §7.2-A2) — the harness-knob values in effect for this round. Purely
    /// observational (see [`crate::gvu::knob_snapshot`]): never read back by
    /// any decision in this loop, only carried into telemetry so a future
    /// retrospective read has "what happened" and "under which settings" in
    /// the same row. `None` only for rounds recorded via [`Self::skipped`]
    /// before any per-agent context was resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knobs: Option<KnobSnapshot>,
}

impl AeeRoundRecord {
    /// The event payload shape from §3.2.4.
    pub fn to_payload(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }

    /// Convenience constructor for a round that never got started.
    pub fn skipped(
        agent_id: &str,
        strategy: EvolutionStrategy,
        trigger: AeeTrigger,
        round_seq: u64,
        reason: &SkipReason,
    ) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            intent: None,
            strategy: strategy.as_str().to_string(),
            trigger: trigger.as_str().to_string(),
            round_seq,
            inner_rounds: 0,
            llm_calls: 0,
            deltas_proposed: 0,
            deltas_applied: 0,
            gate_rejections: Vec::new(),
            verdict: None,
            skipped: Some(reason.as_str().to_string()),
            exit: "skipped".to_string(),
            case_dimension_available: false,
            // Skipped rounds return before `agent_dir`/`home_dir` are
            // resolved into a snapshot — see the field doc above.
            knobs: None,
        }
    }

    pub fn with_verdict(mut self, verdict: &CommitVerdict) -> Self {
        self.verdict = Some(verdict.label().to_string());
        self
    }
}

/// Fold the inner loop's buffered failure notes into a delta batch's target
/// entries.
///
/// Called at the outer boundary **whether or not the round commits** (§3.6
/// last row): an abandoned round's failure notes are the single most valuable
/// artefact it produced, because they are what stops the next round walking
/// into the same wall.
pub fn merge_pending_notes(
    snapshot: &mut PlaybookSnapshot,
    notes: &[PendingFailureNote],
    now: chrono::DateTime<chrono::Utc>,
) -> usize {
    use crate::playbook::entry::{FailureNote, FAILURE_HISTORY_CAP};
    let mut written = 0;
    for note in notes {
        if note.target.is_empty() {
            continue;
        }
        let Some(entry) = snapshot.entries.iter_mut().find(|e| e.id == note.target) else {
            continue;
        };
        entry.meta.failure_history.insert(
            0,
            FailureNote {
                at: now.to_rfc3339(),
                what: note.what.clone(),
                source: note.source.clone(),
            },
        );
        entry.meta.failure_history.truncate(FAILURE_HISTORY_CAP);
        written += 1;
    }
    written
}
