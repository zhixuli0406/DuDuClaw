//! WP2.3 §3.3 + §3.5 — Generator prompt assembly and the held-out firewall.
//!
//! ## Three cache blocks, two markers
//!
//! The prompt is split on [`CACHE_SPLIT_MARKER`] into exactly three segments,
//! matching `direct_api::MAX_SYSTEM_SEGMENTS`:
//!
//! | Block | Volatility | Contents |
//! |---|---|---|
//! | 1 | never changes | role, [`PLAYBOOK_EDITING_GUIDE`], gate checklist, output schema |
//! | 2 | days | agent `must_not`/`must_always`, current playbook, version lineage, rejection telemetry |
//! | 3 | every inner round | this round's intent + material, last round's gate gradients, round counter |
//!
//! Block 1 is the largest (~4 KB of guide) and is deliberately first so it is
//! always a cache prefix hit. Inner rounds 2 and 3 change only block 3 —
//! which is the whole reason a three-round inner loop can cost about what one
//! blind single shot used to.
//!
//! ## Held-out firewall (§3.5)
//!
//! Held-out cases live under an `_holdout` path segment. Two of the design's
//! four locks are implemented here and in
//! [`crate::gvu::aee::inner_loop`]: entries may not *link* a held-out case
//! ([`reject_holdout_links`]), and the assembled prompt is filtered a second
//! time ([`visible_cases`]) so that even if the first lock were bypassed the
//! name never reaches the model. The remaining two locks are the directory
//! convention and operator-only rotation, which live outside this crate.

use crate::direct_api::CACHE_SPLIT_MARKER;
use crate::gvu::mistake_notebook::MistakeEntry;
use crate::gvu::stagnation::{StagnationSignal, StagnationSnapshot};
use crate::gvu::telemetry::TelemetrySummary;
use crate::gvu::text_gradient::TextGradient;
use crate::gvu::version_store::{ExperimentSummary, SoulVersion};
use crate::playbook::delta::ExistingEntry;
use crate::playbook::gene::EvalCaseRef;

use super::intent::{AeeTrigger, EvolutionStrategy, RoundIntent};
use super::snapshot::PlaybookSnapshot;

/// The structured editing manual, compiled into the binary so a stripped
/// deployment can never lose it at runtime.
pub const PLAYBOOK_EDITING_GUIDE: &str =
    include_str!("../../playbook/PLAYBOOK_EDITING_GUIDE.md");

/// Path segments marking a held-out eval case.
///
/// Two spellings, deliberately: the design specified `_holdout/`, but the
/// suites that actually shipped (`commercial/evals/<agent>/held-out/`) use
/// `held-out`. Recognising only the design's spelling would have made this
/// firewall a no-op against every real corpus on disk — the worst possible
/// failure mode for a leak guard, because it looks like it is working.
pub const HOLDOUT_SEGMENTS: &[&str] = &["_holdout", "held-out"];

/// Is this a held-out case reference?
///
/// Segment-exact, never a substring test (CLAUDE.md security convention #2):
/// a suite legitimately named `my_holdout_notes` must not be mistaken for the
/// held-out subset, and — more dangerously — a case named
/// `x/not_holdout/case` must not be treated as held-out and thereby excluded
/// from the very comparison that catches over-fitting.
pub fn is_holdout(case: &EvalCaseRef) -> bool {
    case.0.split('/').any(|seg| HOLDOUT_SEGMENTS.contains(&seg))
}

/// §3.5 lock 2: refuse to let an entry link a held-out case.
///
/// Held-out cases exist to judge the whole candidate at the commit gate. An
/// entry that could link one would be able to tune itself against the exam.
pub fn reject_holdout_links(cases: &[EvalCaseRef]) -> Result<(), String> {
    for c in cases {
        if is_holdout(c) {
            return Err(format!("held-out cases cannot be linked to entries: {}", c.0));
        }
    }
    Ok(())
}

/// §3.5 lock 3: the second-pass filter applied at render time.
pub fn visible_cases(cases: &[EvalCaseRef]) -> Vec<&EvalCaseRef> {
    cases.iter().filter(|c| !is_holdout(c)).collect()
}

