//! WP2.4 — the **Measure** half of the Gate/Measure split
//! (`commercial/docs/DESIGN-evolution-v3-aee.md` §2.3 / §2.4).
//!
//! A Measure dimension scores a candidate. **No dimension has a veto.** The
//! only thing that can zero a whole vector is a CONTRACT `must_not` hit
//! observed *after* the Gates already passed (e.g. an eval case's actual
//! answer trips a forbidden pattern) — see [`MeasureVector::zeroed`].
//!
//! Dimensions (§2.3):
//! - `cases` — one deterministic pass/fail per eval case actually run,
//!   compared as the mixed mean plus two independent fences
//!   ([`DIM_CASES_VISIBLE`] / [`DIM_CASES_HOLDOUT`], WP-4E) so a visible-set
//!   gain cannot pay for a held-out-set loss inside one noise band
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

    /// Mean of the held-out case dimension alone — the half the Generator
    /// never sees, and therefore the half that carries the anti-gaming signal.
    ///
    /// WP-4E: kept separate from [`Self::cases_mean`] on purpose. A suite is
    /// usually dominated by visible cases, so averaging the two together lets a
    /// large visible gain pay for a held-out loss (`AutoDesign`,
    /// arXiv:2608.13560, keeps the two conditions independent for exactly this
    /// reason). `None` when no held-out case ran.
    pub fn holdout_cases_mean(&self) -> Option<f64> {
        let held: Vec<&CaseScore> = self.cases.iter().filter(|c| c.held_out).collect();
        if held.is_empty() {
            return None;
        }
        Some(held.iter().map(|c| c.score).sum::<f64>() / held.len() as f64)
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
    /// WP-4E: the band applied to the **held-out** case sub-dimension alone.
    ///
    /// Deliberately stricter than [`Self::cases`] (default: half of it). The
    /// held-out subset is the only measurement the Generator cannot see, so it
    /// is the only one whose "unchanged" claim is worth much — a tolerance as
    /// wide as the visible one would spend that signal on noise absorption.
    /// [`Self::from_agent_dir`] never lets it exceed `cases`.
    pub holdout: f64,
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

/// Design §2.4.2's suggested `cases` starting value.
const NOISE_BAND_CASES_DEFAULT: f64 = 0.05;

/// The held-out band's default is *derived* from the visible one, so the
/// "half as tolerant" relationship survives a `cases` override instead of
/// silently decoupling from it.
///
/// The single policy point: the settle-time fence
/// (`aee::settle::CaseBands::from_cases_only`) reconstructs a legacy pending
/// row's held-out tolerance through this same function rather than growing a
/// second, drifting copy of the rule.
pub fn derived_holdout_band(cases: f64) -> f64 {
    cases / 2.0
}

impl Default for NoiseBand {
    fn default() -> Self {
        // Design §2.4.2 suggested starting values — PENDING EMPIRICAL
        // CALIBRATION (2σ over 20 repeats). Do not cite these as tuned.
        Self {
            cases: NOISE_BAND_CASES_DEFAULT,
            holdout: derived_holdout_band(NOISE_BAND_CASES_DEFAULT),
            judge: 0.15,
            anti_sycophancy: 0.0,
            novelty: 0.05,
            relevance: 0.10,
        }
    }
}

impl NoiseBand {
    /// Read `[evolution.noise_band]` from an agent's `agent.toml`.
    ///
    /// Missing file / malformed TOML / absent table → [`Default`]. Every key
    /// is individually optional. `cases` is clamped to
    /// [`NOISE_BAND_CASES_MAX`]; `holdout` defaults to half the *effective*
    /// `cases` band and is clamped to `[0, cases]` — it may be made stricter,
    /// never looser, than the visible half (WP-4E).
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
            // Re-derive so widening the visible band does not silently leave
            // the held-out fence at the old default (or, worse, wider than the
            // visible one).
            band.holdout = derived_holdout_band(band.cases);
        }
        if let Some(v) = raw.holdout {
            band.holdout = v.max(0.0).min(band.cases);
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
            DIM_CASES_VISIBLE => self.cases,
            DIM_CASES_HOLDOUT => self.holdout,
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
    /// **non-fence** dimension is `> champion + band` (see
    /// [`FENCE_ONLY_DIMENSIONS`]).
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

/// The visible half of the eval-case signal. Same band as `cases`.
pub const DIM_CASES_VISIBLE: &str = "cases_visible";
/// The held-out half. Stricter band ([`NoiseBand::holdout`]).
pub const DIM_CASES_HOLDOUT: &str = "cases_holdout";

/// Dimensions that may **veto** a commit but may never *promote* one.
///
/// WP-4E adds two case sub-dimensions as fences. Letting them set `any_better`
/// would turn ties into `Improves` — which resets the §2.4.3 anti-drift
/// counter, skips the forced observation window and clears the held-out
/// rotation flag. A hardening change must not switch safety companions off as
/// a side effect, so the Improves/Matches classification stays driven by
/// exactly the dimensions that drove it before.
const FENCE_ONLY_DIMENSIONS: [&str; 2] = [DIM_CASES_VISIBLE, DIM_CASES_HOLDOUT];

/// Extract the comparable scalar dimensions of a vector.
///
/// `judge = None` is simply absent — a dimension present on only one side is
/// not comparable and is skipped, never scored as 0.
///
/// **WP-4E — the eval-case signal is emitted three ways, on purpose.**
/// `cases` (the mixed mean) is kept verbatim as the legacy fence, and
/// [`DIM_CASES_VISIBLE`] / [`DIM_CASES_HOLDOUT`] are added beside it. The
/// mixed mean alone let "visible集大幅改善 + held-out集小幅退步" average out
/// inside one noise band, which is precisely the gaming pattern the held-out
/// subset exists to catch; the split gives the held-out half its own, stricter
/// condition (AutoDesign arXiv:2608.13560 keeps the two conditions
/// independent). Dropping the mixed dimension instead of keeping it would have
/// been a *loosening* in two places — the reported dimension name for a
/// no-held-out agent would change, and any champion whose stored vector has no
/// held-out entries would lose its only comparison against a candidate's
/// held-out failures, since a dimension present on one side only is skipped.
/// Extra fences can only ever add rejections.
///
/// **WP-5A footnote.** That second case used to include *every* freshly
/// bootstrapped champion (`aee::run` measured the baseline with
/// `include_holdout: false` while the candidate used `true`). The bootstrap is
/// now measured with held-out included, so both sides are the same shape from
/// round one. What remains is the genuinely legacy set: champion rows written
/// before that fix, and agents whose suite simply has no held-out case. For
/// those, `cases_holdout` stays absent-on-one-side (skipped) and the mixed
/// `cases` fence is what still catches a held-out collapse — which is exactly
/// why it is kept. Such a row self-heals on its next commit, because the
/// committed candidate's own (held-out-inclusive) vector becomes the champion.
fn dimensions(v: &MeasureVector) -> BTreeMap<&'static str, f64> {
    let mut m = BTreeMap::new();
    if let Some(c) = v.cases_mean() {
        m.insert("cases", c);
    }
    if let Some(c) = v.visible_cases_mean() {
        m.insert(DIM_CASES_VISIBLE, c);
    }
    if let Some(c) = v.holdout_cases_mean() {
        m.insert(DIM_CASES_HOLDOUT, c);
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
///
/// Every dimension may veto (`< champion - band` ⇒ `Regresses`), but the
/// WP-4E case fences may not promote — see [`FENCE_ONLY_DIMENSIONS`].
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
        if *c_val > h_val + b && !FENCE_ONLY_DIMENSIONS.contains(dim) {
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

    // ── WP-4E: visible / held-out are separate fences ────────────────────

    /// Build a vector from explicit visible + held-out score lists.
    fn split_vec(judge: Option<f64>, visible: &[f64], holdout: &[f64]) -> MeasureVector {
        let mut cases: Vec<CaseScore> = visible
            .iter()
            .enumerate()
            .map(|(i, s)| CaseScore { case: format!("s/v{i}"), score: *s, held_out: false })
            .collect();
        cases.extend(holdout.iter().enumerate().map(|(i, s)| CaseScore {
            case: format!("s/_holdout/h{i}"),
            score: *s,
            held_out: true,
        }));
        MeasureVector {
            cases,
            judge,
            anti_sycophancy: 1.0,
            novelty: 1.0,
            relevance: 1.0,
            hard_zero: false,
        }
    }

    /// Exactly what the pre-WP-4E gate saw: one mixed `cases` dimension. The
    /// same scores with every held-out flag cleared collapse to that single
    /// comparison, so this is the old behaviour, reproduced rather than
    /// asserted from memory.
    fn as_legacy_mixed(v: &MeasureVector) -> MeasureVector {
        let mut out = v.clone();
        for c in &mut out.cases {
            c.held_out = false;
        }
        out
    }

    #[test]
    fn visible_gain_can_no_longer_mask_a_held_out_regression() {
        let band = NoiseBand::default(); // cases 0.05, holdout 0.025
        // 4 visible cases 0.50 → 1.00 (a big, real gain);
        // 40 held-out cases 1.00 → 0.95 (a small, real loss).
        // Mixed mean is 42/44 on BOTH sides — the loss is perfectly hidden.
        let champion = split_vec(Some(0.7), &[1.0, 1.0, 0.0, 0.0], &[1.0; 40]);
        let mut cand_holdout = [1.0f64; 40];
        cand_holdout[0] = 0.0;
        cand_holdout[1] = 0.0;
        let candidate = split_vec(Some(0.7), &[1.0, 1.0, 1.0, 1.0], &cand_holdout);
        assert_eq!(
            champion.cases_mean(),
            candidate.cases_mean(),
            "the scenario only bites when the mixed mean is unmoved"
        );
        assert_eq!(candidate.visible_cases_mean(), Some(1.0));
        assert_eq!(candidate.holdout_cases_mean(), Some(0.95));

        // OLD behaviour (mixed dimension only): a tie, and a tie commits.
        let legacy = commit_verdict(
            &as_legacy_mixed(&candidate),
            Some(&as_legacy_mixed(&champion)),
            &band,
        );
        assert_eq!(legacy, CommitVerdict::Matches);
        assert!(legacy.is_committable(), "this is the hole WP-4E closes");

        // NEW behaviour: the held-out fence catches it.
        match commit_verdict(&candidate, Some(&champion), &band) {
            CommitVerdict::Regresses { dimension, champion: h, candidate: c } => {
                assert_eq!(dimension, DIM_CASES_HOLDOUT);
                assert_eq!(h, 1.0);
                assert!((c - 0.95).abs() < 1e-9);
            }
            other => panic!("expected a held-out regression, got {other:?}"),
        }
    }

    #[test]
    fn a_held_out_loss_paid_for_by_a_visible_gain_that_lifts_the_mixed_mean() {
        // The nastier variant: the mixed mean actually IMPROVES, so the old
        // gate did not merely tie — it read the candidate as an improvement.
        let band = NoiseBand::default();
        let champion = split_vec(Some(0.7), &[1.0, 0.0, 0.0, 0.0], &[1.0; 20]);
        let mut cand_holdout = [1.0f64; 20];
        cand_holdout[0] = 0.0;
        let candidate = split_vec(Some(0.7), &[1.0, 1.0, 1.0, 1.0], &cand_holdout);
        assert!(candidate.cases_mean() > champion.cases_mean());
        assert_eq!(
            commit_verdict(&as_legacy_mixed(&candidate), Some(&as_legacy_mixed(&champion)), &band),
            CommitVerdict::Improves
        );
        assert!(matches!(
            commit_verdict(&candidate, Some(&champion), &band),
            CommitVerdict::Regresses { ref dimension, .. } if dimension == DIM_CASES_HOLDOUT
        ));
    }

    #[test]
    fn held_out_band_is_stricter_than_the_visible_one() {
        let band = NoiseBand::default();
        assert!(band.holdout < band.cases);
        // A 0.04 drop: inside the visible band (0.05), outside the held-out
        // one (0.025). 100 cases so one flip is exactly 0.01.
        let mut dipped = [1.0f64; 100];
        for s in dipped.iter_mut().take(4) {
            *s = 0.0;
        }
        let champ_visible = split_vec(Some(0.7), &[1.0; 100], &[]);
        let cand_visible = split_vec(Some(0.7), &dipped, &[]);
        assert_eq!(
            commit_verdict(&cand_visible, Some(&champ_visible), &band),
            CommitVerdict::Matches,
            "the visible half keeps its own, wider tolerance"
        );

        // The identical 0.04 drop, this time on the held-out half.
        let champ_holdout = split_vec(Some(0.7), &[1.0], &[1.0; 100]);
        let cand_holdout = split_vec(Some(0.7), &[1.0], &dipped);
        assert!(matches!(
            commit_verdict(&cand_holdout, Some(&champ_holdout), &band),
            CommitVerdict::Regresses { ref dimension, .. } if dimension == DIM_CASES_HOLDOUT
        ));
    }

    #[test]
    fn no_held_out_case_means_the_verdict_is_unchanged_label_included() {
        // Compatibility floor: an agent whose suite has no held-out case must
        // behave EXACTLY as before — same verdict, same reported dimension
        // name (`cases`, not `cases_visible`), same Improves/Matches split.
        let band = NoiseBand::default();
        let champion = split_vec(Some(0.7), &[1.0, 1.0, 0.0, 0.0], &[]);
        assert_eq!(champion.holdout_cases_mean(), None);

        // Regression → still reported against the legacy `cases` dimension.
        let worse = split_vec(Some(0.7), &[1.0, 0.0, 0.0, 0.0], &[]);
        assert_eq!(
            commit_verdict(&worse, Some(&champion), &band),
            CommitVerdict::Regresses {
                dimension: "cases".to_string(),
                champion: 0.5,
                candidate: 0.25
            }
        );
        // Tie / improvement classification is untouched.
        let same = split_vec(Some(0.7), &[1.0, 1.0, 0.0, 0.0], &[]);
        assert_eq!(commit_verdict(&same, Some(&champion), &band), CommitVerdict::Matches);
        let better = split_vec(Some(0.7), &[1.0, 1.0, 1.0, 0.0], &[]);
        assert_eq!(commit_verdict(&better, Some(&champion), &band), CommitVerdict::Improves);
        // …and it matches the legacy single-dimension computation row for row.
        for cand in [&worse, &same, &better] {
            assert_eq!(
                commit_verdict(cand, Some(&champion), &band),
                commit_verdict(
                    &as_legacy_mixed(cand),
                    Some(&as_legacy_mixed(&champion)),
                    &band
                ),
                "no-held-out agents must not notice WP-4E at all"
            );
        }
    }

    #[test]
    fn a_held_out_gain_alone_does_not_manufacture_an_improves() {
        // The new fences may veto, never promote: turning a tie into an
        // `Improves` would reset the anti-drift counter and skip the forced
        // observation window (§2.4.3).
        let band = NoiseBand::default();
        let champion = split_vec(Some(0.7), &[1.0, 1.0, 1.0, 1.0], &[0.0; 4]);
        let candidate = split_vec(Some(0.7), &[1.0, 1.0, 1.0, 1.0], &[1.0; 4]);
        // The mixed mean rose too, so this one legitimately improves…
        assert_eq!(commit_verdict(&candidate, Some(&champion), &band), CommitVerdict::Improves);

        // …but a held-out-only gain that leaves the mixed mean inside its band
        // stays a tie: 4 held-out cases 0.0 → 1.0 moves the mixed mean by
        // 4/104 = 0.038 < 0.05, so the legacy dimension says "tie".
        let champ = split_vec(Some(0.7), &[1.0; 100], &[0.0; 4]);
        let cand = split_vec(Some(0.7), &[1.0; 100], &[1.0; 4]);
        assert!(cand.cases_mean().unwrap() - champ.cases_mean().unwrap() < band.cases);
        let flat = commit_verdict(&cand, Some(&champ), &band);
        assert_eq!(
            flat,
            CommitVerdict::Matches,
            "a fence must not promote a tie into an Improves"
        );
        let d = anti_drift(&flat, AntiDriftState::default());
        assert!(d.force_observation_window && d.holdout_rotation_due);
    }

    #[test]
    fn holdout_band_defaults_to_half_of_the_effective_cases_band() {
        // Absent key → half the default.
        let dir = band_dir("[evolution.noise_band]\njudge = 0.2\n");
        let band = NoiseBand::from_agent_dir(dir.path());
        assert_eq!(band.holdout, NoiseBand::default().cases / 2.0);

        // A `cases` override re-derives it.
        let dir = band_dir("[evolution.noise_band]\ncases = 0.08\n");
        let band = NoiseBand::from_agent_dir(dir.path());
        assert_eq!(band.cases, 0.08);
        assert_eq!(band.holdout, 0.04);

        // Explicit value honoured.
        let dir = band_dir("[evolution.noise_band]\ncases = 0.08\nholdout = 0.01\n");
        assert_eq!(NoiseBand::from_agent_dir(dir.path()).holdout, 0.01);

        // Never looser than the visible band, never negative.
        let dir = band_dir("[evolution.noise_band]\ncases = 0.04\nholdout = 0.9\n");
        let band = NoiseBand::from_agent_dir(dir.path());
        assert_eq!(band.holdout, 0.04, "clamped down to the visible band");
        let dir = band_dir("[evolution.noise_band]\nholdout = -1.0\n");
        assert_eq!(NoiseBand::from_agent_dir(dir.path()).holdout, 0.0);
    }

    #[test]
    fn holdout_band_wrong_type_or_integer_falls_back_to_the_derived_default() {
        // Same `opt_float_strict` quirk as its section-mates: an integer
        // literal is ignored rather than widening a commit gate.
        for body in [
            "[evolution.noise_band]\nholdout = 1\n",
            "[evolution.noise_band]\nholdout = \"0.01\"\n",
            "[evolution.noise_band]\nholdout = true\n",
            "[evolution.noise_band]\n",
            "",
        ] {
            let dir = band_dir(body);
            let band = NoiseBand::from_agent_dir(dir.path());
            assert_eq!(band, NoiseBand::default(), "for {body:?}");
            assert_eq!(band.holdout, band.cases / 2.0);
        }
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
