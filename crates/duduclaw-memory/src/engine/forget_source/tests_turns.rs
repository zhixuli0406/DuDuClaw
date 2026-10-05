//! Live-verification fixes (2026-10-05): a forgotten message also forgets
//! its turn's `mcp_turn` key (issue 1) and does not report that key as
//! another source (issue 2); stale refusals name every reason (issue 6);
//! targets say whether they were found directly; the upstream-unknown
//! marker counts as untracked and is never a source.

use super::*;
use crate::lineage::test_support::*;
use crate::lineage::{FenceReason, Provenance, SourceKind, SourceRef};

const S: &str = "webchat:agent:oc#c1";

fn turn(t: &str) -> SourceRef {
    SourceRef::other(SourceKind::McpTurn, S, format!("turn:{t}"), Utc::now())
}

/// What an MCP write during the turn of user message `seq` records.
fn turn_write(t: &str, seq: i64) -> Provenance {
    Provenance::Sources(vec![turn(t), msg(S, seq)])
}

fn turn_only(t: &str) -> Provenance {
    Provenance::Sources(vec![turn(t)])
}

#[tokio::test]
async fn forgetting_a_message_fences_a_write_that_names_only_its_turn() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let a = put(
        &e,
        "stored in turn 5",
        TemporalMeta::default(),
        turn_write("t5", 5),
    )
    .await;
    let p = plan(&e, &by_message(S, &["m:5", "m:6"])).await;
    // The turn key is a tombstone of its own: three, not two.
    assert_eq!(p.document.body.tombstones.len(), 3);
    let t = &p.document.body.targets[0];
    assert_eq!(t.id, a);
    assert!(t.direct);
    // Issue 2: the same turn is not "another source".
    assert!(t.other_sources.is_empty(), "{:?}", t.other_sources);
    assert!(p.document.body.collateral.is_empty());
    apply(&e, &p.plan_id).await;
    assert!(!exists(&e, &a).await);
    // The live repro: same turn and session, no user message seq.
    let o = try_put(&e, "replayed", TemporalMeta::default(), turn_only("t5")).await;
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
    // A later turn of the same session still writes.
    put(
        &e,
        "next turn",
        TemporalMeta::default(),
        turn_write("t7", 7),
    )
    .await;
    put(
        &e,
        "next turn, turn only",
        TemporalMeta::default(),
        turn_only("t8"),
    )
    .await;
}

#[tokio::test]
async fn an_unrelated_turn_is_not_linked_by_inherited_lineage() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let a = put(
        &e,
        "from message 5",
        TemporalMeta::default(),
        src(msg(S, 5)),
    )
    .await;
    let b = put(&e, "turn 9 only", TemporalMeta::default(), turn_only("t9")).await;
    // A derived row inherits both, but no single write recorded them together.
    put(
        &e,
        "derived",
        TemporalMeta::default(),
        Provenance::derived(vec![a.clone(), b.clone()]),
    )
    .await;
    let p = plan(&e, &by_message(S, &["m:5"])).await;
    assert_eq!(p.document.body.tombstones.len(), 1);
    let derived = p
        .document
        .body
        .targets
        .iter()
        .find(|t| t.id != a)
        .expect("derived row is a target");
    assert!(!derived.direct);
    apply(&e, &p.plan_id).await;
    assert!(exists(&e, &b).await);
    put(&e, "turn 9 again", TemporalMeta::default(), turn_only("t9")).await;
}

#[tokio::test]
async fn forgetting_a_conversation_fences_every_turn_of_it() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "turn 1", TemporalMeta::default(), turn_write("t1", 1)).await;
    put(&e, "turn 3", TemporalMeta::default(), turn_only("t3")).await;
    let upto = Utc::now() + chrono::Duration::seconds(1);
    let p = plan(&e, &by_session(S, Some(4), upto)).await;
    // The session watermark plus one tombstone per turn key.
    assert_eq!(p.document.body.tombstones.len(), 3);
    apply(&e, &p.plan_id).await;
    // A replay long after the watermark is still fenced by its turn key.
    let late = |t: &str| {
        Provenance::Sources(vec![SourceRef::other(
            SourceKind::McpTurn,
            S,
            format!("turn:{t}"),
            Utc::now() + chrono::Duration::days(1),
        )])
    };
    for t in ["t1", "t3"] {
        let o = try_put(&e, "replay", TemporalMeta::default(), late(t)).await;
        assert!(is_fenced(&o, FenceReason::SourceForgotten), "{t}: {o:?}");
    }
    // A new turn of the same session after the forget writes normally.
    put(&e, "new turn", TemporalMeta::default(), late("t10")).await;
}

