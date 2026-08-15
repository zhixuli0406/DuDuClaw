//! WP2.5 — entry-level accept / rollback (GEP G6, design §2.5 / P3).
//!
//! The observation verdict drops from "the whole snapshot" to "this entry".
//! An entry's confirm/rollback is decided by **its own linked eval cases**,
//! which is both finer and cheaper than re-running the suite; the full suite
//! score stays in play as a *global regression fence* so an entry that looks
//! fine on its own three cases cannot quietly wreck the other seventeen.
//!
//! ## Double-counting boundary (explicit, and tested)
//!
//! Two ledgers already exist and they must not both record the same event:
//!
//! | Ledger | Owner | Written by | Records |
//! |---|---|---|---|
//! | `rule_stats` (`helpful`/`harmful`) | `prediction::rule_lifecycle::settle_injected_rules` | a real channel turn settling | live-traffic outcomes |
//! | `applications` + `success_streak` | this module | an eval comparison | eval outcomes |
//!
//! This module writes **only** `applications`/`success_streak` (via
//! `PlaybookDelta::Record`) and, for a regression, `PlaybookDelta::Retire`.
//! It never touches `rule_stats` — so an eval failure does not also burn a
//! `harmful` strike that live traffic would then burn again. The Janus
//! one-strike machinery in `rule_lifecycle` is left exactly as it is
//! (`eval_settlement_never_writes_rule_stats`).

use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::info;

use crate::gvu::verifier_measure::{derived_holdout_band, CaseScore, MeasureVector, NoiseBand};
use crate::playbook::delta::PlaybookDelta;
use crate::playbook::gene::EvalCaseRef;

use super::snapshot::PlaybookSnapshot;

/// Per-entry verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum EntryVerdict {
    /// Linked cases held or improved.
    Confirm { before: f64, after: f64 },
    /// Linked cases regressed beyond the band — retire THIS entry only.
    Rollback { before: f64, after: f64, reason: String },
    /// No comparable linked case ran. Not a pass and not a failure: the
    /// entry is left exactly as it was and the gap is reported, because
    /// "we could not check" must never render as "checked and fine".
    Insufficient { reason: String },
}

impl EntryVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Confirm { .. } => "confirm",
            Self::Rollback { .. } => "rollback",
            Self::Insufficient { .. } => "insufficient",
        }
    }
}

/// One entry's before/after evidence.
#[derive(Debug, Clone)]
pub struct EntryObservation {
    pub entry_id: String,
    pub verdict: EntryVerdict,
}

/// Global fence: did the whole suite regress?
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum SuiteVerdict {
    Pass { before: f64, after: f64 },
    /// The suite mean fell outside the band. Entry-level confirms do not
    /// survive this — the whole candidate is rolled back.
    Regressed { before: f64, after: f64 },
    /// One side had no cases at all.
    Insufficient,
}

/// The two case tolerances the suite fence needs, frozen at commit time.
///
/// WP-4E: the fence used to be a single `f64` (the `cases` band). It now needs
/// the held-out band too — and it must be the *same* number the commit gate
/// used, not a second policy. Both constructors below funnel through
/// [`NoiseBand`] / [`derived_holdout_band`] for exactly that reason.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaseBands {
    /// Tolerance for the mixed and visible means.
    pub cases: f64,
    /// Stricter tolerance for the held-out mean.
    pub holdout: f64,
}

impl CaseBands {
    /// Straight from the band the commit gate ran with.
    pub fn from_noise_band(b: &NoiseBand) -> Self {
        Self { cases: b.cases, holdout: b.holdout }
    }

    /// A pending row written before WP-4E recorded only `cases`. Reconstruct
    /// the held-out tolerance through the same derivation `NoiseBand` uses, so
    /// an in-flight observation window that spans the upgrade settles under
    /// the documented default rather than under a silently different rule.
    pub fn from_cases_only(cases: f64) -> Self {
        Self { cases, holdout: derived_holdout_band(cases) }
    }
}

