//! `approvals.decide` for `knowledge_quarantine` (v1.67.1): the dashboard is
//! the only decision surface; an expired card cannot be approved; the side
//! effect reports the decision's counts.
use super::*;

fn admin_ctx() -> UserContext {
    UserContext::admin_fallback()
}

fn response(frame: &WsFrame) -> Result<Value, String> {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => Ok(p.clone()),
        WsFrame::Response { error, .. } => Err(format!("{error:?}")),
        other => panic!("unexpected frame shape: {other:?}"),
    }
}

fn entry(agent: &str, content: &str) -> duduclaw_core::types::MemoryEntry {
    duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        tags: vec![],
        embedding: None,
        layer: duduclaw_core::types::MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "test".to_string(),
    }
}

fn meta(object: &str, origin: &str) -> duduclaw_memory::TemporalMeta {
    duduclaw_memory::TemporalMeta {
        subject: Some("policy:refund".into()),
        predicate: Some("window".into()),
        object: Some(object.into()),
        origin: Some(origin.into()),
        ..Default::default()
    }
}

/// A held claim against an operator fact, with its pending review card.
async fn held_claim_card(home: &std::path::Path, ttl: i64) -> (crate::approval::ApprovalId, String) {
    let db = home.join("memory.db");
    let engine = duduclaw_memory::SqliteMemoryEngine::new(&db).unwrap();
    engine
        .store_temporal("support", entry("support", "7 days"), meta("7", "operator"), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let held = engine
        .hold_refused_claim("support", entry("support", "forever"), meta("forever", "channel"), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let digest = engine.held_claim_view("support", &held).await.unwrap().unwrap().claim_digest;
    let id = file_card(home, "support", vec![held.clone()], true, Some(digest), ttl, &db).await;
    (id, held)
}

async fn file_card(
    home: &std::path::Path,
    agent: &str,
    ids: Vec<String>,
    promote: bool,
    digest: Option<String>,
    ttl: i64,
    db: &std::path::Path,
) -> crate::approval::ApprovalId {
    let broker = crate::approval::ApprovalBroker::open(home).unwrap();
    let mut payload = json!({
        "memory_db": db.to_string_lossy(),
        "agent_id": agent,
        "quarantined_ids": ids,
        "promote_on_approve": promote,
        "disposition": if promote { "trust_held" } else { "" },
    });
    if let Some(d) = digest {
        payload["claim_digest"] = json!(d);
    }
    broker
        .request(agent, crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE, "知識審核", payload, ttl)
        .await
        .unwrap()
}

fn status(home: &std::path::Path, id: &crate::approval::ApprovalId) -> crate::approval::ApprovalStatus {
    let rt = tokio::runtime::Handle::current();
    let broker = crate::approval::ApprovalBroker::open(home).unwrap();
    tokio::task::block_in_place(|| rt.block_on(broker.get(id))).unwrap().unwrap().status
}

#[tokio::test(flavor = "multi_thread")]
async fn dashboard_approval_promotes_and_reports_counts() {
    let home = tempfile::tempdir().unwrap();
    let (id, _held) = held_claim_card(home.path(), 3600).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_approvals_decide(json!({ "id": id.as_str(), "approve": true }), &admin_ctx())
        .await;
    let p = response(&frame).unwrap();
    assert_eq!(
        p["side_effect"],
        json!({ "quarantine_promoted": 1, "quarantine_stale": 0 })
    );
}

/// L1: a card past its deadline is a denial even before the sweep flips it.
#[tokio::test(flavor = "multi_thread")]
async fn expired_knowledge_card_cannot_be_approved() {
    let home = tempfile::tempdir().unwrap();
    let (id, held) = held_claim_card(home.path(), 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_approvals_decide(json!({ "id": id.as_str(), "approve": true }), &admin_ctx())
        .await;
    assert!(response(&frame).is_err());
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    assert_eq!(
        broker.get(&id).await.unwrap().unwrap().status,
        crate::approval::ApprovalStatus::Expired
    );
    // Nothing was promoted.
    let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    assert_eq!(engine.is_quarantined("support", &held).await.unwrap(), Some(true));
    let h = engine.get_history("support", "policy:refund", "window").await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].content, "7 days");
}


/// R-M5: when the side effect fails, the approval stays pending and the
/// caller gets the error; a retry with a working store then succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn failed_side_effect_leaves_the_card_pending() {
    let home = tempfile::tempdir().unwrap();
    let bad_db = home.path().join("no-such-dir").join("memory.db");
    let id = file_card(home.path(), "support", vec!["x".into()], true, Some("d".into()), 3600, &bad_db).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_approvals_decide(json!({ "id": id.as_str(), "approve": true }), &admin_ctx())
        .await;
    assert!(response(&frame).is_err());
    assert_eq!(status(home.path(), &id), crate::approval::ApprovalStatus::Pending);
}

