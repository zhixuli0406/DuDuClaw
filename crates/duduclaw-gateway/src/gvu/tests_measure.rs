//! WP2.4 §2.6 — Measure vector, commit verdict and champion tests.
//!
//! The unit-level dimension arithmetic is covered inside
//! `verifier_measure.rs` itself; this file covers the parts that only make
//! sense end-to-end — a candidate travelling through champion storage, and
//! the reward-hacking scenarios §2.7 calls out by name.

use chrono::Utc;

use crate::gvu::champion::{Champion, ChampionStore, snapshot_hash};
use crate::gvu::verifier::JudgeResult;
use crate::gvu::verifier_measure::{
    AntiDriftState, CaseScore, CommitVerdict, MeasureInput, MeasureScorer, MeasureVector,
    NoiseBand, NullScorer, ScoreRequest, anti_drift, commit_verdict, measure,
};

fn case(name: &str, score: f64, held_out: bool) -> CaseScore {
    CaseScore {
        case: name.to_string(),
        score,
        held_out,
    }
}

fn vector(judge: Option<f64>, cases: Vec<CaseScore>) -> MeasureVector {
    MeasureVector {
        cases,
        judge,
        anti_sycophancy: 1.0,
        novelty: 1.0,
        relevance: 1.0,
        hard_zero: false,
        causal_support: None,
    }
}

// ── X1 方案 2: causal_support is telemetry, never a gate ──────────────

#[tokio::test]
async fn causal_support_is_none_when_the_agent_has_no_accepted_claim() {
    let input = MeasureInput {
        agent_id: "a".into(),
        contents: vec!["some rule".into()],
        signals_match: vec!["tool_error:db_select".into()],
        accepted_causal_causes: Vec::new(),
        ..Default::default()
    };
    let out = measure(&input, &NullScorer, &ScoreRequest::default()).await;
    assert_eq!(
        out.vector.causal_support, None,
        "no accepted claim is NOT MEASURED, never a measured zero"
    );
}

#[tokio::test]
async fn causal_support_is_measured_when_an_accepted_claim_matches() {
    let input = MeasureInput {
        agent_id: "a".into(),
        contents: vec!["some rule".into()],
        signals_match: vec!["tool_error:db_select".into(), "mistake:factual".into()],
        accepted_causal_causes: vec!["tool_error:db_select".into()],
        ..Default::default()
    };
    let out = measure(&input, &NullScorer, &ScoreRequest::default()).await;
    assert_eq!(out.vector.causal_support, Some(0.5));
}

#[test]
fn causal_support_never_moves_the_commit_verdict() {
    // Two vectors identical on every gated dimension, opposite on
    // `causal_support`: the verdict must not notice.
    let mut champion = vector(Some(0.8), vec![case("s/a", 1.0, false)]);
    champion.causal_support = Some(1.0);
    let mut candidate = vector(Some(0.8), vec![case("s/a", 1.0, false)]);
    candidate.causal_support = Some(0.0);
    let band = NoiseBand::default();
    let with_support = commit_verdict(&candidate, Some(&champion), &band);
    candidate.causal_support = Some(1.0);
    champion.causal_support = None;
    let without_support = commit_verdict(&candidate, Some(&champion), &band);
    assert_eq!(
        format!("{with_support:?}"),
        format!("{without_support:?}"),
        "the commit gate must be blind to causal_support in both directions"
    );
}

/// A scorer that returns a fixed table — stands in for the eval bridge.
struct FixedScorer(Vec<CaseScore>);

#[async_trait::async_trait]
impl MeasureScorer for FixedScorer {
    async fn score(&self, _r: &ScoreRequest) -> Result<Option<Vec<CaseScore>>, String> {
        Ok(Some(self.0.clone()))
    }
    fn name(&self) -> &'static str {
        "fixed"
    }
}

#[tokio::test]
async fn measure_composes_every_dimension_from_one_pass() {
    let input = MeasureInput {
        agent_id: "a".into(),
        contents: vec![
            "\u{56DE}\u{8986}\u{524D}\u{5148}\u{78BA}\u{8A8D}\u{6536}\u{4EF6}\u{5730}\u{5740}"
                .into(),
        ],
        current_reference: String::new(),
        rolled_back_summaries: vec!["something completely unrelated".into()],
        mistake_descriptions: Vec::new(),
        judge: Some(JudgeResult {
            approved: true,
            score: 0.82,
            feedback: String::new(),
        }),
        post_hoc_must_not_hit: false,
        signals_match: Vec::new(),
        accepted_causal_causes: Vec::new(),
    };
    let scorer = FixedScorer(vec![case("s/a", 1.0, false), case("s/b", 0.0, false)]);
    let out = measure(&input, &scorer, &ScoreRequest::default()).await;
    assert!(out.case_dimension_available);
    assert_eq!(out.vector.cases_mean(), Some(0.5));
    assert_eq!(out.vector.judge, Some(0.82));
    assert_eq!(out.vector.anti_sycophancy, 1.0);
    assert!(out.vector.novelty > 0.8);
    assert_eq!(out.vector.relevance, 0.5, "no mistakes to be relevant to");
    assert!(!out.vector.hard_zero);
}

