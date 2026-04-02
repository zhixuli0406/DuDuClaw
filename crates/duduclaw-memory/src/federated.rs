//! Federated memory query — cross-agent memory retrieval.
//!
//! Each agent keeps its own isolated SQLite database. The `FederatedMemoryProxy`
//! enables searching across multiple agents' memories without a shared database,
//! respecting the `shareable` flag on individual memory entries.
//!
//! Two modes of operation:
//! 1. **Local federation** (default): Directly queries multiple `SqliteMemoryEngine`
//!    instances when all agents run in the same process (single-gateway deployment).
//! 2. **IPC federation** (future): Sends `MemoryQuery` IPC messages and collects
//!    `MemoryResponse` replies for distributed deployments.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use tracing::{info, warn};

use duduclaw_core::error::Result;
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::MemoryEntry;

use crate::engine::{RetrievalWeights, SqliteMemoryEngine};

/// A cached memory engine entry with last-access tracking for LRU eviction.
struct CachedEngine {
    engine: Arc<SqliteMemoryEngine>,
    last_access: Instant,
}

/// Result of a federated search, with source attribution.
#[derive(Debug, Clone)]
pub struct FederatedResult {
    /// The memory entry.
    pub entry: MemoryEntry,
    /// Combined relevance score (from the re-ranking pass).
    pub score: f64,
}

/// Proxy that searches across multiple agents' memory databases.
///
/// Operates in local mode: opens each agent's `memory.db` on demand, queries
/// it, and merges results with cross-agent re-ranking.
pub struct FederatedMemoryProxy {
    agents_dir: PathBuf,
    retrieval_weights: RetrievalWeights,
    /// Cache of opened per-agent memory engines (agent_name → CachedEngine).
    /// Lazily populated on first query. Arc allows releasing the cache lock
    /// before awaiting search operations. Eviction uses LRU ordering by last_access.
    cache: tokio::sync::Mutex<HashMap<String, CachedEngine>>,
    /// TTL for cached search results (in seconds). Results from a federated
    /// search are stored in the requesting agent's episodic memory with this TTL.
    pub cache_ttl_seconds: u64,
}

