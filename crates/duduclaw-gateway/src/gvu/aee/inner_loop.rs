//! WP2.3 §3.1 — the agentic inner loop (AVO P5).
//!
//! ```text
//! a. LLM emits PlaybookDelta[]                (1 LLM call)
//! b. run_gates(...)                           (zero cost — a rejection here
//!                                              costs nothing but the round)
//! c. shadow-apply to an in-memory snapshot    (no SQLite)
//! d. score the touched + sampled cases        (replay, no LLM)
//! e. unsatisfied → feed b/d back into a, round += 1; else break
//! ```
//!
//! Three properties this module exists to guarantee:
//!
//! 1. **≤ 3 rounds, hard.** Not a soft budget — the loop counter is the loop
//!    bound. Round 4 and 5 would need the WP0.6 rejection distribution to
//!    justify their marginal cost, and inventing them now is inventing cost.
//! 2. **Nothing persistent is written.** Steps a–e touch a
//!    [`PlaybookSnapshot`] and a `Vec` of pending notes. The caller decides,
//!    at the outer boundary, whether anything reaches SQLite.
//! 3. **Repeating a rejection escalates.** Two identical gate rejections, or
//!    a WP0.5 `RepeatedRejectionReason` at ≥ 2 occurrences, breaks out early
//!    and asks for a human instead of burning round 3 on the same wall.

use std::collections::HashMap;
use std::future::Future;

use chrono::{DateTime, Utc};
use tracing::{debug, warn};

use crate::gvu::stagnation::{StagnationSignal, StagnationSnapshot};
use crate::gvu::text_gradient::TextGradient;
use crate::gvu::verifier::CanaryTest;
use crate::gvu::verifier_gate::{run_gates, GateInput};
use crate::gvu::verifier_measure::{CaseScore, MeasureScorer, ScoreRequest};
use crate::playbook::delta::{validate_all, PlaybookDelta, ValidationCtx};
use crate::playbook::gene::EvalCaseRef;

use super::prompt::{self, is_holdout, PromptContext};
use super::snapshot::{deterministic_sample, PendingFailureNote, PlaybookSnapshot, INNER_LOOP_SAMPLE_MAX};

/// §3.7.3 — hard ceiling on inner rounds.
pub const MAX_INNER_ROUNDS: u32 = 3;

/// An inner round that scores at or above this on its sampled cases is
/// "satisfied" and breaks out early rather than spending another LLM call.
pub const SATISFIED_CASE_SCORE: f64 = 1.0;

/// Why the loop stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum InnerLoopExit {
    /// Every gate cleared and every scored case passed.
    Satisfied,
    /// The round budget ran out with a usable candidate in hand.
    RoundsExhausted,
    /// Nothing survived the gates in any round.
    NoViableCandidate,
    /// The model returned `[]` — a legitimate "nothing worth changing".
    NothingProposed,
    /// The same rejection recurred; a human should look rather than the loop
    /// spending another round.
    EscalateToHuman { reason: String },
    /// The LLM itself failed (not a quality signal).
    GeneratorUnavailable { error: String },
}

impl InnerLoopExit {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::RoundsExhausted => "rounds_exhausted",
            Self::NoViableCandidate => "no_viable_candidate",
            Self::NothingProposed => "nothing_proposed",
            Self::EscalateToHuman { .. } => "escalate_to_human",
            Self::GeneratorUnavailable { .. } => "generator_unavailable",
        }
    }
}

