//! WP2.6 — end-to-end wiring tests for the AEE live path.
//!
//! Everything here drives the **real** chain (`GvuLoop` mode switch →
//! `run_aee_round` → inner loop → Gate → Measure → commit gate →
//! `playbook::store::apply_deltas` → champion → settlement queue →
//! `settle_pending`), with exactly two things faked:
//!
//! 1. the LLM (a scripted closure — same shape every production caller hands
//!    `GvuLoop`), and
//! 2. the `duduclaw` binary the eval scorer spawns (a shell script, the same
//!    technique `eval_runner`'s own tests use).
//!
//! Nothing else is stubbed: the memory engine is a real SQLite file, the
//! playbook is really written and re-read, and the champion / settlement rows
//! really land in `evolution.db`.

use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::gvu::champion::ChampionStore;
use crate::gvu::loop_::{GvuLoop, GvuOutcome};
use crate::gvu::version_store::{VersionMetrics, VersionStore};

use super::eval_scorer::EvalMeasureScorer;
use super::pending::PendingSettlementStore;

// ── Fixture -------------------------------------------------------------

/// A DuDuClaw home with one agent, one eval suite, and a fake `duduclaw`
/// binary that reports every case as a pass.
struct Home {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    agent_dir: PathBuf,
}

const AGENT: &str = "aee-wiring";

#[cfg(unix)]
fn fake_binary(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("fake-duduclaw.sh");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "#!/bin/sh").unwrap();
    write!(f, "{script}").unwrap();
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Passes `c1`/`c2`, and fails whichever ids are named in `fail_ids`.
#[cfg(unix)]
fn eval_script(fail_ids: &[&str]) -> String {
    let mut arms = String::new();
    for id in fail_ids {
        arms.push_str(&format!("  \"{id}\") passed=false ;;\n"));
    }
    format!(
        r#"
report=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--report" ]; then report="$arg"; fi
  prev="$arg"
done
entries=""
for id in c1 c2 h1; do
  passed=true
  case "$id" in
{arms}  esac
  if [ -n "$entries" ]; then entries="$entries,"; fi
  entries="$entries{{\"id\":\"$id\",\"passed\":$passed,\"failed_assertions\":[]}}"
done
cat > "$report" <<EOF
{{"suite":"{AGENT}","total":3,"passed":3,"per_case":[$entries]}}
EOF
exit 0
"#
    )
}

/// The whole-suite (outer) measurement always asks for `c2`; the inner loop
/// only ever asks for the delta's linked case (`c1`). That is the cheapest
/// honest way for a fake binary to tell the two apart without coupling itself
/// to how many times the inner loop happens to run.
///
/// Outer call #1 (the champion bootstrap) fails `c1` and passes `h1`;
/// outer call #2 (the candidate) passes `c1` and fails `h1`. The mixed suite
/// mean is 2/3 on both sides — so the ONLY dimension that moves is the
/// held-out one, which is precisely the visible-gain-pays-for-held-out-loss
/// pattern WP-4E's fence exists to catch.
#[cfg(unix)]
fn held_out_flip_eval_script(counter: &Path) -> String {
    let counter = counter.display();
    format!(
        r#"
report=""
cases=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--report" ]; then report="$arg"; fi
  if [ "$prev" = "--case" ]; then cases="$arg"; fi
  prev="$arg"
done
fail_c1=no
fail_h1=no
case "$cases" in
  *c2*)
    n=0
    if [ -f "{counter}" ]; then n=$(cat "{counter}"); fi
    n=$((n + 1))
    echo "$n" > "{counter}"
    if [ "$n" = "1" ]; then fail_c1=yes; else fail_h1=yes; fi
    ;;
esac
entries=""
for id in c1 c2 h1; do
  passed=true
  if [ "$id" = "c1" ] && [ "$fail_c1" = "yes" ]; then passed=false; fi
  if [ "$id" = "h1" ] && [ "$fail_h1" = "yes" ]; then passed=false; fi
  if [ -n "$entries" ]; then entries="$entries,"; fi
  entries="$entries{{\"id\":\"$id\",\"passed\":$passed,\"failed_assertions\":[]}}"
done
cat > "$report" <<EOF
{{"suite":"{AGENT}","total":3,"passed":3,"per_case":[$entries]}}
EOF
exit 0
"#
    )
}

