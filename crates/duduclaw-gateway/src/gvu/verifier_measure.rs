//! WP2.4 — the **Measure** half of the Gate/Measure split
//! (`commercial/docs/DESIGN-evolution-v3-aee.md` §2.3 / §2.4).
//!
//! A Measure dimension scores a candidate. **No dimension has a veto.** The
//! only thing that can zero a whole vector is a CONTRACT `must_not` hit
//! observed *after* the Gates already passed (e.g. an eval case's actual
//! answer trips a forbidden pattern) — see [`MeasureVector::zeroed`].
//!
//! Dimensions (§2.3):
//! - `cases` — one deterministic pass/fail per eval case actually run
//! - `judge` — the former L3 LLM judge, now `Option<f64>`; **a failed judge
//!   call is `None`, never `0.0`** (infrastructure failure must not
//!   masquerade as a quality regression)
//! - `anti_sycophancy` — the sycophancy-pattern half of the old L3.5
//! - `novelty` — the old L2 rolled-back-similarity veto, inverted into a score
//! - `relevance` — the old L2.5 mistake-regression advisory
//!
//! The commit gate ([`commit_verdict`]) compares a candidate against the
//! reigning champion **dimension by dimension** with a per-dimension noise
//! band, implementing AVO P7 "matches or improves". `Matches` is committable
//! by design (otherwise evolution stalls in a local optimum), which is why
//! §2.4.3's three anti-drift companions exist — two of them live here
//! ([`NoiseBand`] calibration + [`AntiDriftState`]) and the third
//! (observation window) is enforced by the caller.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::verifier::JudgeResult;

/// One eval case's deterministic outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseScore {
    /// `EvalCaseRef` string (`"<suite>/<case-name>"`).
    pub case: String,
    /// 1.0 pass / 0.0 fail. Deterministic assertions only — the eval suite's
    /// own LLM judge feeds [`MeasureVector::judge`], never this field.
    pub score: f64,
    /// Held-out cases are scored but MUST NOT appear in any Generator
    /// context (§3.5).
    pub held_out: bool,
}

/// A candidate's measured quality.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MeasureVector {
    /// One entry per eval case actually run.
    #[serde(default)]
    pub cases: Vec<CaseScore>,
    /// L3 LLM judge, 0.0-1.0. `None` when the judge call failed or was not
    /// attempted — treated as "not comparable", never as zero.
    #[serde(default)]
    pub judge: Option<f64>,
    /// 1.0 clean, 0.0 when a sycophancy pattern is newly introduced.
    /// Deterministic, zero cost.
    ///
    /// `serde(default)` on all three deterministic dimensions so a stored
    /// vector from an older (or empty) row still deserializes — a champion
    /// row that refuses to parse degrades the agent to "no champion", which
    /// is a silent loss of the anti-drift counters.
    #[serde(default)]
    pub anti_sycophancy: f64,
    /// `1.0 - overlap` against previously rolled-back versions. Low means
    /// "very similar to something that already failed".
    #[serde(default)]
    pub novelty: f64,
    /// Does the candidate address a known MistakeNotebook entry?
    #[serde(default)]
    pub relevance: f64,
    /// TRUE when a CONTRACT `must_not` was hit — the whole vector is zeroed.
    #[serde(default)]
    pub hard_zero: bool,
}

impl MeasureVector {
    /// CONTRACT `must_not` hit ⇒ every dimension is 0. Not "low score" —
    /// zero, and flagged, so nothing downstream can average it away.
    pub fn zeroed() -> Self {
        Self { hard_zero: true, ..Default::default() }
    }

    /// Mean of the case dimension. `None` when no case ran.
    pub fn cases_mean(&self) -> Option<f64> {
        if self.cases.is_empty() {
            return None;
        }
        Some(self.cases.iter().map(|c| c.score).sum::<f64>() / self.cases.len() as f64)
    }

    /// Mean of the non-held-out case dimension (what the inner loop is
    /// allowed to see).
    pub fn visible_cases_mean(&self) -> Option<f64> {
        let visible: Vec<&CaseScore> = self.cases.iter().filter(|c| !c.held_out).collect();
        if visible.is_empty() {
            return None;
        }
        Some(visible.iter().map(|c| c.score).sum::<f64>() / visible.len() as f64)
    }

