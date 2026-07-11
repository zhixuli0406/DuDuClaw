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

/// Run `iterations` PPR passes for `agent_id`/`query` and summarize latency.
/// `iterations` is clamped to at least 1. A zero-triple graph returns a
/// zero-latency report (PPR short-circuits) rather than erroring.
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

    let partition_recommended =
        triples >= PARTITION_TRIPLE_THRESHOLD || p95_ms >= PARTITION_P95_MS_THRESHOLD;

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
        assert_eq!(report.triples, 0);
        assert_eq!(report.iterations, 3);
        assert!(!report.partition_recommended);
    }

    #[tokio::test]
    async fn small_graph_benches_and_recommends_nothing() {
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
                )
                .await
                .unwrap();
        }
        let report = graph_rank_bench(&engine, "agent1", "widgets", 2)
            .await
            .unwrap();
        assert_eq!(report.triples, 5);
        // A 5-triple graph is nowhere near either threshold.
        assert!(!report.partition_recommended);
        assert!(report.p95_ms >= 0.0);
    }
}