/// Everything the inner loop produced. Nothing here has been persisted.
#[derive(Debug, Clone)]
pub struct InnerLoopOutcome {
    /// The best (last gate-clearing) delta batch. Empty when none survived.
    pub deltas: Vec<PlaybookDelta>,
    /// The shadow snapshot those deltas produce.
    pub snapshot: PlaybookSnapshot,
    /// Partial case scores from the last scored round — the inner loop
    /// deliberately never runs the full suite (§3.7.1 reserves that for the
    /// single outer measurement).
    pub partial_cases: Vec<CaseScore>,
    pub rounds_used: u32,
    /// LLM calls actually made — the number the cost argument is judged on.
    pub llm_calls: u32,
    /// Gate names that rejected something, in order.
    pub gate_rejections: Vec<String>,
    /// Schema-level rejections `(delta, reason)`.
    pub rejected: Vec<(PlaybookDelta, String)>,
    /// Buffered `failure_history` notes, flushed by the caller — including on
    /// abandon, because that is the data that stops the next round repeating.
    pub pending_notes: Vec<PendingFailureNote>,
    pub exit: InnerLoopExit,
}

impl InnerLoopOutcome {
    pub fn has_candidate(&self) -> bool {
        !self.deltas.is_empty()
    }
}

/// Static inputs that do not change between inner rounds.
pub struct InnerLoopInput<'a> {
    pub agent_id: &'a str,
    pub base: PlaybookSnapshot,
    pub ctx: PromptContext,
    pub must_not: &'a [String],
    pub must_always: &'a [String],
    pub canary_tests: &'a [CanaryTest],
    /// Root the eval-case refs resolve under (`<root>/<suite>/*.toml`).
    pub eval_cases_root: &'a std::path::Path,
    /// WP0.5 snapshot, if available. Absent ⇒ treated as not stagnant.
    pub stagnation: Option<StagnationSnapshot>,
    pub round_seq: u64,
    pub now: DateTime<Utc>,
}

/// Parse the model's reply into deltas.
///
/// Accepts a bare JSON array, a fenced array, or an object with a `deltas`
/// key. Anything else is a parse failure carrying the reason — which becomes
/// gradient material rather than a silent empty batch.
pub fn parse_deltas(response: &str) -> Result<Vec<PlaybookDelta>, String> {
    let stripped = crate::gvu::verifier::strip_json_fences(response);
    if stripped.trim().is_empty() {
        return Err("empty response".to_string());
    }
    if let Ok(v) = serde_json::from_str::<Vec<PlaybookDelta>>(stripped) {
        return Ok(v);
    }
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(stripped) {
        if let Some(arr) = obj.get("deltas") {
            return serde_json::from_value::<Vec<PlaybookDelta>>(arr.clone())
                .map_err(|e| format!("`deltas` key is not a valid delta array: {e}"));
        }
    }
    Err("response is not a JSON array of playbook deltas".to_string())
}

/// §3.5 lock 2, enforced at the loop boundary: strip any delta that tries to
/// link a held-out case. Returns `(kept, rejected)`.
fn strip_holdout_links(
    deltas: Vec<PlaybookDelta>,
) -> (Vec<PlaybookDelta>, Vec<(PlaybookDelta, String)>) {
    let mut kept = Vec::new();
    let mut rejected = Vec::new();
    for d in deltas {
        let cases: &[EvalCaseRef] = match &d {
            PlaybookDelta::Add { eval_cases, .. } | PlaybookDelta::Link { eval_cases, .. } => eval_cases,
            _ => &[],
        };
        match prompt::reject_holdout_links(cases) {
            Ok(()) => kept.push(d),
            Err(e) => rejected.push((d, e)),
        }
    }
    (kept, rejected)
}

/// §3.2.3 — the intent constrains which ops are legal this round.
fn enforce_intent_ops(
    intent: super::intent::RoundIntent,
    deltas: Vec<PlaybookDelta>,
) -> (Vec<PlaybookDelta>, Vec<(PlaybookDelta, String)>) {
    use super::intent::RoundIntent::*;
    let mut kept = Vec::new();
    let mut rejected = Vec::new();
    let mut adds_kept = 0usize;
    for d in deltas {
        let is_add = matches!(d, PlaybookDelta::Add { .. });
        match intent {
            Optimize if is_add => {
                rejected.push((d, "optimize rounds may not add entries (§3.2.3)".to_string()));
                continue;
            }
            Innovate if is_add && adds_kept >= 1 => {
                rejected.push((d, "innovate rounds may add at most one entry (§3.2.3)".to_string()));
                continue;
            }
            _ => {}
        }
        if is_add {
            adds_kept += 1;
        }
        kept.push(d);
    }
    (kept, rejected)
}

