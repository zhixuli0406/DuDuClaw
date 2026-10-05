//! GT3 (stale, expiry, SQL faults, process kill, restart) and GT4 (fan-out,
//! limits, cycles, legacy-only namespaces, re-embedding).

use super::*;
use crate::lineage::hooks::ApplyHookPoint;
use crate::lineage::test_support::*;
use crate::lineage::{FenceReason, Provenance};
use duduclaw_core::types::MemoryLayer;

const S: &str = "telegram:c1";

async fn seed(e: &SqliteMemoryEngine) -> (String, String) {
    let a = put(
        e,
        "fact from message ten",
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await;
    let b = put(
        e,
        "fact from message eleven",
        TemporalMeta::default(),
        src(msg(S, 11)),
    )
    .await;
    (a, b)
}

async fn planned_status(e: &SqliteMemoryEngine, plan_id: &str) -> String {
    e.get_forget_plan(plan_id).await.unwrap().unwrap().status
}

// ── GT3: stale ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn gt3_new_row_from_the_source_after_plan_is_stale() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    seed(&e).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    put(
        &e,
        "another fact from message ten",
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await;
    match e
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap()
    {
        ApplyOutcome::Stale(StaleReason::Changed {
            added: 1,
            removed: 0,
            ..
        }) => {}
        o => panic!("{o:?}"),
    }
    assert_eq!(count(&e, "SELECT COUNT(*) FROM forgotten_sources").await, 0);
    assert_eq!(planned_status(&e, &p.plan_id).await, "stale");
    assert_eq!(
        e.apply_forget_plan(&p.plan_id, &ExternalInputs::default())
            .await
            .unwrap(),
        ApplyOutcome::Stale(StaleReason::AlreadyStale)
    );
    // A fresh plan includes the new row.
    let p2 = plan(&e, &by_message(S, &["m:10"])).await;
    assert_eq!(p2.document.body.targets.len(), 2);
    apply(&e, &p2.plan_id).await;
}

#[tokio::test]
async fn gt3_changed_content_is_stale() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let (a, _) = seed(&e).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    assert!(e.update_content(AGENT, &a, "rewritten").await.unwrap());
    match e
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap()
    {
        ApplyOutcome::Stale(StaleReason::Changed { changed: 1, .. }) => {}
        o => panic!("{o:?}"),
    }
    assert!(exists(&e, &a).await);
}

#[tokio::test]
async fn gt3_another_apply_in_the_namespace_is_stale_by_epoch() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    seed(&e).await;
    let p1 = plan(&e, &by_message(S, &["m:10"])).await;
    let p2 = plan(&e, &by_message(S, &["m:11"])).await;
    apply(&e, &p2.plan_id).await;
    assert_eq!(
        e.apply_forget_plan(&p1.plan_id, &ExternalInputs::default())
            .await
            .unwrap(),
        ApplyOutcome::Stale(StaleReason::Epoch {
            planned: 0,
            current: 1
        })
    );
}

#[tokio::test]
async fn gt3_expired_plan_is_refused() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let (a, _) = seed(&e).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    {
        let conn = e.conn_for_maintenance().await;
        conn.execute(
            "UPDATE memory_forget_plans
             SET plan_json = json_set(plan_json, '$.expires_at', '2020-01-01T00:00:00.000000Z')
             WHERE plan_id = ?1",
            [&p.plan_id],
        )
        .unwrap();
    }
    assert_eq!(
        e.apply_forget_plan(&p.plan_id, &ExternalInputs::default())
            .await
            .unwrap(),
        ApplyOutcome::Expired
    );
    assert_eq!(planned_status(&e, &p.plan_id).await, "expired");
    assert!(exists(&e, &a).await);
    // TTL is clamped to [1, 1440] minutes.
    let p = match e
        .plan_forget_source(
            AGENT,
            &by_message(S, &["m:10"]),
            PlanOptions {
                ttl_minutes: Some(100_000),
                ..Default::default()
            },
            &ExternalInputs::default(),
        )
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => p,
        o => panic!("{o:?}"),
    };
    let created = parse_rfc3339(&p.document.created_at).unwrap();
    let expires = parse_rfc3339(&p.document.expires_at).unwrap();
    assert_eq!((expires - created).num_minutes(), MAX_TTL_MINUTES);
}

// ── GT3: SQL faults roll back everything ────────────────────────────────────