#[cfg(unix)]
fn home_with(agent_toml: &str, eval_fail_ids: &[&str]) -> Home {
    let owned: Vec<String> = eval_fail_ids.iter().map(|s| (*s).to_string()).collect();
    home_with_script(agent_toml, move |_root| {
        let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
        eval_script(&borrowed)
    })
}

/// [`home_with`] with the fake binary's behaviour supplied by the caller. The
/// closure receives the home root so a stateful script can put its counter
/// file inside the fixture instead of a shared temp dir.
#[cfg(unix)]
fn home_with_script(agent_toml: &str, script: impl FnOnce(&Path) -> String) -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    let agent_dir = root.join("agents").join(AGENT);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("SOUL.md"),
        "# Agent\n\n## \u{6838}\u{5FC3}\u{50F9}\u{503C}\n\n- \u{8AA0}\u{5BE6}\n",
    )
    .unwrap();
    std::fs::write(agent_dir.join("agent.toml"), agent_toml).unwrap();

    // Eval suite: two visible cases plus one held-out.
    let suite = root.join("evals").join(AGENT);
    std::fs::create_dir_all(suite.join("held-out")).unwrap();
    for id in ["c1", "c2"] {
        std::fs::write(
            suite.join(format!("{id}.toml")),
            format!("[case]\nname = \"{id}\"\nagent = \"{AGENT}\"\nprompt = \"hi\"\n"),
        )
        .unwrap();
        // A recorded transcript beside each case — without one, replay cannot
        // measure the case at all and the scorer reports the dimension absent.
        std::fs::write(suite.join(format!("{id}.transcript.jsonl")), "{}\n").unwrap();
    }
    std::fs::write(
        suite.join("held-out").join("h1.toml"),
        format!("[case]\nname = \"h1\"\nagent = \"{AGENT}\"\nprompt = \"hi\"\n"),
    )
    .unwrap();
    std::fs::write(suite.join("held-out").join("h1.transcript.jsonl"), "{}\n").unwrap();

    let bin = fake_binary(&root, &script(&root));

    // Hermetic wiring: the suites root and the spawned binary are BOTH
    // resolved from this home's `config.toml`, so nothing here touches the
    // process environment (which parallel tests share) and nothing spawns the
    // real `duduclaw`.
    std::fs::write(
        root.join("config.toml"),
        format!(
            "[evolution]\neval_suites_root = \"evals\"\neval_binary = \"{}\"\n",
            bin.display()
        ),
    )
    .unwrap();

    Home { _tmp: tmp, root, agent_dir }
}

impl Home {
    fn db(&self) -> PathBuf {
        self.root.join("evolution.db")
    }
    fn scorer(&self) -> EvalMeasureScorer {
        // Same resolution the production path uses — proving the config
        // surface actually works, rather than hand-wiring around it.
        EvalMeasureScorer::from_home(&self.root)
    }
    fn version_store(&self) -> VersionStore {
        VersionStore::new(&self.db())
    }
    async fn playbook(&self) -> Vec<(duduclaw_core::types::MemoryEntry, crate::playbook::entry::PlaybookMeta, crate::prediction::rule_lifecycle::RuleStats)> {
        let engine =
            duduclaw_memory::SqliteMemoryEngine::new(&self.root.join("memory.db")).unwrap();
        crate::playbook::store::list_active(&engine, AGENT).await
    }
}

/// The AEE default fixture: no `legacy_soul_evolution` key at all.
const AEE_TOML: &str = "[evolution]\ngvu_enabled = true\ngvu_cooldown_minutes = 0\n";

fn add_delta(content: &str, case: &str) -> String {
    serde_json::json!([{
        "op": "add",
        "content": content,
        "category": "repair",
        "signals_match": ["mistake:factual"],
        "eval_cases": [case],
        // WP2.8: new Adds must be E1.
        "assertions": {"output_contains": ["ok"]},
        "rationale": "wiring test",
    }])
    .to_string()
}