/// Everything block 2 and block 3 need. Owned so the caller can assemble it
/// from short-lived borrows across await points.
#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    pub agent_id: String,
    pub strategy: EvolutionStrategy,
    pub trigger: AeeTrigger,
    pub intent: RoundIntent,
    pub round_seq: u64,
    /// Inner-loop round, 1-based.
    pub inner_round: u32,
    pub must_not: Vec<String>,
    pub must_always: Vec<String>,
    /// Lineage: last 5 SOUL versions.
    pub versions: Vec<SoulVersion>,
    pub experiments: ExperimentSummary,
    /// Rejection distribution over the trailing 30 days.
    pub telemetry: Option<TelemetrySummary>,
    /// Champion's headline score, when one exists.
    pub champion_headline: Option<f64>,
    /// Stagnation signals — `RepeatedRejectionReason` in particular tells the
    /// model, in so many words, to stop walking into the same wall.
    pub stagnation: Option<StagnationSnapshot>,
    /// Round material: unresolved mistakes (Repair rounds).
    pub mistakes: Vec<MistakeEntry>,
    /// Round material: low-streak entry ids (Optimize rounds).
    pub low_streak_ids: Vec<String>,
    /// Round material: signals with no entry covering them (Innovate rounds).
    pub uncovered_signals: Vec<String>,
    /// Feedback from the previous inner round.
    pub previous_gradients: Vec<TextGradient>,
    /// Per-case failure detail from the previous inner round
    /// (`"<case> failed"`), already held-out filtered by the caller.
    pub previous_case_failures: Vec<String>,
}

/// Assemble the full three-block prompt.
pub fn assemble(snapshot: &PlaybookSnapshot, ctx: &PromptContext) -> String {
    format!(
        "{}\n{CACHE_SPLIT_MARKER}\n{}\n{CACHE_SPLIT_MARKER}\n{}",
        block1(),
        block2(snapshot, ctx),
        block3(ctx),
    )
}

/// Block 1 — invariant across every agent and every round.
fn block1() -> String {
    format!(
        "You are the playbook editor for a DuDuClaw agent.\n\
         Your entire output is a JSON array of playbook deltas. No prose, no \
         markdown fences, no commentary.\n\n\
         {PLAYBOOK_EDITING_GUIDE}\n\n\
         ## Output contract\n\
         Return `[]` when you have nothing worth changing. An empty array is a \
         valid, respectable answer — fabricating a change to look productive is \
         not.\n"
    )
}

