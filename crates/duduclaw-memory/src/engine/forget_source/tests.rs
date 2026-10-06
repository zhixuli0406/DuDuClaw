//! GT1 (graph, dry run, apply), design B.2 (corroborating sources),
//! supersession chain repair, archive copies, GT5 (privacy, db binding, steps).

use super::*;
use crate::lineage::test_support::*;
use crate::lineage::{FenceReason, Provenance};
use crate::supersession_guard::TemporalWriteOutcome;
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::MemoryLayer;

const S: &str = "telegram:c1";
const S2: &str = "telegram:c2";

/// The design §9 fixture graph.
struct Ids {
    m1: String,
    m2: String,
    m3: String,
    m4: String,
    k1: String,
    k2: String,
    l: String,
    l2: String,
}

async fn build_graph(e: &SqliteMemoryEngine) -> Ids {
    let a = msg(S, 10);
    let b = msg(S, 11);
    let c = msg(S2, 5);
    let m1 = put(
        e,
        "user likes oolong",
        triple("user:u1", "likes", "oolong"),
        src(a.clone()),
    )
    .await;
    let m2 = put(
        e,
        "user drinks mocha",
        triple("user:u1", "drinks", "mocha"),
        src(b),
    )
    .await;
    let m3 = put(
        e,
        "summary: oolong plus mocha habits",
        TemporalMeta::default(),
        Provenance::derived(vec![m1.clone(), m2.clone()]),
    )
    .await;
    let m4 = put(
        e,
        "weekly digest mentions drink habits",
        TemporalMeta::default(),
        Provenance::derived(vec![m3.clone()]),
    )
    .await;
    let k1 = e
        .store_fact(
            AGENT,
            "user likes oolong (fact)",
            "telegram",
            "c1",
            S,
            src(a),
        )
        .await
        .unwrap();
    let k2 = e
        .store_fact(
            AGENT,
            "unrelated fact from chat two",
            "telegram",
            "c2",
            S2,
            src(c),
        )
        .await
        .unwrap();
    let (l, l2) = ("legacy-l".to_string(), "legacy-l2".to_string());
    {
        let conn = e.conn_for_maintenance().await;
        conn.execute(
            "INSERT INTO memories (id, agent_id, content, timestamp) VALUES (?1, ?2, 'old untracked row', '2026-01-01T00:00:00Z')",
            rusqlite::params![l, AGENT],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, agent_id, content, timestamp, derived_from)
             VALUES (?1, ?2, 'old binary derived row', '2026-01-01T00:00:00Z', ?3)",
            rusqlite::params![l2, AGENT, serde_json::json!([m1]).to_string()],
        )
        .unwrap();
    }
    Ids {
        m1,
        m2,
        m3,
        m4,
        k1,
        k2,
        l,
        l2,
    }
}

fn target_ids(p: &ForgetPlan) -> std::collections::BTreeSet<String> {
    p.document
        .body
        .targets
        .iter()
        .map(|t| t.id.clone())
        .collect()
}

#[tokio::test]
async fn gt1_plan_lists_exactly_the_closure_and_its_collateral() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let ids = build_graph(&e).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    let expected: std::collections::BTreeSet<String> =
        [&ids.m1, &ids.m3, &ids.m4, &ids.k1, &ids.l2]
            .iter()
            .map(|s| s.to_string())
            .collect();
    assert_eq!(target_ids(&p), expected);
    let b_digest = msg(S, 11).digest(AGENT);
    let collateral: Vec<_> = p
        .document
        .body
        .collateral
        .iter()
        .filter(|c| c.source_digest == b_digest)
        .collect();
    assert_eq!(collateral.len(), 1);
    assert_eq!(collateral[0].rows_lost, 2, "M3 and M4 also carry B");
    assert_eq!(
        p.document.body.untracked_in_namespace, 1,
        "L is untracked and kept"
    );
    let via: std::collections::HashMap<_, _> = p
        .document
        .body
        .targets
        .iter()
        .map(|t| (t.id.clone(), t.via.clone()))
        .collect();
    assert_eq!(via[&ids.l2], "derived_from");
    assert_eq!(via[&ids.m4], "origins");
    // Dry run changed nothing.
    assert!(exists(&e, &ids.m1).await && exists(&e, &ids.l2).await);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM forgotten_sources").await, 0);
    assert_eq!(p.status, "planned");
}