    /// Aggregate used ONLY for logging / dashboard. The commit gate compares
    /// dimension-by-dimension (§2.4) and never reads this scalar.
    pub fn headline(&self) -> f64 {
        if self.hard_zero {
            return 0.0;
        }
        let mut present: Vec<f64> = vec![self.anti_sycophancy, self.novelty, self.relevance];
        if let Some(m) = self.cases_mean() {
            present.push(m);
        }
        if let Some(j) = self.judge {
            present.push(j);
        }
        present.iter().sum::<f64>() / present.len() as f64
    }

    /// Per-case scores keyed by case ref — the WP2.5 entry-level verdict
    /// needs to look a specific entry's linked cases up.
    pub fn case_map(&self) -> BTreeMap<&str, f64> {
        self.cases.iter().map(|c| (c.case.as_str(), c.score)).collect()
    }
}

// ---------------------------------------------------------------------------
// Scorer seam (eval bridge lives behind this trait)
// ---------------------------------------------------------------------------

/// One scoring request: which cases to run, and whether held-out cases are
/// in scope.
#[derive(Debug, Clone, Default)]
pub struct ScoreRequest {
    pub agent_id: String,
    /// Empty = whole suite. Otherwise exact `EvalCaseRef` strings.
    pub cases: Vec<String>,
    /// `false` for every inner-loop call (held-out must never influence, or
    /// even become visible to, the Generator).
    pub include_holdout: bool,
}

/// The narrow seam through which AEE gets deterministic eval scores.
///
/// Deliberately **not** coupled to any concrete eval runner: `duduclaw-cli`
/// depends on `duduclaw-gateway` and not the other way round (design appendix
/// A / B1), so the concrete adapter — a subprocess bridge over
/// `duduclaw eval … --report` — is wired separately. Everything in this
/// module compiles and is testable against [`NullScorer`] alone.
#[async_trait::async_trait]
pub trait MeasureScorer: Send + Sync {
    /// Score the requested cases. `Ok(None)` means "no score available"
    /// (no suite, no transcript, scorer intentionally inert) and MUST be
    /// treated as an absent dimension, not as failure. `Err` is an
    /// infrastructure failure and is also treated as an absent dimension by
    /// [`measure`] — but it is logged, never silently swallowed.
    async fn score(&self, req: &ScoreRequest) -> Result<Option<Vec<CaseScore>>, String>;

    /// Human-readable name for telemetry / degradation warnings.
    fn name(&self) -> &'static str {
        "unnamed"
    }
}

/// The default scorer: always "no score".
///
/// With `NullScorer` attached, Measure degrades to judge-only and the commit
/// gate compares only the dimensions that exist. That degradation is explicit
/// and observable ([`MeasureOutcome::case_dimension_available`]), never
/// silent.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullScorer;

#[async_trait::async_trait]
impl MeasureScorer for NullScorer {
    async fn score(&self, _req: &ScoreRequest) -> Result<Option<Vec<CaseScore>>, String> {
        Ok(None)
    }
    fn name(&self) -> &'static str {
        "null"
    }
}

// ---------------------------------------------------------------------------
// Measure computation
// ---------------------------------------------------------------------------

/// Sycophancy indicators, scored rather than vetoed (§2.1 row 7 first half).
const SYCOPHANCY_INDICATORS: &[&str] = &[
    "always agree",
    "never disagree",
    "avoid conflict",
    "prioritize harmony",
    "don't challenge",
    "validate the user",
    "match user expectations",
    "avoid correcting",
    "never contradict",
    // zh-TW
    "\u{6C38}\u{9060}\u{540C}\u{610F}", // 永遠同意
    "\u{4E0D}\u{8981}\u{53CD}\u{99C1}", // 不要反駁
    "\u{907F}\u{514D}\u{885D}\u{7A81}", // 避免衝突
    "\u{4E0D}\u{8981}\u{7CFE}\u{6B63}", // 不要糾正
];

