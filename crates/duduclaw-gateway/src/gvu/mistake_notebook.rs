//! MistakeNotebook — grounded error memory for the GVU evolution loop.
//!
//! Records concrete conversation failures (not abstract statistics) so the
//! Generator can produce targeted SOUL.md patches. Inspired by:
//! - REMO (arXiv:2508.18749): TextGrad + "mistake notebook" prevents overfitting
//! - MemAPO (arXiv:2603.21520): memory-augmented prompt optimization
//!
//! Each entry stores: what the user asked, what the agent said, what went wrong,
//! and (optionally) the ground truth. The GVU Generator receives relevant entries
//! as grounded context instead of abstract error statistics.
//!
//! Design decisions:
//! - SQLite storage (reuses prediction engine's DB) for durability + FTS potential
//! - Capped at 50 unresolved entries per agent (FIFO eviction) to bound memory
//! - `resolved` flag lets GVU mark entries after a successful evolution addresses them
//! - `query_by_topic()` uses simple keyword overlap (no embedding, zero LLM cost)

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use super::text_gradient::TextGradient;

/// Maximum unresolved entries kept per agent (FIFO eviction beyond this).
const MAX_UNRESOLVED_PER_AGENT: u32 = 50;

/// Category of mistake — determines GVU response priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MistakeCategory {
    /// Agent stated incorrect facts.
    Factual,
    /// Agent's tone, style, or interaction pattern was wrong.
    Behavioral,
    /// Agent lacked ability to complete the task (coding, planning, etc.).
    Capability,
    /// Agent violated safety constraints or leaked sensitive info.
    Safety,
    /// Agent claimed to perform tool actions (create_agent, etc.) without
    /// actually calling the corresponding MCP tool. Ref: Grid-Mind (2602.20683),
    /// AgentHallu (2601.06818).
    Hallucination,
}

impl MistakeCategory {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Factual => "factual",
            Self::Behavioral => "behavioral",
            Self::Capability => "capability",
            Self::Safety => "safety",
            Self::Hallucination => "hallucination",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "factual" => Self::Factual,
            "behavioral" => Self::Behavioral,
            "capability" => Self::Capability,
            "safety" => Self::Safety,
            "hallucination" => Self::Hallucination,
            _ => Self::Behavioral,
        }
    }

    /// Priority weight for GVU — Safety > Hallucination > Factual > Capability > Behavioral.
    ///
    /// Hallucination ranks between Safety and Factual because tool-use
    /// hallucination erodes system trustworthiness (the agent claims actions
    /// it never performed), but doesn't directly leak sensitive data.
    /// Ref: AgentHallu (2601.06818), The Reasoning Trap (2510.22977).
    pub fn priority(&self) -> u8 {
        match self {
            Self::Safety => 5,
            Self::Hallucination => 4,
            Self::Factual => 3,
            Self::Capability => 2,
            Self::Behavioral => 1,
        }
    }
}

/// A single recorded mistake with grounded evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistakeEntry {
    pub id: String,
    pub agent_id: String,
    pub timestamp: DateTime<Utc>,
    pub category: MistakeCategory,
    pub session_id: String,
    /// Truncated user input (≤200 chars).
    pub input_summary: String,
    /// Truncated agent response (≤200 chars).
    pub agent_response_summary: String,
    /// What went wrong — human-readable description.
    pub what_went_wrong: String,
    /// The correct answer/behavior, if known.
    pub ground_truth: Option<String>,
    /// TextGradient produced by the inner loop or verifier.
    pub gradient: TextGradient,
    /// Whether a GVU cycle has addressed this mistake.
    pub resolved: bool,
}

impl MistakeEntry {
    /// Format as a prompt section for the GVU Generator.
    pub fn to_prompt_section(&self) -> String {
        let mut s = format!(
            "- **[{}]** Session `{}`\n  Input: {}\n  Issue: {}",
            self.category.as_str().to_uppercase(),
            &self.session_id[..8.min(self.session_id.len())],
            self.input_summary,
            self.what_went_wrong,
        );
        if let Some(ref gt) = self.ground_truth {
            s.push_str(&format!("\n  Ground truth: {gt}"));
        }
        s
    }
}

/// SQLite-backed mistake notebook.
///
/// Uses a single connection with WAL mode for performance (review issue #2).
pub struct MistakeNotebook {
    db_path: PathBuf,
}

impl MistakeNotebook {
    /// Create a new MistakeNotebook backed by the given SQLite database.
    pub fn new(db_path: &Path) -> Self {
        let nb = Self {
            db_path: db_path.to_path_buf(),
        };
        if let Err(e) = nb.init_table() {
            warn!("Failed to init mistake_notebook table: {e}");
        }
        nb
    }