/// Mean of the cases in `refs`, or `None` when none of them ran.
fn mean_over(scores: &[CaseScore], refs: &[EvalCaseRef]) -> Option<f64> {
    let wanted: std::collections::BTreeSet<&str> = refs.iter().map(|r| r.0.as_str()).collect();
    let hits: Vec<f64> = scores
        .iter()
        .filter(|c| wanted.contains(c.case.as_str()))
        .map(|c| c.score)
        .collect();
    if hits.is_empty() {
        return None;
    }
    Some(hits.iter().sum::<f64>() / hits.len() as f64)
}

/// Verdict for one entry from its linked cases.
///
/// `band` is the `cases` noise band — the same number the commit gate uses,
/// so a difference that is a tie at the suite level cannot be a rollback at
/// the entry level.
pub fn entry_verdict(
    linked: &[EvalCaseRef],
    before: &[CaseScore],
    after: &[CaseScore],
    band: f64,
) -> EntryVerdict {
    if linked.is_empty() {
        return EntryVerdict::Insufficient {
            reason: "entry links no eval case".to_string(),
        };
    }
    let (Some(b), Some(a)) = (mean_over(before, linked), mean_over(after, linked)) else {
        return EntryVerdict::Insufficient {
            reason: "linked cases did not run on both sides".to_string(),
        };
    };
    if a < b - band {
        EntryVerdict::Rollback {
            before: b,
            after: a,
            reason: format!("linked cases regressed {b:.2} → {a:.2} (band {band:.2})"),
        }
    } else {
        EntryVerdict::Confirm { before: b, after: a }
    }
}

/// Global regression fence over every non-held-out **and** held-out case.
///
/// WP-4E — three fences, same shape as the commit gate
/// (`verifier_measure::dimensions`), and for the same reason: one mixed mean
/// let a large visible-set gain pay for a held-out-set loss inside a single
/// band, which is precisely what the held-out subset exists to detect. The
/// legacy mixed fence is kept verbatim and checked FIRST, so every rejection
/// the old code produced is still produced, with the same reported numbers;
/// the visible and held-out fences can only *add* trips. A side that has no
/// held-out (or no visible) case simply skips that fence — an agent whose
/// suite has no held-out case is byte-identical to the pre-WP-4E behaviour.
pub fn suite_verdict(
    before: &MeasureVector,
    after: &MeasureVector,
    bands: CaseBands,
) -> SuiteVerdict {
    let (Some(b), Some(a)) = (before.cases_mean(), after.cases_mean()) else {
        return SuiteVerdict::Insufficient;
    };
    // Legacy fence, unchanged and first.
    if a < b - bands.cases {
        return SuiteVerdict::Regressed { before: b, after: a };
    }
    // Sub-fences, in the same order `dimensions()` reports them.
    for (name, band, pair) in [
        (
            "cases_holdout",
            bands.holdout,
            (before.holdout_cases_mean(), after.holdout_cases_mean()),
        ),
        (
            "cases_visible",
            bands.cases,
            (before.visible_cases_mean(), after.visible_cases_mean()),
        ),
    ] {
        let (Some(sb), Some(sa)) = pair else {
            continue; // present on one side only ⇒ not comparable
        };
        if sa < sb - band {
            info!(
                fence = name,
                before = sb,
                after = sa,
                band,
                "AEE settle: sub-fence tripped — the mixed suite mean alone would have passed"
            );
            return SuiteVerdict::Regressed { before: sb, after: sa };
        }
    }
    SuiteVerdict::Pass { before: b, after: a }
}

/// Evaluate every live entry in `snapshot` against the two score sets.
pub fn observe_entries(
    snapshot: &PlaybookSnapshot,
    before: &[CaseScore],
    after: &[CaseScore],
    band: f64,
) -> Vec<EntryObservation> {
    snapshot
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.meta.state,
                crate::playbook::entry::PlaybookState::Active
                    | crate::playbook::entry::PlaybookState::Probation
            )
        })
        .map(|e| EntryObservation {
            entry_id: e.id.clone(),
            verdict: entry_verdict(&e.meta.eval_cases, before, after, band),
        })
        .collect()
}

