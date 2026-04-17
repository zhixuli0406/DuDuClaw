//! Core Memory (L1) — MemGPT-style working memory that lives in the context window.
//!
//! Core Memory consists of small key-value blocks (e.g., `user_facts`, `agent_state`,
//! `pending_actions`) that are **always injected into the system prompt**. The agent
//! reads and writes these blocks via MCP tools (`core_memory_get`, `core_memory_append`,
//! `core_memory_replace`).
//!
//! This solves the cross-session context problem: when a cron/reminder/proactive message
//! is sent via `send_message`, the agent can persist context in core memory blocks.
//! When the user replies (potentially in a new session), the blocks are loaded and
//! injected, preserving continuity.
//!
//! Reference: MemGPT/Letta — <https://arxiv.org/abs/2310.08560>

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use duduclaw_core::error::{DuDuClawError, Result};

// ── Token estimation (inline, avoids cross-crate dep on duduclaw-inference) ──

fn estimate_tokens(text: &str) -> u32 {
    let mut cjk: u32 = 0;
    let mut total: u32 = 0;
    for c in text.chars() {
        total += 1;
        if matches!(c,
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' |
            '\u{3000}'..='\u{303F}' | '\u{3040}'..='\u{309F}' |
            '\u{30A0}'..='\u{30FF}' | '\u{FF00}'..='\u{FFEF}'
        ) {
            cjk += 1;
        }
    }
    let non_cjk = total - cjk;
    let cjk_tokens = ((cjk as f64) / 1.5).ceil() as u32;
    let ascii_tokens = non_cjk / 4;
    (cjk_tokens + ascii_tokens).max(1)
}

// ── Types ───────────────────────────────────────────────────────

/// A single core memory block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreBlock {
    pub id: String,
    pub agent_id: String,
    pub label: String,
    /// Scope: empty string = global (agent-wide), or `"telegram:12345"` for per-conversation.
    pub scope: String,
    pub content: String,
    pub token_count: u32,
    pub max_tokens: u32,
    pub updated_at: String,
    pub created_at: String,
}

/// Default block definitions for new agents.
pub const DEFAULT_BLOCKS: &[(&str, u32)] = &[
    ("user_facts", 600),
    ("agent_state", 600),
    ("pending_actions", 400),
    ("conversation_context", 400),
];

// ── Manager ─────────────────────────────────────────────────────

/// Manages core memory blocks in SQLite.
pub struct CoreMemoryManager {
    conn: Mutex<Connection>,
}

impl CoreMemoryManager {
    /// Open or create the core memory database at `db_path`.
    pub fn new(db_path: &std::path::Path) -> Result<Self> {
        let conn =
            Connection::open(db_path).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
        Self::init_tables(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory database (for testing).
    pub fn in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Self::init_tables(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS core_memory_blocks (
                id          TEXT PRIMARY KEY,
                agent_id    TEXT NOT NULL,
                label       TEXT NOT NULL,
                scope       TEXT NOT NULL DEFAULT '',
                content     TEXT NOT NULL DEFAULT '',
                token_count INTEGER NOT NULL DEFAULT 0,
                max_tokens  INTEGER NOT NULL DEFAULT 500,
                updated_at  TEXT NOT NULL,
                created_at  TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_core_block_unique
                ON core_memory_blocks(agent_id, label, scope);
            CREATE INDEX IF NOT EXISTS idx_core_block_agent
                ON core_memory_blocks(agent_id);
            ",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(())
    }

    /// Initialize default blocks for an agent (idempotent).
    pub async fn init_defaults(&self, agent_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        for &(label, max_tokens) in DEFAULT_BLOCKS {
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT OR IGNORE INTO core_memory_blocks (id, agent_id, label, scope, content, token_count, max_tokens, updated_at, created_at)
                 VALUES (?1, ?2, ?3, '', '', 0, ?4, ?5, ?5)",
                params![id, agent_id, label, max_tokens, now],
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        }
        Ok(())
    }

    /// Get all blocks for an agent, optionally filtered by scope.
    /// Returns both global (scope='') and scoped blocks, merged.
    pub async fn get_blocks(&self, agent_id: &str, scope: &str) -> Result<Vec<CoreBlock>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, label, scope, content, token_count, max_tokens, updated_at, created_at
                 FROM core_memory_blocks
                 WHERE agent_id = ?1 AND (scope = '' OR scope = ?2)
                 ORDER BY label ASC",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map(params![agent_id, scope], Self::row_to_block)
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        let mut blocks = Vec::new();
        for row in rows {
            blocks.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        Ok(blocks)
    }

    /// Get a specific block by label (prefers scoped, falls back to global).
    pub async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
        scope: &str,
    ) -> Result<Option<CoreBlock>> {
        let conn = self.conn.lock().await;
        // Try scoped first, then global
        let sql = "SELECT id, agent_id, label, scope, content, token_count, max_tokens, updated_at, created_at
                   FROM core_memory_blocks
                   WHERE agent_id = ?1 AND label = ?2 AND (scope = ?3 OR scope = '')
                   ORDER BY CASE WHEN scope = ?3 THEN 0 ELSE 1 END
                   LIMIT 1";
        let result = conn
            .query_row(sql, params![agent_id, label, scope], Self::row_to_block)
            .ok();
        Ok(result)
    }

    /// Append text to a block. Creates the block if it doesn't exist.
    /// Returns the updated block. Truncates from the beginning if over max_tokens.
    pub async fn append(
        &self,
        agent_id: &str,
        label: &str,
        scope: &str,
        text: &str,
    ) -> Result<CoreBlock> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        // Get existing or create
        let existing = conn
            .query_row(
                "SELECT id, content, max_tokens FROM core_memory_blocks WHERE agent_id = ?1 AND label = ?2 AND scope = ?3",
                params![agent_id, label, scope],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, u32>(2)?)),
            )
            .ok();