    fn open_conn(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.db_path).map_err(|e| format!("SQLite open: {e}"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| format!("SQLite pragma: {e}"))?;
        Ok(conn)
    }

    fn init_table(&self) -> Result<(), String> {
        let conn = self.open_conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mistakes (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                category TEXT NOT NULL,
                session_id TEXT NOT NULL,
                input_summary TEXT NOT NULL,
                agent_response_summary TEXT NOT NULL DEFAULT '',
                what_went_wrong TEXT NOT NULL,
                ground_truth TEXT,
                gradient_json TEXT NOT NULL,
                resolved INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_mistake_agent
                ON mistakes(agent_id, resolved, timestamp DESC);",
        )
        .map_err(|e| format!("Init mistakes table: {e}"))
    }

    /// Record a new mistake entry.
    pub fn record(&self, entry: &MistakeEntry) -> Result<(), String> {
        let conn = self.open_conn()?;
        let gradient_json =
            serde_json::to_string(&entry.gradient).map_err(|e| format!("Serialize gradient: {e}"))?;

        conn.execute(
            "INSERT OR REPLACE INTO mistakes
             (id, agent_id, timestamp, category, session_id, input_summary,
              agent_response_summary, what_went_wrong, ground_truth, gradient_json, resolved)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.id,
                entry.agent_id,
                entry.timestamp.to_rfc3339(),
                entry.category.as_str(),
                entry.session_id,
                entry.input_summary,
                entry.agent_response_summary,
                entry.what_went_wrong,
                entry.ground_truth,
                gradient_json,
                entry.resolved as i32,
            ],
        )
        .map_err(|e| format!("Insert mistake: {e}"))?;

        // FIFO eviction: reuse same connection (review issue #3)
        Self::evict_overflow_with_conn(&conn, &entry.agent_id)?;

        // Cleanup old resolved entries (> 30 days) to prevent unbounded growth (review R2-1)
        conn.execute(
            "DELETE FROM mistakes WHERE agent_id = ?1 AND resolved = 1 AND timestamp < ?2",
            params![entry.agent_id, (Utc::now() - chrono::Duration::days(30)).to_rfc3339()],
        ).ok();

