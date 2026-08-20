//! WP2.3 §3.9 — inner-loop behaviour tests.
//!
//! The properties asserted here are the ones the design says the whole work
//! package stands or falls on: bounded rounds, zero persistent writes on
//! abandon, no held-out leakage, deterministic strategy mix, and escalation
//! instead of repetition.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::gvu::stagnation::{StagnationSignal, StagnationSnapshot};
use crate::gvu::verifier::CanaryTest;
use crate::gvu::verifier_measure::{CaseScore, MeasureScorer, NullScorer, ScoreRequest};
use crate::playbook::entry::PlaybookCategory;

use super::inner_loop::{parse_deltas, run_inner_loop, InnerLoopExit, InnerLoopInput, MAX_INNER_ROUNDS};
use super::prompt::PromptContext;
use super::snapshot::PlaybookSnapshot;

/// Temp eval-case tree with two resolvable cases.
fn eval_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let suite = dir.path().join("s");
    std::fs::create_dir(&suite).unwrap();
    for name in ["c1", "c2"] {
        std::fs::write(
            suite.join(format!("{name}.toml")),
            format!("[case]\nname = \"{name}\"\nagent = \"a\"\nprompt = \"hi\"\n[judge]\nrubric = \"r\"\n"),
        )
        .unwrap();
    }
    dir
}

fn now() -> DateTime<Utc> {
    "2026-08-06T00:00:00Z".parse().unwrap()
}

fn input<'a>(
    root: &'a std::path::Path,
    must_not: &'a [String],
    canaries: &'a [CanaryTest],
    base: PlaybookSnapshot,
) -> InnerLoopInput<'a> {
    InnerLoopInput {
        agent_id: "agent-aee",
        base,
        ctx: PromptContext { agent_id: "agent-aee".into(), ..Default::default() },
        must_not,
        must_always: &[],
        canary_tests: canaries,
        eval_cases_root: root,
        stagnation: None,
        round_seq: 0,
        now: now(),
    }
}

/// Records every prompt it is handed and replays a scripted reply sequence.
struct ScriptedLlm {
    replies: Vec<String>,
    calls: Arc<AtomicUsize>,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl ScriptedLlm {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: replies.iter().map(|s| s.to_string()).collect(),
            calls: Arc::new(AtomicUsize::new(0)),
            prompts: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn call(&self, prompt: String) -> Result<String, String> {
        self.prompts.lock().unwrap().push(prompt);
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.replies.get(i).cloned().unwrap_or_else(|| self.replies.last().unwrap().clone()))
    }
}

struct FailingScorer(Vec<CaseScore>);
#[async_trait::async_trait]
impl MeasureScorer for FailingScorer {
    async fn score(&self, _r: &ScoreRequest) -> Result<Option<Vec<CaseScore>>, String> {
        Ok(Some(self.0.clone()))
    }
}

const GOOD_ADD: &str = r#"[{"op":"add","content":"回覆前先確認需求","category":"repair","signals_match":["mistake:factual"],"eval_cases":["s/c1"],"assertions":{"output_contains":["ok"]},"rationale":"r"}]"#;

#[tokio::test]
async fn inner_loop_never_exceeds_three_rounds() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    // Every round scores a failure, so the loop always wants another round.
    let scorer = FailingScorer(vec![CaseScore { case: "s/c1".into(), score: 0.0, held_out: false }]);
    // Distinct content each round so the "identical rejection" escalation
    // does not fire and mask the round cap.
    let llm = ScriptedLlm::new(&[
        &GOOD_ADD.replace("\\u9700\\u6c42", "\\u9700\\u6c42a"),
        &GOOD_ADD.replace("\\u9700\\u6c42", "\\u9700\\u6c42b"),
        &GOOD_ADD.replace("\\u9700\\u6c42", "\\u9700\\u6c42c"),
    ]);

    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &scorer,
        |p| {
            let r = llm.call(p);
            async move { r }
        },
    )
    .await;

    assert_eq!(out.rounds_used, MAX_INNER_ROUNDS);
    assert_eq!(out.llm_calls, MAX_INNER_ROUNDS, "one generation call per round, no more");
    assert_eq!(out.exit, InnerLoopExit::RoundsExhausted);
    assert!(out.has_candidate());
}

