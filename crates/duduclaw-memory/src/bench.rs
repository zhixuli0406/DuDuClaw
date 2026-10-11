//! HippoRAG-lite PPR latency bench — the measurement gate for the deferred
//! LightRAG-style subgraph-partition work.
//!
//! The design guidance is "measure before you build": don't introduce subgraph
//! partitioning until Personalized-PageRank latency is actually a problem. This
//! module times `graph_rank_scores` over the agent's live triple count and
//! reports P50/P95 plus a deterministic partition recommendation, so the
//! trigger is a number, not a guess.

use crate::engine::SqliteMemoryEngine;
use chrono::Utc;
use duduclaw_core::error::Result;
use std::time::Instant;

/// Recommend partitioning when the graph is large **or** tail latency is high.
pub const PARTITION_TRIPLE_THRESHOLD: u64 = 10_000;
pub const PARTITION_P95_MS_THRESHOLD: f64 = 50.0;

/// Timing report for one bench run.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GraphBenchReport {
    pub agent_id: String,
    pub triples: u64,
    pub iterations: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub mean_ms: f64,
    pub max_ms: f64,
    /// True when triples ≥ threshold OR P95 ≥ threshold — the signal to start
    /// the deferred subgraph-partition engineering.
    pub partition_recommended: bool,
}

/// Percentile (nearest-rank) of an already-sorted millisecond slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn should_partition(triples: u64, p95_ms: f64) -> bool {
    triples >= PARTITION_TRIPLE_THRESHOLD || p95_ms >= PARTITION_P95_MS_THRESHOLD
}