/// Should the loop stop early and ask a human?
///
/// Once per rejection streak, not once per round: if we already escalated
/// and no rejection evidence NEWER than that escalation has arrived, the
/// round runs normally. Without this exit the escalation records themselves
/// kept the loop parked forever — each pre-flight exit wrote a log row, the
/// next pre-flight read it as ongoing stagnation, and no AEE round could
/// ever run again (2026-08 trader-lead deadlock; the stagnation module doc
/// says detection must never block a run — this check is the one deliberate
/// exception, so it must not be able to feed itself).
fn stagnation_escalation(stag: &Option<StagnationSnapshot>) -> Option<String> {
    let s = stag.as_ref()?;
    if let (Some(escalated_at), Some(rejected_at)) =
        (s.latest_escalation_at, s.latest_real_rejection_at)
    {
        if escalated_at >= rejected_at {
            return None; // already escalated for this streak — let the round run
        }
    }
    s.signals.iter().find_map(|sig| match sig {
        StagnationSignal::RepeatedRejectionReason { occurrences, reason_prefix, .. }
            if *occurrences >= 2 =>
        {
            Some(format!(
                "the same rejection has fired {occurrences} times (\"{reason_prefix}\") — \
                 escalating instead of spending another inner round"
            ))
        }
        _ => None,
    })
}