/// Block 2 — semi-stable: this agent's contract, playbook and lineage.
fn block2(snapshot: &PlaybookSnapshot, ctx: &PromptContext) -> String {
    let mut s = String::new();
    s.push_str(&format!("## Agent\n{}\n\n", ctx.agent_id));

    s.push_str("## Contract boundaries\n");
    if ctx.must_not.is_empty() {
        s.push_str("must_not: (none)\n");
    } else {
        s.push_str(&format!("must_not: {:?}\n", ctx.must_not));
    }
    if !ctx.must_always.is_empty() {
        s.push_str(&format!("must_always: {:?}\n", ctx.must_always));
    }
    s.push('\n');

    s.push_str("## Current playbook\n");
    let live: Vec<&ExistingEntry> = snapshot
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.meta.state,
                crate::playbook::entry::PlaybookState::Active
                    | crate::playbook::entry::PlaybookState::Probation
            )
        })
        .collect();
    if live.is_empty() {
        s.push_str("(empty — this agent has no entries yet)\n");
    } else {
        for e in &live {
            // §3.5 lock 3: the SECOND held-out filter. Even if a held-out ref
            // somehow made it into an entry's metadata, it stops here.
            let cases: Vec<&str> =
                visible_cases(&e.meta.eval_cases).iter().map(|c| c.0.as_str()).collect();
            s.push_str(&format!(
                "- id={} category={} state={} net={} streak={} signals={:?} cases={:?}\n  {}\n",
                e.id,
                e.meta.category.as_str(),
                e.meta.state.as_str(),
                e.stats.net(),
                e.meta.success_streak,
                e.meta.signals_match,
                cases,
                e.content,
            ));
            for f in e.meta.failure_history.iter().take(3) {
                s.push_str(&format!("  ! failed [{}] {}\n", f.source, f.what));
            }
        }
    }
    s.push_str(&format!(
        "\nCapacity: {}/{} entries in use.\n\n",
        snapshot.active_count(),
        crate::playbook::PLAYBOOK_MAX_ENTRIES
    ));

    s.push_str("## Version lineage (most recent first)\n");
    if ctx.versions.is_empty() {
        s.push_str("(no prior versions)\n");
    } else {
        for v in ctx.versions.iter().take(5) {
            s.push_str(&format!(
                "- [{}] {} ({})\n",
                v.status.as_str(),
                duduclaw_core::truncate_chars(&v.soul_summary, 120),
                v.applied_at.format("%Y-%m-%d"),
            ));
        }
    }
    s.push_str(&format!(
        "\nExperiments: {} total, {} applied, {} abandoned, success rate {:.0}%.\n",
        ctx.experiments.total_experiments,
        ctx.experiments.applied_count,
        ctx.experiments.abandoned_count,
        ctx.experiments.success_rate * 100.0,
    ));
    if let Some(h) = ctx.champion_headline {
        s.push_str(&format!("Reigning champion headline score: {h:.3}\n"));
    }
    s.push('\n');

    s.push_str("## Rejection history (last 30 days)\n");
    match &ctx.telemetry {
        Some(t) if t.total > 0 => {
            s.push_str(&format!("{} rejections recorded.\n", t.total));
            for (stage, layers) in &t.by_stage_layer {
                for (layer, count) in layers {
                    s.push_str(&format!("- {stage}/{layer}: {count}\n"));
                }
            }
        }
        // Explicit degradation note rather than a silently missing section —
        // the model should know the difference between "clean record" and
        // "we could not read the record".
        Some(_) => s.push_str("(no rejections recorded)\n"),
        None => s.push_str("(unavailable — telemetry could not be read this round)\n"),
    }

    s
}