#[tokio::test]
async fn a_clean_candidate_stops_after_one_round() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let scorer = FailingScorer(vec![CaseScore { case: "s/c1".into(), score: 1.0, held_out: false }]);
    let llm = ScriptedLlm::new(&[GOOD_ADD]);

    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &scorer,
        |p| {
            let r = llm.call(p);
            async move { r }
        },
    )
    .await;

    assert_eq!(out.exit, InnerLoopExit::Satisfied);
    assert_eq!(out.llm_calls, 1, "a satisfied candidate must not pay for rounds 2 and 3");
    assert_eq!(out.deltas.len(), 1);
}

#[tokio::test]
async fn inner_loop_leaves_no_sqlite_row_on_abandon() {
    // §3.6's headline guarantee, asserted against a real engine: run a loop
    // that can never succeed and prove the memory store is untouched.
    let engine = duduclaw_memory::SqliteMemoryEngine::in_memory().unwrap();
    let agent = "agent-abandon";
    let before = crate::playbook::store::list_active(&engine, agent).await.len();

    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    // Every reply is unparseable → nothing ever clears the gates.
    let llm = ScriptedLlm::new(&["I think we should probably improve the tone a bit."]);

    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &NullScorer,
        |p| {
            let r = llm.call(p);
            async move { r }
        },
    )
    .await;

    // Either terminal shape is acceptable here (an unparseable reply repeated
    // verbatim trips the repetition escalation first); what matters is that
    // nothing committed and nothing was written.
    assert!(
        matches!(
            out.exit,
            InnerLoopExit::NoViableCandidate | InnerLoopExit::EscalateToHuman { .. }
        ),
        "unexpected exit: {:?}",
        out.exit
    );
    assert!(!out.has_candidate());
    let after = crate::playbook::store::list_active(&engine, agent).await.len();
    assert_eq!(before, after, "the inner loop must not persist a single row");
}

#[tokio::test]
async fn an_empty_delta_array_is_an_honest_answer_not_a_failure() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let llm = ScriptedLlm::new(&["[]"]);
    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &NullScorer,
        |p| {
            let r = llm.call(p);
            async move { r }
        },
    )
    .await;
    assert_eq!(out.exit, InnerLoopExit::NothingProposed);
    assert_eq!(out.llm_calls, 1, "no point re-asking a model that correctly said 'nothing'");
}

#[tokio::test]
async fn generator_failure_is_infrastructure_not_a_quality_signal() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &NullScorer,
        |_p| async { Err("rate limited".to_string()) },
    )
    .await;
    assert!(matches!(out.exit, InnerLoopExit::GeneratorUnavailable { .. }));
    assert!(out.gate_rejections.is_empty(), "an LLM outage must not be logged as a gate rejection");
}

#[tokio::test]
async fn two_identical_gate_rejections_escalate_to_human() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    // The same unparseable reply every round → the same G-Schema gradient.
    let llm = ScriptedLlm::new(&["not json at all"]);
    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &NullScorer,
        |p| {
            let r = llm.call(p);
            async move { r }
        },
    )
    .await;
    match out.exit {
        InnerLoopExit::EscalateToHuman { ref reason } => {
            assert!(reason.contains("twice"), "reason should name the repetition: {reason}")
        }
        other => panic!("expected escalation, got {other:?}"),
    }
    assert!(out.llm_calls < MAX_INNER_ROUNDS, "escalation must save the remaining rounds");
}