/// 1.0 clean / 0.0 when the candidate NEWLY introduces a sycophancy pattern
/// (already-present patterns are the operator's business, not the
/// candidate's fault).
pub fn anti_sycophancy_score(content: &str, current_reference: &str) -> f64 {
    let lower = content.to_lowercase();
    let lower_ref = current_reference.to_lowercase();
    for pattern in SYCOPHANCY_INDICATORS {
        let p = pattern.to_lowercase();
        if lower.contains(&p) && !lower_ref.contains(&p) {
            return 0.0;
        }
    }
    1.0
}

/// `1.0 - max overlap against any rolled-back version`. This is the old L2
/// veto turned into a score: "looks a lot like something that already failed"
/// now *lowers* a dimension instead of destroying the candidate on a 0.5
/// Jaccard threshold that has no empirical backing (§2.1 row 4).
pub fn novelty_score(content: &str, rolled_back_summaries: &[String]) -> f64 {
    let mut worst: f64 = 0.0;
    for s in rolled_back_summaries {
        worst = worst.max(super::verifier::keyword_overlap_pub(content, s));
    }
    (1.0 - worst).clamp(0.0, 1.0)
}

/// 1.0 when the candidate visibly addresses at least one unresolved mistake,
/// 0.5 when there were no mistakes to address (nothing to be relevant *to* —
/// scoring 0 would punish an agent for having a clean notebook), else 0.0.
pub fn relevance_score(content: &str, mistake_descriptions: &[String]) -> f64 {
    if mistake_descriptions.is_empty() {
        return 0.5;
    }
    let lower = content.to_lowercase();
    let stop_words = [
        "that", "this", "with", "from", "have", "should", "would", "could", "been", "being",
        "about", "their", "there", "which", "where", "when", "than", "then", "them", "they",
        "does", "will",
    ];
    let addresses_any = mistake_descriptions.iter().any(|m| {
        let keywords: Vec<&str> = m
            .split_whitespace()
            .filter(|w| w.chars().count() > 4)
            .filter(|w| !stop_words.contains(&w.to_lowercase().as_str()))
            .collect();
        let hits = keywords.iter().filter(|kw| lower.contains(&kw.to_lowercase())).count();
        if hits >= 2 {
            return true;
        }
        if !keywords.is_empty() && keywords.len() <= 2 && hits >= 1 {
            return true;
        }
        // CJK path: mistake descriptions are usually zh-TW, which
        // `split_whitespace` cannot tokenize at all. Fall back to bigram
        // overlap so the dimension is not silently always-zero for zh-TW
        // agents.
        super::verifier::keyword_overlap_pub(&lower, &m.to_lowercase()) > 0.15
    });
    if addresses_any {
        1.0
    } else {
        0.0
    }
}

/// Everything [`measure`] needs. Owned strings so the caller can assemble it
/// from short-lived borrows.
#[derive(Debug, Clone, Default)]
pub struct MeasureInput {
    pub agent_id: String,
    /// The texts that will take effect (same set the Gates cleared).
    pub contents: Vec<String>,
    pub current_reference: String,
    /// `soul_summary` of every version currently marked rolled-back.
    pub rolled_back_summaries: Vec<String>,
    /// `what_went_wrong` of the unresolved mistakes in scope.
    pub mistake_descriptions: Vec<String>,
    /// Judge outcome. `None` = not attempted or the call failed.
    pub judge: Option<JudgeResult>,
    /// Post-shadow `must_not` hit observed downstream of the Gates.
    pub post_hoc_must_not_hit: bool,
}

/// Result of measuring, plus the honest degradation flags.
#[derive(Debug, Clone)]
pub struct MeasureOutcome {
    pub vector: MeasureVector,
    /// FALSE when the scorer returned "no score" — the `cases` dimension is
    /// absent, not zero. Surfaced so the caller can `warn!` rather than
    /// silently pretend the candidate was evaluated against a suite.
    pub case_dimension_available: bool,
    /// Non-fatal scorer error text, if any.
    pub scorer_error: Option<String>,
}