impl FederatedMemoryProxy {
    /// Create a new proxy targeting the agents directory at `agents_dir`.
    ///
    /// Each agent is expected to have a `state/memory.db` or the proxy falls back
    /// to `~/.duduclaw/agents/<name>/memory.db`.
    pub fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            retrieval_weights: RetrievalWeights::default(),
            cache: tokio::sync::Mutex::new(HashMap::new()),
            cache_ttl_seconds: 3600,
        }
    }

    /// Discover available agent names by scanning subdirectories.
    ///
    /// Only directories with valid agent names (alphanumeric, `-`, `_`) are
    /// considered. This prevents path traversal via symlinks or special chars.
    async fn discover_agents(&self) -> Vec<String> {
        let mut agents = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.agents_dir).await {
            Ok(e) => e,
            Err(_) => return agents,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if name.starts_with('_') || !is_valid_agent_name(&name) {
                continue;
            }
            // Check if agent has a memory.db
            if memory_db_path(&path).exists() {
                agents.push(name);
            }
        }
        agents
    }

    /// Get or open a memory engine for the given agent.
    ///
    /// Validates that the resolved path stays within `agents_dir` to prevent
    /// path traversal, then caches the engine for reuse.
    async fn get_engine(&self, agent_name: &str) -> Result<()> {
        if !is_valid_agent_name(agent_name) {
            return Err(duduclaw_core::error::DuDuClawError::Memory(
                format!("invalid agent name: {agent_name}"),
            ));
        }

        let mut cache = self.cache.lock().await;
        if let Some(entry) = cache.get_mut(agent_name) {
            entry.last_access = Instant::now();
            return Ok(());
        }

        // Evict the LRU half when the cache is full to prevent unbounded growth
        if cache.len() >= 50 {
            let mut entries: Vec<(String, Instant)> = cache.iter()
                .map(|(k, v)| (k.clone(), v.last_access))
                .collect();
            entries.sort_by_key(|(_, t)| *t);
            for (key, _) in entries.iter().take(25) {
                cache.remove(key);
            }
            tracing::info!("Evicted {} LRU memory engines from cache", 25);
        }

        let agent_dir = self.agents_dir.join(agent_name);

        // Verify the resolved path stays within agents_dir (symlink defense).
        // Fail-closed: if agents_dir can't be canonicalized, reject the request.
        // If agent_dir doesn't exist yet (new agent), skip the check — SQLite will
        // create the DB under the safe base path (is_valid_agent_name already prevents
        // path separators).
        let canonical_base = std::fs::canonicalize(&self.agents_dir).map_err(|e| {
            duduclaw_core::error::DuDuClawError::Memory(
                format!("cannot resolve agents directory: {e}"),
            )
        })?;
        if agent_dir.exists() {
            let canonical_agent = std::fs::canonicalize(&agent_dir).map_err(|e| {
                duduclaw_core::error::DuDuClawError::Memory(
                    format!("cannot resolve agent path {agent_name}: {e}"),
                )
            })?;
            if !canonical_agent.starts_with(&canonical_base) {
                return Err(duduclaw_core::error::DuDuClawError::Memory(
                    format!("agent path escapes agents directory: {agent_name}"),
                ));
            }
        }

        let db_path = memory_db_path(&agent_dir);
        let engine = Arc::new(SqliteMemoryEngine::new(&db_path)?);
        cache.insert(agent_name.to_string(), CachedEngine { engine, last_access: Instant::now() });
        Ok(())
    }

    /// Search across all discoverable agents' memories.
    ///
    /// Only returns entries where `shareable == true`. Results are merged,
    /// deduplicated, and re-ranked using the standard 3D weighting.
    ///
    /// - `requesting_agent`: The agent performing the query (excluded from search targets).
    /// - `query`: Full-text search query.
    /// - `max_agents`: Maximum number of agents to query (0 = all).
    /// - `limit`: Maximum total results to return.
    pub async fn federated_search(
        &self,
        requesting_agent: &str,
        query: &str,
        max_agents: usize,
        limit: usize,
    ) -> Result<Vec<FederatedResult>> {
        let mut agents = self.discover_agents().await;

        // Exclude the requesting agent (they can search their own memory directly)
        agents.retain(|a| a != requesting_agent);

        // Limit number of agents to query.
        // Sort before truncation for deterministic selection.
        agents.sort();
        if max_agents > 0 && agents.len() > max_agents {
            agents.truncate(max_agents);
        }

        if agents.is_empty() {
            return Ok(Vec::new());
        }

        info!(
            requesting = requesting_agent,
            target_count = agents.len(),
            "Federated memory search"
        );

        let per_agent_limit = (limit * 2).max(10);
        let mut all_candidates: Vec<(MemoryEntry, f64)> = Vec::new();

        for agent_name in &agents {
            // Ensure engine is cached (acquires + releases lock internally)
            if let Err(e) = self.get_engine(agent_name).await {
                warn!(agent = agent_name, error = %e, "Failed to open agent memory DB");
                continue;
            }

            // Clone the Arc to release the cache lock before awaiting the search.
            let engine = {
                let cache = self.cache.lock().await;
                match cache.get(agent_name) {
                    Some(e) => Arc::clone(&e.engine),
                    None => continue,
                }
                // cache lock dropped here
            };

            let search_result = engine.search(agent_name, query, per_agent_limit).await;

            match search_result {
                Ok(entries) => {
                    for entry in entries {
                        if !entry.shareable {
                            continue;
                        }
                        all_candidates.push((entry, 0.0));
                    }
                }
                Err(e) => {
                    warn!(agent = agent_name, error = %e, "Federated search failed for agent");
                }
            }
        }

        if all_candidates.is_empty() {
            return Ok(Vec::new());
        }

        // Re-rank all candidates using 3D weighting
        let now = Utc::now();
        let w = &self.retrieval_weights;

        let mut scored: Vec<FederatedResult> = all_candidates
            .into_iter()
            .enumerate()
            .map(|(rank_pos, (entry, _))| {
                let hours_ago = now
                    .signed_duration_since(entry.last_accessed.unwrap_or(entry.timestamp))
                    .num_hours()
                    .max(0) as f64;
                let recency = w.recency_decay.powf(hours_ago);
                let importance = entry.importance / 10.0;
                let fts_rank = 1.0 / (1.0 + rank_pos as f64);

                let score = w.w_recency * recency + w.w_importance * importance + w.w_fts * fts_rank;
                FederatedResult { entry, score }
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Deduplicate by content (same content from different queries)
        let mut seen_ids = std::collections::HashSet::new();
        scored.retain(|r| seen_ids.insert(r.entry.id.clone()));

        scored.truncate(limit);

        info!(
            results = scored.len(),
            agents_queried = agents.len(),
            "Federated search complete"
        );

        Ok(scored)
    }
}

/// Validate that an agent name is safe for filesystem use (no path traversal).
///
/// Mirrors `IpcBroker::is_valid_agent_id` from `duduclaw-agent`.
fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Determine the memory DB path for an agent directory.
///
/// Tries `<agent_dir>/state/memory.db`, falls back to `<agent_dir>/memory.db`.
fn memory_db_path(agent_dir: &Path) -> PathBuf {
    let state_path = agent_dir.join("state").join("memory.db");
    if state_path.exists() {
        state_path
    } else {
        agent_dir.join("memory.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::MemoryLayer;
    use std::fs;
    use uuid::Uuid;

    fn shareable_entry(agent_id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Episodic,
            importance: 6.0,
            access_count: 0,
            last_accessed: None,
            source_event: "test".to_string(),
            shareable: true,
        }
    }

    fn private_entry(agent_id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Episodic,
            importance: 6.0,
            access_count: 0,
            last_accessed: None,
            source_event: "test".to_string(),
            shareable: false,
        }
    }

    #[tokio::test]
    async fn federated_search_only_shareable() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().to_path_buf();

        // Create agent-a with one shareable and one private memory
        let agent_a_dir = agents_dir.join("agent-a");
        fs::create_dir_all(&agent_a_dir).unwrap();
        let db_a = agent_a_dir.join("memory.db");
        let engine_a = SqliteMemoryEngine::new(&db_a).unwrap();
        engine_a.store("agent-a", shareable_entry("agent-a", "shareable rust tips")).await.unwrap();
        engine_a.store("agent-a", private_entry("agent-a", "private rust secrets")).await.unwrap();

        // Create agent-b with one shareable memory
        let agent_b_dir = agents_dir.join("agent-b");
        fs::create_dir_all(&agent_b_dir).unwrap();
        let db_b = agent_b_dir.join("memory.db");
        let engine_b = SqliteMemoryEngine::new(&db_b).unwrap();
        engine_b.store("agent-b", shareable_entry("agent-b", "shareable python rust guide")).await.unwrap();

        let proxy = FederatedMemoryProxy::new(agents_dir);
        let results = proxy.federated_search("agent-c", "rust", 0, 10).await.unwrap();

        // Should find 2 shareable entries (from agent-a and agent-b)
        assert_eq!(results.len(), 2, "Expected 2 shareable results, got {}", results.len());

        // Should NOT include the private entry
        assert!(
            results.iter().all(|r| r.entry.shareable),
            "All results should be shareable"
        );
        assert!(
            results.iter().all(|r| !r.entry.content.contains("private")),
            "Private entries should not appear"
        );
    }

    #[tokio::test]
    async fn federated_search_excludes_requesting_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().to_path_buf();

        // Create agent-a with shareable memory
        let agent_a_dir = agents_dir.join("agent-a");
        fs::create_dir_all(&agent_a_dir).unwrap();
        let db_a = agent_a_dir.join("memory.db");
        let engine_a = SqliteMemoryEngine::new(&db_a).unwrap();
        engine_a.store("agent-a", shareable_entry("agent-a", "my shareable knowledge about cats")).await.unwrap();

        let proxy = FederatedMemoryProxy::new(agents_dir);

        // agent-a searching should NOT return its own memories
        let results = proxy.federated_search("agent-a", "cats", 0, 10).await.unwrap();
        assert_eq!(results.len(), 0, "Requesting agent's own memories should be excluded");

        // agent-b searching should find agent-a's shareable memory
        let results = proxy.federated_search("agent-b", "cats", 0, 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn federated_search_max_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().to_path_buf();

        // Create 3 agents
        for name in &["alpha", "beta", "gamma"] {
            let dir = agents_dir.join(name);
            fs::create_dir_all(&dir).unwrap();
            let db = dir.join("memory.db");
            let engine = SqliteMemoryEngine::new(&db).unwrap();
            engine.store(name, shareable_entry(name, &format!("knowledge from {name} about testing"))).await.unwrap();
        }

        let proxy = FederatedMemoryProxy::new(agents_dir);
        // Limit to 1 agent
        let results = proxy.federated_search("observer", "testing", 1, 10).await.unwrap();
        assert!(results.len() <= 1, "max_agents=1 should return at most 1 agent's results");
    }

    #[tokio::test]
    async fn federated_search_empty_when_no_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy = FederatedMemoryProxy::new(tmp.path().to_path_buf());
        let results = proxy.federated_search("agent-x", "anything", 0, 10).await.unwrap();
        assert!(results.is_empty());
    }
}