#[tokio::test]
async fn gt1_apply_removes_the_rows_from_every_read_path() {
    let mut e = SqliteMemoryEngine::in_memory()
        .unwrap()
        .with_embedder(std::sync::Arc::new(crate::vector::NgramHashEmbedder::new()));
    e.retrieval_weights.graph_embed_seed = true;
    let ids = build_graph(&e).await;
    // Populate entity embeddings through the search path.
    e.search(AGENT, "user:u1 oolong mocha", 10).await.unwrap();
    let ent = |name: &'static str| {
        format!("SELECT COUNT(*) FROM entity_embedding WHERE entity = '{name}'")
    };
    assert!(
        count(&e, &ent("oolong")).await > 0,
        "fixture: embedding seeded"
    );

    let report = forget(&e, &by_message(S, &["m:10"])).await;
    assert_eq!(report.memories_deleted, 4);
    assert_eq!(report.key_facts_deleted, 1);
    assert_eq!(report.entity_embeddings_deleted, 1);

    let gone = [&ids.m1, &ids.m3, &ids.m4, &ids.l2];
    for q in ["oolong", "habits", "digest", "binary"] {
        let hits = e.search(AGENT, q, 20).await.unwrap();
        assert!(hits.iter().all(|h| !gone.contains(&&h.id)), "search {q}");
    }
    let recent = e.list_recent(AGENT, 100).await.unwrap();
    assert!(recent.iter().all(|h| !gone.contains(&&h.id)));
    let by_id = e
        .get_by_ids(
            AGENT,
            &gone.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    assert!(by_id.is_empty());
    let traits = crate::user_profile::profile_traits(&e, AGENT, "u1")
        .await
        .unwrap();
    assert!(traits.iter().all(|t| t.value != "oolong"));
    assert!(traits.iter().any(|t| t.value == "mocha"));
    let facts = e.search_facts(AGENT, "oolong", 10).await.unwrap();
    assert!(facts.iter().all(|f| f.id != ids.k1));
    assert!(
        e.get_history(AGENT, "user:u1", "likes")
            .await
            .unwrap()
            .is_empty()
    );
    let deleted = gone
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        count(
            &e,
            &format!(
                "SELECT COUNT(*) FROM memories WHERE embedding IS NOT NULL AND id IN ({deleted})"
            )
        )
        .await,
        0
    );
    assert_eq!(
        count(&e, &ent("oolong")).await,
        0,
        "orphaned entity embedding removed"
    );
    assert!(
        count(&e, &ent("mocha")).await > 0,
        "a surviving entity keeps its embedding"
    );
    assert!(count(&e, &ent("user:u1")).await > 0);
    // Untouched: B's own row, the untracked row, another chat's fact.
    assert!(
        e.search(AGENT, "mocha", 10)
            .await
            .unwrap()
            .iter()
            .any(|h| h.id == ids.m2)
    );
    assert!(exists(&e, &ids.l).await);
    assert!(
        e.get_recent_facts(AGENT, 10)
            .await
            .unwrap()
            .iter()
            .any(|f| f.id == ids.k2)
    );
}

// ── design B.2: corroborating (reaffirm) sources ───────────────────────────

/// Rule 1: a row the forgotten source supports directly or by inheritance is
/// removed whole; its other sources are reported as collateral.
#[tokio::test]
async fn b2_rule1_direct_or_inherited_source_removes_the_row() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let mixed = put(
        &e,
        "both said it",
        TemporalMeta::default(),
        Provenance::Sources(vec![msg(S, 10), msg(S2, 1)]),
    )
    .await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    assert_eq!(target_ids(&p), [mixed.clone()].into_iter().collect());
    assert_eq!(
        p.document.body.collateral[0].source_digest,
        msg(S2, 1).digest(AGENT)
    );
    apply(&e, &p.plan_id).await;
    assert!(!exists(&e, &mixed).await);
}

async fn reaffirmed_fact(e: &SqliteMemoryEngine) -> String {
    let base = TemporalMeta {
        confidence: Some(0.5),
        ..triple("user:u1", "name", "Ada")
    };
    let id = put(e, "user is called Ada", base, src(msg(S2, 1))).await;
    // Re-observed in the chat being forgotten, from an independent origin
    // class: a reaffirmation that raises confidence.
    let again = TemporalMeta {
        origin: Some("operator".into()),
        ..triple("user:u1", "name", "Ada")
    };
    match try_put(e, "user is called Ada", again, src(msg(S, 10))).await {
        TemporalWriteOutcome::Stored(got) => assert_eq!(got, id, "reaffirm keeps the row"),
        o => panic!("{o:?}"),
    }
    id
}