        Ok(())
    }

    /// Query recent unresolved mistakes for an agent, ordered by priority then recency.
    pub fn query_by_agent(&self, agent_id: &str, limit: usize) -> Vec<MistakeEntry> {
        let conn = match self.open_conn() {
            Ok(c) => c,
            Err(e) => {
                warn!("MistakeNotebook query failed: {e}");
                return Vec::new();
            }
        };

        let mut stmt = match conn.prepare(
            "SELECT id, agent_id, timestamp, category, session_id, input_summary,
                    agent_response_summary, what_went_wrong, ground_truth, gradient_json, resolved
             FROM mistakes
             WHERE agent_id = ?1 AND resolved = 0
             ORDER BY
                 CASE category
                     WHEN 'safety'        THEN 0
                     WHEN 'hallucination' THEN 1
                     WHEN 'factual'       THEN 2
                     WHEN 'capability'    THEN 3
                     ELSE 4
                 END,
                 timestamp DESC
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!("MistakeNotebook prepare failed: {e}");
                return Vec::new();
            }
        };

        let rows = stmt
            .query_map(params![agent_id, limit as u32], |row| {
                Ok(MistakeEntryRow {
                    id: row.get(0)?,
                    agent_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    category: row.get(3)?,
                    session_id: row.get(4)?,
                    input_summary: row.get(5)?,
                    agent_response_summary: row.get(6)?,
                    what_went_wrong: row.get(7)?,
                    ground_truth: row.get(8)?,
                    gradient_json: row.get(9)?,
                    resolved: row.get(10)?,
                })
            })
            .ok();

        rows.map(|iter| {
            iter.filter_map(|r| r.ok())
                .filter_map(|row| row.into_entry())
                .collect()
        })
        .unwrap_or_default()
    }

    /// Query mistakes by topic keyword overlap.
    ///
    /// Searches `what_went_wrong` and `input_summary` for any keyword match.
    /// Zero LLM cost — pure string matching.
    pub fn query_by_topic(&self, keywords: &[&str], agent_id: &str, limit: usize) -> Vec<MistakeEntry> {
        if keywords.is_empty() {
            return self.query_by_agent(agent_id, limit);
        }

        let all = self.query_by_agent(agent_id, MAX_UNRESOLVED_PER_AGENT as usize);
        let mut scored: Vec<(usize, &MistakeEntry)> = all
            .iter()
            .map(|entry| {
                let text = format!(
                    "{} {} {}",
                    entry.input_summary, entry.what_went_wrong, entry.ground_truth.as_deref().unwrap_or("")
                )
                .to_lowercase();
                let score = keywords
                    .iter()
                    .filter(|kw| text.contains(&kw.to_lowercase()))
                    .count();
                (score, entry)
            })
            .filter(|(score, _)| *score > 0)
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    /// Mark entries as resolved (addressed by a GVU cycle).
    pub fn mark_resolved(&self, ids: &[&str]) -> Result<u32, String> {
        if ids.is_empty() {
            return Ok(0);
        }

        let conn = self.open_conn()?;
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "UPDATE mistakes SET resolved = 1 WHERE id IN ({})",
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("Prepare mark_resolved: {e}"))?;
        let params: Vec<&dyn rusqlite::types::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let updated = stmt
            .execute(params.as_slice())
            .map_err(|e| format!("Execute mark_resolved: {e}"))?;

        Ok(updated as u32)
    }

    /// Count unresolved mistakes for an agent.
    pub fn count_unresolved(&self, agent_id: &str) -> u32 {
        let conn = match self.open_conn() {
            Ok(c) => c,
            Err(_) => return 0,
        };

        conn.query_row(
            "SELECT COUNT(*) FROM mistakes WHERE agent_id = ?1 AND resolved = 0",
            params![agent_id],
            |row| row.get::<_, u32>(0),
        )
        .unwrap_or(0)
    }

    /// Record a tool-use hallucination as a Hallucination-category mistake.
    ///
    /// This is a convenience method called by the dispatcher when the
    /// action claim verifier detects ungrounded action claims.
    ///
    /// `agent_output_summary` should be pre-truncated by the caller
    /// (dispatcher passes `chars().take(200)`). No double-truncation here
    /// (review R3-L2).
    pub fn record_hallucination(
        &self,
        agent_id: &str,
        session_id: &str,
        claimed_action: &str,
        expected_tool: &str,
        agent_output_summary: &str,
    ) -> Result<(), String> {
        let entry = MistakeEntry {
            id: Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            timestamp: Utc::now(),
            category: MistakeCategory::Hallucination,
            session_id: session_id.to_string(),
            input_summary: "(dispatcher task)".to_string(),
            agent_response_summary: agent_output_summary.to_string(),
            what_went_wrong: format!(
                "Agent claimed '{}' but never called MCP tool '{}'",
                claimed_action, expected_tool,
            ),
            ground_truth: Some(format!(
                "Must call '{}' MCP tool to perform this action",
                expected_tool,
            )),
            gradient: TextGradient {
                target: "SOUL.md — 工具使用原則".to_string(),
                critique: format!(
                    "Agent fabricated action '{}' without tool call. \
                     Ref: Grid-Mind forced routing, AgentHallu tool-use category.",
                    claimed_action,
                ),
                suggestion: format!(
                    "Add explicit constraint: '{}' action MUST be performed via '{}' \
                     MCP tool call. Never claim completion without tool confirmation.",
                    claimed_action, expected_tool,
                ),
                severity: super::text_gradient::GradientSeverity::Blocking,
                source_layer: "action_claim_verifier".to_string(),
            },
            resolved: false,
        };
        self.record(&entry)
    }

    /// Evict oldest unresolved entries beyond the per-agent cap.
    fn evict_overflow_with_conn(conn: &Connection, agent_id: &str) -> Result<(), String> {
        conn.execute(
            "DELETE FROM mistakes WHERE id IN (
                SELECT id FROM mistakes
                WHERE agent_id = ?1 AND resolved = 0
                ORDER BY timestamp DESC
                LIMIT -1 OFFSET ?2
            )",
            params![agent_id, MAX_UNRESOLVED_PER_AGENT],
        )
        .map_err(|e| format!("Evict overflow: {e}"))?;
        Ok(())
    }
}

/// Helper for deserializing rows from SQLite.
struct MistakeEntryRow {
    id: String,
    agent_id: String,
    timestamp: String,
    category: String,
    session_id: String,
    input_summary: String,
    agent_response_summary: String,
    what_went_wrong: String,
    ground_truth: Option<String>,
    gradient_json: String,
    resolved: i32,
}

impl MistakeEntryRow {
    fn into_entry(self) -> Option<MistakeEntry> {
        let timestamp = match DateTime::parse_from_rfc3339(&self.timestamp) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => {
                warn!("MistakeEntry '{}': bad timestamp: {e}", self.id);
                return None;
            }
        };
        let gradient: TextGradient = match serde_json::from_str(&self.gradient_json) {
            Ok(g) => g,
            Err(e) => {
                warn!("MistakeEntry '{}': gradient deserialization failed: {e}", self.id);
                return None;
            }
        };