#[tokio::test]
async fn gt3_fault_at_any_point_applies_nothing_and_retry_succeeds() {
    for point in [
        ApplyHookPoint::AfterTombstones,
        ApplyHookPoint::AfterDeletes,
        ApplyHookPoint::BeforeCommit,
    ] {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        let (a, b) = seed(&e).await;
        e.store_fact(AGENT, "fact ten", "telegram", "c1", S, src(msg(S, 10)))
            .await
            .unwrap();
        let p = plan(&e, &by_message(S, &["m:10"])).await;
        e.set_apply_hook(Some(std::sync::Arc::new(move |at| {
            if at == point {
                Err(DuDuClawError::Memory(format!("injected fault at {at:?}")))
            } else {
                Ok(())
            }
        })));
        let r = e
            .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
            .await;
        assert!(r.is_err(), "{point:?}: {r:?}");
        assert_eq!(
            count(&e, "SELECT COUNT(*) FROM forgotten_sources").await,
            0,
            "{point:?}"
        );
        assert_eq!(
            count(&e, "SELECT COUNT(*) FROM forgotten_memories").await,
            0,
            "{point:?}"
        );
        assert_eq!(
            count(&e, "SELECT COUNT(*) FROM key_facts").await,
            1,
            "{point:?}"
        );
        assert_eq!(
            count(&e, "SELECT COUNT(*) FROM memory_forget_steps").await,
            0,
            "{point:?}"
        );
        assert!(exists(&e, &a).await && exists(&e, &b).await);
        assert_eq!(planned_status(&e, &p.plan_id).await, "planned");
        assert_eq!(
            crate::lineage::db::forget_epoch(&*e.conn_for_maintenance().await, AGENT).unwrap(),
            0
        );
        // The connection is usable (no transaction left open).
        put(
            &e,
            "still writable",
            TemporalMeta::default(),
            src(msg(S, 20)),
        )
        .await;

        e.set_apply_hook(None);
        let p = plan(&e, &by_message(S, &["m:10"])).await;
        let r = apply(&e, &p.plan_id).await;
        assert_eq!(
            (r.memories_deleted, r.key_facts_deleted),
            (1, 1),
            "{point:?}"
        );
    }
}

// ── GT3: a killed process applies all or nothing ───────────────────────────

const KILL_DB: &str = "P2B_KILL_DB";
const KILL_PLAN: &str = "P2B_KILL_PLAN";
const KILL_AT: &str = "P2B_KILL_AT";

/// Child-process entry point for the kill tests. A no-op unless the parent
/// test set the environment; then it applies the plan and aborts the process
/// at the requested point.
#[tokio::test]
async fn kill_child_entry() {
    let (Ok(db), Ok(plan_id), Ok(at)) = (
        std::env::var(KILL_DB),
        std::env::var(KILL_PLAN),
        std::env::var(KILL_AT),
    ) else {
        return;
    };
    let e = SqliteMemoryEngine::new(std::path::Path::new(&db)).unwrap();
    let target = match at.as_str() {
        "before_commit" => ApplyHookPoint::BeforeCommit,
        "after_commit" => ApplyHookPoint::AfterCommit,
        other => panic!("unknown kill point {other}"),
    };
    e.set_apply_hook(Some(std::sync::Arc::new(move |p| {
        if p == target {
            std::process::abort();
        }
        Ok(())
    })));
    let _ = e
        .apply_forget_plan(&plan_id, &ExternalInputs::default())
        .await;
    panic!("the child should have aborted");
}

fn run_child(db: &std::path::Path, plan_id: &str, at: &str) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "engine::forget_source::tests_gt34::kill_child_entry",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(KILL_DB, db)
        .env(KILL_PLAN, plan_id)
        .env(KILL_AT, at)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "child must die at {at}");
}

#[tokio::test]
async fn gt3_kill_before_commit_applies_nothing_then_retry_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let (plan_id, a) = {
        let e = SqliteMemoryEngine::new(&db).unwrap();
        let (a, _) = seed(&e).await;
        (plan(&e, &by_message(S, &["m:10"])).await.plan_id, a)
    };
    run_child(&db, &plan_id, "before_commit");
    let e = SqliteMemoryEngine::new(&db).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM forgotten_sources").await, 0);
    assert!(exists(&e, &a).await);
    assert_eq!(planned_status(&e, &plan_id).await, "planned");
    let r = apply(&e, &plan_id).await;
    assert_eq!(r.memories_deleted, 1);
}

#[tokio::test]
async fn gt3_kill_after_commit_is_fully_applied_and_steps_remain() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let (plan_id, a) = {
        let e = SqliteMemoryEngine::new(&db).unwrap();
        let (a, _) = seed(&e).await;
        (plan(&e, &by_message(S, &["m:10"])).await.plan_id, a)
    };
    run_child(&db, &plan_id, "after_commit");
    let e = SqliteMemoryEngine::new(&db).unwrap();
    assert!(!exists(&e, &a).await);
    assert_eq!(planned_status(&e, &plan_id).await, "applied");
    let open = e.unfinished_forget_steps().await.unwrap();
    assert!(
        open.iter().any(|s| s.step == STEP_SESSION_SUMMARY_CLEAR),
        "{open:?}"
    );
    // While the steps are outstanding, the source stays fenced.
    let o = try_put(&e, "again", TemporalMeta::default(), src(msg(S, 10))).await;
    assert!(is_fenced(&o, FenceReason::SourceForgotten));
    for s in open {
        e.mark_forget_step(&s.plan_id, &s.step, &s.target, Ok(()))
            .await
            .unwrap();
    }
    assert!(e.unfinished_forget_steps().await.unwrap().is_empty());
}