async fn confidence(e: &SqliteMemoryEngine, id: &str) -> f64 {
    let conn = e.conn_for_maintenance().await;
    conn.query_row("SELECT confidence FROM memories WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .unwrap()
}

/// Rule 2: a row the forgotten source only corroborated is kept; only that
/// corroboration record goes, and confidence is not rolled back.
#[tokio::test]
async fn b2_rule2_reaffirm_only_keeps_the_row_and_drops_the_record() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let id = reaffirmed_fact(&e).await;
    let before = confidence(&e, &id).await;
    assert!(before > 0.5, "fixture: the reaffirmation raised confidence");
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    assert!(p.document.body.targets.is_empty());
    assert_eq!(p.document.body.reaffirm_only.len(), 1);
    assert_eq!(
        p.document.body.reaffirm_only[0].removed_sources,
        vec![msg(S, 10).digest(AGENT)]
    );
    let r = apply(&e, &p.plan_id).await;
    assert_eq!((r.memories_deleted, r.reaffirm_lineage_removed), (0, 1));
    assert!(exists(&e, &id).await);
    assert_eq!(
        confidence(&e, &id).await,
        before,
        "confidence is not rolled back"
    );
    assert_eq!(
        count(
            &e,
            &format!("SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{id}'")
        )
        .await,
        1,
        "only the original source remains"
    );
}

/// Rule 3: the "kept, corroboration removed" list is part of the plan and of
/// its hash — a new corroboration between plan and apply makes it stale.
#[tokio::test]
async fn b2_rule3_reaffirm_only_list_is_hashed() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let id = reaffirmed_fact(&e).await;
    let p = plan(&e, &by_message(S, &["m:10", "m:13"])).await;
    assert!(
        p.document
            .canonical_json()
            .unwrap()
            .contains("reaffirm_only")
    );
    let again = TemporalMeta {
        origin: Some("user_direct".into()),
        ..triple("user:u1", "name", "Ada")
    };
    assert!(matches!(
        try_put(&e, "user is called Ada", again, src(msg(S, 13))).await,
        TemporalWriteOutcome::Stored(_)
    ));
    match e
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap()
    {
        ApplyOutcome::Stale(StaleReason::Changed { .. }) => {}
        o => panic!("expected stale, got {o:?}"),
    }
    assert!(exists(&e, &id).await);
}

/// Rule 4: once forgotten, the source cannot corroborate again.
#[tokio::test]
async fn b2_rule4_forgotten_source_cannot_reaffirm() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let id = reaffirmed_fact(&e).await;
    forget(&e, &by_message(S, &["m:10"])).await;
    let conf = confidence(&e, &id).await;
    let access = count(
        &e,
        &format!("SELECT access_count FROM memories WHERE id = '{id}'"),
    )
    .await;
    let again = TemporalMeta {
        origin: Some("user_direct".into()),
        ..triple("user:u1", "name", "Ada")
    };
    let o = try_put(&e, "user is called Ada", again, src(msg(S, 10))).await;
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
    assert_eq!(confidence(&e, &id).await, conf);
    assert_eq!(
        count(
            &e,
            &format!("SELECT access_count FROM memories WHERE id = '{id}'")
        )
        .await,
        access
    );
}

// ── supersession chain and archive copies ──────────────────────────────────

