//! Third review fixes: the turn ↔ message link read across namespaces (F1),
//! reaffirming turns (F6, review question 1), and a long conversation's turn
//! tombstones (F2).

use super::*;
use crate::lineage::test_support::*;
use crate::lineage::{FenceReason, Provenance, SourceKind, SourceRef};
use duduclaw_core::types::MemoryLayer;

const S: &str = "webchat:agent:oc#c2";
const B: &str = "agent-b";

fn turn(t: &str) -> SourceRef {
    SourceRef::other(SourceKind::McpTurn, S, format!("turn:{t}"), Utc::now())
}

fn turn_write(t: &str, seq: i64) -> Provenance {
    Provenance::Sources(vec![turn(t), msg(S, seq)])
}

/// What employee B's dispatched run records when A's turn `t` sent it work:
/// the upstream turn (complete pair) and B's own run, no user message.
fn dispatched(t: &str, run_key: &str) -> Provenance {
    Provenance::Sources(vec![turn(t), run("dispatch:agent-b", run_key, Utc::now())])
}

async fn put_in(e: &SqliteMemoryEngine, ns: &str, content: &str, prov: Provenance) -> String {
    match e
        .store_temporal_outcome(
            ns,
            entry(content, MemoryLayer::Semantic),
            TemporalMeta::default(),
            prov,
        )
        .await
        .unwrap()
    {
        crate::supersession_guard::TemporalWriteOutcome::Stored(id) => id,
        o => panic!("expected Stored, got {o:?}"),
    }
}

async fn plan_in(e: &SqliteMemoryEngine, ns: &str, sel: &ForgetSelector) -> ForgetPlan {
    match e
        .plan_forget_source(ns, sel, PlanOptions::default(), &ExternalInputs::default())
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => *p,
        o => panic!("expected a plan, got {o:?}"),
    }
}

/// F1: A's turn (message 1) dispatched work to B. Following the printed
/// "other namespace" command (`--message 1` in B) reaches B's write, even
/// after A's forget removed A's own lineage, and fences B's later writes
/// from that turn.
#[tokio::test]
async fn a_message_plan_in_the_dispatched_namespace_reaches_its_turn() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(
        &e,
        "A stored in turn 9",
        TemporalMeta::default(),
        turn_write("t9", 1),
    )
    .await;
    let b_row = put_in(&e, B, "B stored for turn 9", dispatched("t9", "r1")).await;
    // A forgets first; its lineage rows of the turn go with its rows.
    forget(&e, &by_message(S, &["m:1"])).await;

    let p = plan_in(&e, B, &by_message(S, &["m:1"])).await;
    assert_eq!(p.document.body.linked_turns, vec!["turn:t9".to_string()]);
    assert!(
        p.document.body.targets.iter().any(|t| t.id == b_row),
        "{:?}",
        p.document.body.targets
    );
    apply(&e, &p.plan_id).await;
    assert!(!exists(&e, &b_row).await);
    let o = e
        .store_temporal_outcome(
            B,
            entry("again", MemoryLayer::Semantic),
            TemporalMeta::default(),
            dispatched("t9", "r2"),
        )
        .await
        .unwrap();
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
    // Tombstones stay in B's namespace: A's other turns are untouched.
    put_in(&e, B, "B for turn 10", dispatched("t10", "r3")).await;
}

/// Review question 1: one memory written directly in turn A (message 5) and
/// written again in turn B (message 9, a reaffirmation). Forgetting message
/// 5 links only turn A; turn B keeps writing.
#[tokio::test]
async fn a_reaffirmation_in_a_later_turn_is_not_linked_to_the_first_message() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let meta = || triple("user", "likes", "tea");
    let x = put(&e, "user likes tea", meta(), turn_write("tA", 5)).await;
    let o = try_put(&e, "user likes tea", meta(), turn_write("tB", 9)).await;
    assert!(
        !matches!(
            o,
            crate::supersession_guard::TemporalWriteOutcome::Fenced(_)
        ),
        "{o:?}"
    );
    let role: String = {
        let conn = e.conn_for_maintenance().await;
        conn.query_row(
            "SELECT role FROM memory_origins WHERE memory_id = ?1 AND source_message = 'turn:tB'",
            [&x],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(role, "reaffirm");
    let p = plan(&e, &by_message(S, &["m:5"])).await;
    assert_eq!(p.document.body.linked_turns, vec!["turn:tA".to_string()]);
    apply(&e, &p.plan_id).await;
    put(
        &e,
        "turn B goes on",
        TemporalMeta::default(),
        Provenance::Sources(vec![turn("tB")]),
    )
    .await;
}

/// F6: a turn whose only write was a reaffirmation is still linked to its
/// message (the link table records the write's own sources), so forgetting
/// that message removes the turn's corroboration row and fences the turn.
#[tokio::test]
async fn a_reaffirm_only_turn_is_linked_and_fenced() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let meta = || triple("user", "likes", "tea");
    let x = put(&e, "user likes tea", meta(), turn_write("tA", 5)).await;
    try_put(&e, "user likes tea", meta(), turn_write("tB", 9)).await;
    let p = plan(&e, &by_message(S, &["m:9"])).await;
    assert_eq!(p.document.body.linked_turns, vec!["turn:tB".to_string()]);
    assert!(p.document.body.targets.is_empty());
    apply(&e, &p.plan_id).await;
    assert!(
        exists(&e, &x).await,
        "the memory itself stays (reaffirm only)"
    );
    let left = count(
        &e,
        "SELECT COUNT(*) FROM memory_origins WHERE source_message IN ('turn:tB', 'm:9')",
    )
    .await;
    assert_eq!(left, 0);
    let o = try_put(
        &e,
        "replay",
        TemporalMeta::default(),
        Provenance::Sources(vec![turn("tB")]),
    )
    .await;
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
}

/// F2 measurement: a conversation with 5,000 turns. Plan and apply each
/// finish in reasonable time; every turn key gets a tombstone; a later write
/// in the session is still checked quickly.
#[tokio::test]
async fn a_conversation_with_5000_turns_plans_and_applies_in_reasonable_time() {
    const N: usize = 5_000;
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let started = std::time::Instant::now();
    for i in 0..N {
        put(
            &e,
            &format!("turn {i} fact"),
            TemporalMeta::default(),
            Provenance::Sources(vec![turn(&format!("t{i}"))]),
        )
        .await;
    }
    let seeded = started.elapsed();
    let upto = Utc::now() + chrono::Duration::seconds(1);
    let t = std::time::Instant::now();
    let p = plan(&e, &by_session(S, None, upto)).await;
    let planned = t.elapsed();
    assert_eq!(p.document.body.tombstones.len(), N + 1);
    assert_eq!(p.document.body.targets.len(), N);
    let t = std::time::Instant::now();
    apply(&e, &p.plan_id).await;
    let applied = t.elapsed();
    let t = std::time::Instant::now();
    put(
        &e,
        "a new turn",
        TemporalMeta::default(),
        Provenance::Sources(vec![SourceRef::other(
            SourceKind::McpTurn,
            S,
            "turn:new",
            upto + chrono::Duration::days(1),
        )]),
    )
    .await;
    let next_write = t.elapsed();
    eprintln!(
        "F2 measurement (debug build, in-memory, {N} turns): seed {seeded:?}, \
         plan {planned:?}, apply {applied:?}, next write {next_write:?}"
    );
    assert!(planned.as_secs() < 60 && applied.as_secs() < 60);
    assert!(next_write.as_millis() < 2_000);
}