/// R-M5: two concurrent approvals of one card promote once; the other writes
/// nothing (zero counts) — never a second promotion.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_double_approval_promotes_once() {
    let home = tempfile::tempdir().unwrap();
    let (id, _held) = held_claim_card(home.path(), 3600).await;
    let h1 = MethodHandler::new(home.path().to_path_buf()).await;
    let h2 = MethodHandler::new(home.path().to_path_buf()).await;
    let params = json!({ "id": id.as_str(), "approve": true });
    let ctx = admin_ctx();
    let (a, b) = tokio::join!(
        h1.handle_approvals_decide(params.clone(), &ctx),
        h2.handle_approvals_decide(params.clone(), &ctx)
    );
    let promoted: u64 = [a, b]
        .iter()
        .filter_map(|f| response(f).ok())
        .map(|p| p["side_effect"]["quarantine_promoted"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(promoted, 1);
    let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let h = engine.get_history("support", "policy:refund", "window").await.unwrap();
    assert_eq!(h.iter().filter(|r| r.valid_until.is_none()).count(), 1);
    assert_eq!(status(home.path(), &id), crate::approval::ApprovalStatus::Approved);
}

/// A trust-held card whose ids are not held claims, or belong to another
/// agent, promotes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn card_with_non_held_or_foreign_ids_promotes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    let engine = duduclaw_memory::SqliteMemoryEngine::new(&db).unwrap();
    let fact = engine
        .store_temporal("support", entry("support", "7 days"), meta("7", "operator"), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let foreign = engine
        .hold_refused_claim("other", entry("other", "x"), meta("x", "channel"), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let foreign_digest = engine.held_claim_view("other", &foreign).await.unwrap().unwrap().claim_digest;
    drop(engine);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for ids in [vec![fact.clone()], vec![foreign.clone()]] {
        let id = file_card(home.path(), "support", ids, true, Some(foreign_digest.clone()), 3600, &db).await;
        let p = response(
            &handler
                .handle_approvals_decide(json!({ "id": id.as_str(), "approve": true }), &admin_ctx())
                .await,
        )
        .unwrap();
        assert_eq!(p["side_effect"], json!({ "quarantine_promoted": 0, "quarantine_stale": 0 }));
    }
    let engine = duduclaw_memory::SqliteMemoryEngine::new(&db).unwrap();
    assert!(engine.held_claim_view("other", &foreign).await.unwrap().is_some(), "untouched");
    let h = engine.get_history("support", "policy:refund", "window").await.unwrap();
    assert_eq!(h.len(), 1);
}

/// H2(b) end to end: approving a burst card through `approvals.decide` turns
/// an outranked row into a held claim AND files its own conflict card.
#[tokio::test(flavor = "multi_thread")]
async fn release_converted_rows_get_a_card_through_the_rpc() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    let mut engine = duduclaw_memory::SqliteMemoryEngine::new(&db).unwrap();
    engine
        .store_temporal("support", entry("support", "7 days"), meta("7", "operator"), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    engine.supersession_trust_guard = false;
    let mut q = meta("forever", "channel");
    q.quarantined = true;
    let row = engine.store_temporal("support", entry("support", "forever"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    drop(engine);
    let burst = file_card(home.path(), "support", vec![row.clone()], false, None, 3600, &db).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = response(
        &handler
            .handle_approvals_decide(json!({ "id": burst.as_str(), "approve": true }), &admin_ctx())
            .await,
    )
    .unwrap();
    assert_eq!(p["side_effect"], json!({ "quarantine_released": 0, "quarantine_held": 1 }));
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let pending = broker.list_pending(Some("support")).await.unwrap();
    let card = pending
        .iter()
        .find(|r| r.payload["quarantined_ids"] == json!([row.clone()]) && r.id != burst)
        .expect("conflict card filed for the converted row");
    assert_eq!(card.payload["disposition"], "trust_held");
    assert_eq!(card.payload["snippet"], "forever");
    assert_eq!(card.payload["new_value"], "forever");
    assert_eq!(card.payload["existing_value"], "7");
}

// ── fourth batch ────────────────────────────────────────────────────────────

/// Item 2: a row converted by an earlier (failed) release attempt, whose card
/// was never filed, gets its own conflict card on the retry even though the
/// burst card being decided still lists it.
#[tokio::test(flavor = "multi_thread")]
async fn retried_release_files_the_missing_conflict_card() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    let mut engine = duduclaw_memory::SqliteMemoryEngine::new(&db).unwrap();
    engine
        .store_temporal("support", entry("support", "7 days"), meta("7", "operator"), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    engine.supersession_trust_guard = false;
    let mut q = meta("forever", "channel");
    q.quarantined = true;
    let x = engine.store_temporal("support", entry("support", "forever"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    engine.supersession_trust_guard = true;
    let burst = file_card(home.path(), "support", vec![x.clone()], false, None, 3600, &db).await;
    // First attempt: converted X, then failed before any card was filed.
    let first = engine.release_quarantine("support", &[x.clone()]).await.unwrap();
    assert!(first.held[0].newly_converted);
    drop(engine);
    // Retry through the RPC.
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = response(
        &handler
            .handle_approvals_decide(json!({ "id": burst.as_str(), "approve": true }), &admin_ctx())
            .await,
    )
    .unwrap();
    assert_eq!(p["side_effect"], json!({ "quarantine_released": 0, "quarantine_held": 1 }));
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let pending = broker.list_pending(Some("support")).await.unwrap();
    assert!(
        pending
            .iter()
            .any(|r| r.payload["disposition"] == "trust_held" && r.payload["quarantined_ids"] == json!([x.clone()])),
        "X has its own pending conflict card"
    );
}

/// Item 3: approve and deny on one card are serialized; the recorded
/// decision always matches what was applied.
#[tokio::test(flavor = "multi_thread")]
async fn approve_and_deny_race_records_what_was_applied() {
    for _ in 0..5 {
        let home = tempfile::tempdir().unwrap();
        let (id, held) = held_claim_card(home.path(), 3600).await;
        let h1 = MethodHandler::new(home.path().to_path_buf()).await;
        let h2 = MethodHandler::new(home.path().to_path_buf()).await;
        let ctx = admin_ctx();
        let (a, d) = tokio::join!(
            h1.handle_approvals_decide(json!({ "id": id.as_str(), "approve": true }), &ctx),
            h2.handle_approvals_decide(json!({ "id": id.as_str(), "approve": false }), &ctx)
        );
        assert_eq!(
            [response(&a).is_ok(), response(&d).is_ok()].iter().filter(|b| **b).count(),
            1,
            "exactly one decision wins"
        );
        let st = status(home.path(), &id);
        let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
        let h = engine.get_history("support", "policy:refund", "window").await.unwrap();
        let current = h.iter().find(|r| r.valid_until.is_none()).unwrap();
        match st {
            crate::approval::ApprovalStatus::Approved => assert_eq!(current.content, "forever"),
            crate::approval::ApprovalStatus::Denied => {
                assert_eq!(current.content, "7 days");
                assert_eq!(engine.get_origin_trust("support", &held).await.unwrap(), Some(0.1));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// Item 3: the side effect applied but the decision could not be recorded →
/// an error frame naming what was applied, plus an audit row; never OK.
#[tokio::test(flavor = "multi_thread")]
async fn unrecorded_decision_is_an_error_with_an_audit_row() {
    let home = tempfile::tempdir().unwrap();
    let (id, _held) = held_claim_card(home.path(), 3600).await;
    crate::handlers::approvals_decide::force_decide_failure(id.as_str(), "disk I/O error");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_approvals_decide(json!({ "id": id.as_str(), "approve": true }), &admin_ctx())
        .await;
    let err = response(&frame).unwrap_err();
    assert!(err.contains("已套用") && err.contains("quarantine_promoted") && err.contains("disk I/O error"), "{err}");
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("knowledge_review_decision_unrecorded"));
}