/// Turn verdicts into deltas.
///
/// - `Confirm` → one `Record{outcome:"eval_pass"}` capsule.
/// - `Rollback` → one `Record{outcome:"eval_fail"}` capsule **plus** a
///   `Retire` for that entry alone. Nothing else in the batch is disturbed —
///   that is the whole point of entry-level rollback (design "混合提案部分
///   保留").
/// - `Insufficient` → nothing at all. An unmeasured entry is not evidence.
///
/// None of these deltas touch `rule_stats`; see the module docs.
pub fn settlement_deltas(observations: &[EntryObservation]) -> Vec<PlaybookDelta> {
    let mut out = Vec::new();
    for obs in observations {
        match &obs.verdict {
            EntryVerdict::Confirm { after, .. } => out.push(PlaybookDelta::Record {
                id: obs.entry_id.clone(),
                outcome: "eval_pass".to_string(),
                score: after.clamp(0.0, 1.0),
                ctx: Some("aee_entry_settle".to_string()),
            }),
            EntryVerdict::Rollback { after, reason, .. } => {
                out.push(PlaybookDelta::Record {
                    id: obs.entry_id.clone(),
                    outcome: "eval_fail".to_string(),
                    score: after.clamp(0.0, 1.0),
                    ctx: Some("aee_entry_settle".to_string()),
                });
                out.push(PlaybookDelta::Retire {
                    id: obs.entry_id.clone(),
                    reason: reason.clone(),
                });
            }
            EntryVerdict::Insufficient { .. } => {}
        }
    }
    out
}

/// `success_streak` bookkeeping that `merge` does not do for us.
///
/// `Record` deliberately does not touch the streak inside
/// [`crate::playbook::delta::merge`] (it is a pure function that must not
/// encode AEE policy), so the streak update is applied here, right beside
/// the rule that defines it: a confirm increments, a rollback zeroes.
pub fn apply_streaks(snapshot: &mut PlaybookSnapshot, observations: &[EntryObservation]) {
    for obs in observations {
        let Some(e) = snapshot.entries.iter_mut().find(|e| e.id == obs.entry_id) else {
            continue;
        };
        match obs.verdict {
            EntryVerdict::Confirm { .. } => {
                e.meta.success_streak = e.meta.success_streak.saturating_add(1)
            }
            EntryVerdict::Rollback { .. } => e.meta.success_streak = 0,
            EntryVerdict::Insufficient { .. } => {}
        }
    }
}

/// A settlement summary suitable for an evolution event / dashboard row.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SettleReport {
    pub confirmed: usize,
    pub rolled_back: Vec<String>,
    pub insufficient: usize,
    /// TRUE when the whole-suite fence tripped and the entry-level confirms
    /// were discarded.
    pub suite_regressed: bool,
}

/// Combine the entry verdicts with the global fence.
///
/// When the suite regressed, entry-level confirms are **not** honoured: the
/// caller rolls the whole candidate back. Reporting is unchanged so the
/// operator can still see which entries individually looked fine — that
/// asymmetry (looked fine locally, regressed globally) is the exact signature
/// of an entry over-fitting to its own linked cases.
pub fn finalise(
    observations: &[EntryObservation],
    suite: &SuiteVerdict,
    agent_id: &str,
) -> (Vec<PlaybookDelta>, SettleReport) {
    let mut report = SettleReport::default();
    for o in observations {
        match &o.verdict {
            EntryVerdict::Confirm { .. } => report.confirmed += 1,
            EntryVerdict::Rollback { .. } => report.rolled_back.push(o.entry_id.clone()),
            EntryVerdict::Insufficient { .. } => report.insufficient += 1,
        }
    }

    if matches!(suite, SuiteVerdict::Regressed { .. }) {
        report.suite_regressed = true;
        info!(
            agent = %agent_id,
            confirmed_entries = report.confirmed,
            "AEE settle: whole-suite regression fence tripped — entry-level confirms discarded"
        );
        // Only the failures are still worth recording: they are evidence.
        let failures: Vec<EntryObservation> = observations
            .iter()
            .filter(|o| matches!(o.verdict, EntryVerdict::Rollback { .. }))
            .cloned()
            .collect();
        return (settlement_deltas(&failures), report);
    }

    (settlement_deltas(observations), report)
}