        let (id, new_content, max_tokens) = match existing {
            Some((id, old_content, max_tokens)) => {
                let combined = if old_content.is_empty() {
                    text.to_string()
                } else {
                    format!("{old_content}\n{text}")
                };
                (id, combined, max_tokens)
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let max_tokens = DEFAULT_BLOCKS
                    .iter()
                    .find(|(l, _)| *l == label)
                    .map(|(_, m)| *m)
                    .unwrap_or(500);
                // Insert new block
                conn.execute(
                    "INSERT INTO core_memory_blocks (id, agent_id, label, scope, content, token_count, max_tokens, updated_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, '', 0, ?5, ?6, ?6)",
                    params![id, agent_id, label, scope, max_tokens, now],
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                (id, text.to_string(), max_tokens)
            }
        };

        // Truncate from beginning if over budget
        let truncated = truncate_to_budget(&new_content, max_tokens);
        let token_count = estimate_tokens(&truncated);

        conn.execute(
            "UPDATE core_memory_blocks SET content = ?1, token_count = ?2, updated_at = ?3 WHERE id = ?4",
            params![truncated, token_count, now, id],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        Ok(CoreBlock {
            id,
            agent_id: agent_id.to_string(),
            label: label.to_string(),
            scope: scope.to_string(),
            content: truncated,
            token_count,
            max_tokens,
            updated_at: now.clone(),
            created_at: now,
        })
    }

    /// Replace a substring in a block's content.
    pub async fn replace(
        &self,
        agent_id: &str,
        label: &str,
        scope: &str,
        old_text: &str,
        new_text: &str,
    ) -> Result<CoreBlock> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        let row = conn
            .query_row(
                "SELECT id, content, max_tokens FROM core_memory_blocks WHERE agent_id = ?1 AND label = ?2 AND scope = ?3",
                params![agent_id, label, scope],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, u32>(2)?)),
            )
            .map_err(|e| DuDuClawError::Memory(format!("Block '{label}' not found: {e}")))?;

        let (id, content, max_tokens) = row;
        if !content.contains(old_text) {
            return Err(DuDuClawError::Memory(format!(
                "Substring not found in block '{label}'"
            )));
        }

        let new_content = content.replacen(old_text, new_text, 1);
        let truncated = truncate_to_budget(&new_content, max_tokens);
        let token_count = estimate_tokens(&truncated);

        conn.execute(
            "UPDATE core_memory_blocks SET content = ?1, token_count = ?2, updated_at = ?3 WHERE id = ?4",
            params![truncated, token_count, now, id],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        Ok(CoreBlock {
            id,
            agent_id: agent_id.to_string(),
            label: label.to_string(),
            scope: scope.to_string(),
            content: truncated,
            token_count,
            max_tokens,
            updated_at: now.clone(),
            created_at: now,
        })
    }

    /// Set the entire content of a block (overwrite).
    pub async fn set(
        &self,
        agent_id: &str,
        label: &str,
        scope: &str,
        content: &str,
    ) -> Result<CoreBlock> {
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();

        let existing = conn
            .query_row(
                "SELECT id, max_tokens FROM core_memory_blocks WHERE agent_id = ?1 AND label = ?2 AND scope = ?3",
                params![agent_id, label, scope],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
            )
            .ok();

        let (id, max_tokens) = match existing {
            Some((id, mt)) => (id, mt),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let max_tokens = DEFAULT_BLOCKS
                    .iter()
                    .find(|(l, _)| *l == label)
                    .map(|(_, m)| *m)
                    .unwrap_or(500);
                conn.execute(
                    "INSERT INTO core_memory_blocks (id, agent_id, label, scope, content, token_count, max_tokens, updated_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, '', 0, ?5, ?6, ?6)",
                    params![id, agent_id, label, scope, max_tokens, now],
                )
                .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                (id, max_tokens)
            }
        };

        let truncated = truncate_to_budget(content, max_tokens);
        let token_count = estimate_tokens(&truncated);

        conn.execute(
            "UPDATE core_memory_blocks SET content = ?1, token_count = ?2, updated_at = ?3 WHERE id = ?4",
            params![truncated, token_count, now, id],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

        Ok(CoreBlock {
            id,
            agent_id: agent_id.to_string(),
            label: label.to_string(),
            scope: scope.to_string(),
            content: truncated,
            token_count,
            max_tokens,
            updated_at: now.clone(),
            created_at: now,
        })
    }

    /// Delete a block.
    pub async fn delete(&self, agent_id: &str, label: &str, scope: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM core_memory_blocks WHERE agent_id = ?1 AND label = ?2 AND scope = ?3",
            params![agent_id, label, scope],
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(())
    }

    /// Render all blocks as a system prompt section.
    ///
    /// Format:
    /// ```text
    /// ## Core Memory (use core_memory_* tools to update)
    ///
    /// ### user_facts
    /// <content>
    ///
    /// ### agent_state
    /// <content>
    /// ```
    pub fn render_prompt_section(blocks: &[CoreBlock], budget_tokens: u32) -> String {
        if blocks.is_empty() {
            return String::new();
        }

        let mut parts = Vec::with_capacity(blocks.len() + 1);
        parts.push("## Core Memory (use core_memory_* tools to update)".to_string());

        let mut used_tokens: u32 = estimate_tokens(&parts[0]);

        for block in blocks {
            if block.content.is_empty() {
                continue;
            }
            let section = format!("### {}\n{}", block.label, block.content);
            let section_tokens = estimate_tokens(&section);
            if used_tokens + section_tokens > budget_tokens {
                break;
            }
            used_tokens += section_tokens;
            parts.push(section);
        }

        if parts.len() <= 1 {
            return String::new();
        }

        parts.join("\n\n")
    }

    fn row_to_block(row: &rusqlite::Row<'_>) -> std::result::Result<CoreBlock, rusqlite::Error> {
        Ok(CoreBlock {
            id: row.get(0)?,
            agent_id: row.get(1)?,
            label: row.get(2)?,
            scope: row.get(3)?,
            content: row.get(4)?,
            token_count: row.get(5)?,
            max_tokens: row.get(6)?,
            updated_at: row.get(7)?,
            created_at: row.get(8)?,
        })
    }
}

