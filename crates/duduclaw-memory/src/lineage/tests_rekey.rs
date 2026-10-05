//! Re-keying (reassign, cross-database hand-off, namespace migration)
//! carries lineage and tombstones (P2-B, design §5.3 M18/M19).

use super::test_support::*;
use super::*;
use crate::engine::{SqliteMemoryEngine, TemporalMeta};
use duduclaw_core::types::MemoryLayer;

const S: &str = "telegram:c1";

// ── re-keying carries lineage and tombstones ──────────────────────────────

#[tokio::test]
async fn reassign_moves_lineage_and_copies_tombstones() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "forget me", TemporalMeta::default(), src(msg(S, 10))).await;
    let kept = put(&e, "keep me", TemporalMeta::default(), src(msg(S, 11))).await;
    forget(&e, &by_message(S, &["m:10"])).await;

    let sum = crate::lifecycle::reassign_agent(&e, AGENT, "successor")
        .await
        .unwrap();
    assert_eq!(sum.memories, 1);
    assert_eq!(sum.fenced, 0);
    assert_eq!(
        count(&e, &format!("SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{kept}' AND agent_id = 'successor'")).await,
        1
    );
    // The successor cannot relearn the forgotten message.
    let o = e
        .store_temporal_outcome(
            "successor",
            entry("again", MemoryLayer::Semantic),
            TemporalMeta::default(),
            src(msg(S, 10)),
        )
        .await
        .unwrap();
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
}

#[tokio::test]
async fn reassign_leaves_rows_the_target_forgot() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    // The successor already forgot telegram:c1 m:10 …
    e.store_temporal_outcome(
        "successor",
        entry("succ", MemoryLayer::Semantic),
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await
    .unwrap();
    let sel = by_message(S, &["m:10"]);
    let p = match e
        .plan_forget_source("successor", &sel, Default::default(), &Default::default())
        .await
        .unwrap()
    {
        crate::engine::forget_source::PlanOutcome::Planned(p) => p,
        o => panic!("{o:?}"),
    };
    e.apply_forget_plan(&p.plan_id, &Default::default())
        .await
        .unwrap();
    // … and the predecessor has a row from that same source.
    let stay = put(&e, "pred row", TemporalMeta::default(), src(msg(S, 10))).await;
    let go = put(&e, "pred row 2", TemporalMeta::default(), src(msg(S, 11))).await;
    let sum = crate::lifecycle::reassign_agent(&e, AGENT, "successor")
        .await
        .unwrap();
    assert_eq!((sum.memories, sum.fenced), (1, 1));
    assert_eq!(
        count(
            &e,
            &format!("SELECT COUNT(*) FROM memories WHERE id = '{stay}' AND agent_id = '{AGENT}'")
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &e,
            &format!("SELECT COUNT(*) FROM memories WHERE id = '{go}' AND agent_id = 'successor'")
        )
        .await,
        1
    );
}

#[tokio::test]
async fn cross_db_reassign_skips_forgotten_ids_and_carries_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("src.db");
    let dst_path = dir.path().join("dst.db");
    let source = SqliteMemoryEngine::new(&src_path).unwrap();
    let moved = put(&source, "moves", TemporalMeta::default(), src(msg(S, 11))).await;
    let ghost = put(
        &source,
        "forgotten in dst",
        TemporalMeta::default(),
        src(msg(S, 12)),
    )
    .await;
    put(
        &source,
        "forgotten here",
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await;
    forget(&source, &by_message(S, &["m:10"])).await;
    {
        let dst = SqliteMemoryEngine::new(&dst_path).unwrap();
        let conn = dst.conn_for_maintenance().await;
        conn.execute(
            "INSERT INTO forgotten_memories (memory_store, memory_id, agent_id, plan_id, forgotten_at)
             VALUES ('memories', ?1, 'successor', 'p', 'x')",
            [&ghost],
        )
        .unwrap();
    }
    let sum = crate::lifecycle::reassign_agent_cross_db(&source, &dst_path, AGENT, "successor")
        .await
        .unwrap();
    assert_eq!((sum.memories, sum.fenced), (1, 1));
    assert!(
        exists(&source, &ghost).await,
        "a fenced row stays in the source"
    );
    let dst = SqliteMemoryEngine::new(&dst_path).unwrap();
    assert_eq!(
        count(&dst, &format!("SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{moved}' AND agent_id = 'successor'")).await,
        1
    );
    let o = dst
        .store_temporal_outcome(
            "successor",
            entry("x", MemoryLayer::Semantic),
            TemporalMeta::default(),
            src(msg(S, 10)),
        )
        .await
        .unwrap();
    assert!(
        is_fenced(&o, FenceReason::SourceForgotten),
        "tombstones travel with the move"
    );
}

#[tokio::test]
async fn namespace_migration_moves_lineage_and_fences_forgotten_sources() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let from = "internal/gateway-internal";
    let moved = e
        .store_temporal(
            from,
            entry("m", MemoryLayer::Semantic),
            TemporalMeta::default(),
            src(msg(S, 11)),
        )
        .await
        .unwrap();
    let fenced = e
        .store_temporal(
            from,
            entry("f", MemoryLayer::Semantic),
            TemporalMeta::default(),
            src(msg(S, 10)),
        )
        .await
        .unwrap();
    // The target namespace forgot m:10.
    e.store_temporal(
        AGENT,
        entry("t", MemoryLayer::Semantic),
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await
    .unwrap();
    forget(&e, &by_message(S, &["m:10"])).await;

    let (out, _) = e
        .migrate_namespace_rows(
            from,
            AGENT,
            &[moved.clone(), fenced.clone()],
            crate::engine::OnRefused::Skip,
            false,
            false,
        )
        .await
        .unwrap();
    let d: std::collections::HashMap<_, _> = out.into_iter().collect();
    assert_eq!(d[&fenced], crate::engine::MigrationDisposition::Fenced);
    assert!(d[&moved].moved());
    assert_eq!(
        count(&e, &format!("SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{moved}' AND agent_id = '{AGENT}'")).await,
        1
    );
    assert_eq!(
        count(
            &e,
            &format!("SELECT COUNT(*) FROM memories WHERE id = '{fenced}' AND agent_id = '{from}'")
        )
        .await,
        1
    );
}