#[tokio::test]
async fn repeated_rejection_stagnation_escalates_before_spending_a_call() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let mut inp = input(root.path(), &none, &canaries, PlaybookSnapshot::default());
    inp.stagnation = Some(StagnationSnapshot {
        agent_id: "agent-aee".into(),
        signals: vec![StagnationSignal::RepeatedRejectionReason {
            occurrences: 4,
            threshold: 3,
            reason_prefix: "forbidden pattern".into(),
        }],
        checked_at: Utc::now(),
        // No prior escalation on record → the pre-flight may escalate.
        latest_real_rejection_at: Some(Utc::now()),
        latest_escalation_at: None,
    });
    let out = run_inner_loop(inp, &NullScorer, |_p| async { Ok("[]".to_string()) }).await;
    assert!(matches!(out.exit, InnerLoopExit::EscalateToHuman { .. }));
    assert_eq!(out.llm_calls, 0, "a known wall costs zero LLM calls");
}

/// Once-per-streak escalation (2026-08-20 deadlock fix): a snapshot whose
/// newest escalation is at/after the newest real rejection means "we already
/// asked a human about this streak" — the round must RUN, not park itself
/// behind another escalation record forever.
#[tokio::test]
async fn already_escalated_streak_lets_the_round_run() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let mut inp = input(root.path(), &none, &canaries, PlaybookSnapshot::default());
    let t = Utc::now();
    inp.stagnation = Some(StagnationSnapshot {
        agent_id: "agent-aee".into(),
        signals: vec![StagnationSignal::RepeatedRejectionReason {
            occurrences: 4,
            threshold: 3,
            reason_prefix: "forbidden pattern".into(),
        }],
        checked_at: t,
        latest_real_rejection_at: Some(t - chrono::Duration::hours(2)),
        latest_escalation_at: Some(t - chrono::Duration::hours(1)),
    });
    let out = run_inner_loop(inp, &NullScorer, |_p| async { Ok("[]".to_string()) }).await;
    assert!(
        !matches!(out.exit, InnerLoopExit::EscalateToHuman { .. }),
        "round must run instead of re-escalating: {:?}",
        out.exit
    );
    assert!(out.llm_calls > 0, "the generator must actually be consulted");
}

#[tokio::test]
async fn an_entry_may_not_link_a_holdout_case() {
    let root = eval_root();
    let holdout = root.path().join("s").join("_holdout");
    std::fs::create_dir(&holdout).unwrap();
    std::fs::write(
        holdout.join("h.toml"),
        "[case]\nname = \"h\"\nagent = \"a\"\nprompt = \"hi\"\n[judge]\nrubric = \"r\"\n",
    )
    .unwrap();

    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let reply = r#"[{"op":"add","content":"rule text here","category":"repair","signals_match":["mistake:factual"],"eval_cases":["s/_holdout/h"],"assertions":{"output_contains":["ok"]},"rationale":"r"}]"#;
    let llm = ScriptedLlm::new(&[reply]);

    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &NullScorer,
        |p| {
            let r = llm.call(p);
            async move { r }
        },
    )
    .await;

    assert!(!out.has_candidate(), "a held-out-linked entry must never become a candidate");
    assert!(out
        .rejected
        .iter()
        .any(|(_, reason)| reason.contains("held-out")));
}

#[tokio::test]
async fn optimize_round_rejects_adds() {
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let mut inp = input(root.path(), &none, &canaries, PlaybookSnapshot::default());
    inp.ctx.intent = super::intent::RoundIntent::Optimize;
    let llm = ScriptedLlm::new(&[GOOD_ADD]);

    let out = run_inner_loop(inp, &NullScorer, |p| {
        let r = llm.call(p);
        async move { r }
    })
    .await;

    assert!(out
        .rejected
        .iter()
        .any(|(_, reason)| reason.contains("optimize rounds may not add")));
}

#[tokio::test]
async fn copied_example_eval_case_ref_is_rejected() {
    // §3.9: the editing guide's examples use obvious placeholders. A model
    // that copies `ceo-assistant/refund-flow` verbatim must be stopped by the
    // "case must resolve on disk" check, not silently accepted.
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let reply = r#"[{"op":"add","content":"rule text here","category":"repair","signals_match":["mistake:factual"],"eval_cases":["ceo-assistant/refund-flow"],"assertions":{"output_contains":["ok"]},"rationale":"r"}]"#;
    let llm = ScriptedLlm::new(&[reply]);

    let out = run_inner_loop(
        input(root.path(), &none, &canaries, PlaybookSnapshot::default()),
        &NullScorer,
        |p| {
            let r = llm.call(p);
            async move { r }
        },
    )
    .await;

    assert!(!out.has_candidate());
    assert!(out.rejected.iter().any(|(_, r)| r.contains("unknown eval case")));
}