/// Compute the Measure vector. Never panics, never vetoes.
pub async fn measure(
    input: &MeasureInput,
    scorer: &dyn MeasureScorer,
    req: &ScoreRequest,
) -> MeasureOutcome {
    if input.post_hoc_must_not_hit {
        return MeasureOutcome {
            vector: MeasureVector::zeroed(),
            case_dimension_available: false,
            scorer_error: None,
        };
    }

    let joined = input.contents.join("\n");

    let (cases, available, scorer_error) = match scorer.score(req).await {
        Ok(Some(c)) => {
            let has = !c.is_empty();
            (c, has, None)
        }
        Ok(None) => (Vec::new(), false, None),
        Err(e) => {
            tracing::warn!(
                agent = %input.agent_id,
                scorer = scorer.name(),
                "AEE measure: scorer failed — case dimension treated as ABSENT (not zero): {e}"
            );
            (Vec::new(), false, Some(e))
        }
    };

    let vector = MeasureVector {
        cases,
        judge: input.judge.as_ref().map(|j| j.score.clamp(0.0, 1.0)),
        anti_sycophancy: anti_sycophancy_score(&joined, &input.current_reference),
        novelty: novelty_score(&joined, &input.rolled_back_summaries),
        relevance: relevance_score(&joined, &input.mistake_descriptions),
        hard_zero: false,
    };

    MeasureOutcome { vector, case_dimension_available: available, scorer_error }
}

// ---------------------------------------------------------------------------
// Commit gate — matches-or-improves (§2.4)
// ---------------------------------------------------------------------------

/// Per-dimension noise band (§2.4.2).
///
/// **The values below are the design's starting suggestions and are
/// explicitly marked "待實測校準"** — they must be replaced by `2σ` measured
/// from 20 repeat runs of the same candidate on one production agent before
/// anyone treats them as tuned. The config surface exists precisely so that
/// calibration does not require a code change.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NoiseBand {
    /// Deterministic eval assertions are near-reproducible; the band only
    /// absorbs tool-ordering nondeterminism. Hard ceiling
    /// [`NOISE_BAND_CASES_MAX`]: a wider band means the *cases* are
    /// nondeterministic, and the thing to fix is the case, not the band.
    pub cases: f64,
    /// An LLM judge on identical input varies materially run to run.
    pub judge: f64,
    /// Fully deterministic — zero band.
    pub anti_sycophancy: f64,
    pub novelty: f64,
    pub relevance: f64,
}

/// §2.4.2 rule 4: a `cases` band above this means the eval cases themselves
/// are unstable. Enforced by [`NoiseBand::from_agent_dir`] on read.
pub const NOISE_BAND_CASES_MAX: f64 = 0.10;

impl Default for NoiseBand {
    fn default() -> Self {
        // Design §2.4.2 suggested starting values — PENDING EMPIRICAL
        // CALIBRATION (2σ over 20 repeats). Do not cite these as tuned.
        Self { cases: 0.05, judge: 0.15, anti_sycophancy: 0.0, novelty: 0.05, relevance: 0.10 }
    }
}

impl NoiseBand {
    /// Read `[evolution.noise_band]` from an agent's `agent.toml`.
    ///
    /// Missing file / malformed TOML / absent table → [`Default`]. Every key
    /// is individually optional. `cases` is clamped to
    /// [`NOISE_BAND_CASES_MAX`].
    ///
    /// Goes through the shared typed parse point
    /// ([`duduclaw_core::agent_toml`]). The fields use `opt_float_strict`, so
    /// an **integer literal is still ignored** (`cases = 1` keeps the default)
    /// exactly as `as_float()` did — see
    /// `default_direction_noise_band_ignores_integer_literal`. Clamping stays
    /// here because it is a policy of the gate, not of the file format.
    pub fn from_agent_dir(agent_dir: &std::path::Path) -> Self {
        let mut band = Self::default();
        let raw = duduclaw_core::agent_toml::load(agent_dir)
            .evolution
            .noise_band;
        if let Some(v) = raw.cases {
            band.cases = v.clamp(0.0, NOISE_BAND_CASES_MAX);
        }
        if let Some(v) = raw.judge {
            band.judge = v.max(0.0);
        }
        if let Some(v) = raw.anti_sycophancy {
            band.anti_sycophancy = v.max(0.0);
        }
        if let Some(v) = raw.novelty {
            band.novelty = v.max(0.0);
        }
        if let Some(v) = raw.relevance {
            band.relevance = v.max(0.0);
        }
        band
    }

