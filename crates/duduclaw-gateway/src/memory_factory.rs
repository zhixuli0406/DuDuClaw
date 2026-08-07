//! R2 — central `SqliteMemoryEngine` construction for gateway-internal
//! auto-write paths.
//!
//! Before this module existed, `[memory] novelty_gate` in `config.toml` was
//! wired ONLY at the MCP server's engine construction
//! (`duduclaw_cli::mcp::maybe_with_semantic_embedder`). Every gateway-internal
//! writer (`channel_reply.rs`, `wiki_ingest.rs`, `footprint_distill.rs`,
//! `autopilot_engine.rs`, `prediction/*.rs`, ...) called
//! `SqliteMemoryEngine::new()` directly — no embedder attached, no
//! `NoveltyGateConfig` override — which makes the B1 write-time
//! near-duplicate gate a documented no-op on those paths regardless of the
//! config value (see `duduclaw_memory::novelty_gate`'s "Hard invariant": no
//! attached embedder ⇒ the gate never rejects). An operator who set
//! `novelty_gate = false` (or left the `true` default) had no effect outside
//! MCP-tool-driven writes.
//!
//! `build_memory_engine` closes that gap: it is the one place gateway code
//! should call to get an engine that will actually honor the config.
//!
//! The config-reading half (`novelty_gate_enabled_from_config`) mirrors
//! `duduclaw_cli::mcp::novelty_gate_enabled_from_config` byte-for-byte.
//! It is duplicated rather than imported because `duduclaw-gateway` does not
//! (and should not start to) depend on `duduclaw-cli` — keep the two in sync
//! by hand if the parsing logic ever changes.

use std::path::Path;
use std::sync::Arc;

use duduclaw_core::error::Result;
use duduclaw_memory::{NgramHashEmbedder, NoveltyGateConfig, SqliteMemoryEngine};

/// Reads `[memory] novelty_gate` from `<home_dir>/config.toml`.
/// Default `true` (gate on) when the file is absent, unreadable, malformed,
/// or the key is unset — fails safe toward the existing dedup protection
/// rather than silently disabling it.
pub fn novelty_gate_enabled_from_config(home_dir: &Path) -> bool {
    let default = true;
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return default;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return default;
    };
    table
        .get("memory")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("novelty_gate"))
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// Open a `SqliteMemoryEngine` at `db_path`, honoring `[memory] novelty_gate`
/// from `<home_dir>/config.toml`.
///
/// - `db_path` is the actual `memory.db` file to open — per-agent, and not
///   always a simple `home_dir`-relative join (some callers cross-reference a
///   different agent's db, e.g. `handlers.rs`'s reassign/merge RPCs).
/// - `home_dir` is only consulted for `config.toml`.
///
/// When the config key is `true` (default), a fresh `NgramHashEmbedder` is
/// attached and the engine's `NoveltyGateConfig` is left at its default
/// (`enabled: true, threshold: 0.92` — same threshold the MCP surface uses).
/// When `false`, no embedder is attached at all: per `novelty_gate.rs`'s
/// "Hard invariant" this makes the gate a guaranteed no-op, so there is
/// nothing gained by also flipping `NoveltyGateConfig.enabled` to `false` —
/// omitting the embedder is simpler and avoids the (tiny, but nonzero) cost
/// of computing embeddings on every write for engines that will never use
/// them for search ranking either.
///
/// Config is read fresh on every call — see call sites for whether that
/// construction happens per-turn (config changes take effect on the very
/// next write) or against a longer-lived cached engine (changes take effect
/// on next construction / process restart); this module makes no caching
/// decision of its own.
pub fn build_memory_engine(db_path: &Path, home_dir: &Path) -> Result<SqliteMemoryEngine> {
    let engine = SqliteMemoryEngine::new(db_path)?;
    let engine = if novelty_gate_enabled_from_config(home_dir) {
        engine
            .with_embedder(Arc::new(NgramHashEmbedder::new()))
            .with_novelty_gate_config(NoveltyGateConfig::default())
    } else {
        engine
    };
    Ok(engine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::traits::MemoryEngine as _;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};

    fn entry(agent_id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "test".to_string(),
        }
    }

    #[test]
    fn config_defaults_to_enabled_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(novelty_gate_enabled_from_config(dir.path()));
    }

    #[test]
    fn config_reads_explicit_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[memory]\nnovelty_gate = false\n").unwrap();
        assert!(!novelty_gate_enabled_from_config(dir.path()));
    }

    #[tokio::test]
    async fn enabled_engine_rejects_near_duplicate_semantic_writes() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[memory]\nnovelty_gate = true\n").unwrap();
        let db_path = home.path().join("memory.db");
        let engine = build_memory_engine(&db_path, home.path()).unwrap();

        engine
            .store("agent-a", entry("agent-a", "The deployment pipeline uses GitHub Actions for CI/CD."))
            .await
            .unwrap();

        let rejection = engine
            .check_novelty(
                "agent-a",
                MemoryLayer::Semantic,
                "The deployment pipeline uses GitHub Actions for CI/CD.",
            )
            .await;
        assert!(
            rejection.is_some(),
            "an engine built with novelty_gate=true must reject a near-duplicate write"
        );
    }

    #[tokio::test]
    async fn disabled_engine_never_rejects_near_duplicate_writes() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[memory]\nnovelty_gate = false\n").unwrap();
        let db_path = home.path().join("memory.db");
        let engine = build_memory_engine(&db_path, home.path()).unwrap();

        engine
            .store("agent-a", entry("agent-a", "The deployment pipeline uses GitHub Actions for CI/CD."))
            .await
            .unwrap();

        let rejection = engine
            .check_novelty(
                "agent-a",
                MemoryLayer::Semantic,
                "The deployment pipeline uses GitHub Actions for CI/CD.",
            )
            .await;
        assert!(
            rejection.is_none(),
            "an engine built with novelty_gate=false (no embedder attached) must never reject a write"
        );
    }
}