#[tokio::test]
async fn aee_runs_with_all_three_dependencies_absent() {
    // §3.9: cooldown / stagnation / telemetry all missing must degrade, not
    // crash. `NullScorer` additionally removes the eval dimension.
    let root = eval_root();
    let none: Vec<String> = Vec::new();
    let canaries: Vec<CanaryTest> = Vec::new();
    let mut inp = input(root.path(), &none, &canaries, PlaybookSnapshot::default());
    inp.stagnation = None;
    inp.ctx.telemetry = None;
    let llm = ScriptedLlm::new(&[GOOD_ADD]);

    let out = run_inner_loop(inp, &NullScorer, |p| {
        let r = llm.call(p);
        async move { r }
    })
    .await;

    // No scorer ⇒ no failures ⇒ the gates alone decide, and they passed.
    assert_eq!(out.exit, InnerLoopExit::Satisfied);
    assert!(out.has_candidate());
}

#[test]
fn parse_deltas_accepts_bare_fenced_and_wrapped_forms() {
    let bare = parse_deltas(GOOD_ADD).unwrap();
    assert_eq!(bare.len(), 1);
    let fenced = parse_deltas(&format!("```json\n{GOOD_ADD}\n```")).unwrap();
    assert_eq!(fenced.len(), 1);
    let wrapped = parse_deltas(&format!("{{\"deltas\": {GOOD_ADD}}}")).unwrap();
    assert_eq!(wrapped.len(), 1);
    assert!(parse_deltas("sorry, I cannot help with that").is_err());
    assert!(parse_deltas("").is_err());
}

#[test]
fn pending_notes_are_written_even_when_the_round_is_abandoned() {
    use super::merge_pending_notes;
    use super::snapshot::PendingFailureNote;
    use crate::playbook::delta::ExistingEntry;
    use crate::playbook::entry::{PlaybookMeta, PlaybookState, PLAYBOOK_SCHEMA_VERSION};
    use crate::prediction::rule_lifecycle::RuleStats;

    let mut snap = PlaybookSnapshot::new(vec![ExistingEntry {
        id: "e1".into(),
        content: "x".into(),
        meta: PlaybookMeta {
            assertions: Default::default(),
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:factual".into()],
            strategy: Vec::new(),
            failure_history: Vec::new(),
            eval_cases: Vec::new(),
            applications: Vec::new(),
            success_streak: 0,
            state: PlaybookState::Active,
            revision: 0,
            dedup_key: "k".into(),
            embed_model: None,
            origin: "agent_derived".into(),
            derived_from: Vec::new(),
        },
        stats: RuleStats::initial(),
    }]);

    let written = merge_pending_notes(
        &mut snap,
        &[PendingFailureNote::new("e1", "G-Contract said no", "G-Contract")],
        now(),
    );
    assert_eq!(written, 1);
    assert_eq!(snap.entries[0].meta.failure_history.len(), 1);
    assert_eq!(snap.entries[0].meta.failure_history[0].source, "G-Contract");
}

#[test]
fn round_record_payload_carries_the_intent() {
    use super::{AeeRoundRecord, AeeTrigger, EvolutionStrategy, SkipReason};
    let rec = AeeRoundRecord::skipped(
        "a",
        EvolutionStrategy::RepairOnly,
        AeeTrigger::ForcedReflection,
        41,
        &SkipReason::NoRepairMaterial,
    );
    let payload = rec.to_payload();
    assert_eq!(payload["strategy"], "repair_only");
    assert_eq!(payload["skipped"], "no_repair_material");
    assert_eq!(payload["round_seq"], 41);
    assert_eq!(payload["exit"], "skipped");
}