    fn for_dimension(&self, dim: &str) -> f64 {
        match dim {
            "cases" => self.cases,
            "judge" => self.judge,
            "anti_sycophancy" => self.anti_sycophancy,
            "novelty" => self.novelty,
            "relevance" => self.relevance,
            _ => 0.0,
        }
    }
}

/// Outcome of comparing a candidate to the reigning champion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum CommitVerdict {
    /// Every comparable dimension is `>= champion - band`, and at least one
    /// is `> champion + band`.
    Improves,
    /// Every comparable dimension is within `± band`. Committable (AVO P7) —
    /// with the §2.4.3 anti-drift companions attached.
    Matches,
    /// At least one dimension is `< champion - band`.
    Regresses { dimension: String, champion: f64, candidate: f64 },
    /// `hard_zero`, or no dimension was comparable at all.
    Invalid { reason: String },
}

impl CommitVerdict {
    pub fn is_committable(&self) -> bool {
        matches!(self, Self::Improves | Self::Matches)
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Improves => "improves",
            Self::Matches => "matches",
            Self::Regresses { .. } => "regresses",
            Self::Invalid { .. } => "invalid",
        }
    }
}

/// Extract the comparable scalar dimensions of a vector.
///
/// `judge = None` is simply absent — a dimension present on only one side is
/// not comparable and is skipped, never scored as 0.
fn dimensions(v: &MeasureVector) -> BTreeMap<&'static str, f64> {
    let mut m = BTreeMap::new();
    if let Some(c) = v.cases_mean() {
        m.insert("cases", c);
    }
    if let Some(j) = v.judge {
        m.insert("judge", j);
    }
    m.insert("anti_sycophancy", v.anti_sycophancy);
    m.insert("novelty", v.novelty);
    m.insert("relevance", v.relevance);
    m
}

/// Dimension-by-dimension "matches or improves" (§2.4).
///
/// `champion = None` is the bootstrap case: any non-`Invalid` candidate
/// becomes the first champion.
pub fn commit_verdict(
    candidate: &MeasureVector,
    champion: Option<&MeasureVector>,
    band: &NoiseBand,
) -> CommitVerdict {
    if candidate.hard_zero {
        return CommitVerdict::Invalid {
            reason: "candidate hit a CONTRACT must_not after the gates (hard zero)".to_string(),
        };
    }
    let cand = dimensions(candidate);
    if cand.is_empty() {
        return CommitVerdict::Invalid { reason: "no comparable dimension".to_string() };
    }
    let Some(champ_vec) = champion else {
        // Bootstrap: nothing to beat yet.
        return CommitVerdict::Improves;
    };
    if champ_vec.hard_zero {
        return CommitVerdict::Improves;
    }
    let champ = dimensions(champ_vec);

    let mut comparable = 0usize;
    let mut any_better = false;
    for (dim, c_val) in &cand {
        let Some(h_val) = champ.get(dim) else {
            continue; // present on one side only → not comparable
        };
        comparable += 1;
        let b = band.for_dimension(dim);
        if *c_val < h_val - b {
            return CommitVerdict::Regresses {
                dimension: (*dim).to_string(),
                champion: *h_val,
                candidate: *c_val,
            };
        }
        if *c_val > h_val + b {
            any_better = true;
        }
    }

    if comparable == 0 {
        return CommitVerdict::Invalid {
            reason: "champion and candidate share no comparable dimension".to_string(),
        };
    }
    if any_better {
        CommitVerdict::Improves
    } else {
        CommitVerdict::Matches
    }
}

// ---------------------------------------------------------------------------
// §2.4.3 anti-drift companion 1: consecutive-`Matches` ceiling
// ---------------------------------------------------------------------------

/// After [`MAX_CONSECUTIVE_MATCHES`] tie-commits in a row, only `Improves` is
/// accepted. Reset to 0 by any `Improves`.
pub const MAX_CONSECUTIVE_MATCHES: u32 = 3;

/// The persisted anti-drift counters that travel with the champion row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct AntiDriftState {
    pub consecutive_matches: u32,
}