/// A scripted LLM: playbook deltas for the generator prompt, an approving
/// verdict for the AEE judge prompt.
fn scripted(delta_json: String) -> impl Fn(String) -> std::future::Ready<Result<String, String>> {
    move |prompt: String| {
        let reply = if prompt.starts_with("You are an evolution quality judge") {
            r#"{"approved": true, "score": 0.9, "feedback": "concrete"}"#.to_string()
        } else {
            delta_json.clone()
        };
        std::future::ready(Ok(reply))
    }
}

async fn run_round(
    home: &Home,
    delta_json: String,
) -> super::run::AeeRoundResult {
    let vs = home.version_store();
    super::run::run_aee_round_with_scorer(
        super::run::AeeRoundInput {
            agent_id: AGENT,
            agent_dir: &home.agent_dir,
            home_dir: home.root.clone(),
            trigger_context: "wiring test",
            must_not: &[],
            must_always: &[],
            relevant_mistakes: &[],
            current_soul: "# Agent\n",
            now: Utc::now(),
        },
        &vs,
        &home.scorer(),
        scripted(delta_json),
    )
    .await
}

// ── The full chain -------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn aee_round_commits_a_delta_and_installs_a_champion() {
    let home = home_with(AEE_TOML, &[]);
    assert!(home.playbook().await.is_empty(), "fixture starts with an empty playbook");

    let result = run_round(
        &home,
        add_delta("\u{56DE}\u{8986}\u{524D}\u{5148}\u{78BA}\u{8A8D}\u{9700}\u{6C42}", &format!("{AGENT}/c1")),
    )
    .await;

    // 1. The delta landed.
    let entry_ids = match &result.verdict {
        super::run::AeeVerdict::Committed { applied, entry_ids, verdict } => {
            assert_eq!(*applied, 1, "one delta applied");
            assert!(
                verdict == "improves" || verdict == "matches",
                "committed under an unexpected verdict: {verdict}"
            );
            entry_ids.clone()
        }
        other => panic!("expected a commit, got {other:?}"),
    };
    assert_eq!(entry_ids.len(), 1);

    let rows = home.playbook().await;
    assert_eq!(rows.len(), 1, "the entry is really in the playbook");
    assert_eq!(rows[0].0.id, entry_ids[0], "the reported id is the stored id");

    // 2. The champion moved to the committed snapshot.
    let champ = ChampionStore::new(&home.db()).get(AGENT).expect("champion row");
    assert!(!champ.snapshot_hash.is_empty());
    assert_eq!(
        champ.snapshot_hash,
        crate::gvu::champion::snapshot_hash(&[rows[0].1.dedup_key.clone()]),
        "champion identity is the committed snapshot, not the pre-commit one"
    );
    assert!(
        champ.measure.cases.iter().any(|c| c.held_out),
        "the single full measurement scores held-out cases (§3.5)"
    );

    // 3. The settlement was enqueued, not settled on the spot.
    let pending = PendingSettlementStore::new(&home.db()).get(AGENT).expect("settlement row");
    assert_eq!(pending.entry_ids, entry_ids);
    assert!(pending.settle_after > pending.applied_at);

    // 4. The audit record carries the intent and the eval availability.
    assert_eq!(result.record.deltas_applied, 1);
    assert!(result.record.case_dimension_available, "eval was reachable in this fixture");
    assert!(result.record.intent.is_some(), "§3.2.4: the intent must be auditable");
}

#[cfg(unix)]
#[tokio::test]
async fn champion_is_bootstrapped_from_the_current_playbook_before_the_first_candidate() {
    let home = home_with(AEE_TOML, &[]);
    let db = home.db();
    assert!(ChampionStore::new(&db).get(AGENT).is_none(), "no champion yet");

    let _ = run_round(&home, add_delta("\u{5148}\u{78BA}\u{8A8D}\u{5F8C}\u{56DE}\u{8986}", &format!("{AGENT}/c1"))).await;

    let champ = ChampionStore::new(&db).get(AGENT).unwrap();
    assert!(champ.round_seq >= 1, "the round counter persisted");
    assert!(
        !champ.measure.cases.is_empty(),
        "the bootstrap measured real cases rather than inventing a baseline"
    );
}