/// Truncate text from the beginning to fit within a token budget.
/// Preserves line boundaries where possible.
fn truncate_to_budget(text: &str, max_tokens: u32) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }

    // Remove lines from the beginning until within budget
    let lines: Vec<&str> = text.lines().collect();
    for start in 1..lines.len() {
        let candidate = lines[start..].join("\n");
        if estimate_tokens(&candidate) <= max_tokens {
            return candidate;
        }
    }

    // Fallback: char-level truncation from end
    let mut result = String::new();
    for c in text.chars().rev() {
        result.insert(0, c);
        if estimate_tokens(&result) >= max_tokens {
            result.remove(0);
            break;
        }
    }
    result
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_init_defaults_and_get_blocks() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();

        let blocks = mgr.get_blocks("agent-1", "").await.unwrap();
        assert_eq!(blocks.len(), 4);
        let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();
        assert!(labels.contains(&"user_facts"));
        assert!(labels.contains(&"agent_state"));
        assert!(labels.contains(&"pending_actions"));
        assert!(labels.contains(&"conversation_context"));
    }

    #[tokio::test]
    async fn test_init_defaults_idempotent() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();
        mgr.init_defaults("agent-1").await.unwrap(); // should not fail
        let blocks = mgr.get_blocks("agent-1", "").await.unwrap();
        assert_eq!(blocks.len(), 4);
    }

    #[tokio::test]
    async fn test_append_and_get() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();

        mgr.append("agent-1", "user_facts", "", "Likes coffee")
            .await
            .unwrap();
        mgr.append("agent-1", "user_facts", "", "Lives in Taipei")
            .await
            .unwrap();

        let block = mgr
            .get_block("agent-1", "user_facts", "")
            .await
            .unwrap()
            .unwrap();
        assert!(block.content.contains("Likes coffee"));
        assert!(block.content.contains("Lives in Taipei"));
    }

    #[tokio::test]
    async fn test_append_creates_block_if_missing() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        // Don't call init_defaults — append should create the block
        let block = mgr
            .append("agent-1", "custom_block", "", "some data")
            .await
            .unwrap();
        assert_eq!(block.label, "custom_block");
        assert_eq!(block.content, "some data");
    }

    #[tokio::test]
    async fn test_replace() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();
        mgr.append("agent-1", "user_facts", "", "Likes coffee")
            .await
            .unwrap();

        let block = mgr
            .replace("agent-1", "user_facts", "", "coffee", "tea")
            .await
            .unwrap();
        assert!(block.content.contains("tea"));
        assert!(!block.content.contains("coffee"));
    }

    #[tokio::test]
    async fn test_replace_not_found() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();
        mgr.append("agent-1", "user_facts", "", "Likes coffee")
            .await
            .unwrap();

        let result = mgr
            .replace("agent-1", "user_facts", "", "nonexistent", "tea")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_overwrite() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();
        mgr.append("agent-1", "user_facts", "", "old data")
            .await
            .unwrap();

        let block = mgr
            .set("agent-1", "user_facts", "", "completely new data")
            .await
            .unwrap();
        assert_eq!(block.content, "completely new data");
        assert!(!block.content.contains("old data"));
    }

    #[tokio::test]
    async fn test_scope_isolation() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();

        mgr.append("agent-1", "user_facts", "", "Global fact")
            .await
            .unwrap();
        mgr.append("agent-1", "user_facts", "telegram:123", "Chat-specific fact")
            .await
            .unwrap();

        // Scoped query should return both global and scoped
        let blocks = mgr.get_blocks("agent-1", "telegram:123").await.unwrap();
        let user_facts: Vec<&CoreBlock> = blocks.iter().filter(|b| b.label == "user_facts").collect();
        assert_eq!(user_facts.len(), 2);

        // Global-only query should not return scoped
        let blocks = mgr.get_blocks("agent-1", "").await.unwrap();
        let scoped: Vec<&CoreBlock> = blocks
            .iter()
            .filter(|b| b.scope == "telegram:123")
            .collect();
        assert_eq!(scoped.len(), 0);
    }

    #[tokio::test]
    async fn test_delete() {
        let mgr = CoreMemoryManager::in_memory().unwrap();
        mgr.init_defaults("agent-1").await.unwrap();
        mgr.append("agent-1", "user_facts", "", "data")
            .await
            .unwrap();

        mgr.delete("agent-1", "user_facts", "").await.unwrap();
        let block = mgr.get_block("agent-1", "user_facts", "").await.unwrap();
        assert!(block.is_none());
    }

    #[tokio::test]
    async fn test_render_prompt_section() {
        let blocks = vec![
            CoreBlock {
                id: "1".into(),
                agent_id: "a".into(),
                label: "user_facts".into(),
                scope: "".into(),
                content: "Likes tea".into(),
                token_count: 3,
                max_tokens: 600,
                updated_at: "".into(),
                created_at: "".into(),
            },
            CoreBlock {
                id: "2".into(),
                agent_id: "a".into(),
                label: "pending_actions".into(),
                scope: "".into(),
                content: "Enable 3 triggers".into(),
                token_count: 5,
                max_tokens: 400,
                updated_at: "".into(),
                created_at: "".into(),
            },
        ];

        let prompt = CoreMemoryManager::render_prompt_section(&blocks, 2000);
        assert!(prompt.contains("## Core Memory"));
        assert!(prompt.contains("### user_facts"));
        assert!(prompt.contains("Likes tea"));
        assert!(prompt.contains("### pending_actions"));
        assert!(prompt.contains("Enable 3 triggers"));
    }

    #[tokio::test]
    async fn test_render_skips_empty_blocks() {
        let blocks = vec![CoreBlock {
            id: "1".into(),
            agent_id: "a".into(),
            label: "user_facts".into(),
            scope: "".into(),
            content: "".into(), // empty
            token_count: 0,
            max_tokens: 600,
            updated_at: "".into(),
            created_at: "".into(),
        }];

        let prompt = CoreMemoryManager::render_prompt_section(&blocks, 2000);
        assert!(prompt.is_empty());
    }

    #[tokio::test]
    async fn test_truncate_to_budget() {
        let long_text = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10";
        let truncated = truncate_to_budget(long_text, 5);
        let tokens = estimate_tokens(&truncated);
        assert!(tokens <= 5, "Got {tokens} tokens, expected <= 5");
    }
}