        Some(MistakeEntry {
            id: self.id,
            agent_id: self.agent_id,
            timestamp,
            category: MistakeCategory::from_str(&self.category),
            session_id: self.session_id,
            input_summary: self.input_summary,
            agent_response_summary: self.agent_response_summary,
            what_went_wrong: self.what_went_wrong,
            ground_truth: self.ground_truth,
            gradient,
            resolved: self.resolved != 0,
        })
    }
}

/// Helper to create a MistakeEntry from conversation data.
pub fn build_mistake_entry(
    agent_id: &str,
    session_id: &str,
    category: MistakeCategory,
    user_input: &str,
    agent_response: &str,
    what_went_wrong: &str,
    ground_truth: Option<&str>,
) -> MistakeEntry {
    MistakeEntry {
        id: Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        timestamp: Utc::now(),
        category,
        session_id: session_id.to_string(),
        input_summary: truncate_str(user_input, 200),
        agent_response_summary: truncate_str(agent_response, 200),
        what_went_wrong: what_went_wrong.to_string(),
        ground_truth: ground_truth.map(|s| s.to_string()),
        gradient: TextGradient::blocking(
            "InnerLoop",
            "conversation",
            what_went_wrong,
            &format!("Address this {category} issue in SOUL.md", category = category.as_str()),
        ),
        resolved: false,
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else if max_chars <= 3 {
        chars[..max_chars].iter().collect()
    } else {
        let truncated: String = chars[..max_chars - 3].iter().collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn test_db() -> (NamedTempFile, MistakeNotebook) {
        let tmp = NamedTempFile::new().unwrap();
        let nb = MistakeNotebook::new(tmp.path());
        (tmp, nb)
    }

    fn sample_entry(agent_id: &str, category: MistakeCategory) -> MistakeEntry {
        build_mistake_entry(
            agent_id,
            "session-001",
            category,
            "幫我寫一個 Python sort",
            "好的，這是 bubble sort...",
            "User wanted O(n log n) but agent gave O(n²)",
            Some("Use merge sort or timsort"),
        )
    }

    #[test]
    fn test_record_and_query() {
        let (_tmp, nb) = test_db();
        let entry = sample_entry("agent-1", MistakeCategory::Capability);
        nb.record(&entry).unwrap();

        let results = nb.query_by_agent("agent-1", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].agent_id, "agent-1");
        assert!(!results[0].resolved);
    }

    #[test]
    fn test_mark_resolved() {
        let (_tmp, nb) = test_db();
        let entry = sample_entry("agent-1", MistakeCategory::Factual);
        let id = entry.id.clone();
        nb.record(&entry).unwrap();

        assert_eq!(nb.count_unresolved("agent-1"), 1);
        nb.mark_resolved(&[&id]).unwrap();
        assert_eq!(nb.count_unresolved("agent-1"), 0);
    }

    #[test]
    fn test_query_by_topic() {
        let (_tmp, nb) = test_db();

        let e1 = build_mistake_entry(
            "agent-1", "s1", MistakeCategory::Capability,
            "寫 Python sort", "bubble sort", "太慢", Some("merge sort"),
        );
        let e2 = build_mistake_entry(
            "agent-1", "s2", MistakeCategory::Behavioral,
            "你好嗎", "我是 AI", "太冷漠", None,
        );
        nb.record(&e1).unwrap();
        nb.record(&e2).unwrap();

        let results = nb.query_by_topic(&["sort", "Python"], "agent-1", 10);
        assert_eq!(results.len(), 1);
        assert!(results[0].input_summary.contains("sort"));
    }

    #[test]
    fn test_priority_ordering() {
        let (_tmp, nb) = test_db();

        nb.record(&sample_entry("a", MistakeCategory::Behavioral)).unwrap();
        nb.record(&sample_entry("a", MistakeCategory::Safety)).unwrap();
        nb.record(&sample_entry("a", MistakeCategory::Factual)).unwrap();

        let results = nb.query_by_agent("a", 10);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].category, MistakeCategory::Safety);
        assert_eq!(results[1].category, MistakeCategory::Factual);
        assert_eq!(results[2].category, MistakeCategory::Behavioral);
    }

    #[test]
    fn test_fifo_eviction() {
        let (_tmp, nb) = test_db();

        for i in 0..55 {
            let mut entry = sample_entry("a", MistakeCategory::Capability);
            entry.id = format!("id-{i:03}");
            entry.what_went_wrong = format!("Issue #{i}");
            nb.record(&entry).unwrap();
        }

        assert_eq!(nb.count_unresolved("a"), MAX_UNRESOLVED_PER_AGENT);
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world, this is long", 10), "hello w...");
    }
}