// ── WP-5A: the bootstrap is measured the same shape as the candidate ─────
//
// Before WP-5A the champion bootstrap ran with `include_holdout: false` while
// the candidate measurement ran with `true`. The first comparison of an
// agent's life was therefore "champion's visible-only mean" against
// "candidate's visible+held-out mean" — mixed dimensions on one axis, and the
// `cases_holdout` fence skipped entirely (a dimension present on one side only
// is not comparable). The three tests below lock both directions: the new
// bootstrap carries held-out scores and the fence bites from round one, and a
// champion row persisted in the old shape still compares without panicking.

/// Direction 1 — a freshly bootstrapped champion's stored vector contains
/// held-out case scores.
///
/// Driven through a round that proposes nothing (`[]`), so the champion left
/// in the table is the BOOTSTRAP vector rather than a committed candidate's.
#[cfg(unix)]
#[tokio::test]
async fn bootstrap_champion_is_measured_with_the_held_out_subset() {
    let home = home_with(AEE_TOML, &[]);
    let result = run_round(&home, "[]".to_string()).await;
    assert!(
        matches!(result.verdict, super::run::AeeVerdict::Skipped { .. }),
        "fixture precondition: an empty delta array ends the round before any commit"
    );

    let champ = ChampionStore::new(&home.db()).get(AGENT).expect("bootstrap champion row");
    assert!(
        champ.measure.cases.iter().any(|c| c.held_out),
        "the bootstrap must measure the held-out subset too, or the first round's \
         cases_holdout fence is skipped for want of a champion-side value"
    );
    assert!(
        champ.measure.holdout_cases_mean().is_some(),
        "the held-out sub-dimension is comparable on the champion side"
    );
    assert!(
        champ.measure.visible_cases_mean().is_some(),
        "and so is the visible half — both, not one"
    );
}

/// Direction 2 — with the bootstrap aligned, a first-round candidate that
/// trades a held-out loss for a visible gain is fenced.
///
/// Numbers (see [`held_out_flip_eval_script`]): champion `c1=0 c2=1 h1=1`,
/// candidate `c1=1 c2=1 h1=0`. The mixed `cases` mean is 2/3 on both sides and
/// `cases_visible` improves, so every pre-WP-4E signal says "commit"; only
/// `cases_holdout` (1.0 → 0.0) objects. Before WP-5A the champion had no
/// held-out value at all and this candidate committed.
#[cfg(unix)]
#[tokio::test]
async fn a_first_round_held_out_regression_is_fenced_against_the_bootstrap() {
    let home = home_with_script(AEE_TOML, |root| {
        held_out_flip_eval_script(&root.join("eval-phase.count"))
    });

    let result = run_round(
        &home,
        add_delta("\u{56DE}\u{8986}\u{524D}\u{5148}\u{78BA}\u{8A8D}\u{9700}\u{6C42}", &format!("{AGENT}/c1")),
    )
    .await;

    match &result.verdict {
        super::run::AeeVerdict::NotCommitted { reason, .. } => assert!(
            reason.contains("cases_holdout"),
            "the held-out fence must be the dimension that blocked it; got: {reason}"
        ),
        other => panic!(
            "a held-out regression in the very first round must not commit: {other:?}"
        ),
    }
    assert!(
        home.playbook().await.is_empty(),
        "nothing reached the playbook"
    );
    assert!(
        PendingSettlementStore::new(&home.db()).get(AGENT).is_none(),
        "and no settlement was queued for a round that committed nothing"
    );
}

