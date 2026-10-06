//! P2-B independent-review fixes, memory side: watermark needs both seq and
//! time (M-8), lineage of deleted rows and plan content hashes are cleared
//! (M1/M-2), reaffirm rows are not inherited (M2/M-7), a source with no
//! memory is still planned (H-1), imports are content-addressed and
//! forgotten whatever the time (H-3), the session is stored trimmed (L-3),
//! and namespace migration carries tombstones (L-5).

use chrono::Duration;

use super::*;
use crate::lineage::test_support::*;
use crate::lineage::{FenceReason, Provenance, SourceRef};
use crate::supersession_guard::TemporalWriteOutcome;
use duduclaw_core::types::MemoryLayer;

const S: &str = "telegram:rv";

#[tokio::test]
async fn m8_a_session_watermark_needs_both_seq_and_time() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "first", TemporalMeta::default(), src(msg(S, 1))).await;
    // Watermark: seq 10, time t0 + 5 s.
    forget(&e, &by_session(S, Some(10), t0() + Duration::seconds(5))).await;
    // seq 3 at t0 + 3 s: inside both ⇒ fenced.
    let inside = try_put(&e, "x", TemporalMeta::default(), src(msg(S, 3))).await;
    assert!(
        is_fenced(&inside, FenceReason::SourceForgotten),
        "{inside:?}"
    );
    // seq 8 at t0 + 8 s: seq inside, time after ⇒ a new source.
    let later = try_put(&e, "y", TemporalMeta::default(), src(msg(S, 8))).await;
    assert!(
        matches!(later, TemporalWriteOutcome::Stored(_)),
        "{later:?}"
    );
}

#[tokio::test]
async fn m1_apply_drops_the_lineage_of_deleted_rows_and_the_content_hashes() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let id = put(&e, "gone", TemporalMeta::default(), src(msg(S, 1))).await;
    let kept = put(&e, "kept", TemporalMeta::default(), src(msg(S, 2))).await;
    let p = plan(&e, &by_message(S, &["m:1"])).await;
    assert!(
        p.document
            .body
            .targets
            .iter()
            .all(|t| !t.content_sha256.is_empty())
    );
    apply(&e, &p.plan_id).await;
    let q = |id: &str| format!("SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{id}'");
    assert_eq!(count(&e, &q(&id)).await, 0);
    assert_eq!(count(&e, &q(&kept)).await, 1);
    let stored = e.get_forget_plan(&p.plan_id).await.unwrap().unwrap();
    assert_eq!(stored.plan_hash, p.plan_hash, "the plan hash stays");
    assert!(
        stored
            .document
            .body
            .targets
            .iter()
            .all(|t| t.content_sha256.is_empty()),
        "content hashes are cleared after apply"
    );
}

#[tokio::test]
async fn m2_a_reaffirming_source_is_not_inherited() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let base = TemporalMeta {
        confidence: Some(0.5),
        ..triple("user:u9", "name", "Bo")
    };
    let id = put(&e, "user is Bo", base, src(msg("telegram:other", 1))).await;
    let again = TemporalMeta {
        origin: Some("operator".into()),
        ..triple("user:u9", "name", "Bo")
    };
    match try_put(&e, "user is Bo", again, src(msg(S, 10))).await {
        TemporalWriteOutcome::Stored(got) => assert_eq!(got, id),
        o => panic!("{o:?}"),
    }
    let child = put(
        &e,
        "summary about Bo",
        TemporalMeta::default(),
        Provenance::derived(vec![id.clone()]),
    )
    .await;
    assert_eq!(
        count(
            &e,
            &format!(
                "SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{child}' AND source_session = '{S}'"
            )
        )
        .await,
        0,
        "the reaffirmation is not inherited"
    );
    forget(&e, &by_message(S, &["m:10"])).await;
    assert!(exists(&e, &child).await);
    assert!(exists(&e, &id).await, "reaffirm only: the row stays");
}

