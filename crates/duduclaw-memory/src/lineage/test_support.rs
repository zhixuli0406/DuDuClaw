//! Shared fixtures for the P2-B tests (lineage and forget-source).

use chrono::{DateTime, Duration, TimeZone, Utc};
use duduclaw_core::types::{MemoryEntry, MemoryLayer};

use crate::engine::forget_source::{
    ApplyOutcome, ApplyReport, ExternalInputs, ForgetPlan, ForgetSelector, PlanOptions, PlanOutcome,
};
use crate::engine::{SqliteMemoryEngine, TemporalMeta};
use crate::lineage::{Provenance, SourceKind, SourceRef};
use crate::supersession_guard::TemporalWriteOutcome;

pub(crate) const AGENT: &str = "agent-p2b";

/// A fixed base time, so watermarks compare deterministically.
pub(crate) fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 4, 14, 0, 0).unwrap()
}

/// Channel message `m:<seq>` of `session`, observed `seq` seconds after t0.
pub(crate) fn msg(session: &str, seq: i64) -> SourceRef {
    SourceRef::channel_message(session, seq, t0() + Duration::seconds(seq), None)
}

/// A dispatch run (no seq; matched by time under a session watermark).
pub(crate) fn run(session: &str, key: &str, at: DateTime<Utc>) -> SourceRef {
    SourceRef::other(SourceKind::DispatchRun, session, format!("run:{key}"), at)
}

pub(crate) fn src(s: SourceRef) -> Provenance {
    Provenance::source(s)
}

pub(crate) fn entry(content: &str, layer: MemoryLayer) -> MemoryEntry {
    MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: AGENT.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        tags: vec![],
        embedding: None,
        layer,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "p2b_test".to_string(),
    }
}

pub(crate) fn triple(s: &str, p: &str, o: &str) -> TemporalMeta {
    TemporalMeta {
        subject: Some(s.to_string()),
        predicate: Some(p.to_string()),
        object: Some(o.to_string()),
        origin: Some("channel".to_string()),
        ..Default::default()
    }
}

/// Store and expect `Stored`; returns the id.
pub(crate) async fn put(
    e: &SqliteMemoryEngine,
    content: &str,
    meta: TemporalMeta,
    prov: Provenance,
) -> String {
    match e
        .store_temporal_outcome(AGENT, entry(content, MemoryLayer::Semantic), meta, prov)
        .await
        .unwrap()
    {
        TemporalWriteOutcome::Stored(id) => id,
        other => panic!("expected Stored, got {other:?}"),
    }
}

/// Store and return the outcome.
pub(crate) async fn try_put(
    e: &SqliteMemoryEngine,
    content: &str,
    meta: TemporalMeta,
    prov: Provenance,
) -> TemporalWriteOutcome {
    e.store_temporal_outcome(AGENT, entry(content, MemoryLayer::Semantic), meta, prov)
        .await
        .unwrap()
}

pub(crate) fn by_message(session: &str, messages: &[&str]) -> ForgetSelector {
    ForgetSelector {
        session: session.to_string(),
        messages: messages.iter().map(|m| m.to_string()).collect(),
        upto_seq: None,
        upto_time: None,
    }
}

pub(crate) fn by_session(
    session: &str,
    upto_seq: Option<i64>,
    upto_time: DateTime<Utc>,
) -> ForgetSelector {
    ForgetSelector {
        session: session.to_string(),
        messages: vec![],
        upto_seq,
        upto_time: Some(upto_time),
    }
}

pub(crate) async fn plan(e: &SqliteMemoryEngine, sel: &ForgetSelector) -> ForgetPlan {
    match e
        .plan_forget_source(
            AGENT,
            sel,
            PlanOptions::default(),
            &ExternalInputs::default(),
        )
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => *p,
        other => panic!("expected a plan, got {other:?}"),
    }
}

pub(crate) async fn apply(e: &SqliteMemoryEngine, plan_id: &str) -> ApplyReport {
    match e
        .apply_forget_plan(plan_id, &ExternalInputs::default())
        .await
        .unwrap()
    {
        ApplyOutcome::Applied(r) => r,
        other => panic!("expected Applied, got {other:?}"),
    }
}

/// Plan + apply one selector.
pub(crate) async fn forget(e: &SqliteMemoryEngine, sel: &ForgetSelector) -> ApplyReport {
    let p = plan(e, sel).await;
    apply(e, &p.plan_id).await
}

pub(crate) async fn count(e: &SqliteMemoryEngine, sql: &str) -> i64 {
    let conn = e.conn_for_maintenance().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

pub(crate) async fn exists(e: &SqliteMemoryEngine, id: &str) -> bool {
    count(
        e,
        &format!("SELECT COUNT(*) FROM memories WHERE id = '{id}'"),
    )
    .await
        > 0
}

pub(crate) fn is_fenced(o: &TemporalWriteOutcome, reason: crate::lineage::FenceReason) -> bool {
    matches!(o, TemporalWriteOutcome::Fenced(r) if r.reason == reason)
}