/// Run the inner loop.
///
/// `call_llm` is the generation closure (the same shape `GvuLoop` already
/// uses, so the caller can hand it the account-rotated CLI/API path). It is
/// invoked at most [`MAX_INNER_ROUNDS`] times.
pub async fn run_inner_loop<F, Fut>(
    input: InnerLoopInput<'_>,
    scorer: &dyn MeasureScorer,
    call_llm: F,
) -> InnerLoopOutcome
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let mut outcome = InnerLoopOutcome {
        deltas: Vec::new(),
        snapshot: input.base.clone(),
        partial_cases: Vec::new(),
        rounds_used: 0,
        llm_calls: 0,
        gate_rejections: Vec::new(),
        rejected: Vec::new(),
        pending_notes: Vec::new(),
        exit: InnerLoopExit::NoViableCandidate,
    };

    if let Some(reason) = stagnation_escalation(&input.stagnation) {
        outcome.exit = InnerLoopExit::EscalateToHuman { reason };
        return outcome;
    }

    let mut ctx = input.ctx.clone();
    // Rejection fingerprints seen so far: two identical ones escalate.
    let mut seen_rejections: HashMap<String, u32> = HashMap::new();

    for round in 1..=MAX_INNER_ROUNDS {
        ctx.inner_round = round;
        outcome.rounds_used = round;

        // ---- a. generate -------------------------------------------------
        let prompt_text = prompt::assemble(&input.base, &ctx);
        let response = match call_llm(prompt_text).await {
            Ok(r) => {
                outcome.llm_calls += 1;
                r
            }
            Err(e) => {
                // Infrastructure failure, not a quality signal — do not turn
                // it into a rejection gradient.
                warn!(agent = %input.agent_id, round, "AEE inner loop: generator call failed: {e}");
                outcome.exit = InnerLoopExit::GeneratorUnavailable { error: e };
                return outcome;
            }
        };

        let proposed = match parse_deltas(&response) {
            Ok(v) => v,
            Err(e) => {
                let g = TextGradient::blocking(
                    "G-Schema",
                    "response",
                    &format!("Could not parse your reply as playbook deltas: {e}"),
                    "Return ONLY a JSON array of delta objects, no prose and no fences",
                );
                ctx.previous_gradients = vec![g];
                ctx.previous_case_failures.clear();
                outcome.gate_rejections.push("G-Schema".to_string());
                // A model that emits the same unparseable shape twice will
                // emit it a third time too — escalate rather than pay for it.
                if let Some(r) = bump_and_check(&mut seen_rejections, &ctx.previous_gradients) {
                    outcome.exit = InnerLoopExit::EscalateToHuman { reason: r };
                    return outcome;
                }
                continue;
            }
        };

        if proposed.is_empty() {
            outcome.exit = InnerLoopExit::NothingProposed;
            outcome.deltas.clear();
            return outcome;
        }

        // ---- b. gates (zero cost) ----------------------------------------
        let (kept, holdout_rejects) = strip_holdout_links(proposed);
        outcome.rejected.extend(holdout_rejects.iter().cloned());
        let (kept, intent_rejects) = enforce_intent_ops(ctx.intent, kept);
        outcome.rejected.extend(intent_rejects.iter().cloned());

        let wildcard_count = input.base.wildcard_count();
        let vctx = ValidationCtx {
            must_not: input.must_not,
            eval_cases_root: input.eval_cases_root,
            existing_wildcard_count: wildcard_count,
        };
        let (schema_ok, schema_rejects) = validate_all(kept, &vctx);
        outcome.rejected.extend(schema_rejects.iter().cloned());

        let contents: Vec<String> = schema_ok
            .iter()
            .filter_map(|d| match d {
                PlaybookDelta::Add { content, .. } | PlaybookDelta::Revise { content, .. } => {
                    Some(content.clone())
                }
                _ => None,
            })
            .collect();

        let mut round_gradients: Vec<TextGradient> = Vec::new();
        for (d, reason) in holdout_rejects.iter().chain(&intent_rejects).chain(&schema_rejects) {
            round_gradients.push(TextGradient::blocking(
                "G-Schema",
                "delta",
                reason,
                "Fix the delta and re-emit; a rejected delta is a wasted slot",
            ));
            outcome.pending_notes.push(PendingFailureNote::new(
                &delta_target(d),
                reason,
                "G-Schema",
            ));
        }
        if !schema_rejects.is_empty() || !holdout_rejects.is_empty() || !intent_rejects.is_empty() {
            outcome.gate_rejections.push("G-Schema".to_string());
        }

        if schema_ok.is_empty() {
            ctx.previous_gradients = round_gradients;
            ctx.previous_case_failures.clear();
            if let Some(r) = bump_and_check(&mut seen_rejections, &ctx.previous_gradients) {
                outcome.exit = InnerLoopExit::EscalateToHuman { reason: r };
                return outcome;
            }
            continue;
        }

        // Content gates only run when there IS content (Link/Record/Retire
        // batches carry none, and an empty `contents` would trip the
        // fail-closed empty-candidate rule for the wrong reason).
        if !contents.is_empty() {
            let gi = GateInput {
                agent_id: input.agent_id,
                contents,
                simulated_final: None, // playbook deltas never touch SOUL.md
                current_reference: "",
                must_not: input.must_not,
                must_always: input.must_always,
                canary_tests: input.canary_tests,
            };
            match run_gates(&gi) {
                Ok(adv) => round_gradients.extend(adv),
                Err(g) => {
                    outcome.gate_rejections.push(g.source_layer.clone());
                    for d in &schema_ok {
                        outcome.pending_notes.push(PendingFailureNote::new(
                            &delta_target(d),
                            &g.critique,
                            &g.source_layer,
                        ));
                    }
                    ctx.previous_gradients = vec![g];
                    ctx.previous_case_failures.clear();
                    if let Some(r) = bump_and_check(&mut seen_rejections, &ctx.previous_gradients) {
                        outcome.exit = InnerLoopExit::EscalateToHuman { reason: r };
                        return outcome;
                    }
                    continue;
                }
            }
        }

        // ---- b2. G-Assertions: E1 replay (WP2.8, D8=B) -------------------
        // Zero-LLM replay of each Add/Revise-carried assertion set against
        // its linked cases' recorded transcripts. Violation ⇒ deterministic
        // veto (same handling as run_gates); no recorded transcript ⇒
        // advisory only — the veto arms itself as WP2.1 recordings land.
        {
            let mut assertion_gradient: Option<TextGradient> = None;
            for d in &schema_ok {
                let crate::playbook::delta::PlaybookDelta::Add { eval_cases, assertions, content, .. } = d
                else {
                    continue;
                };
                match crate::playbook::assertions::replay_assertions(
                    input.eval_cases_root,
                    eval_cases,
                    assertions,
                ) {
                    crate::playbook::assertions::AssertionReplay::Pass => {}
                    crate::playbook::assertions::AssertionReplay::Unverified(why) => {
                        round_gradients.push(TextGradient::advisory(
                            "G-Assertions",
                            &delta_target(d),
                            &format!(
                                "E1 assertions unverified for `{}`: {why}",
                                duduclaw_core::truncate_chars(content, 60)
                            ),
                            "",
                        ));
                    }
                    crate::playbook::assertions::AssertionReplay::Violations(v) => {
                        assertion_gradient = Some(TextGradient::blocking(
                            "G-Assertions",
                            &delta_target(d),
                            &format!(
                                "E1 assertion replay failed for `{}`: {}",
                                duduclaw_core::truncate_chars(content, 60),
                                v.join("; ")
                            ),
                            "align the entry's assertions with what the linked cases' \
                             transcripts actually show, or fix the entry content",
                        ));
                        break;
                    }
                }
            }
            if let Some(g) = assertion_gradient {
                outcome.gate_rejections.push(g.source_layer.clone());
                for d in &schema_ok {
                    outcome.pending_notes.push(PendingFailureNote::new(
                        &delta_target(d),
                        &g.critique,
                        &g.source_layer,
                    ));
                }
                ctx.previous_gradients = vec![g];
                ctx.previous_case_failures.clear();
                if let Some(r) = bump_and_check(&mut seen_rejections, &ctx.previous_gradients) {
                    outcome.exit = InnerLoopExit::EscalateToHuman { reason: r };
                    return outcome;
                }
                continue;
            }
        }

        // ---- b3. WP2.10 reward-hacking means audit (D11) -----------------
        // H1-H3 fold into the G-Contract veto family (no new layer); H4 is
        // a Measure-side signal recorded as an advisory gradient + logged
        // for telemetry-driven threshold tuning, never a veto.
        {
            let mut hack_gradient: Option<TextGradient> = None;
            for d in &schema_ok {
                let crate::playbook::delta::PlaybookDelta::Add { content, assertions, eval_cases, .. } = d
                else {
                    continue;
                };
                for f in crate::gvu::reward_hack::audit_entry(
                    input.eval_cases_root,
                    content,
                    assertions,
                    eval_cases,
                ) {
                    if f.blocking {
                        hack_gradient = Some(TextGradient::blocking(
                            "G-Contract",
                            &delta_target(d),
                            &format!("reward-hacking signature {}: {}", f.id, f.detail),
                            "state a user-facing behaviour; do not target the test, the \
                             verifier, or the failure-reporting machinery",
                        ));
                        break;
                    } else {
                        warn!(
                            agent = %input.agent_id,
                            round,
                            signature = f.id,
                            "AEE reward-hack telemetry (Measure-side): {}",
                            f.detail
                        );
                        round_gradients.push(TextGradient::advisory(
                            "M-RewardHack",
                            &delta_target(d),
                            &format!("reward-hacking signal {} (Measure-side): {}", f.id, f.detail),
                            "",
                        ));
                    }
                }
                if hack_gradient.is_some() {
                    break;
                }
            }
            if let Some(g) = hack_gradient {
                outcome.gate_rejections.push(g.source_layer.clone());
                for d in &schema_ok {
                    outcome.pending_notes.push(PendingFailureNote::new(
                        &delta_target(d),
                        &g.critique,
                        &g.source_layer,
                    ));
                }
                ctx.previous_gradients = vec![g];
                ctx.previous_case_failures.clear();
                if let Some(r) = bump_and_check(&mut seen_rejections, &ctx.previous_gradients) {
                    outcome.exit = InnerLoopExit::EscalateToHuman { reason: r };
                    return outcome;
                }
                continue;
            }
        }

        // ---- c. shadow apply (memory only) -------------------------------
        let (shadow, merge_outcome) = input.base.shadow_apply(schema_ok.clone(), input.now);
        debug!(
            agent = %input.agent_id,
            round,
            applied = merge_outcome.applied.len(),
            notes = merge_outcome.notes.len(),
            "AEE inner loop: shadow applied"
        );
        outcome.deltas = schema_ok.clone();
        outcome.snapshot = shadow;

        // ---- d. score the touched + sampled subset -----------------------
        let touched = input.base.cases_touched_by(&schema_ok);
        let pool: Vec<EvalCaseRef> = outcome
            .snapshot
            .linked_cases()
            .into_iter()
            .filter(|c| !touched.contains(c) && !is_holdout(c))
            .collect();
        let mut to_run: Vec<String> = touched
            .iter()
            .filter(|c| !is_holdout(c))
            .map(|c| c.0.clone())
            .collect();
        to_run.extend(
            deterministic_sample(&pool, input.round_seq, INNER_LOOP_SAMPLE_MAX)
                .into_iter()
                .map(|c| c.0),
        );

        let req = ScoreRequest {
            agent_id: input.agent_id.to_string(),
            cases: to_run,
            // Never. The inner loop must not even learn the held-out names.
            include_holdout: false,
        };
        let scored = match scorer.score(&req).await {
            Ok(Some(c)) => c,
            Ok(None) => Vec::new(),
            Err(e) => {
                warn!(
                    agent = %input.agent_id,
                    round,
                    scorer = scorer.name(),
                    "AEE inner loop: scorer unavailable — proceeding on gates alone: {e}"
                );
                Vec::new()
            }
        };
        outcome.partial_cases = scored.clone();

        // ---- e. satisfied? ----------------------------------------------
        let failures: Vec<&CaseScore> =
            scored.iter().filter(|c| c.score < SATISFIED_CASE_SCORE).collect();
        if failures.is_empty() {
            outcome.exit = InnerLoopExit::Satisfied;
            return outcome;
        }

        for f in &failures {
            outcome
                .pending_notes
                .push(PendingFailureNote::new("", &format!("case {} failed", f.case), &format!("eval:{}", f.case)));
        }
        ctx.previous_gradients = round_gradients;
        ctx.previous_case_failures =
            failures.iter().map(|f| format!("{} failed", f.case)).collect();
    }

    outcome.exit = if outcome.deltas.is_empty() {
        InnerLoopExit::NoViableCandidate
    } else {
        InnerLoopExit::RoundsExhausted
    };
    outcome
}

fn delta_target(d: &PlaybookDelta) -> String {
    match d {
        PlaybookDelta::Add { content, category, .. } => {
            crate::playbook::dedup::dedup_key(content, *category)
        }
        PlaybookDelta::Revise { id, .. }
        | PlaybookDelta::Link { id, .. }
        | PlaybookDelta::Record { id, .. }
        | PlaybookDelta::Retire { id, .. } => id.clone(),
    }
}

/// Count identical rejection fingerprints; the second occurrence escalates
/// (§3.9 `two_identical_gate_rejections_escalate_to_human`).
fn bump_and_check(seen: &mut HashMap<String, u32>, gradients: &[TextGradient]) -> Option<String> {
    for g in gradients {
        let key = format!("{}|{}", g.source_layer, g.critique);
        let n = seen.entry(key.clone()).or_insert(0);
        *n += 1;
        if *n >= 2 {
            return Some(format!(
                "the same gate rejection fired twice in one inner loop ({}): {}",
                g.source_layer, g.critique
            ));
        }
    }
    None
}

/// `now()` indirection so tests can pin the instant.
pub fn default_now() -> DateTime<Utc> {
    Utc::now()
}
