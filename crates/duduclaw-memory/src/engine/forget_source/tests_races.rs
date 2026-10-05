//! GT2: a background producer stopped before / inside its publish
//! transaction while a forget is applied from another connection. Ordering is
//! forced with `Barrier`s on the test hooks, never with sleeps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use super::*;
use crate::lineage::FenceReason;
use crate::lineage::hooks::{ApplyHookPoint, HookPoint};
use crate::lineage::test_support::*;
use duduclaw_core::types::MemoryLayer;

const S: &str = "telegram:c1";

/// Two engines (two connections) on one database file.
fn pair(dir: &tempfile::TempDir) -> (Arc<SqliteMemoryEngine>, SqliteMemoryEngine) {
    let path = dir.path().join("memory.db");
    let worker = Arc::new(SqliteMemoryEngine::new(&path).unwrap());
    let main = SqliteMemoryEngine::new(&path).unwrap();
    (worker, main)
}

/// Three recurrent episodes from message m:10 (one night theme).
async fn episodes(e: &SqliteMemoryEngine) -> Vec<String> {
    let mut ids = Vec::new();
    for c in [
        "gateway deploy needs api token configured",
        "gateway deploy needs api token in env",
        "gateway deploy needs api token before start",
    ] {
        let id = match e
            .store_temporal_outcome(
                AGENT,
                entry(c, MemoryLayer::Episodic),
                TemporalMeta::default(),
                src(msg(S, 10)),
            )
            .await
            .unwrap()
        {
            crate::supersession_guard::TemporalWriteOutcome::Stored(id) => id,
            o => panic!("{o:?}"),
        };
        ids.push(id);
    }
    ids
}

/// A one-shot hook that parks the first time `point` is reached: it meets
/// `reached`, then waits on `release`.
fn park_once(
    point: HookPoint,
    reached: Arc<Barrier>,
    release: Arc<Barrier>,
) -> crate::lineage::hooks::PublishHook {
    let armed = Arc::new(AtomicBool::new(true));
    Arc::new(move |p| {
        if p == point && armed.swap(false, Ordering::SeqCst) {
            reached.wait();
            release.wait();
        }
    })
}

/// Run `f` on its own thread with its own runtime (the hook blocks it).
fn spawn_worker<T: Send + 'static>(
    f: impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>> + Send + 'static,
) -> std::thread::JoinHandle<T> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f())
    })
}

async fn consolidated_rows(e: &SqliteMemoryEngine) -> i64 {
    count(
        e,
        "SELECT COUNT(*) FROM memories WHERE source_event = 'night_consolidation'",
    )
    .await
}

#[tokio::test]
async fn gt2_night_consolidation_stopped_before_its_transaction_is_fenced() {
    let dir = tempfile::tempdir().unwrap();
    let (worker, main) = pair(&dir);
    episodes(&main).await;
    let (reached, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    worker.set_publish_hook(Some(park_once(
        HookPoint::BeforeTxn,
        reached.clone(),
        release.clone(),
    )));
    let w = worker.clone();
    let handle = spawn_worker(move || {
        Box::pin(async move {
            crate::night::consolidate_recurrent(&w, AGENT, 100, 3, 1)
                .await
                .unwrap()
        })
    });
    reached.wait(); // the worker has read its sources and is about to publish
    forget(&main, &by_message(S, &["m:10"])).await;
    release.wait();
    let results = handle.join().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].stored_id, None);
    assert_eq!(
        results[0].fenced.as_ref().map(|f| f.reason),
        Some(FenceReason::ParentForgotten)
    );
    assert_eq!(consolidated_rows(&main).await, 0);
}