#[tokio::test]
async fn chain_cut_leaves_predecessor_closed_and_clears_pointers() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let p_id = put(
        &e,
        "city is Taipei",
        triple("user:u1", "city", "Taipei"),
        src(msg(S2, 1)),
    )
    .await;
    let x_id = put(
        &e,
        "city is Tainan",
        triple("user:u1", "city", "Tainan"),
        src(msg(S, 10)),
    )
    .await;
    let n_id = put(
        &e,
        "city is Hsinchu",
        triple("user:u1", "city", "Hsinchu"),
        src(msg(S2, 2)),
    )
    .await;
    let plan_doc = plan(&e, &by_message(S, &["m:10"])).await;
    assert_eq!(plan_doc.document.body.supersession_cuts.len(), 2);
    let r = apply(&e, &plan_doc.plan_id).await;
    assert_eq!(r.supersession_links_cut, 2);
    let conn = e.conn_for_maintenance().await;
    let (vu, sb, meta): (Option<String>, Option<String>, String) = conn
        .query_row(
            "SELECT valid_until, superseded_by, metadata FROM memories WHERE id = ?1",
            [&p_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(vu.is_some(), "D2: the predecessor is not reopened");
    assert_eq!(sb, None);
    assert!(meta.contains("lineage_chain_cut") && meta.contains(&plan_doc.plan_id));
    let (sup, vu_n): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT supersedes, valid_until FROM memories WHERE id = ?1",
            [&n_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((sup, vu_n), (None, None));
    drop(conn);
    assert!(!exists(&e, &x_id).await);
}

#[tokio::test]
async fn archived_copies_are_removed_too() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let id = put(
        &e,
        "archived oolong note",
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await;
    assert!(
        e.forget(AGENT, &id).await.unwrap(),
        "single forget archives the content"
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories_archive").await, 1);
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    assert_eq!(p.document.body.archive_ids, vec![id.clone()]);
    let r = apply(&e, &p.plan_id).await;
    assert_eq!(r.archive_deleted, 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories_archive").await, 0);
    assert_eq!(
        count(
            &e,
            &format!("SELECT COUNT(*) FROM forgotten_memories WHERE memory_id = '{id}'")
        )
        .await,
        1
    );
}

// ── GT5 (memory crate part) ────────────────────────────────────────────────

#[tokio::test]
async fn gt5_plan_and_report_carry_no_content_and_the_session_once() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    build_graph(&e).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    let stored: String = {
        let conn = e.conn_for_maintenance().await;
        conn.query_row(
            "SELECT plan_json FROM memory_forget_plans WHERE plan_id = ?1",
            [&p.plan_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    for word in ["oolong", "mocha", "habits", "weekly", "untracked row"] {
        assert!(!stored.contains(word), "plan leaks content: {word}");
    }
    assert_eq!(stored.matches(S).count(), 1, "session only in the selector");
    let r = apply(&e, &p.plan_id).await;
    let report = serde_json::to_string(&r).unwrap();
    assert!(!report.contains(S) && !report.contains("oolong"));
}

#[tokio::test]
async fn gt5_plan_is_bound_to_its_database() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    build_graph(&e).await;
    let p = plan(&e, &by_message(S, &["m:10"])).await;
    {
        let conn = e.conn_for_maintenance().await;
        conn.execute(
            "UPDATE memory_meta SET value = 'another-db' WHERE key = 'db_instance_id'",
            [],
        )
        .unwrap();
    }
    let o = e
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap();
    assert_eq!(o, ApplyOutcome::DbMismatch);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM forgotten_sources").await, 0);
    assert_eq!(
        e.apply_forget_plan("no-such-plan", &ExternalInputs::default())
            .await
            .unwrap(),
        ApplyOutcome::NotFound
    );
}

#[tokio::test]
async fn external_steps_are_recorded_and_tracked() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    build_graph(&e).await;
    let ext = ExternalInputs {
        wiki_pages: vec![WikiPageRef {
            path: "auto/sop/x.md".into(),
            file_sha256: "ab".repeat(32),
        }],
        review_cards_matching: 1,
        session_messages: vec![],
    };
    let p = match e
        .plan_forget_source(
            AGENT,
            &by_message(S, &["m:10"]),
            PlanOptions::default(),
            &ext,
        )
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => p,
        o => panic!("{o:?}"),
    };
    // Different external findings at apply time ⇒ stale.
    let o = e
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap();
    assert!(matches!(o, ApplyOutcome::Stale(_)), "{o:?}");
    let p = match e
        .plan_forget_source(
            AGENT,
            &by_message(S, &["m:10"]),
            PlanOptions::default(),
            &ext,
        )
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => p,
        o => panic!("{o:?}"),
    };
    let r = match e.apply_forget_plan(&p.plan_id, &ext).await.unwrap() {
        ApplyOutcome::Applied(r) => r,
        o => panic!("{o:?}"),
    };
    let steps = e.forget_steps(&p.plan_id).await.unwrap();
    let kinds: Vec<(&str, &str)> = steps
        .iter()
        .map(|s| (s.step.as_str(), s.target.as_str()))
        .collect();
    for want in [
        (STEP_WIKI_PAGE_DELETE, "auto/sop/x.md"),
        (STEP_REVIEW_SCRUB, "targets"),
        (STEP_SESSION_HIDE, "m:10"),
        (STEP_SESSION_SUMMARY_CLEAR, "session"),
    ] {
        assert!(kinds.contains(&want), "missing step {want:?}: {kinds:?}");
    }
    assert_eq!(r.steps_pending, steps.len() as u64);
    assert!(steps.iter().all(|s| s.status == "pending"));
    assert!(
        e.mark_forget_step(&p.plan_id, STEP_SESSION_HIDE, "m:10", Ok(()))
            .await
            .unwrap()
    );
    assert!(
        e.mark_forget_step(
            &p.plan_id,
            STEP_REVIEW_SCRUB,
            "targets",
            Err("x".repeat(900))
        )
        .await
        .unwrap()
    );
    let open = e.unfinished_forget_steps().await.unwrap();
    assert_eq!(open.len(), steps.len() - 1);
    let failed = open.iter().find(|s| s.step == STEP_REVIEW_SCRUB).unwrap();
    assert_eq!((failed.status.as_str(), failed.attempts), ("failed", 1));
    assert_eq!(failed.last_error.as_ref().unwrap().chars().count(), 500);
    // Re-running an applied plan only reports it.
    assert_eq!(
        e.apply_forget_plan(&p.plan_id, &ext).await.unwrap(),
        ApplyOutcome::AlreadyApplied
    );
}

#[tokio::test]
async fn trait_store_rows_are_never_forgotten_by_source() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let mut x = entry("system telemetry row", MemoryLayer::Episodic);
    x.id = "sys-row".into();
    e.store(AGENT, x).await.unwrap();
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
            untracked_in_namespace: 0
        }
    );
    assert!(exists(&e, "sys-row").await);
}
