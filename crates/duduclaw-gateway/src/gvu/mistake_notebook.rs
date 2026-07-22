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
///
/// `pub(crate)` so `reflexion.rs` can size its per-category fetch to the same
/// upper bound when grouping mistakes by `source_kind` (WP2).
pub(crate) const MAX_UNRESOLVED_PER_AGENT: u32 = 50;

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
    /// Origin of the failure signal within `category` (WP2, GovMem 2607.02579).
    ///
    /// `category` groups mistakes by *kind of error* (Capability/Factual/...);
    /// `source_kind` further distinguishes *how the failure was detected*
    /// within that category — e.g. `"decision_gap"` (RFC-24 unresolved
    /// decision reference) vs `"task_failure"` (general task-outcome
    /// failure) both land in `MistakeCategory::Capability` today but are
    /// unrelated failure modes and must not be pooled together for
    /// consolidation counting. Empty string = unattributed / legacy rows —
    /// they form their own group rather than silently joining another
    /// (fail-safe, backward compatible with pre-WP2 data).
    #[serde(default)]
    pub source_kind: String,
}

/// Max chars of `ground_truth` injected into a prompt section (CJK-safe cap).
/// STV (arXiv:2605.30290): the reference/correct answer is the supervision
/// signal, so it is worth keeping — but bounded so one long entry can't crowd
/// out the prompt budget.
const GROUND_TRUTH_PROMPT_MAX_CHARS: usize = 300;

impl MistakeEntry {
    /// Format as a prompt section for the GVU Generator / Reflexion F2a.
    ///
    /// When `ground_truth` is present the section carries **two grounded parts**
    /// — the mistake (`Issue`) and the correct answer (`Correct answer`, the STV
    /// reference solution) — so the model sees both what went wrong and what
    /// right looks like. The reference is truncated with
    /// [`duduclaw_core::truncate_chars`] (codepoint count, CJK-safe) so a long
    /// entry can't blow the prompt budget or panic on a multi-byte boundary.
    pub fn to_prompt_section(&self) -> String {
        let mut s = format!(
            "- **[{}]** Session `{}`\n  Input: {}\n  Issue: {}",
            self.category.as_str().to_uppercase(),
            &self.session_id[..8.min(self.session_id.len())],
            self.input_summary,
            self.what_went_wrong,
        );
        if let Some(ref gt) = self.ground_truth {
            let gt = gt.trim();
            if !gt.is_empty() {
                let shown = duduclaw_core::truncate_chars(gt, GROUND_TRUTH_PROMPT_MAX_CHARS);
                let ellipsis = if shown.chars().count() < gt.chars().count() {
                    "…"
                } else {
                    ""
                };
                s.push_str(&format!("\n  Correct answer: {shown}{ellipsis}"));
            }
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
        .map_err(|e| format!("Init mistakes table: {e}"))?;

        // WP2 (GovMem 2607.02579): distinguish *how* a mistake within the same
        // `category` was detected, so unrelated failure modes (e.g. RFC-24
        // decision-gap vs. generic task-failure — both land in `Capability`)
        // aren't pooled into one consolidation count. Idempotent migration:
        // SQLite has no `ADD COLUMN IF NOT EXISTS`, so a duplicate-column
        // error on re-run is expected and ignored.
        match conn.execute_batch("ALTER TABLE mistakes ADD COLUMN source_kind TEXT NOT NULL DEFAULT ''") {
            Ok(()) => {}
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(format!("Migrate mistakes.source_kind: {e}"));
                }
            }
        }
        Ok(())
    }

    /// Record a new mistake entry.
    pub fn record(&self, entry: &MistakeEntry) -> Result<(), String> {
        let conn = self.open_conn()?;
        let gradient_json =
            serde_json::to_string(&entry.gradient).map_err(|e| format!("Serialize gradient: {e}"))?;

        conn.execute(
            "INSERT OR REPLACE INTO mistakes
             (id, agent_id, timestamp, category, session_id, input_summary,
              agent_response_summary, what_went_wrong, ground_truth, gradient_json, resolved,
              source_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                entry.source_kind,
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
                    agent_response_summary, what_went_wrong, ground_truth, gradient_json, resolved,
                    source_kind
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
                    source_kind: row.get(11)?,
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

        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
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

    /// Count unresolved mistakes of a specific category for an agent (F2b).
    pub fn count_unresolved_by_category(&self, agent_id: &str, category: MistakeCategory) -> u32 {
        let conn = match self.open_conn() {
            Ok(c) => c,
            Err(e) => {
                warn!("count_unresolved_by_category failed: {e}");
                return 0;
            }
        };
        conn.query_row(
            "SELECT COUNT(*) FROM mistakes
             WHERE agent_id = ?1 AND category = ?2 AND resolved = 0",
            params![agent_id, category.as_str()],
            |row| row.get::<_, u32>(0),
        )
        .unwrap_or(0)
    }

    /// Query unresolved mistakes of a specific category, newest/priority first (F2b).
    pub fn query_unresolved_by_category(
        &self,
        agent_id: &str,
        category: MistakeCategory,
        limit: usize,
    ) -> Vec<MistakeEntry> {
        // Reuse query_by_agent's row mapping + priority ordering, then filter.
        self.query_by_agent(agent_id, MAX_UNRESOLVED_PER_AGENT as usize)
            .into_iter()
            .filter(|m| m.category == category)
            .take(limit)
            .collect()
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
            // Not one of the two WP2-attributed paths (decision_gap /
            // task_failure) — leave unattributed so it groups on its own
            // rather than joining either bucket (fail-safe default).
            source_kind: String::new(),
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
    source_kind: String,
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
            source_kind: self.source_kind,
        })
    }
}