#[tokio::test]
async fn gt2_night_consolidation_inside_its_transaction_makes_apply_stale() {
    let dir = tempfile::tempdir().unwrap();
    let (worker, main) = pair(&dir);
    episodes(&main).await;
    let p = plan(&main, &by_message(S, &["m:10"])).await;

    let (reached, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    worker.set_publish_hook(Some(park_once(
        HookPoint::AfterFenceCheck,
        reached.clone(),
        release.clone(),
    )));
    // The apply releases the worker right before it asks for the write lock;
    // it then waits for the worker's commit (busy timeout) whatever the order.
    let rel = release.clone();
    let fired = Arc::new(AtomicBool::new(true));
    main.set_apply_hook(Some(Arc::new(move |at| {
        if at == ApplyHookPoint::BeforeBegin && fired.swap(false, Ordering::SeqCst) {
            rel.wait();
        }
        Ok(())
    })));
    let w = worker.clone();
    let handle = spawn_worker(move || {
        Box::pin(async move {
            crate::night::consolidate_recurrent(&w, AGENT, 100, 3, 1)
                .await
                .unwrap()
        })
    });
    reached.wait(); // inside the worker's write transaction, fence passed
    let o = main
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap();
    let results = handle.join().unwrap();
    assert!(results[0].stored_id.is_some(), "the worker published first");
    assert!(
        matches!(
            o,
            ApplyOutcome::Stale(StaleReason::Changed { added: 1, .. })
        ),
        "{o:?}"
    );
    assert_eq!(consolidated_rows(&main).await, 1);

    // A new plan includes the published row (it inherited m:10); apply removes it.
    main.set_apply_hook(None);
    let p2 = plan(&main, &by_message(S, &["m:10"])).await;
    assert_eq!(p2.document.body.targets.len(), 4);
    apply(&main, &p2.plan_id).await;
    assert_eq!(consolidated_rows(&main).await, 0);
}

#[tokio::test]
async fn gt2_key_fact_publish_before_and_inside_the_transaction() {
    // Before: fenced.
    let dir = tempfile::tempdir().unwrap();
    let (worker, main) = pair(&dir);
    put(
        &main,
        "seed from m:10",
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await;
    let (reached, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    worker.set_publish_hook(Some(park_once(
        HookPoint::BeforeTxn,
        reached.clone(),
        release.clone(),
    )));
    let w = worker.clone();
    let h = spawn_worker(move || {
        Box::pin(async move {
            w.store_fact_outcome(AGENT, "late fact", "telegram", "c1", S, src(msg(S, 10)))
                .await
                .unwrap()
        })
    });
    reached.wait();
    forget(&main, &by_message(S, &["m:10"])).await;
    release.wait();
    assert!(matches!(
        h.join().unwrap(),
        crate::lineage::FactWriteOutcome::Fenced(_)
    ));
    assert_eq!(count(&main, "SELECT COUNT(*) FROM key_facts").await, 0);

    // Inside: the fact commits, the apply goes stale, a new plan removes it.
    let dir = tempfile::tempdir().unwrap();
    let (worker, main) = pair(&dir);
    put(
        &main,
        "seed from m:10",
        TemporalMeta::default(),
        src(msg(S, 10)),
    )
    .await;
    let p = plan(&main, &by_message(S, &["m:10"])).await;
    let (reached, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    worker.set_publish_hook(Some(park_once(
        HookPoint::AfterFenceCheck,
        reached.clone(),
        release.clone(),
    )));
    let rel = release.clone();
    let fired = Arc::new(AtomicBool::new(true));
    main.set_apply_hook(Some(Arc::new(move |at| {
        if at == ApplyHookPoint::BeforeBegin && fired.swap(false, Ordering::SeqCst) {
            rel.wait();
        }
        Ok(())
    })));
    let w = worker.clone();
    let h = spawn_worker(move || {
        Box::pin(async move {
            w.store_fact_outcome(AGENT, "late fact", "telegram", "c1", S, src(msg(S, 10)))
                .await
                .unwrap()
        })
    });
    reached.wait();
    let o = main
        .apply_forget_plan(&p.plan_id, &ExternalInputs::default())
        .await
        .unwrap();
    assert!(matches!(
        h.join().unwrap(),
        crate::lineage::FactWriteOutcome::Stored(_)
    ));
    assert!(matches!(o, ApplyOutcome::Stale(_)), "{o:?}");
    main.set_apply_hook(None);
    forget(&main, &by_message(S, &["m:10"])).await;
    assert_eq!(count(&main, "SELECT COUNT(*) FROM key_facts").await, 0);
}

#[tokio::test]
async fn gt2_profile_summary_built_from_a_forgotten_trait_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let (worker, main) = pair(&dir);
    crate::user_profile::record_trait(&main, AGENT, "u1", "likes", "oolong", 0.5, src(msg(S, 10)))
        .await
        .unwrap();
    crate::user_profile::record_trait(&main, AGENT, "u1", "city", "Taipei", 0.5, src(msg(S, 11)))
        .await
        .unwrap();
    let (reached, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    worker.set_publish_hook(Some(park_once(
        HookPoint::BeforeTxn,
        reached.clone(),
        release.clone(),
    )));
    let w = worker.clone();
    let h = spawn_worker(move || {
        Box::pin(async move {
            crate::user_profile::consolidate_profile(&w, AGENT, "u1", 2)
                .await
                .unwrap()
        })
    });
    reached.wait();
    forget(&main, &by_message(S, &["m:10"])).await;
    release.wait();
    assert_eq!(h.join().unwrap(), None);
    assert_eq!(
        count(
            &main,
            "SELECT COUNT(*) FROM memories WHERE source_event = 'user_profile_consolidation'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn gt2_promotion_and_decision_after_a_forget_write_nothing() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    // A held claim from m:10 (M8): deleted by the forget, so nothing to promote.
    put(
        &e,
        "refund window 7 days",
        TemporalMeta {
            origin: Some("operator".into()),
            ..triple("policy", "window", "7")
        },
        src(msg(S, 1)),
    )
    .await;
    let held = e
        .hold_refused_claim(
            AGENT,
            entry("refund window 365 days", MemoryLayer::Semantic),
            triple("policy", "window", "365"),
            src(msg(S, 10)),
        )
        .await
        .unwrap();
    // An open decision (M9) whose choice arrives in the forgotten message.
    for (p, o) in [
        ("question", "which plan?"),
        ("option:a", "plan A"),
        ("status", "open"),
    ] {
        put(
            &e,
            o,
            TemporalMeta {
                origin: Some("channel".into()),
                ..triple("decision:d1", p, o)
            },
            src(msg(S, 2)),
        )
        .await;
    }
    forget(&e, &by_message(S, &["m:10"])).await;
    assert!(!exists(&e, &held).await);
    let r = e
        .promote_quarantined(AGENT, &[held], "operator")
        .await
        .unwrap();
    assert_eq!(r.promoted, 0);
    let err = e
        .resolve_decision(AGENT, "d1", "a", src(msg(S, 10)))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("source forgotten:"), "{err}");
    // The same choice from a later message resolves.
    assert!(matches!(
        e.resolve_decision(AGENT, "d1", "a", src(msg(S, 12)))
            .await
            .unwrap(),
        crate::engine::DecisionResolveOutcome::Resolved { .. }
    ));
}