/// What the caller must do once a verdict is in — the three §2.4.3
/// companions, resolved deterministically.
#[derive(Debug, Clone, PartialEq)]
pub struct AntiDriftDecision {
    /// May the candidate be committed at all?
    pub commit: bool,
    /// New value for `consecutive_matches`.
    pub next_consecutive_matches: u32,
    /// Companion 2: a tie-commit must NOT skip the real-traffic observation
    /// window.
    pub force_observation_window: bool,
    /// Companion 3: a tie-commit signals that the held-out subset should be
    /// rotated. Rotation itself is an operator CLI action — AEE only raises
    /// the flag (letting the examinee shuffle its own exam is the failure
    /// mode this avoids).
    pub holdout_rotation_due: bool,
    /// Human-readable reason when `commit == false`.
    pub blocked_reason: Option<String>,
}

/// Apply the three anti-drift companions to a verdict.
pub fn anti_drift(verdict: &CommitVerdict, state: AntiDriftState) -> AntiDriftDecision {
    match verdict {
        CommitVerdict::Improves => AntiDriftDecision {
            commit: true,
            next_consecutive_matches: 0,
            force_observation_window: false,
            holdout_rotation_due: false,
            blocked_reason: None,
        },
        CommitVerdict::Matches => {
            if state.consecutive_matches >= MAX_CONSECUTIVE_MATCHES {
                AntiDriftDecision {
                    commit: false,
                    next_consecutive_matches: state.consecutive_matches,
                    force_observation_window: false,
                    holdout_rotation_due: true,
                    blocked_reason: Some(format!(
                        "{} consecutive tie-commits without an improvement — only Improves is accepted now",
                        state.consecutive_matches
                    )),
                }
            } else {
                AntiDriftDecision {
                    commit: true,
                    next_consecutive_matches: state.consecutive_matches + 1,
                    force_observation_window: true,
                    holdout_rotation_due: true,
                    blocked_reason: None,
                }
            }
        }
        CommitVerdict::Regresses { dimension, champion, candidate } => AntiDriftDecision {
            commit: false,
            next_consecutive_matches: state.consecutive_matches,
            force_observation_window: false,
            holdout_rotation_due: false,
            blocked_reason: Some(format!(
                "dimension '{dimension}' regressed: champion {champion:.3} vs candidate {candidate:.3}"
            )),
        },
        CommitVerdict::Invalid { reason } => AntiDriftDecision {
            commit: false,
            next_consecutive_matches: state.consecutive_matches,
            force_observation_window: false,
            holdout_rotation_due: false,
            blocked_reason: Some(reason.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── R5: `[evolution.noise_band]` direction, pinned ───────────────────
    //
    // absent / malformed / wrong-typed ⇒ the `Default` band. Each key is
    // independently optional, and — the quirk worth pinning — the raw reader
    // used `as_float()` ONLY, so an INTEGER literal (`cases = 1`) was
    // silently ignored and kept the default. Widening it would loosen a
    // commit gate as a side effect of a schema refactor, so the typed field
    // uses `opt_float_strict`, not `opt_number_lossy`. Note `[evolution]
    // aee_settle_hours` in the same section deliberately points the OTHER
    // way — it does accept an integer.

    fn band_dir(body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), body).unwrap();
        dir
    }

    #[test]
    fn default_direction_noise_band_absent_or_unreadable_is_default() {
        for body in [
            "",
            "[evolution]\n",
            "[evolution.noise_band]\n",
            "evolution = \"scalar\"\n",
            "[evolution]\nnoise_band = \"scalar\"\n",
            "not toml [[[",
        ] {
            let dir = band_dir(body);
            assert_eq!(
                NoiseBand::from_agent_dir(dir.path()),
                NoiseBand::default(),
                "for {body:?}"
            );
        }

        let empty = tempfile::tempdir().unwrap();
        assert_eq!(NoiseBand::from_agent_dir(empty.path()), NoiseBand::default());
    }

    #[test]
    fn default_direction_noise_band_ignores_integer_literal() {
        // Historical quirk, preserved: `as_float()` returned None for an
        // integer, so the default survived.
        let dir = band_dir("[evolution.noise_band]\ncases = 1\njudge = 2\n");
        let band = NoiseBand::from_agent_dir(dir.path());
        assert_eq!(band, NoiseBand::default(), "integer literals stay ignored");

        // A real float is honoured (and clamped).
        let dir = band_dir(
            "[evolution.noise_band]\ncases = 0.02\njudge = -1.0\nnovelty = 0.5\n",
        );
        let band = NoiseBand::from_agent_dir(dir.path());
        assert_eq!(band.cases, 0.02);
        assert_eq!(band.judge, 0.0, "negatives clamp to 0");
        assert_eq!(band.novelty, 0.5);
        assert_eq!(
            band.relevance,
            NoiseBand::default().relevance,
            "unset keys keep their defaults"
        );
    }

    #[test]
    fn default_direction_noise_band_wrong_typed_key_keeps_the_default() {
        let dir = band_dir("[evolution.noise_band]\ncases = \"0.05\"\njudge = true\n");
        assert_eq!(NoiseBand::from_agent_dir(dir.path()), NoiseBand::default());
    }

    #[test]
    fn default_direction_noise_band_cases_is_clamped_to_the_max() {
        let dir = band_dir("[evolution.noise_band]\ncases = 99.0\n");
        assert_eq!(
            NoiseBand::from_agent_dir(dir.path()).cases,
            NOISE_BAND_CASES_MAX
        );
    }

    fn vec_with(judge: Option<f64>, cases: &[(&str, f64)]) -> MeasureVector {
        MeasureVector {
            cases: cases
                .iter()
                .map(|(c, s)| CaseScore { case: (*c).to_string(), score: *s, held_out: false })
                .collect(),
            judge,
            anti_sycophancy: 1.0,
            novelty: 1.0,
            relevance: 1.0,
            hard_zero: false,
        }
    }

    #[test]
    fn judge_failure_yields_none_not_zero() {
        let v = vec_with(None, &[]);
        assert_eq!(v.judge, None);
        // A None judge must not drag the headline down the way a 0.0 would.
        assert!(v.headline() > 0.99);
    }

    #[test]
    fn hard_zero_cannot_be_averaged_away() {
        let z = MeasureVector::zeroed();
        assert_eq!(z.headline(), 0.0);
        assert!(matches!(
            commit_verdict(&z, Some(&vec_with(Some(0.1), &[])), &NoiseBand::default()),
            CommitVerdict::Invalid { .. }
        ));
    }

    #[test]
    fn low_judge_score_alone_does_not_block_but_does_regress() {
        let band = NoiseBand::default();
        let champion = vec_with(Some(0.9), &[]);
        let candidate = vec_with(Some(0.3), &[]);
        // Not Invalid — the judge has no veto.
        let verdict = commit_verdict(&candidate, Some(&champion), &band);
        assert!(matches!(verdict, CommitVerdict::Regresses { ref dimension, .. } if dimension == "judge"));
        // …but with no champion (bootstrap) the same low score commits.
        assert_eq!(commit_verdict(&candidate, None, &band), CommitVerdict::Improves);
    }

    #[test]
    fn within_band_is_matches_and_beyond_band_is_improves() {
        let band = NoiseBand::default();
        let champion = vec_with(Some(0.70), &[]);
        assert_eq!(
            commit_verdict(&vec_with(Some(0.80), &[]), Some(&champion), &band),
            CommitVerdict::Matches,
            "0.10 < judge band 0.15 → a tie"
        );
        assert_eq!(
            commit_verdict(&vec_with(Some(0.95), &[]), Some(&champion), &band),
            CommitVerdict::Improves
        );
    }

    #[test]
    fn candidate_gaming_judge_but_failing_held_out_is_rejected() {
        let band = NoiseBand::default();
        let mut champion = vec_with(Some(0.60), &[("s/a", 1.0)]);
        champion.cases.push(CaseScore { case: "s/_holdout/h".into(), score: 1.0, held_out: true });
        // Candidate flatters the judge (0.95) but breaks the held-out case.
        let mut candidate = vec_with(Some(0.95), &[("s/a", 1.0)]);
        candidate.cases.push(CaseScore { case: "s/_holdout/h".into(), score: 0.0, held_out: true });
        assert!(matches!(
            commit_verdict(&candidate, Some(&champion), &band),
            CommitVerdict::Regresses { ref dimension, .. } if dimension == "cases"
        ));
    }

    #[test]
    fn absent_dimension_on_one_side_is_skipped_not_scored_zero() {
        let band = NoiseBand::default();
        let champion = vec_with(Some(0.9), &[("s/a", 1.0)]);
        // Candidate's judge call failed → judge absent. Cases identical.
        let candidate = vec_with(None, &[("s/a", 1.0)]);
        assert_eq!(commit_verdict(&candidate, Some(&champion), &band), CommitVerdict::Matches);
    }

    #[test]
    fn three_consecutive_matches_then_only_improves_accepted() {
        let mut state = AntiDriftState::default();
        for i in 0..MAX_CONSECUTIVE_MATCHES {
            let d = anti_drift(&CommitVerdict::Matches, state);
            assert!(d.commit, "tie #{i} must still commit");
            assert!(d.force_observation_window);
            assert!(d.holdout_rotation_due);
            state.consecutive_matches = d.next_consecutive_matches;
        }
        let blocked = anti_drift(&CommitVerdict::Matches, state);
        assert!(!blocked.commit);
        assert!(blocked.blocked_reason.is_some());
        // An Improves resets the counter.
        let reset = anti_drift(&CommitVerdict::Improves, state);
        assert!(reset.commit);
        assert_eq!(reset.next_consecutive_matches, 0);
    }

    #[tokio::test]
    async fn null_scorer_degrades_to_judge_only_explicitly() {
        let input = MeasureInput {
            agent_id: "a".into(),
            contents: vec!["be precise".into()],
            judge: Some(JudgeResult { approved: true, score: 0.8, feedback: String::new() }),
            ..Default::default()
        };
        let out = measure(&input, &NullScorer, &ScoreRequest::default()).await;
        assert!(!out.case_dimension_available, "degradation must be observable, not silent");
        assert!(out.vector.cases.is_empty());
        assert_eq!(out.vector.judge, Some(0.8));
        assert!(out.scorer_error.is_none());
    }

    #[tokio::test]
    async fn scorer_error_is_an_absent_dimension_not_a_zero() {
        struct Broken;
        #[async_trait::async_trait]
        impl MeasureScorer for Broken {
            async fn score(&self, _r: &ScoreRequest) -> Result<Option<Vec<CaseScore>>, String> {
                Err("eval binary missing".into())
            }
        }
        let out = measure(&MeasureInput::default(), &Broken, &ScoreRequest::default()).await;
        assert!(out.vector.cases.is_empty());
        assert_eq!(out.vector.cases_mean(), None, "absent, not 0.0");
        assert_eq!(out.scorer_error.as_deref(), Some("eval binary missing"));
    }

    #[test]
    fn noise_band_reads_agent_toml_and_clamps_cases() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[evolution.noise_band]\ncases = 0.9\njudge = 0.2\n",
        )
        .unwrap();
        let band = NoiseBand::from_agent_dir(dir.path());
        assert_eq!(band.cases, NOISE_BAND_CASES_MAX, "an unstable-case band is clamped, not honoured");
        assert!((band.judge - 0.2).abs() < 1e-9);
        // Absent file → defaults.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(NoiseBand::from_agent_dir(empty.path()), NoiseBand::default());
    }

    #[test]
    fn anti_sycophancy_only_penalises_newly_introduced_patterns() {
        assert_eq!(anti_sycophancy_score("always agree with the user", ""), 0.0);
        assert_eq!(
            anti_sycophancy_score("always agree with the user", "the operator wrote: always agree"),
            1.0
        );
    }

    #[test]
    fn novelty_is_low_when_similar_to_a_rolled_back_version() {
        let rolled = vec!["be more concise in every reply to the user".to_string()];
        let similar = novelty_score("be more concise in every reply to the user", &rolled);
        let different = novelty_score("\u{56DE}\u{8986}\u{524D}\u{5148}\u{78BA}\u{8A8D}\u{9700}\u{6C42}", &rolled);
        assert!(similar < 0.2, "similar={similar}");
        assert!(different > 0.8, "different={different}");
    }
}