#[tokio::test]
async fn post_hoc_must_not_hit_zeroes_everything() {
    let input = MeasureInput {
        agent_id: "a".into(),
        contents: vec!["fine looking text".into()],
        judge: Some(JudgeResult {
            approved: true,
            score: 0.99,
            feedback: String::new(),
        }),
        post_hoc_must_not_hit: true,
        ..Default::default()
    };
    let out = measure(&input, &NullScorer, &ScoreRequest::default()).await;
    assert!(out.vector.hard_zero);
    assert_eq!(out.vector.headline(), 0.0);
    assert_eq!(
        out.vector.judge, None,
        "a hard zero is not a judge score of 0.99"
    );
}

#[test]
fn entry_passing_own_cases_but_regressing_suite_is_not_committed() {
    // §1.11's reward-hacking row, at the commit gate: the entry's own case
    // improves while the rest of the suite falls apart.
    let band = NoiseBand::default();
    let champion = vector(
        Some(0.7),
        vec![
            case("s/own", 0.0, false),
            case("s/x", 1.0, false),
            case("s/y", 1.0, false),
        ],
    );
    let candidate = vector(
        Some(0.7),
        vec![
            case("s/own", 1.0, false),
            case("s/x", 0.0, false),
            case("s/y", 0.0, false),
        ],
    );
    // Own case 0.0 → 1.0, but the suite mean fell 0.667 → 0.333.
    assert!(matches!(
        commit_verdict(&candidate, Some(&champion), &band),
        CommitVerdict::Regresses { ref dimension, .. } if dimension == "cases"
    ));
}

#[test]
fn held_out_cases_count_toward_the_commit_gate() {
    let band = NoiseBand::default();
    let champion = vector(
        Some(0.7),
        vec![case("s/a", 1.0, false), case("s/_holdout/h", 1.0, true)],
    );
    let candidate = vector(
        Some(0.7),
        vec![case("s/a", 1.0, false), case("s/_holdout/h", 0.0, true)],
    );
    assert!(matches!(
        commit_verdict(&candidate, Some(&champion), &band),
        CommitVerdict::Regresses { .. }
    ));
}

#[test]
fn bootstrap_installs_the_first_champion_then_compares_against_it() {
    let f = tempfile::NamedTempFile::new().unwrap();
    let store = ChampionStore::new(f.path());
    let band = NoiseBand::default();

    // No champion → bootstrap.
    assert!(store.get("a").is_none());
    let first = vector(Some(0.6), vec![case("s/a", 1.0, false)]);
    assert_eq!(commit_verdict(&first, None, &band), CommitVerdict::Improves);

    store
        .put(&Champion {
            agent_id: "a".into(),
            snapshot_hash: snapshot_hash(&["k1".into()]),
            measure: first.clone(),
            established_at: Utc::now(),
            anti_drift: AntiDriftState::default(),
            round_seq: 1,
            holdout_rotation_due: false,
        })
        .unwrap();

    // A worse candidate now regresses instead of sailing through.
    let worse = vector(Some(0.6), vec![case("s/a", 0.0, false)]);
    let reigning = store.get("a").unwrap();
    assert!(matches!(
        commit_verdict(&worse, Some(&reigning.measure), &band),
        CommitVerdict::Regresses { .. }
    ));
}

#[test]
fn tie_commits_accumulate_until_only_improves_is_accepted() {
    let f = tempfile::NamedTempFile::new().unwrap();
    let store = ChampionStore::new(f.path());
    let mut champ = Champion {
        agent_id: "a".into(),
        snapshot_hash: snapshot_hash(&[]),
        measure: vector(Some(0.7), vec![]),
        established_at: Utc::now(),
        anti_drift: AntiDriftState::default(),
        round_seq: 0,
        holdout_rotation_due: false,
    };

    for _ in 0..3 {
        let d = anti_drift(&CommitVerdict::Matches, champ.anti_drift);
        assert!(d.commit);
        assert!(
            d.force_observation_window,
            "a tie must never skip the real-traffic window"
        );
        champ.anti_drift.consecutive_matches = d.next_consecutive_matches;
        champ.holdout_rotation_due = d.holdout_rotation_due;
        store.put(&champ).unwrap();
    }

    let reloaded = store.get("a").unwrap();
    assert_eq!(reloaded.anti_drift.consecutive_matches, 3);
    assert!(
        reloaded.holdout_rotation_due,
        "rotation is flagged, not performed"
    );
    let blocked = anti_drift(&CommitVerdict::Matches, reloaded.anti_drift);
    assert!(!blocked.commit);
}

#[test]
fn judge_call_failure_yields_none_not_zero_through_the_gate() {
    let band = NoiseBand::default();
    let champion = vector(Some(0.9), vec![case("s/a", 1.0, false)]);
    // Judge failed → dimension absent. Cases identical → tie, not regression.
    let candidate = vector(None, vec![case("s/a", 1.0, false)]);
    assert_eq!(
        commit_verdict(&candidate, Some(&champion), &band),
        CommitVerdict::Matches
    );
}

#[test]
fn a_candidate_with_nothing_measurable_is_invalid_not_committable() {
    let band = NoiseBand::default();
    let empty = MeasureVector {
        judge: None,
        ..Default::default()
    };
    // Deterministic dimensions always exist, so `empty` is comparable to a
    // champion — but against NO champion it bootstraps, and a hard zero is
    // always invalid.
    assert_eq!(commit_verdict(&empty, None, &band), CommitVerdict::Improves);
    assert!(matches!(
        commit_verdict(&MeasureVector::zeroed(), None, &band),
        CommitVerdict::Invalid { .. }
    ));
}