#[tokio::test]
async fn gt3_restart_keeps_every_fence() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    {
        let e = SqliteMemoryEngine::new(&db).unwrap();
        seed(&e).await;
        crate::user_profile::record_trait(&e, AGENT, "u1", "likes", "oolong", 0.5, src(msg(S, 10)))
            .await
            .unwrap();
        forget(&e, &by_message(S, &["m:10"])).await;
    }
    let e = SqliteMemoryEngine::new(&db).unwrap();
    assert!(is_fenced(
        &try_put(&e, "x", TemporalMeta::default(), src(msg(S, 10))).await,
        FenceReason::SourceForgotten
    ));
    assert!(
        e.store_fact(AGENT, "x", "telegram", "c1", S, src(msg(S, 10)))
            .await
            .is_err()
    );
    assert!(
        crate::user_profile::record_trait(&e, AGENT, "u1", "likes", "oolong", 0.5, src(msg(S, 10)))
            .await
            .is_err()
    );
    put(&e, "from m:12", TemporalMeta::default(), src(msg(S, 12))).await;
}

// ── GT4: fan-out and limits ─────────────────────────────────────────────────

/// Bulk-write `n` rows that inherit message m:10 (one transaction, direct SQL).
async fn bulk_children(e: &SqliteMemoryEngine, parent: &str, n: usize) {
    let conn = e.conn_for_maintenance().await;
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut m = conn
            .prepare("INSERT INTO memories (id, agent_id, content, timestamp, layer) VALUES (?1, ?2, ?3, '2026-10-04T14:00:00Z', 'semantic')")
            .unwrap();
        let mut f = conn
            .prepare("INSERT INTO memories_fts (content, agent_id, memory_id) VALUES (?1, ?2, ?3)")
            .unwrap();
        let mut o = conn
            .prepare(
                "INSERT INTO memory_origins (memory_store, memory_id, agent_id, source_kind, source_session,
                     source_message, source_seq, source_observed_at, role, via_memory_id, created_at)
                 VALUES ('memories', ?1, ?2, 'channel_message', ?3, 'm:10', 10, ?4, 'inherited', ?5, 'x')",
            )
            .unwrap();
        let observed = crate::lineage::format_ts(msg(S, 10).observed_at);
        for i in 0..n {
            let id = format!("fan-{i}");
            let content = format!("derived row number {i}");
            m.execute(rusqlite::params![id, AGENT, content]).unwrap();
            f.execute(rusqlite::params![content, AGENT, id]).unwrap();
            o.execute(rusqlite::params![id, AGENT, S, observed, parent])
                .unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
}

#[tokio::test]
async fn gt4_twenty_thousand_derived_rows_plan_and_apply_with_timing() {
    let dir = tempfile::tempdir().unwrap();
    let e = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
    let (a, _) = seed(&e).await;
    bulk_children(&e, &a, 20_000).await;
    let t = std::time::Instant::now();
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    let plan_ms = t.elapsed().as_millis();
    assert_eq!(p.document.body.targets.len(), 20_001);
    let r = apply(&e, &p.plan_id).await;
    println!(
        "P2B_GT4 fan-out 20001 rows: plan {plan_ms} ms, apply {} ms (write lock held inside)",
        r.elapsed_ms
    );
    assert_eq!(r.memories_deleted, 20_001);
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM memories_fts WHERE memory_id LIKE 'fan-%'"
        )
        .await,
        0
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories").await, 1);
}

#[tokio::test]
async fn gt4_sixty_thousand_rows_is_too_large_and_changes_nothing() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let (a, _) = seed(&e).await;
    bulk_children(&e, &a, 60_000).await;
    let o = e
        .plan_forget_source(
            AGENT,
            &by_message(S, &["m:10"]),
            PlanOptions::default(),
            &ExternalInputs::default(),
        )
        .await
        .unwrap();
    assert!(matches!(o, PlanOutcome::TooLarge { .. }), "{o:?}");
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM memory_forget_plans").await,
        0
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories").await, 60_002);
    // The operator may raise the limit up to the hard cap.
    let o = e
        .plan_forget_source(
            AGENT,
            &by_message(S, &["m:10"]),
            PlanOptions {
                max_rows: Some(70_000),
                ..Default::default()
            },
            &ExternalInputs::default(),
        )
        .await
        .unwrap();
    assert!(matches!(o, PlanOutcome::Planned(_)));
}

