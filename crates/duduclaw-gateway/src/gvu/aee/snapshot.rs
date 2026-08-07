//! WP2.3 §3.6 — the in-memory playbook shadow.
//!
//! **The inner loop's five steps must not write a single persistent row.**
//! Everything the Generator proposes is applied to a `PlaybookSnapshot`, a
//! plain `Vec` of entries, through the same pure
//! [`crate::playbook::delta::merge`] the real store uses — so what the inner
//! loop measures is exactly what a commit would produce, without a commit
//! having happened.
//!
//! Two consequences worth stating explicitly:
//!
//! 1. A round that ends in `abandoned` leaves `memories`, `gvu_champion` and
//!    `soul_versions` byte-identical to how it found them
//!    (`inner_loop_leaves_no_sqlite_row_on_abandon`).
//! 2. `failure_history` accumulates **in memory** during the loop and is
//!    flushed once at the outer boundary — including when the round is
//!    abandoned, because "we already tried this and it failed" is precisely
//!    the data that stops the next round repeating it.

use chrono::{DateTime, Utc};

use crate::playbook::delta::{merge, ExistingEntry, MergeOutcome, PlaybookDelta};
use crate::playbook::entry::{PlaybookMeta, PlaybookState};
use crate::playbook::gene::EvalCaseRef;

/// An immutable-by-convention view of one agent's playbook.
#[derive(Debug, Clone, Default)]
pub struct PlaybookSnapshot {
    pub entries: Vec<ExistingEntry>,
}

impl PlaybookSnapshot {
    pub fn new(entries: Vec<ExistingEntry>) -> Self {
        Self { entries }
    }

    /// Build from what [`crate::playbook::store::list_active`] returns.
    pub fn from_rows(
        rows: &[(
            duduclaw_core::types::MemoryEntry,
            PlaybookMeta,
            crate::prediction::rule_lifecycle::RuleStats,
        )],
    ) -> Self {
        Self::new(
            rows.iter()
                .map(|(e, meta, stats)| ExistingEntry {
                    id: e.id.clone(),
                    content: e.content.clone(),
                    meta: meta.clone(),
                    stats: *stats,
                })
                .collect(),
        )
    }

    fn is_live(meta: &PlaybookMeta) -> bool {
        matches!(meta.state, PlaybookState::Active | PlaybookState::Probation)
    }

    /// Active + probation count — what [`crate::playbook::PLAYBOOK_MAX_ENTRIES`]
    /// caps and what `G-Capacity` measures.
    pub fn active_count(&self) -> usize {
        self.entries.iter().filter(|e| Self::is_live(&e.meta)).count()
    }

    /// Entries whose `success_streak` is below `threshold` — the raw material
    /// for an `Optimize` round.
    pub fn low_streak(&self, threshold: u32) -> Vec<&ExistingEntry> {
        self.entries
            .iter()
            .filter(|e| Self::is_live(&e.meta) && e.meta.success_streak < threshold)
            .collect()
    }