/// Direction 3 — a champion row written in the OLD shape (no held-out scores)
/// keeps comparing, and self-heals.
///
/// Deliberately asserts the *unfenced* outcome: `cases_holdout` is present on
/// the candidate side only, so it is skipped exactly as any one-sided
/// dimension is, and the candidate commits on the mixed `cases` gain. That is
/// the documented cost of not retro-measuring stored champions — one round,
/// after which the committed candidate's own held-out-inclusive vector becomes
/// the champion and the fence is live again. What must never happen is a panic
/// or a champion that stops being comparable at all.
#[cfg(unix)]
#[tokio::test]
async fn a_legacy_champion_without_held_out_scores_still_compares_and_self_heals() {
    // `h1` fails every time; only the champion's shape distinguishes this test
    // from the fenced one above.
    let home = home_with(AEE_TOML, &["h1"]);
    let store = ChampionStore::new(&home.db());

    // A pre-WP-5A bootstrap: visible cases only, no held-out entry.
    store
        .put(&crate::gvu::champion::Champion {
            agent_id: AGENT.to_string(),
            snapshot_hash: crate::gvu::champion::snapshot_hash(&["legacy".to_string()]),
            measure: crate::gvu::verifier_measure::MeasureVector {
                cases: vec![
                    crate::gvu::verifier_measure::CaseScore {
                        case: format!("{AGENT}/c1"),
                        score: 0.0,
                        held_out: false,
                    },
                    crate::gvu::verifier_measure::CaseScore {
                        case: format!("{AGENT}/c2"),
                        score: 1.0,
                        held_out: false,
                    },
                ],
                judge: None,
                anti_sycophancy: 1.0,
                novelty: 1.0,
                relevance: 0.5,
                hard_zero: false,
            },
            established_at: Utc::now(),
            anti_drift: Default::default(),
            round_seq: 7,
            holdout_rotation_due: false,
        })
        .unwrap();

    let result = run_round(
        &home,
        add_delta("\u{56DE}\u{8986}\u{524D}\u{5148}\u{78BA}\u{8A8D}\u{9700}\u{6C42}", &format!("{AGENT}/c1")),
    )
    .await;
    assert!(
        matches!(result.verdict, super::run::AeeVerdict::Committed { .. }),
        "an old-shaped champion must stay comparable (one-sided dimension skipped), \
         not break the round: {:?}",
        result.verdict
    );

    let champ = store.get(AGENT).expect("champion row");
    assert!(
        champ.measure.cases.iter().any(|c| c.held_out),
        "self-heal: the committed candidate's held-out-inclusive vector is the new champion, \
         so the next round IS fenced"
    );
    assert!(
        champ.measure.holdout_cases_mean().is_some_and(|m| m < 0.5),
        "and it records the real held-out failure rather than dropping it"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_pending_settlement_blocks_the_next_round() {
    let home = home_with(AEE_TOML, &[]);
    let first = run_round(&home, add_delta("\u{5148}\u{78BA}\u{8A8D}\u{9700}\u{6C42}\u{518D}\u{56DE}", &format!("{AGENT}/c1"))).await;
    assert!(matches!(first.verdict, super::run::AeeVerdict::Committed { .. }));

    // §2.4.3 companion 2: the committed round owns its observation window.
    let second = run_round(&home, add_delta("\u{53E6}\u{4E00}\u{689D}\u{898F}\u{5247}\u{5167}\u{5BB9}", &format!("{AGENT}/c2"))).await;
    match second.verdict {
        super::run::AeeVerdict::Skipped { reason } => {
            assert!(reason.contains("settlement window"), "unexpected skip: {reason}");
        }
        other => panic!("a second round must wait for the settlement window: {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn settle_confirms_a_holding_entry_and_clears_the_queue() {
    let home = home_with(AEE_TOML, &[]);
    let _ = run_round(&home, add_delta("\u{78BA}\u{8A8D}\u{5F8C}\u{624D}\u{56DE}\u{8986}", &format!("{AGENT}/c1"))).await;

    let db = home.db();
    let store = PendingSettlementStore::new(&db);
    let mut pending = store.get(AGENT).unwrap();
    // Fast-forward the window rather than sleeping 24h.
    pending.settle_after = Utc::now() - chrono::Duration::seconds(1);
    store.put(&pending).unwrap();

    let due = store.due(Utc::now());
    assert_eq!(due.len(), 1);

    let report = super::run::settle_pending(&due[0], &db, &home.root, &home.scorer())
        .await
        .expect("a verdict was reachable");
    assert_eq!(report.confirmed, 1, "the entry's linked case held → confirm");
    assert!(report.rolled_back.is_empty());
    assert!(store.get(AGENT).is_none(), "the settlement queue is drained");

    let rows = home.playbook().await;
    assert_eq!(rows[0].1.success_streak, 1, "a confirm increments the streak");
    assert_ne!(
        rows[0].1.state,
        crate::playbook::entry::PlaybookState::Retired,
        "a confirmed entry stays live"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn settle_retires_only_the_entry_whose_linked_case_regressed() {
    let home = home_with(AEE_TOML, &[]);
    let _ = run_round(&home, add_delta("\u{5148}\u{6838}\u{5C0D}\u{6578}\u{5B57}\u{518D}\u{56DE}", &format!("{AGENT}/c1"))).await;

    let db = home.db();
    let store = PendingSettlementStore::new(&db);
    let mut pending = store.get(AGENT).unwrap();
    pending.settle_after = Utc::now() - chrono::Duration::seconds(1);
    store.put(&pending).unwrap();

    // Re-score with c1 now failing: the entry's own linked case regressed.
    let failing = EvalMeasureScorer::with_binary(
        fake_binary(&home.root.join("agents"), &eval_script(&["c1"])),
        home.root.join("evals"),
    );
    let report = super::run::settle_pending(&store.due(Utc::now())[0], &db, &home.root, &failing)
        .await
        .expect("a verdict was reachable");
    assert_eq!(report.rolled_back.len(), 1, "the regressing entry is retired");
    assert_eq!(report.confirmed, 0);

    let rows = home.playbook().await;
    assert_eq!(
        rows[0].1.state,
        crate::playbook::entry::PlaybookState::Retired,
        "entry-level rollback actually persisted"
    );
    assert_eq!(rows[0].1.success_streak, 0, "a rollback zeroes the streak");
}

// ── Degradation ----------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn eval_unavailable_degrades_without_blocking_evolution() {
    let home = home_with(AEE_TOML, &[]);
    // A scorer whose binary does not exist: every score call is an Err.
    let broken = EvalMeasureScorer::with_binary(
        home.root.join("no-such-binary"),
        home.root.join("evals"),
    );
    let vs = home.version_store();
    let result = super::run::run_aee_round_with_scorer(
        super::run::AeeRoundInput {
            agent_id: AGENT,
            agent_dir: &home.agent_dir,
            home_dir: home.root.clone(),
            trigger_context: "wiring test",
            must_not: &[],
            must_always: &[],
            relevant_mistakes: &[],
            current_soul: "# Agent\n",
            now: Utc::now(),
        },
        &vs,
        &broken,
        scripted(add_delta("\u{56DE}\u{8986}\u{524D}\u{5148}\u{6838}\u{5C0D}", &format!("{AGENT}/c1"))),
    )
    .await;

    assert!(
        matches!(result.verdict, super::run::AeeVerdict::Committed { .. }),
        "an unreachable eval must degrade, not block: {:?}",
        result.verdict
    );
    assert!(
        !result.record.case_dimension_available,
        "the degradation is visible in the audit record, not silent"
    );
    assert!(
        ChampionStore::new(&home.db())
            .get(AGENT)
            .is_some_and(|c| c.measure.cases.is_empty()),
        "the champion carries no fabricated case scores"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_empty_delta_array_is_a_respectable_answer_not_a_failure() {
    let home = home_with(AEE_TOML, &[]);
    let result = run_round(&home, "[]".to_string()).await;
    match result.verdict {
        super::run::AeeVerdict::Skipped { reason } => {
            assert!(reason.contains("no change"), "unexpected reason: {reason}");
        }
        other => panic!("`[]` must not be treated as a failure: {other:?}"),
    }
    assert!(home.playbook().await.is_empty(), "nothing was written");
    assert!(
        PendingSettlementStore::new(&home.db()).get(AGENT).is_none(),
        "no settlement is queued for a round that changed nothing"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_delta_linking_a_held_out_case_never_commits() {
    let home = home_with(AEE_TOML, &[]);
    // §3.5 lock 2 — the ref names the held-out case.
    let result = run_round(
        &home,
        add_delta("\u{9019}\u{689D}\u{898F}\u{5247}\u{60F3}\u{7DAD}\u{984C}", &format!("{AGENT}/h1")),
    )
    .await;
    assert!(
        !matches!(result.verdict, super::run::AeeVerdict::Committed { .. }),
        "a held-out link must never reach the playbook: {:?}",
        result.verdict
    );
    assert!(home.playbook().await.is_empty());
}

// ── The escape hatch -----------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn legacy_escape_hatch_still_takes_the_soul_path() {
    // The switch is asserted behaviourally: with the flag ON, the loop asks
    // the LLM for a SOUL patch (the legacy Generator prompt) and never for
    // playbook deltas; with it OFF (the default) it is the other way round.
    async fn first_prompt(agent_toml: &str) -> String {
        let home = home_with(agent_toml, &[]);
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let sink = seen.clone();
        let gvu = GvuLoop::new(&home.db(), Some(24.0), Some(1));
        let _ = gvu
            .run(
                AGENT,
                &home.agent_dir,
                "trigger",
                VersionMetrics::default(),
                &[],
                &[],
                move |prompt: String| {
                    sink.lock().unwrap().push(prompt);
                    // An unparseable reply on either path — we only care which
                    // prompt was built.
                    std::future::ready(Ok::<String, String>("not json".to_string()))
                },
            )
            .await;
        let prompts = seen.lock().unwrap();
        prompts.first().cloned().unwrap_or_default()
    }

    let legacy = first_prompt(
        "[evolution]\ngvu_enabled = true\ngvu_cooldown_minutes = 0\nlegacy_soul_evolution = true\n",
    )
    .await;
    assert!(
        legacy.starts_with("You are the evolution engine for agent"),
        "the escape hatch must reach the legacy SOUL generator; got: {}",
        duduclaw_core::truncate_chars(&legacy, 120)
    );

    let default = first_prompt(AEE_TOML).await;
    assert!(
        default.starts_with("You are the playbook editor"),
        "AEE is the default path; got: {}",
        duduclaw_core::truncate_chars(&default, 120)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gvu_loop_reports_a_committed_round_as_playbook_evolved() {
    let home = home_with(AEE_TOML, &[]);
    let gvu = GvuLoop::new(&home.db(), Some(24.0), Some(1));
    let delta = add_delta(
        "\u{56DE}\u{8986}\u{524D}\u{5148}\u{78BA}\u{8A8D}\u{5C0D}\u{65B9}\u{7684}\u{5BE6}\u{969B}\u{554F}\u{984C}",
        &format!("{AGENT}/c1"),
    );
    let outcome = gvu
        .run(
            AGENT,
            &home.agent_dir,
            "trigger",
            VersionMetrics::default(),
            &[],
            &[],
            scripted(delta),
        )
        .await;

    match outcome {
        GvuOutcome::PlaybookEvolved { applied, entry_ids, .. } => {
            assert_eq!(applied, 1);
            assert_eq!(entry_ids.len(), 1);
        }
        other => panic!("expected PlaybookEvolved from the live loop, got {other:?}"),
    }
    // SOUL.md is untouched: AEE writes playbook entries, never the persona.
    let soul = std::fs::read_to_string(home.agent_dir.join("SOUL.md")).unwrap();
    assert!(soul.starts_with("# Agent"), "SOUL.md must be byte-identical after an AEE round");
    assert!(!soul.contains("\u{78BA}\u{8A8D}\u{5C0D}\u{65B9}"), "the delta did not leak into SOUL.md");
}

// ── WP3.1: B-window replay scenarios ─────────────────────────────────────
//
// The B-window incident (`TODO-evolution-v3-2026-08.md` Phase 3): a
// ceo-assistant home whose SOUL.md had grown past the size cap while GVU
// burned ~20 straight all-rejected runs against it — cap deadlock plus
// unlimited retries. These tests replay the two failure signatures against
// the AEE path (the original `live_b_window_replay.rs` file was retired by
// the Evolution v3 line; WP0.4's ExpiredNoData half already has direct unit
// coverage in `gvu::updater`, so it is not duplicated here).

/// B-window signature 1 — an over-cap SOUL.md must route into WP0.2's
/// consolidate mode instead of the incident's cap deadlock (where every
/// proposal was rejected forever with no way out). The consolidation
/// *success* path (valid shrink, immutable sections byte-identical) has its
/// own 20-test suite in `gvu::consolidate`; the wiring fact asserted here is
/// the ROUTING — the loop enters consolidation (visible in the Skip reason
/// when the scripted LLM cannot answer the consolidation prompt) and leaves
/// the persona file untouched rather than freezing silently.
#[cfg(unix)]
#[tokio::test]
async fn b_window_supercap_soul_routes_to_consolidation_not_deadlock() {
    let home = home_with(AEE_TOML, &[]);
    // ~9.5KB of Evolvable-section bloat, mirroring the incident's SOUL.md.
    let mut soul = String::from("# Agent\n\n## \u{6838}\u{5FC3}\u{50F9}\u{503C}\n\n- \u{8AA0}\u{5BE6}\n\n## \u{884C}\u{70BA}\u{898F}\u{5247}\n\n");
    while soul.len() < 9_500 {
        soul.push_str("- \u{6BCF}\u{6B21}\u{56DE}\u{8986}\u{524D}\u{5148}\u{78BA}\u{8A8D}\u{9700}\u{6C42}\u{8207}\u{9A57}\u{6536}\u{6A19}\u{6E96}\n");
    }
    std::fs::write(home.agent_dir.join("SOUL.md"), &soul).unwrap();

    let gvu = GvuLoop::new(&home.db(), Some(24.0), Some(1));
    let delta = add_delta("\u{9000}\u{6B3E}\u{524D}\u{5148}\u{67E5}\u{8A02}\u{55AE}\u{72C0}\u{614B}", &format!("{AGENT}/c1"));
    let outcome = gvu
        .run(AGENT, &home.agent_dir, "b-window", VersionMetrics::default(), &[], &[], scripted(delta))
        .await;
    match &outcome {
        GvuOutcome::Skipped { reason } => assert!(
            reason.contains("Consolidation") || reason.contains("\u{6574}\u{4F75}"),
            "over-cap SOUL must visibly route to consolidate mode, got: {reason}"
        ),
        other => panic!("expected the consolidation route (Skipped with a consolidation reason under an uncooperative LLM), got {other:?}"),
    }
    // No partial write: the bloated persona is byte-identical afterward.
    assert_eq!(std::fs::read_to_string(home.agent_dir.join("SOUL.md")).unwrap(), soul);
}

/// B-window signature 2 — a second immediate attempt is visibly
/// rate-limited (cooldown), instead of the incident's ~20 unlimited retries.
/// Accepts the production "cooldown" phrasing alongside "rate" — the retired
/// test once went red purely because its accepted wording was narrower than
/// the real message.
#[cfg(unix)]
#[tokio::test]
async fn b_window_second_attempt_is_rate_limited_not_retried() {
    let home = home_with("[evolution]\ngvu_enabled = true\n", &[]);
    let gvu = GvuLoop::new(&home.db(), Some(48.0), Some(5));
    let failing = |_p: String| std::future::ready(Err::<String, String>("no creds".to_string()));

    let _first = gvu
        .run(AGENT, &home.agent_dir, "first", VersionMetrics::default(), &[], &[], failing)
        .await;
    let second = gvu
        .run(AGENT, &home.agent_dir, "second", VersionMetrics::default(), &[], &[], failing)
        .await;

    match &second {
        GvuOutcome::Skipped { reason } => {
            let lower = reason.to_lowercase();
            assert!(
                lower.contains("cooldown") || lower.contains("rate"),
                "second run must be visibly rate-limited, got: {reason}"
            );
        }
        other => panic!("expected the second run to be Skipped by cooldown, got {other:?}"),
    }
}