#[tokio::test]
async fn h1_a_source_known_only_to_the_conversation_record_is_planned() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let sel = by_message(S, &["m:4"]);
    let ext = ExternalInputs {
        session_messages: vec![SessionMessageRef {
            session_digest: crate::lineage::source_digest(AGENT, S, "", ""),
            seq: 4,
        }],
        ..Default::default()
    };
    let p = match e
        .plan_forget_source(AGENT, &sel, PlanOptions::default(), &ext)
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => p,
        other => panic!("{other:?}"),
    };
    assert!(p.document.body.targets.is_empty());
    match e.apply_forget_plan(&p.plan_id, &ext).await.unwrap() {
        ApplyOutcome::Applied(r) => assert_eq!(r.memories_deleted, 0),
        other => panic!("{other:?}"),
    }
    let later = try_put(&e, "late", TemporalMeta::default(), src(msg(S, 4))).await;
    assert!(is_fenced(&later, FenceReason::SourceForgotten), "{later:?}");
    // With nothing anywhere, there is still nothing to forget.
    assert!(matches!(
        e.plan_forget_source(
            AGENT,
            &by_message(S, &["m:99"]),
            PlanOptions::default(),
            &Default::default()
        )
        .await
        .unwrap(),
        PlanOutcome::NothingToForget { .. }
    ));
}

#[tokio::test]
async fn h3_an_imported_file_is_forgotten_whatever_the_time_and_order() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let file = "/data/notes.csv";
    let session = crate::lineage::import_session(file);
    let first = SourceRef::import_item(file, b"row one", t0());
    assert_eq!(
        first,
        SourceRef::import_item(file, b"row one", t0()),
        "content-addressed"
    );
    assert_ne!(
        first.message,
        SourceRef::import_item(file, b"row two", t0()).message
    );
    put(&e, "row one", TemporalMeta::default(), src(first)).await;
    forget(&e, &by_session(&session, None, t0())).await;
    // Re-imported years later, rows in another order: still forgotten.
    let later = SourceRef::import_item(file, b"row two", t0() + Duration::days(900));
    let o = try_put(&e, "row two", TemporalMeta::default(), src(later)).await;
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
}

#[tokio::test]
async fn l3_the_session_is_stored_trimmed_and_control_characters_are_refused() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "a", TemporalMeta::default(), src(msg(S, 1))).await;
    let p = plan(&e, &by_message(&format!("  {S} "), &["m:1"])).await;
    assert_eq!(p.document.selector.session, S);
    assert!(
        e.plan_forget_source(
            AGENT,
            &by_message("telegram:\u{1b}x", &["m:1"]),
            PlanOptions::default(),
            &Default::default()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn l5_namespace_migration_carries_tombstones() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "gone", TemporalMeta::default(), src(msg(S, 1))).await;
    let moved = put(&e, "moved", TemporalMeta::default(), src(msg(S, 2))).await;
    forget(&e, &by_message(S, &["m:1"])).await;
    e.migrate_namespace_rows(
        AGENT,
        "target-ns",
        &[moved],
        crate::engine::OnRefused::Skip,
        false,
        false,
    )
    .await
    .unwrap();
    let o = e
        .store_temporal_outcome(
            "target-ns",
            entry("again", MemoryLayer::Semantic),
            TemporalMeta::default(),
            src(msg(S, 1)),
        )
        .await
        .unwrap();
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
}

/// M-2: a plan that goes stale or expires keeps its plan hash but loses its
/// per-row content hashes.
#[tokio::test]
async fn m2_stale_and_expired_plans_lose_their_content_hashes() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "first", TemporalMeta::default(), src(msg(S, 1))).await;
    let stale = plan(&e, &by_message(S, &["m:1"])).await;
    put(
        &e,
        "second from the same message",
        TemporalMeta::default(),
        src(msg(S, 1)),
    )
    .await;
    assert!(matches!(
        e.apply_forget_plan(&stale.plan_id, &ExternalInputs::default())
            .await
            .unwrap(),
        ApplyOutcome::Stale(_)
    ));
    let expired = plan(&e, &by_message(S, &["m:1"])).await;
    e.conn_for_maintenance()
        .await
        .execute(
            "UPDATE memory_forget_plans
             SET plan_json = json_set(plan_json, '$.expires_at', '2000-01-01T00:00:00.000000Z')
             WHERE plan_id = ?1",
            [&expired.plan_id],
        )
        .unwrap();
    assert!(matches!(
        e.apply_forget_plan(&expired.plan_id, &ExternalInputs::default())
            .await
            .unwrap(),
        ApplyOutcome::Expired
    ));
    for (p, status) in [(&stale, "stale"), (&expired, "expired")] {
        let got = e.get_forget_plan(&p.plan_id).await.unwrap().unwrap();
        assert_eq!(got.status, status);
        assert_eq!(got.plan_hash, p.plan_hash);
        assert!(!got.document.body.targets.is_empty());
        assert!(
            got.document
                .body
                .targets
                .iter()
                .all(|t| t.content_sha256.is_empty()),
            "{status}"
        );
    }
}