#[tokio::test]
async fn stale_reports_every_reason_that_holds() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "ten", TemporalMeta::default(), src(msg(S, 10))).await;
    put(&e, "eleven", TemporalMeta::default(), src(msg(S, 11))).await;
    let p1 = plan(&e, &by_message(S, &["m:10"])).await;
    forget(&e, &by_message(S, &["m:11"])).await;
    put(&e, "ten again", TemporalMeta::default(), src(msg(S, 10))).await;
    let o = e
        .apply_forget_plan(&p1.plan_id, &ExternalInputs::default())
        .await
        .unwrap();
    let ApplyOutcome::Stale(reason) = o else {
        panic!("{o:?}")
    };
    let all = reason.reasons();
    assert_eq!(all.len(), 2, "{reason:?}");
    assert!(matches!(all[0], StaleReason::Epoch { .. }));
    assert!(matches!(all[1], StaleReason::Changed { added: 1, .. }));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories").await, 2);
}

#[tokio::test]
async fn a_changed_external_part_is_named_when_no_target_changed() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "ten", TemporalMeta::default(), src(msg(S, 10))).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    let now = ExternalInputs {
        review_cards_matching: 1,
        ..Default::default()
    };
    match e.apply_forget_plan(&p.plan_id, &now).await.unwrap() {
        ApplyOutcome::Stale(StaleReason::Changed {
            added: 0,
            removed: 0,
            changed: 0,
            other_parts,
            ..
        }) => assert_eq!(other_parts, vec!["review_cards_matching".to_string()]),
        o => panic!("{o:?}"),
    }
}

#[tokio::test]
async fn upstream_unknown_marker_counts_as_untracked_and_is_no_source() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let marker = SourceRef::upstream_unknown("k1", Utc::now());
    assert!(marker.validate().is_ok());
    let run_src = run("cron:oc", "k1", Utc::now());
    let a = put(
        &e,
        "dispatched with unknown upstream",
        TemporalMeta::default(),
        Provenance::Sources(vec![run_src, marker.clone()]),
    )
    .await;
    put(&e, "unrelated", TemporalMeta::default(), src(msg(S, 1))).await;
    let p = plan(&e, &by_message(S, &["m:1"])).await;
    assert_eq!(p.document.body.untracked_in_namespace, 1);
    let p = plan(&e, &by_message("cron:oc", &["run:k1"])).await;
    let t = &p.document.body.targets[0];
    assert_eq!(t.id, a);
    assert!(t.other_sources.is_empty(), "{:?}", t.other_sources);
    // The marker's session is reserved: it can never be planned or reused.
    let mut bad = marker.clone();
    bad.session = "telegram:1".into();
    assert!(bad.validate().is_err());
    assert!(
        e.plan_forget_source(
            AGENT,
            &by_message(crate::lineage::UNTRACKED_SESSION, &["upstream:run:k1"]),
            PlanOptions::default(),
            &ExternalInputs::default()
        )
        .await
        .is_err()
    );
}

/// The plan hash leaves out the informational counts: an unrelated
/// untracked write in the namespace and another namespace's write from the
/// same conversation, after the plan, do not make it stale. The report
/// gives the plan-time and apply-time values.
#[tokio::test]
async fn unrelated_writes_after_the_plan_do_not_make_it_stale() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let a = put(&e, "ten", TemporalMeta::default(), src(msg(S, 10))).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    assert_eq!(p.document.body.untracked_in_namespace, 0);
    // An untracked row: a dispatch write whose upstream was unknown.
    put(
        &e,
        "dispatched, upstream unknown",
        TemporalMeta::default(),
        Provenance::Sources(vec![
            run("cron:oc", "k9", Utc::now()),
            SourceRef::upstream_unknown("k9", Utc::now()),
        ]),
    )
    .await;
    // Another employee recorded the same conversation.
    e.store_temporal_outcome(
        "other-agent",
        entry(
            "other namespace",
            duduclaw_core::types::MemoryLayer::Semantic,
        ),
        TemporalMeta::default(),
        src(msg(S, 11)),
    )
    .await
    .unwrap();
    let r = apply(&e, &p.plan_id).await;
    assert!(!exists(&e, &a).await);
    assert_eq!(
        (
            r.untracked_in_namespace_planned,
            r.untracked_in_namespace_at_apply
        ),
        (0, 1)
    );
    assert_eq!(
        (
            r.other_namespaces_referencing_planned,
            r.other_namespaces_referencing_at_apply
        ),
        (0, 1)
    );
}

/// A memory derived from the forgotten source after the plan still makes
/// the plan stale (the hash binds the rows to delete).
#[tokio::test]
async fn a_memory_derived_from_the_source_after_the_plan_is_stale() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let a = put(&e, "ten", TemporalMeta::default(), src(msg(S, 10))).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    put(
        &e,
        "derived from ten",
        TemporalMeta::default(),
        Provenance::derived(vec![a.clone()]),
    )
    .await;
    match e
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap()
    {
        ApplyOutcome::Stale(StaleReason::Changed { added: 1, .. }) => {}
        o => panic!("{o:?}"),
    }
    assert!(exists(&e, &a).await);
}
