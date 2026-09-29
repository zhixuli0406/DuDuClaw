//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! Memory decay visualisation (R1, 2026-08-12): every browse/search row
//! carries its Ebbinghaus figures, and `memory.decay_overview` aggregates
//! them. The invariant these tests defend is that the dashboard and
//! `duduclaw_memory::decay::run_decay` read the *same* curve — a memory the
//! archival job is about to take must not look "fresh" on the page.
use super::*;
use duduclaw_memory::engine::RetrievalWeights;

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(d),
            ..
        } => d,
        other => panic!("expected an ok response, got {other:?}"),
    }
}

fn entry(
    agent: &str,
    content: &str,
    age_days: i64,
    access_count: u32,
    importance: f64,
) -> duduclaw_core::types::MemoryEntry {
    let when = Utc::now() - ChronoDuration::days(age_days);
    duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.to_string(),
        content: content.to_string(),
        timestamp: when,
        tags: vec![],
        embedding: None,
        layer: Default::default(),
        importance,
        access_count,
        last_accessed: None,
        source_event: "conversation_summary".to_string(),
    }
}

async fn seed(
    home: &std::path::Path,
    agent: &str,
    entries: Vec<duduclaw_core::types::MemoryEntry>,
) {
    let db = home.join("agents").join(agent).join("memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let engine = SqliteMemoryEngine::new(&db).unwrap();
    for e in entries {
        engine.store(agent, e).await.unwrap();
    }
}

#[test]
fn freshness_bands_are_ordered_and_total_the_whole_range() {
    // Ordered freshest → faintest, strictly descending, bottoming out at 0
    // so no retrievability value can fall through unclassified.
    let mut previous = f64::INFINITY;
    for (_, lower) in MEMORY_FRESHNESS_BANDS {
        assert!(*lower < previous, "bands must strictly descend");
        previous = *lower;
    }
    assert_eq!(MEMORY_FRESHNESS_BANDS.last().unwrap().1, 0.0);

    assert_eq!(memory_freshness_band(1.0), "fresh");
    assert_eq!(memory_freshness_band(0.7), "fresh");
    assert_eq!(memory_freshness_band(0.69), "stable");
    assert_eq!(memory_freshness_band(0.4), "stable");
    assert_eq!(memory_freshness_band(0.39), "fading");
    assert_eq!(memory_freshness_band(0.15), "fading");
    assert_eq!(memory_freshness_band(0.14), "archiving");
    assert_eq!(memory_freshness_band(0.0), "archiving");
}

#[test]
fn decay_figures_match_the_engine_and_use_the_recall_anchor() {
    let w = RetrievalWeights::default();
    let now = Utc::now();
    let mut e = entry("a", "x", 30, 3, 5.0);

    // Never recalled → the anchor is the creation time.
    let (r, s) = memory_decay_figures(&e, &w, now);
    let expect_s = duduclaw_memory::engine::ebbinghaus_stability_days(3, 5.0, &w);
    assert!((s - expect_s).abs() < 1e-9);
    assert!(
        (r - duduclaw_memory::engine::ebbinghaus_retrievability(30.0, 3, 5.0, &w)).abs() < 1e-6
    );

    // Recalled yesterday → the same entry is far fresher, which is the
    // whole "被回想會讓記憶更持久" story the UI tells.
    e.last_accessed = Some(now - ChronoDuration::days(1));
    let (r_recent, _) = memory_decay_figures(&e, &w, now);
    assert!(r_recent > r, "a recent recall must raise retrievability");
}

#[tokio::test]
async fn browse_rows_carry_retrievability_and_stability() {
    let home = tempfile::tempdir().unwrap();
    let agent = "decay-browse";
    seed(
        home.path(),
        agent,
        vec![entry(agent, "fresh memory", 0, 0, 5.0)],
    )
    .await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let body = payload(
        handler
            .handle_memory_browse(json!({ "agent_id": agent, "limit": 50 }))
            .await,
    );
    let row = &body["entries"][0];
    let r = row["retrievability"].as_f64().expect("retrievability");
    let s = row["stability_days"].as_f64().expect("stability_days");
    assert!(
        (0.0..=1.0).contains(&r),
        "R must be a 0–1 probability, got {r}"
    );
    assert!(r > 0.9, "a just-written memory must read as fresh, got {r}");
    assert!(s > 0.0, "stability must be positive, got {s}");
    // `last_accessed` rides along so the detail sheet can explain *why* the
    // curve sits where it does.
    assert!(row.get("last_accessed").is_some());
}

#[tokio::test]
async fn decay_overview_buckets_top_lists_and_trend() {
    let home = tempfile::tempdir().unwrap();
    let agent = "decay-overview";
    seed(
        home.path(),
        agent,
        vec![
            entry(agent, "written today", 0, 0, 5.0),
            // Same age as "drifting" below, but recalled 40 times and more
            // important — reinforcement is exactly what keeps it fresh.
            entry(agent, "recalled often", 20, 40, 8.0),
            entry(agent, "drifting", 20, 0, 5.0),
            entry(agent, "nearly gone", 300, 0, 1.0),
        ],
    )
    .await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let body = payload(
        handler
            .handle_memory_decay_overview(json!({ "agent_id": agent, "days": 30, "top_n": 2 }))
            .await,
    );

    assert_eq!(body["total"], json!(4));
    assert_eq!(body["truncated"], json!(false));
    assert_eq!(body["window_days"], json!(30));
    // The archive line the curve draws is the archival job's own threshold.
    assert_eq!(
        body["archive_threshold"].as_f64().unwrap(),
        duduclaw_memory::decay::MemoryDecayPolicy::default().min_retrievability
    );

    // All four bands are always present, even at zero, so the distribution
    // chart never silently drops a category.
    let buckets = body["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), MEMORY_FRESHNESS_BANDS.len());
    let keys: Vec<&str> = buckets.iter().map(|b| b["key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec!["fresh", "stable", "fading", "archiving"]);
    let total: u64 = buckets.iter().map(|b| b["count"].as_u64().unwrap()).sum();
    assert_eq!(total, 4, "every scanned entry lands in exactly one band");
    let bucket = |key: &str| -> u64 {
        buckets.iter().find(|b| b["key"] == key).unwrap()["count"]
            .as_u64()
            .unwrap()
    };
    // Today's entry plus the reinforced one: repeated recall is what keeps
    // a 20-day-old memory in the same band as one written this morning,
    // while its never-recalled twin has already slipped two bands down.
    assert_eq!(bucket("fresh"), 2);
    assert_eq!(bucket("fading"), 1, "the never-recalled 20-day-old drifts");
    assert_eq!(bucket("archiving"), 1, "the 300-day-old one is about to go");

    // Faintest first, capped at top_n.
    let fading = body["fading_soon"].as_array().unwrap();
    assert_eq!(fading.len(), 2);
    assert_eq!(fading[0]["content"], json!("nearly gone"));
    assert!(
        fading[0]["retrievability"].as_f64().unwrap()
            <= fading[1]["retrievability"].as_f64().unwrap()
    );

    // Only entries actually recalled at least once appear here.
    let recalled = body["most_recalled"].as_array().unwrap();
    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0]["content"], json!("recalled often"));

    // 30 daily points, cumulative and non-decreasing, ending at the total.
    let trend = body["trend"].as_array().unwrap();
    assert_eq!(trend.len(), 30);
    let mut previous = 0u64;
    for point in trend {
        let value = point["total"].as_u64().unwrap();
        assert!(value >= previous, "cumulative trend must never fall");
        previous = value;
        assert!(point["date"].as_str().unwrap().len() == 10);
    }
    assert_eq!(previous, 4, "the trend must end at the full pile");
    // Day one of the window carries only the baseline: the 300-day-old
    // entry predates the window, so it seeds the running total instead of
    // showing up as an addition (which would misread as a burst of
    // learning on the window's first day).
    assert_eq!(trend[0]["total"].as_u64().unwrap(), 1);
    assert_eq!(trend[0]["added"].as_u64().unwrap(), 0);
}

#[tokio::test]
async fn decay_overview_is_full_shaped_when_there_is_no_db() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let body = payload(
        handler
            .handle_memory_decay_overview(json!({ "agent_id": "nobody" }))
            .await,
    );
    // Missing db is "nothing learned yet", not an error — and the caller
    // must never have to special-case an absent key.
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["buckets"].as_array().unwrap().len(), 4);
    assert_eq!(body["fading_soon"], json!([]));
    assert_eq!(body["most_recalled"], json!([]));
    assert_eq!(body["trend"], json!([]));
}

#[tokio::test]
async fn decay_overview_rejects_a_bad_agent_id() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_memory_decay_overview(json!({ "agent_id": "../etc" }))
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "path-traversal ids must be refused: {frame:?}"
    );
}

#[tokio::test]
async fn decay_overview_clamps_the_window() {
    let home = tempfile::tempdir().unwrap();
    let agent = "decay-clamp";
    seed(home.path(), agent, vec![entry(agent, "one", 0, 0, 5.0)]).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for (asked, expected) in [(0u64, 7i64), (1000, 90), (14, 14)] {
        let body = payload(
            handler
                .handle_memory_decay_overview(json!({ "agent_id": agent, "days": asked }))
                .await,
        );
        assert_eq!(body["window_days"], json!(expected), "days={asked}");
        assert_eq!(body["trend"].as_array().unwrap().len(), expected as usize);
    }
}