/// Run `iterations` PPR passes for `agent_id`/`query` and summarize latency.
/// `iterations` is clamped to at least 1. A zero-triple graph returns a
/// zero-latency report without timing no-op PPR passes.
pub async fn graph_rank_bench(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    query: &str,
    iterations: usize,
) -> Result<GraphBenchReport> {
    let iterations = iterations.max(1);
    let conn = engine.conn_for_maintenance().await;
    let now = Utc::now().to_rfc3339();
    let triples = crate::graph_rank::count_agent_triples(&conn, agent_id, &now)?;

    if triples == 0 {
        return Ok(GraphBenchReport {
            agent_id: agent_id.to_string(),
            triples,
            iterations,
            p50_ms: 0.0,
            p95_ms: 0.0,
            mean_ms: 0.0,
            max_ms: 0.0,
            partition_recommended: false,
        });
    }

    let mut samples_ms: Vec<f64> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let _ = crate::graph_rank::graph_rank_scores(&conn, agent_id, query, &now)?;
        samples_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let sum: f64 = samples_ms.iter().sum();
    let mean_ms = sum / samples_ms.len() as f64;
    let p50_ms = percentile(&samples_ms, 50.0);
    let p95_ms = percentile(&samples_ms, 95.0);
    let max_ms = *samples_ms.last().unwrap_or(&0.0);

    let partition_recommended = should_partition(triples, p95_ms);

    Ok(GraphBenchReport {
        agent_id: agent_id.to_string(),
        triples,
        iterations,
        p50_ms,
        p95_ms,
        mean_ms,
        max_ms,
        partition_recommended,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SqliteMemoryEngine;
    use crate::TemporalMeta;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};

    fn assert_empty_report(report: &GraphBenchReport, agent_id: &str, iterations: usize) {
        assert_eq!(report.agent_id, agent_id);
        assert_eq!(report.triples, 0);
        assert_eq!(report.iterations, iterations);
        assert_eq!(report.p50_ms, 0.0);
        assert_eq!(report.p95_ms, 0.0);
        assert_eq!(report.mean_ms, 0.0);
        assert_eq!(report.max_ms, 0.0);
        assert!(!report.partition_recommended);
    }

    #[test]
    fn partition_thresholds_are_inclusive_and_independent() {
        assert!(!should_partition(PARTITION_TRIPLE_THRESHOLD - 1, 0.0));
        assert!(should_partition(PARTITION_TRIPLE_THRESHOLD, 0.0));
        assert!(should_partition(PARTITION_TRIPLE_THRESHOLD + 1, 0.0));
        assert!(!should_partition(1, PARTITION_P95_MS_THRESHOLD - 0.1));
        assert!(should_partition(1, PARTITION_P95_MS_THRESHOLD));
        assert!(should_partition(1, PARTITION_P95_MS_THRESHOLD + 0.1));
    }

    #[test]
    fn percentile_nearest_rank() {
        let s = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&s, 50.0), 5.0);
        assert_eq!(percentile(&s, 95.0), 10.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[tokio::test]
    async fn empty_graph_zero_latency_no_partition() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let report = graph_rank_bench(&engine, "agent1", "anything", 3)
            .await
            .unwrap();
        assert_empty_report(&report, "agent1", 3);
    }

    #[tokio::test]
    async fn empty_graph_clamps_zero_iterations() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let report = graph_rank_bench(&engine, "agent1", "anything", 0)
            .await
            .unwrap();
        assert_empty_report(&report, "agent1", 1);
    }

    #[tokio::test]
    async fn another_agents_graph_does_not_make_target_graph_nonempty() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        engine
            .store_temporal(
                "other-agent",
                MemoryEntry {
                    id: "other-memory".into(),
                    agent_id: "other-agent".into(),
                    content: "entity relates to widgets".into(),
                    timestamp: Utc::now(),
                    tags: vec![],
                    embedding: None,
                    layer: MemoryLayer::Semantic,
                    importance: 5.0,
                    access_count: 0,
                    last_accessed: None,
                    source_event: String::new(),
                },
                TemporalMeta {
                    subject: Some("entity".into()),
                    predicate: Some("relates".into()),
                    object: Some("widgets".into()),
                    ..Default::default()
                },
                crate::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        let other_report = graph_rank_bench(&engine, "other-agent", "widgets", 1)
            .await
            .unwrap();
        assert_eq!(other_report.triples, 1);
        let report = graph_rank_bench(&engine, "target-agent", "widgets", 2)
            .await
            .unwrap();
        assert_empty_report(&report, "target-agent", 2);
    }

    #[tokio::test]
    async fn small_graph_benches_with_measured_partition_decision() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for i in 0..5 {
            engine
                .store_temporal(
                    "agent1",
                    MemoryEntry {
                        id: format!("m{i}"),
                        agent_id: "agent1".into(),
                        content: format!("fact number {i} about widgets"),
                        timestamp: Utc::now(),
                        tags: vec![],
                        embedding: None,
                        layer: MemoryLayer::Semantic,
                        importance: 5.0,
                        access_count: 0,
                        last_accessed: None,
                        source_event: String::new(),
                    },
                    TemporalMeta {
                        subject: Some(format!("entity{i}")),
                        predicate: Some("relates".into()),
                        object: Some("widgets".into()),
                        ..Default::default()
                    },
                    crate::lineage::Provenance::test_only(),
                )
                .await
                .unwrap();
        }
        let report = graph_rank_bench(&engine, "agent1", "widgets", 2)
            .await
            .unwrap();
        assert_eq!(report.triples, 5);
        assert_eq!(report.agent_id, "agent1");
        assert_eq!(report.iterations, 2);
        // A busy runner may cross the real latency threshold even on a small graph.
        assert_eq!(
            report.partition_recommended,
            report.triples >= PARTITION_TRIPLE_THRESHOLD
                || report.p95_ms >= PARTITION_P95_MS_THRESHOLD
        );
        assert!(report.p95_ms >= 0.0);
        assert!(report.p50_ms <= report.p95_ms);
        assert!(report.p95_ms <= report.max_ms);
        assert!(report.mean_ms.is_finite());
    }
}