/// Helper to create a MistakeEntry from conversation data.
///
/// `source_kind` (WP2) records *how* this mistake was detected within its
/// `category` — e.g. `"decision_gap"` / `"task_failure"` — so consolidation
/// can count independent failure modes separately instead of pooling them.
/// Pass `""` when the call site has no such distinction to make.
#[allow(clippy::too_many_arguments)]
pub fn build_mistake_entry(
    agent_id: &str,
    session_id: &str,
    category: MistakeCategory,
    user_input: &str,
    agent_response: &str,
    what_went_wrong: &str,
    ground_truth: Option<&str>,
    source_kind: &str,
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
        source_kind: source_kind.to_string(),
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
            "",
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
            "寫 Python sort", "bubble sort", "太慢", Some("merge sort"), "",
        );
        let e2 = build_mistake_entry(
            "agent-1", "s2", MistakeCategory::Behavioral,
            "你好嗎", "我是 AI", "太冷漠", None, "",
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

    #[test]
    fn test_prompt_section_includes_ground_truth() {
        let entry = build_mistake_entry(
            "agent-1",
            "session-abcdef01",
            MistakeCategory::Capability,
            "寫 Python sort",
            "bubble sort",
            "太慢",
            Some("Use merge sort or timsort"),
            "",
        );
        let section = entry.to_prompt_section();
        assert!(section.contains("Issue: 太慢"));
        // Ground truth surfaces as the STV "Correct answer" reference part.
        assert!(section.contains("Correct answer: Use merge sort or timsort"));
    }

    #[test]
    fn test_prompt_section_without_ground_truth_unchanged() {
        let entry = build_mistake_entry(
            "agent-1",
            "session-abcdef01",
            MistakeCategory::Behavioral,
            "你好嗎",
            "我是 AI",
            "太冷漠",
            None,
            "",
        );
        let section = entry.to_prompt_section();
        assert!(section.contains("Issue: 太冷漠"));
        // No ground truth ⇒ no correct-answer part appended.
        assert!(!section.contains("Correct answer"));
    }

    #[test]
    fn test_prompt_section_truncates_long_cjk_ground_truth() {
        // A ground truth well past the cap, all multi-byte CJK — must not panic
        // and must be bounded to the char cap (+ ellipsis).
        let long_gt: String = "正".repeat(GROUND_TRUTH_PROMPT_MAX_CHARS + 50);
        let mut entry = sample_entry("agent-1", MistakeCategory::Factual);
        entry.ground_truth = Some(long_gt);
        let section = entry.to_prompt_section();
        assert!(section.contains("Correct answer:"));
        assert!(section.ends_with('…'), "over-cap ground truth is ellipsized");
        // Count only the shown ground-truth chars: cap of 正 plus the ellipsis.
        let shown: String = section
            .split("Correct answer: ")
            .nth(1)
            .unwrap()
            .chars()
            .filter(|c| *c == '正')
            .collect();
        assert_eq!(shown.chars().count(), GROUND_TRUTH_PROMPT_MAX_CHARS);
    }

    #[test]
    fn test_source_kind_round_trips() {
        let (_tmp, nb) = test_db();
        let entry = build_mistake_entry(
            "agent-1",
            "s1",
            MistakeCategory::Capability,
            "user text",
            "agent text",
            "wrong thing",
            None,
            "decision_gap",
        );
        nb.record(&entry).unwrap();

        let results = nb.query_by_agent("agent-1", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_kind, "decision_gap");
    }

    #[test]
    fn test_source_kind_defaults_to_empty_string() {
        // A default `MistakeEntry` (e.g. legacy pre-WP2 construction path)
        // must not fail to record/query — empty source_kind is its own group.
        let (_tmp, nb) = test_db();
        let entry = sample_entry("agent-1", MistakeCategory::Capability);
        assert_eq!(entry.source_kind, "");
        nb.record(&entry).unwrap();

        let results = nb.query_by_agent("agent-1", 10);
        assert_eq!(results[0].source_kind, "");
    }

    #[test]
    fn test_source_kind_migration_is_idempotent_across_reopen() {
        // Re-opening a notebook on the same db file re-runs `init_table`,
        // which re-issues the `ALTER TABLE ... ADD COLUMN source_kind`
        // migration. The duplicate-column error must be swallowed, not
        // propagated as a fatal failure.
        let tmp = NamedTempFile::new().unwrap();
        let nb1 = MistakeNotebook::new(tmp.path());
        let entry = build_mistake_entry(
            "agent-1", "s1", MistakeCategory::Capability,
            "u", "a", "w", None, "task_failure",
        );
        nb1.record(&entry).unwrap();
        drop(nb1);

        // Second open on the same file re-runs the (now no-op) migration.
        let nb2 = MistakeNotebook::new(tmp.path());
        let results = nb2.query_by_agent("agent-1", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_kind, "task_failure");
    }
}