/// Block 3 — changes every inner round. Kept last and kept small.
fn block3(ctx: &PromptContext) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "## This round\nintent: {}\nstrategy: {}\ntrigger: {}\nround_seq: {}\ninner_round: {}\n\n",
        ctx.intent.as_str(),
        ctx.strategy.as_str(),
        ctx.trigger.as_str(),
        ctx.round_seq,
        ctx.inner_round,
    ));

    s.push_str(match ctx.intent {
        RoundIntent::Repair => {
            "Goal: consume a concrete recorded failure. Allowed ops: add(repair) / revise / link.\n\n"
        }
        RoundIntent::Optimize => {
            "Goal: sharpen an existing entry. Allowed ops: revise / link / retire. \
             **Do not add.**\n\n"
        }
        RoundIntent::Innovate => {
            "Goal: explore one new rule. Allowed ops: add(innovate), **at most one**.\n\n"
        }
    });

    match ctx.intent {
        RoundIntent::Repair => {
            s.push_str("## Unresolved mistakes\n");
            if ctx.mistakes.is_empty() {
                s.push_str("(none)\n");
            } else {
                for m in ctx.mistakes.iter().take(5) {
                    let kind = if m.source_kind.is_empty() { "unattributed" } else { &m.source_kind };
                    s.push_str(&format!(
                        "- [{}/{}] {}\n",
                        m.category.as_str(),
                        kind,
                        duduclaw_core::truncate_chars(&m.what_went_wrong, 200),
                    ));
                }
            }
        }
        RoundIntent::Optimize => {
            s.push_str("## Entries to sharpen (low success_streak)\n");
            if ctx.low_streak_ids.is_empty() {
                s.push_str("(none)\n");
            } else {
                for id in ctx.low_streak_ids.iter().take(5) {
                    s.push_str(&format!("- {id}\n"));
                }
            }
        }
        RoundIntent::Innovate => {
            s.push_str("## Signals no entry currently covers\n");
            if ctx.uncovered_signals.is_empty() {
                s.push_str("(none — pick a genuinely new behaviour or return [])\n");
            } else {
                for sig in ctx.uncovered_signals.iter().take(10) {
                    s.push_str(&format!("- {sig}\n"));
                }
            }
        }
    }
    s.push('\n');

    if let Some(stag) = &ctx.stagnation {
        if stag.is_stagnant() {
            s.push_str("## Stagnation detected — change direction\n");
            for sig in &stag.signals {
                match sig {
                    StagnationSignal::RepeatedRejectionReason { occurrences, reason_prefix, .. } => {
                        s.push_str(&format!(
                            "- The SAME rejection has fired {occurrences} times: \"{reason_prefix}\". \
                             Do not go that way again.\n"
                        ));
                    }
                    StagnationSignal::ConsecutiveNonApplied { count, .. } => {
                        s.push_str(&format!("- {count} consecutive rounds applied nothing.\n"));
                    }
                    StagnationSignal::ZeroApplyWindow { days, trigger_count } => {
                        s.push_str(&format!(
                            "- {trigger_count} attempts over {days} days, none applied.\n"
                        ));
                    }
                }
            }
            s.push('\n');
        }
    }

    if !ctx.previous_gradients.is_empty() || !ctx.previous_case_failures.is_empty() {
        s.push_str("## Feedback from your previous attempt this round\n");
        for g in &ctx.previous_gradients {
            s.push_str(&format!("- [{}] {} → {}\n", g.source_layer, g.critique, g.suggestion));
        }
        for f in &ctx.previous_case_failures {
            s.push_str(&format!("- [eval] {f}\n"));
        }
        s.push_str("\nFix these specifically. Repeating a rejected shape wastes the round.\n");
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::delta::ExistingEntry;
    use crate::playbook::entry::{PlaybookCategory, PlaybookMeta, PlaybookState, PLAYBOOK_SCHEMA_VERSION};
    use crate::prediction::rule_lifecycle::RuleStats;

    fn entry_with_cases(id: &str, cases: &[&str]) -> ExistingEntry {
        ExistingEntry {
            id: id.to_string(),
            content: "some rule".to_string(),
            meta: PlaybookMeta {
                assertions: Default::default(),
                schema_version: PLAYBOOK_SCHEMA_VERSION,
                category: PlaybookCategory::Repair,
                signals_match: vec!["mistake:factual".to_string()],
                strategy: Vec::new(),
                failure_history: Vec::new(),
                eval_cases: cases.iter().map(|c| EvalCaseRef((*c).to_string())).collect(),
                applications: Vec::new(),
                success_streak: 0,
                state: PlaybookState::Active,
                revision: 0,
                dedup_key: "k".to_string(),
                embed_model: None,
                origin: "agent_derived".to_string(),
                derived_from: Vec::new(),
            },
            stats: RuleStats::initial(),
        }
    }

    #[test]
    fn holdout_detection_is_segment_exact_not_substring() {
        assert!(is_holdout(&EvalCaseRef("ceo/_holdout/refund".into())));
        assert!(is_holdout(&EvalCaseRef("_holdout/refund".into())));
        // The spelling the shipped suites actually use.
        assert!(is_holdout(&EvalCaseRef("b2b-billing/held-out/p1-heldout-auditor-001".into())));
        // A suite whose NAME merely contains the token must not be captured…
        assert!(!is_holdout(&EvalCaseRef("my_holdout_notes/refund".into())));
        // …nor a case name that embeds it.
        assert!(!is_holdout(&EvalCaseRef("ceo/not_holdout_case".into())));
    }

    #[test]
    fn holdout_case_cannot_be_linked_to_entry() {
        let err = reject_holdout_links(&[EvalCaseRef("ceo/_holdout/x".into())]).unwrap_err();
        assert!(err.contains("held-out"));
        assert!(reject_holdout_links(&[EvalCaseRef("ceo/normal".into())]).is_ok());
    }

    #[test]
    fn assembled_prompt_contains_no_holdout_ref() {
        // Defence in depth: even with a held-out ref smuggled into an entry's
        // metadata (lock 2 bypassed), rendering must not leak the name.
        let snap = PlaybookSnapshot::new(vec![
            entry_with_cases("e1", &["ceo/normal-a", "ceo/_holdout/secret-one"]),
            entry_with_cases("e2", &["ceo/_holdout/secret-two"]),
        ]);
        let ctx = PromptContext { agent_id: "ceo".into(), ..Default::default() };
        let prompt = assemble(&snap, &ctx);
        assert!(!prompt.contains("secret-one"), "held-out case name leaked into the prompt");
        assert!(!prompt.contains("secret-two"));
        assert!(prompt.contains("ceo/normal-a"), "non-held-out cases must still be visible");

        // The token itself appears exactly once, in block 1's guide section
        // telling the model held-out cases exist and are off limits. It must
        // never appear in block 2, which is where real case refs are listed.
        let blocks: Vec<&str> = prompt.split(CACHE_SPLIT_MARKER).collect();
        for seg in super::HOLDOUT_SEGMENTS {
            assert!(!blocks[1].contains(seg), "'{seg}' leaked into the playbook listing");
            assert!(!blocks[2].contains(seg), "'{seg}' leaked into the round block");
        }
    }

    #[test]
    fn prompt_has_exactly_three_cache_blocks() {
        let ctx = PromptContext { agent_id: "a".into(), ..Default::default() };
        let prompt = assemble(&PlaybookSnapshot::default(), &ctx);
        assert_eq!(prompt.matches(CACHE_SPLIT_MARKER).count(), 2);
        let blocks: Vec<&str> = prompt.split(CACHE_SPLIT_MARKER).collect();
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].contains("Playbook Editing Guide"), "guide belongs in block 1");
        assert!(blocks[1].contains("Current playbook"));
        assert!(blocks[2].contains("This round"));
    }

    #[test]
    fn block1_is_byte_identical_across_agents_and_rounds() {
        // The whole cost argument for a 3-round inner loop rests on block 1
        // being a stable cache prefix.
        let a = assemble(
            &PlaybookSnapshot::default(),
            &PromptContext { agent_id: "agent-a".into(), inner_round: 1, ..Default::default() },
        );
        let b = assemble(
            &PlaybookSnapshot::new(vec![entry_with_cases("e", &["s/c"])]),
            &PromptContext { agent_id: "agent-b".into(), inner_round: 3, ..Default::default() },
        );
        let first = |p: &str| p.split(CACHE_SPLIT_MARKER).next().unwrap().to_string();
        assert_eq!(first(&a), first(&b));
    }

    #[test]
    fn optimize_round_tells_the_model_not_to_add() {
        let ctx = PromptContext {
            agent_id: "a".into(),
            intent: RoundIntent::Optimize,
            ..Default::default()
        };
        let prompt = assemble(&PlaybookSnapshot::default(), &ctx);
        assert!(prompt.contains("**Do not add.**"));
    }

    #[test]
    fn missing_telemetry_is_labelled_not_silently_omitted() {
        let ctx = PromptContext { agent_id: "a".into(), telemetry: None, ..Default::default() };
        let prompt = assemble(&PlaybookSnapshot::default(), &ctx);
        assert!(prompt.contains("unavailable"));
    }

    #[test]
    fn repeated_rejection_reason_is_surfaced_verbatim() {
        let ctx = PromptContext {
            agent_id: "a".into(),
            stagnation: Some(StagnationSnapshot {
                agent_id: "a".into(),
                signals: vec![StagnationSignal::RepeatedRejectionReason {
                    occurrences: 4,
                    threshold: 3,
                    reason_prefix: "forbidden pattern: refund".into(),
                }],
                checked_at: chrono::Utc::now(),
                latest_real_rejection_at: None,
                latest_escalation_at: None,
            }),
            ..Default::default()
        };
        let prompt = assemble(&PlaybookSnapshot::default(), &ctx);
        assert!(prompt.contains("forbidden pattern: refund"));
        assert!(prompt.contains("Do not go that way again"));
    }
}