async fn legacy_row(e: &SqliteMemoryEngine, id: &str, derived_from: &[&str]) {
    let conn = e.conn_for_maintenance().await;
    conn.execute(
        "INSERT INTO memories (id, agent_id, content, timestamp, derived_from)
         VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', ?4)",
        rusqlite::params![
            id,
            AGENT,
            format!("legacy {id}"),
            serde_json::json!(derived_from).to_string()
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn gt4_recorded_parent_cycles_terminate() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let (a, _) = seed(&e).await;
    legacy_row(&e, "L3", &[&a, "L4"]).await;
    legacy_row(&e, "L4", &["L3"]).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    let ids = p
        .document
        .body
        .targets
        .iter()
        .map(|t| t.id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"L3") && ids.contains(&"L4"), "{ids:?}");
}

#[tokio::test]
async fn gt4_recorded_parent_chain_depth_limit() {
    for (len, too_large) in [(32usize, false), (33, true)] {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        let (a, _) = seed(&e).await;
        let mut prev = a.clone();
        for i in 1..=len {
            let id = format!("C{i}");
            legacy_row(&e, &id, &[&prev]).await;
            prev = id;
        }
        let o = e
            .plan_forget_source(
                AGENT,
                &by_message(S, &["m:10"]),
                PlanOptions::default(),
                &ExternalInputs::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            matches!(o, PlanOutcome::TooLarge { .. }),
            too_large,
            "chain of {len}: {o:?}"
        );
    }
}

#[tokio::test]
async fn gt4_namespace_with_only_untracked_rows_reports_and_writes_nothing() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    legacy_row(&e, "old-1", &[]).await;
    legacy_row(&e, "old-2", &[]).await;
    let o = e
        .plan_forget_source(
            AGENT,
            &by_message(S, &["m:10"]),
            PlanOptions::default(),
            &ExternalInputs::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        o,
        PlanOutcome::NothingToForget {
            untracked_in_namespace: 2
        }
    );
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM memory_forget_plans").await,
        0
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM forgotten_sources").await, 0);
}

#[tokio::test]
async fn gt4_legacy_rows_recorded_with_the_session_are_included_under_a_watermark() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    {
        let conn = e.conn_for_maintenance().await;
        conn.execute(
            "INSERT INTO key_facts (id, agent_id, fact, source_session, timestamp)
             VALUES ('kf-old', ?1, 'old fact', ?2, '2026-10-04T13:00:00+00:00'),
                    ('kf-new', ?1, 'newer fact', ?2, '2026-10-04T15:00:00+00:00')",
            rusqlite::params![AGENT, S],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, agent_id, content, timestamp, metadata)
             VALUES ('dec-old', ?1, 'decision', '2026-10-04T13:30:00+08:00', ?2)",
            rusqlite::params![AGENT, serde_json::json!({"session_id": S}).to_string()],
        )
        .unwrap();
    }
    let p = plan(&e, &by_session(S, None, t0())).await;
    let ids: std::collections::BTreeMap<_, _> = p
        .document
        .body
        .targets
        .iter()
        .map(|t| (t.id.clone(), t.via.clone()))
        .collect();
    assert_eq!(
        ids.get("kf-old").map(String::as_str),
        Some("key_facts.source_session")
    );
    assert_eq!(
        ids.get("dec-old").map(String::as_str),
        Some("metadata.session_id")
    );
    assert!(!ids.contains_key("kf-new"), "after the watermark");
}

#[tokio::test]
async fn gt4_reembedding_after_apply_restores_nothing() {
    let e = SqliteMemoryEngine::in_memory()
        .unwrap()
        .with_embedder(std::sync::Arc::new(crate::vector::NgramHashEmbedder::new()));
    let (a, b) = seed(&e).await;
    forget(&e, &by_message(S, &["m:10"])).await;
    e.backfill_embeddings(AGENT).await.unwrap();
    assert!(!exists(&e, &a).await);
    assert!(exists(&e, &b).await);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories").await, 1);
}

#[tokio::test]
async fn gt4_derived_writes_from_a_forgotten_parent_are_refused() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let (a, _) = seed(&e).await;
    forget(&e, &by_message(S, &["m:10"])).await;
    let o = try_put(
        &e,
        "child",
        TemporalMeta::default(),
        Provenance::derived(vec![a.clone()]),
    )
    .await;
    assert!(is_fenced(&o, FenceReason::ParentForgotten), "{o:?}");
    let o = e
        .store_temporal_outcome(
            AGENT,
            entry("legacy-style child", MemoryLayer::Semantic),
            TemporalMeta {
                derived_from: Some(vec![a]),
                ..Default::default()
            },
            src(msg(S, 30)),
        )
        .await
        .unwrap();
    assert!(
        is_fenced(&o, FenceReason::ParentForgotten),
        "derived_from is a parent too: {o:?}"
    );
}