/// Persist a settlement through the ordinary playbook store path.
///
/// Goes through [`crate::playbook::store::apply_deltas`] rather than any
/// bespoke SQL, so the settlement inherits validation, the deterministic
/// merge, and the sibling-metadata-preserving write for free.
pub async fn persist(
    engine: &duduclaw_memory::SqliteMemoryEngine,
    agent_id: &str,
    deltas: Vec<PlaybookDelta>,
    must_not: &[String],
    eval_cases_root: &std::path::Path,
    now: DateTime<Utc>,
) -> crate::playbook::delta::MergeOutcome {
    crate::playbook::store::apply_deltas(engine, agent_id, deltas, must_not, eval_cases_root, now)
        .await
}

/// Timestamp helper.
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::delta::ExistingEntry;
    use crate::playbook::entry::{PlaybookCategory, PlaybookMeta, PlaybookState, PLAYBOOK_SCHEMA_VERSION};
    use crate::prediction::rule_lifecycle::RuleStats;

    fn cs(case: &str, score: f64) -> CaseScore {
        CaseScore { case: case.to_string(), score, held_out: false }
    }

    fn entry(id: &str, cases: &[&str], streak: u32) -> ExistingEntry {
        ExistingEntry {
            id: id.to_string(),
            content: format!("rule {id}"),
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
                state: PlaybookState::Active,
                revision: 0,
                dedup_key: format!("k-{id}"),
                embed_model: None,
                origin: "agent_derived".to_string(),
                derived_from: Vec::new(),
            },
            stats: RuleStats::initial(),
        }
    }

    #[test]
    fn regressing_entry_is_retired_alone_and_neighbours_are_untouched() {
        let snap = PlaybookSnapshot::new(vec![
            entry("good", &["s/g1", "s/g2"], 1),
            entry("bad", &["s/b1"], 1),
        ]);
        let before = vec![cs("s/g1", 1.0), cs("s/g2", 1.0), cs("s/b1", 1.0)];
        let after = vec![cs("s/g1", 1.0), cs("s/g2", 1.0), cs("s/b1", 0.0)];

        let obs = observe_entries(&snap, &before, &after, 0.05);
        let deltas = settlement_deltas(&obs);

        let retired: Vec<&str> = deltas
            .iter()
            .filter_map(|d| match d {
                PlaybookDelta::Retire { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(retired, vec!["bad"], "only the regressing entry rolls back");

        let recorded_good = deltas.iter().any(|d| {
            matches!(d, PlaybookDelta::Record { id, outcome, .. } if id == "good" && outcome == "eval_pass")
        });
        assert!(recorded_good, "the healthy neighbour is confirmed, not collaterally rolled back");
    }

    #[test]
    fn unmeasured_entry_is_insufficient_not_a_silent_pass() {
        let snap = PlaybookSnapshot::new(vec![entry("orphan", &["s/never-ran"], 0)]);
        let obs = observe_entries(&snap, &[cs("s/other", 1.0)], &[cs("s/other", 1.0)], 0.05);
        assert!(matches!(obs[0].verdict, EntryVerdict::Insufficient { .. }));
        assert!(settlement_deltas(&obs).is_empty(), "no evidence ⇒ no write");
    }

    #[test]
    fn within_band_dip_is_a_confirm_not_a_rollback() {
        let linked = vec![EvalCaseRef("s/a".into()), EvalCaseRef("s/b".into())];
        // 1.0 → 0.5 across two cases is a 0.5 mean vs 1.0: outside a 0.05 band.
        assert!(matches!(
            entry_verdict(&linked, &[cs("s/a", 1.0), cs("s/b", 1.0)], &[cs("s/a", 1.0), cs("s/b", 0.0)], 0.05),
            EntryVerdict::Rollback { .. }
        ));
        // …but with a band wide enough to cover it, it is a tie → confirm.
        assert!(matches!(
            entry_verdict(&linked, &[cs("s/a", 1.0), cs("s/b", 1.0)], &[cs("s/a", 1.0), cs("s/b", 0.0)], 0.6),
            EntryVerdict::Confirm { .. }
        ));
    }

    #[test]
    fn suite_regression_fence_overrides_entry_level_confirms() {
        let snap = PlaybookSnapshot::new(vec![entry("e1", &["s/own"], 1)]);
        // The entry's own case is fine…
        let before = vec![cs("s/own", 1.0), cs("s/other1", 1.0), cs("s/other2", 1.0)];
        // …but the suite as a whole fell apart.
        let after = vec![cs("s/own", 1.0), cs("s/other1", 0.0), cs("s/other2", 0.0)];
        let obs = observe_entries(&snap, &before, &after, 0.05);
        assert!(matches!(obs[0].verdict, EntryVerdict::Confirm { .. }));

        let bv = MeasureVector { cases: before, ..Default::default() };
        let av = MeasureVector { cases: after, ..Default::default() };
        let suite = suite_verdict(&bv, &av, CaseBands::from_cases_only(0.05));
        assert!(matches!(suite, SuiteVerdict::Regressed { .. }));

        let (deltas, report) = finalise(&obs, &suite, "a");
        assert!(report.suite_regressed);
        assert_eq!(report.confirmed, 1, "the local confirm is still REPORTED…");
        assert!(deltas.is_empty(), "…but not honoured — this is the over-fitting signature");
    }

    // ── WP-4E: the suite fence splits visible / held-out too ─────────────

    fn hs(case: &str, score: f64) -> CaseScore {
        CaseScore { case: case.to_string(), score, held_out: true }
    }

    /// The commit gate's masking scenario, replayed at settlement time.
    #[test]
    fn settle_fence_catches_a_held_out_loss_hidden_by_a_visible_gain() {
        let bands = CaseBands::from_cases_only(0.05); // holdout = 0.025
        // 4 visible cases 0.5 → 1.0, 40 held-out cases 1.0 → 0.95.
        // Mixed mean is 42/44 on both sides: the legacy fence sees nothing.
        let mut before: Vec<CaseScore> =
            vec![cs("s/v0", 1.0), cs("s/v1", 1.0), cs("s/v2", 0.0), cs("s/v3", 0.0)];
        let mut after: Vec<CaseScore> =
            vec![cs("s/v0", 1.0), cs("s/v1", 1.0), cs("s/v2", 1.0), cs("s/v3", 1.0)];
        for i in 0..40 {
            before.push(hs(&format!("s/_holdout/h{i}"), 1.0));
            after.push(hs(&format!("s/_holdout/h{i}"), if i < 2 { 0.0 } else { 1.0 }));
        }
        let bv = MeasureVector { cases: before.clone(), ..Default::default() };
        let av = MeasureVector { cases: after.clone(), ..Default::default() };
        assert_eq!(bv.cases_mean(), av.cases_mean(), "the mixed mean is unmoved");

        // OLD behaviour: the same scores with the held-out flag cleared are
        // exactly what the single-mean fence used to compare — it passes.
        let clear = |v: &[CaseScore]| MeasureVector {
            cases: v.iter().map(|c| CaseScore { held_out: false, ..c.clone() }).collect(),
            ..Default::default()
        };
        assert!(matches!(
            suite_verdict(&clear(&before), &clear(&after), bands),
            SuiteVerdict::Pass { .. }
        ));

        // NEW behaviour: the held-out fence trips and the whole candidate is
        // rolled back.
        match suite_verdict(&bv, &av, bands) {
            SuiteVerdict::Regressed { before: b, after: a } => {
                assert_eq!(b, 1.0);
                assert!((a - 0.95).abs() < 1e-9, "reports the held-out means, not the mixed ones");
            }
            other => panic!("expected a held-out regression, got {other:?}"),
        }
    }

    #[test]
    fn settle_fence_is_unchanged_when_no_case_is_held_out() {
        let bands = CaseBands::from_cases_only(0.05);
        let before = vec![cs("s/a", 1.0), cs("s/b", 1.0), cs("s/c", 1.0), cs("s/d", 1.0)];
        // A single failure out of four = 0.25 drop ⇒ regression, reported with
        // the mixed means exactly as before WP-4E.
        let worse = vec![cs("s/a", 1.0), cs("s/b", 1.0), cs("s/c", 1.0), cs("s/d", 0.0)];
        let bv = MeasureVector { cases: before.clone(), ..Default::default() };
        assert_eq!(
            suite_verdict(&bv, &MeasureVector { cases: worse, ..Default::default() }, bands),
            SuiteVerdict::Regressed { before: 1.0, after: 0.75 }
        );
        // Unchanged ⇒ Pass, still carrying the mixed means.
        assert_eq!(
            suite_verdict(&bv, &bv, bands),
            SuiteVerdict::Pass { before: 1.0, after: 1.0 }
        );
        // An improvement is never a regression.
        let poor = MeasureVector {
            cases: vec![cs("s/a", 0.0), cs("s/b", 0.0), cs("s/c", 0.0), cs("s/d", 0.0)],
            ..Default::default()
        };
        assert!(matches!(suite_verdict(&poor, &bv, bands), SuiteVerdict::Pass { .. }));
    }

    #[test]
    fn settle_fence_skips_a_dimension_that_exists_on_one_side_only() {
        let bands = CaseBands::from_cases_only(0.05);
        // `before` has no held-out case (the bootstrap-champion shape), so the
        // held-out fence has no baseline and is skipped rather than invented.
        let before = MeasureVector {
            cases: vec![cs("s/a", 1.0), cs("s/b", 1.0)],
            ..Default::default()
        };
        let after = MeasureVector {
            cases: vec![cs("s/a", 1.0), cs("s/b", 1.0), hs("s/_holdout/h", 1.0)],
            ..Default::default()
        };
        assert!(matches!(suite_verdict(&before, &after, bands), SuiteVerdict::Pass { .. }));
        // …and an empty side is still Insufficient, not a silent pass.
        assert_eq!(
            suite_verdict(&MeasureVector::default(), &after, bands),
            SuiteVerdict::Insufficient
        );
    }

    #[test]
    fn settle_bands_come_from_the_commit_gate_not_a_second_policy() {
        // An explicit operator override, as `NoiseBand::from_agent_dir` would
        // have produced it.
        let band = NoiseBand { cases: 0.08, holdout: 0.01, ..NoiseBand::default() };
        let from_gate = CaseBands::from_noise_band(&band);
        assert_eq!(from_gate.cases, 0.08);
        assert_eq!(from_gate.holdout, 0.01, "an explicit override survives to settlement");
        // A legacy row without the column derives, it does not guess.
        assert_eq!(
            CaseBands::from_cases_only(0.08),
            CaseBands { cases: 0.08, holdout: 0.04 }
        );
    }

    #[test]
    fn eval_settlement_never_writes_rule_stats() {
        // The double-counting boundary, asserted structurally: every delta a
        // settlement can emit is Record/Retire, and neither carries a
        // helpful/harmful mutation. `rule_stats` stays exclusively
        // `rule_lifecycle::settle_injected_rules`' ledger.
        let snap = PlaybookSnapshot::new(vec![entry("a", &["s/a"], 0), entry("b", &["s/b"], 0)]);
        let obs = observe_entries(
            &snap,
            &[cs("s/a", 1.0), cs("s/b", 1.0)],
            &[cs("s/a", 1.0), cs("s/b", 0.0)],
            0.05,
        );
        for d in settlement_deltas(&obs) {
            assert!(
                matches!(d, PlaybookDelta::Record { .. } | PlaybookDelta::Retire { .. }),
                "settlement emitted an unexpected op: {d:?}"
            );
        }
    }

    #[test]
    fn streaks_increment_on_confirm_and_reset_on_rollback() {
        let mut snap = PlaybookSnapshot::new(vec![entry("good", &["s/g"], 4), entry("bad", &["s/b"], 4)]);
        let obs = observe_entries(
            &snap,
            &[cs("s/g", 1.0), cs("s/b", 1.0)],
            &[cs("s/g", 1.0), cs("s/b", 0.0)],
            0.05,
        );
        apply_streaks(&mut snap, &obs);
        assert_eq!(snap.get("good").unwrap().meta.success_streak, 5);
        assert_eq!(snap.get("bad").unwrap().meta.success_streak, 0);
    }
}