    /// Wildcard-only live entries — the §1.3 quota's numerator.
    pub fn wildcard_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| Self::is_live(&e.meta))
            .filter(|e| e.meta.signals_match.len() == 1 && e.meta.signals_match[0] == "*")
            .count()
    }

    /// Every distinct eval case any live entry links, deduped and sorted.
    pub fn linked_cases(&self) -> Vec<EvalCaseRef> {
        let mut out: Vec<EvalCaseRef> = self
            .entries
            .iter()
            .filter(|e| Self::is_live(&e.meta))
            .flat_map(|e| e.meta.eval_cases.iter().cloned())
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.dedup();
        out
    }

    /// The eval cases linked by the entries a delta batch actually touches
    /// (§3.7.1 sampling stage ①). `Add` contributes the cases it declares.
    pub fn cases_touched_by(&self, deltas: &[PlaybookDelta]) -> Vec<EvalCaseRef> {
        let mut out: Vec<EvalCaseRef> = Vec::new();
        for d in deltas {
            match d {
                PlaybookDelta::Add { eval_cases, .. } => out.extend(eval_cases.iter().cloned()),
                PlaybookDelta::Link { id, eval_cases } => {
                    out.extend(eval_cases.iter().cloned());
                    out.extend(self.cases_of(id));
                }
                PlaybookDelta::Revise { id, .. }
                | PlaybookDelta::Record { id, .. }
                | PlaybookDelta::Retire { id, .. } => out.extend(self.cases_of(id)),
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.dedup();
        out
    }

    fn cases_of(&self, id: &str) -> Vec<EvalCaseRef> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.meta.eval_cases.clone())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<&ExistingEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Sorted `dedup_key`s of the live set — the champion snapshot identity.
    pub fn dedup_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .entries
            .iter()
            .filter(|e| Self::is_live(&e.meta))
            .map(|e| e.meta.dedup_key.clone())
            .collect();
        keys.sort();
        keys
    }

    /// Apply `deltas` to a **copy** of this snapshot. Pure: no I/O, no
    /// wall-clock read, `self` untouched.
    ///
    /// Returns the shadow snapshot and the merge outcome so the caller can
    /// feed dedup/missing-target notes back to the Generator as gradient
    /// material.
    pub fn shadow_apply(
        &self,
        deltas: Vec<PlaybookDelta>,
        now: DateTime<Utc>,
    ) -> (PlaybookSnapshot, MergeOutcome) {
        let outcome = merge(&self.entries, deltas, now);
        let mut next = self.clone();
        next.absorb(&outcome);
        (next, outcome)
    }

    fn absorb(&mut self, outcome: &MergeOutcome) {
        use crate::playbook::delta::AppliedOp;
        for op in &outcome.applied {
            match op {
                AppliedOp::Added { dedup_key, content, meta, stats } => {
                    // `merge` uses the dedup key as the within-batch identity
                    // for a not-yet-persisted entry; the shadow does the same
                    // so a follow-up delta in a LATER inner round can address
                    // it. A real id is minted only at commit time.
                    self.entries.push(ExistingEntry {
                        id: dedup_key.clone(),
                        content: content.clone(),
                        meta: meta.clone(),
                        stats: *stats,
                    });
                }
                AppliedOp::Revised { id, content, meta } => {
                    if let Some(e) = self.entries.iter_mut().find(|e| &e.id == id) {
                        e.content = content.clone();
                        e.meta = meta.clone();
                    }
                }
                AppliedOp::Linked { id, meta }
                | AppliedOp::Recorded { id, meta }
                | AppliedOp::Retired { id, meta, .. } => {
                    if let Some(e) = self.entries.iter_mut().find(|e| &e.id == id) {
                        e.meta = meta.clone();
                    }
                }
            }
        }
    }
}

/// A `failure_history` note buffered in memory during the inner loop and
/// flushed once at the outer boundary (§3.6 last row).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingFailureNote {
    /// Entry id, or the `Add`'s dedup key when the entry does not exist yet.
    pub target: String,
    pub what: String,
    /// Gate name or `eval:<case>`.
    pub source: String,
}

impl PendingFailureNote {
    pub fn new(target: &str, what: &str, source: &str) -> Self {
        Self {
            target: target.to_string(),
            // 160-char cap per `FailureNote`'s contract, CJK-safe.
            what: duduclaw_core::truncate_chars(what, 160),
            source: source.to_string(),
        }
    }
}

/// Deterministic sub-sampling of the non-linked, non-held-out cases
/// (§3.7.1 stage ②).
///
/// Seeded by `round_seq` rather than by a random generator so that re-running
/// round N picks the same subset — without that, the §2.4.2 σ calibration
/// would be measuring sample variance, not scorer variance.
pub fn deterministic_sample(pool: &[EvalCaseRef], round_seq: u64, max: usize) -> Vec<EvalCaseRef> {
    if pool.is_empty() || max == 0 {
        return Vec::new();
    }
    let quarter = pool.len().div_ceil(4);
    let take = max.min(quarter).max(1).min(pool.len());
    let start = (round_seq as usize) % pool.len();
    let mut sorted: Vec<EvalCaseRef> = pool.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    (0..take).map(|i| sorted[(start + i) % sorted.len()].clone()).collect()
}

/// The inner loop's sampling budget: at most this many extra cases beyond the
/// entries' own linked cases.
pub const INNER_LOOP_SAMPLE_MAX: usize = 5;

/// `success_streak` below which an entry counts as "worth optimising".
pub const LOW_STREAK_THRESHOLD: u32 = 2;

/// Timestamp helper so the whole inner loop shares one instant.
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::entry::{PlaybookCategory, PLAYBOOK_SCHEMA_VERSION};
    use crate::prediction::rule_lifecycle::RuleStats;

    fn entry(id: &str, content: &str, state: PlaybookState, streak: u32, cases: &[&str]) -> ExistingEntry {
        ExistingEntry {
            id: id.to_string(),
            content: content.to_string(),
            meta: PlaybookMeta {
                assertions: Default::default(),
                schema_version: PLAYBOOK_SCHEMA_VERSION,
                category: PlaybookCategory::Repair,
                signals_match: vec!["mistake:factual".to_string()],
                strategy: Vec::new(),
                failure_history: Vec::new(),
                eval_cases: cases.iter().map(|c| EvalCaseRef((*c).to_string())).collect(),
                applications: Vec::new(),
                success_streak: streak,
                state,
                revision: 0,
                dedup_key: crate::playbook::dedup::dedup_key(content, PlaybookCategory::Repair),
                embed_model: None,
                origin: "agent_derived".to_string(),
                derived_from: Vec::new(),
            },
            stats: RuleStats::initial(),
        }
    }

    fn t() -> DateTime<Utc> {
        "2026-08-06T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn shadow_apply_never_mutates_the_original() {
        let snap = PlaybookSnapshot::new(vec![entry("e1", "old text", PlaybookState::Active, 3, &["s/a"])]);
        let (next, outcome) = snap.shadow_apply(
            vec![PlaybookDelta::Revise {
                id: "e1".into(),
                content: "new text".into(),
                rationale: "x".into(),
            }],
            t(),
        );
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(snap.entries[0].content, "old text", "source snapshot is untouched");
        assert_eq!(next.entries[0].content, "new text");
        assert_eq!(next.entries[0].meta.state, PlaybookState::Probation);
        assert_eq!(next.entries[0].meta.success_streak, 0);
    }

    #[test]
    fn retired_and_stale_entries_do_not_count_toward_capacity() {
        let snap = PlaybookSnapshot::new(vec![
            entry("a", "a", PlaybookState::Active, 0, &["s/a"]),
            entry("b", "b", PlaybookState::Probation, 0, &["s/b"]),
            entry("c", "c", PlaybookState::Stale, 0, &["s/c"]),
            entry("d", "d", PlaybookState::Retired, 0, &["s/d"]),
        ]);
        assert_eq!(snap.active_count(), 2);
        assert_eq!(snap.linked_cases().len(), 2, "stale/retired entries contribute no cases");
    }

    #[test]
    fn cases_touched_by_covers_both_the_delta_and_the_target_entry() {
        let snap = PlaybookSnapshot::new(vec![entry("e1", "x", PlaybookState::Active, 0, &["s/own"])]);
        let touched = snap.cases_touched_by(&[
            PlaybookDelta::Link { id: "e1".into(), eval_cases: vec![EvalCaseRef("s/extra".into())] },
            PlaybookDelta::Add {
                assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
                content: "y".into(),
                category: PlaybookCategory::Innovate,
                signals_match: vec!["*".into()],
                eval_cases: vec![EvalCaseRef("s/new".into())],
                strategy: Vec::new(),
                rationale: "r".into(),
            },
        ]);
        let refs: Vec<&str> = touched.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(refs, vec!["s/extra", "s/new", "s/own"]);
    }

    #[test]
    fn deterministic_sample_is_reproducible_for_the_same_round() {
        let pool: Vec<EvalCaseRef> =
            (0..12).map(|i| EvalCaseRef(format!("s/case{i:02}"))).collect();
        let a = deterministic_sample(&pool, 7, INNER_LOOP_SAMPLE_MAX);
        let b = deterministic_sample(&pool, 7, INNER_LOOP_SAMPLE_MAX);
        assert_eq!(a, b, "same round_seq must pick the same subset");
        assert!(a.len() <= INNER_LOOP_SAMPLE_MAX);
        assert!(!a.is_empty());
        // A different round generally moves the window.
        assert_ne!(a, deterministic_sample(&pool, 8, INNER_LOOP_SAMPLE_MAX));
        // Degenerate inputs are safe.
        assert!(deterministic_sample(&[], 3, 5).is_empty());
        assert!(deterministic_sample(&pool, 3, 0).is_empty());
    }

    #[test]
    fn added_entry_is_addressable_by_a_later_inner_round() {
        let snap = PlaybookSnapshot::default();
        let add = PlaybookDelta::Add {
            assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
            content: "confirm before deleting".into(),
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:capability".into()],
            eval_cases: vec![EvalCaseRef("s/a".into())],
            strategy: Vec::new(),
            rationale: "r".into(),
        };
        let (round1, _) = snap.shadow_apply(vec![add], t());
        assert_eq!(round1.active_count(), 1);
        let new_id = round1.entries[0].id.clone();
        let (round2, outcome2) = round1.shadow_apply(
            vec![PlaybookDelta::Revise {
                id: new_id,
                content: "confirm twice before deleting".into(),
                rationale: "tighten".into(),
            }],
            t(),
        );
        assert_eq!(outcome2.applied.len(), 1, "round 2 can address what round 1 added");
        assert_eq!(round2.entries[0].content, "confirm twice before deleting");
    }

    #[test]
    fn pending_failure_note_truncates_cjk_safely() {
        let long = "\u{6E2C}".repeat(400);
        let note = PendingFailureNote::new("e1", &long, "G-Contract");
        assert_eq!(note.what.chars().count(), 160);
        assert!(note.what.is_char_boundary(note.what.len()));
    }
}
