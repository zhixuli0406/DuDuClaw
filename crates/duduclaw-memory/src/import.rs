//! Memory import module — bulk import from CSV, JSON, and JSONL files.
//!
//! Supports two CSV layouts:
//! - `content,tags` — general memory entries
//! - `question,answer` — FAQ-style entries (merged into a single content string)
//!
//! JSON/JSONL expect objects with `{content, tags?, importance?}`.

use std::path::Path;

use chrono::Utc;
use serde::Deserialize;
use tracing::info;
use uuid::Uuid;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};

use crate::engine::SqliteMemoryEngine;

// ── File size guard ─────────────────────────────────────────

/// Maximum allowed import file size (50 MB).
const MAX_IMPORT_SIZE: u64 = 50 * 1024 * 1024;

/// Reject files exceeding the import size limit to prevent OOM.
fn check_file_size(path: &Path) -> Result<()> {
    let size = std::fs::metadata(path)
        .map_err(|e| DuDuClawError::Memory(format!("Cannot read file: {e}")))?
        .len();
    if size > MAX_IMPORT_SIZE {
        return Err(DuDuClawError::Memory(format!(
            "File too large ({:.1} MB, max {} MB)",
            size as f64 / 1_048_576.0,
            MAX_IMPORT_SIZE / 1_048_576
        )));
    }
    Ok(())
}

// ── Deserialization helpers ──────────────────────────────────

/// A row from a general-purpose CSV (content + tags).
#[derive(Debug, Deserialize)]
struct CsvMemoryRow {
    content: String,
    #[serde(default)]
    tags: Option<String>,
}

/// A row from an FAQ-style CSV (question + answer).
#[derive(Debug, Deserialize)]
struct CsvFaqRow {
    question: String,
    answer: String,
}

/// A record from JSON / JSONL input.
#[derive(Debug, Deserialize)]
struct JsonMemoryRecord {
    content: String,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    importance: Option<f64>,
}

// ── Public API ───────────────────────────────────────────────

/// Build a [`MemoryEntry`] with sensible defaults for imported data.
pub fn build_memory_entry(
    agent_id: &str,
    content: &str,
    tags: &[String],
    importance: f64,
) -> MemoryEntry {
    MemoryEntry {
        id: Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        tags: tags.to_vec(),
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance,
        access_count: 0,
        last_accessed: None,
        source_event: "import".to_string(),
        shareable: false,
    }
}

/// Import memory entries from a CSV file.
///
/// `entry_type` controls the expected column layout:
/// - `"faq"` — expects columns `question,answer`; content is formatted as
///   `"Q: {question}\nA: {answer}"`.
/// - `"memory"` (default) — expects columns `content,tags`; tags are parsed
///   as a comma-separated string.
///
/// All entries are stored with `layer = Semantic` and `importance = 5.0`.
pub async fn import_csv(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    path: &Path,
    entry_type: &str,
) -> Result<usize> {
    check_file_size(path)?;
    let file_content = std::fs::read_to_string(path)
        .map_err(|e| DuDuClawError::Memory(format!("Failed to read CSV file: {e}")))?;

    let mut count = 0usize;

    if entry_type == "faq" {
        let mut rdr = csv::Reader::from_reader(file_content.as_bytes());
        for result in rdr.deserialize::<CsvFaqRow>() {
            let row = result
                .map_err(|e| DuDuClawError::Memory(format!("CSV parse error: {e}")))?;
            let content = format!("Q: {}\nA: {}", row.question.trim(), row.answer.trim());
            let tags = vec!["faq".to_string()];
            let entry = build_memory_entry(agent_id, &content, &tags, 5.0);
            engine.store(agent_id, entry).await?;
            count += 1;
        }
    } else {
        let mut rdr = csv::Reader::from_reader(file_content.as_bytes());
        for result in rdr.deserialize::<CsvMemoryRow>() {
            let row = result
                .map_err(|e| DuDuClawError::Memory(format!("CSV parse error: {e}")))?;
            let tags: Vec<String> = row
                .tags
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let entry = build_memory_entry(agent_id, row.content.trim(), &tags, 5.0);
            engine.store(agent_id, entry).await?;
            count += 1;
        }
    }

    info!(agent_id, count, path = %path.display(), "CSV import complete");
    Ok(count)
}

/// Import memory entries from a JSON file containing an array of objects.
///
/// Each object must have a `content` field. Optional fields: `tags` (array of
/// strings), `importance` (float 0.0–10.0, defaults to 5.0).
pub async fn import_json(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    path: &Path,
) -> Result<usize> {
    check_file_size(path)?;
    let file_content = std::fs::read_to_string(path)
        .map_err(|e| DuDuClawError::Memory(format!("Failed to read JSON file: {e}")))?;

    let records: Vec<JsonMemoryRecord> = serde_json::from_str(&file_content)
        .map_err(|e| DuDuClawError::Memory(format!("JSON parse error: {e}")))?;

    let mut count = 0usize;
    for record in records {
        let tags = record.tags.unwrap_or_default();
        let importance = record.importance.unwrap_or(5.0).clamp(0.0, 10.0);
        let entry = build_memory_entry(agent_id, record.content.trim(), &tags, importance);
        engine.store(agent_id, entry).await?;
        count += 1;
    }

    info!(agent_id, count, path = %path.display(), "JSON import complete");
    Ok(count)
}

/// Import memory entries from a JSONL (newline-delimited JSON) file.
///
/// Each line is a JSON object with the same schema as [`import_json`].
/// Blank lines are silently skipped.
pub async fn import_jsonl(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    path: &Path,
) -> Result<usize> {
    check_file_size(path)?;
    let file_content = std::fs::read_to_string(path)
        .map_err(|e| DuDuClawError::Memory(format!("Failed to read JSONL file: {e}")))?;

    let mut count = 0usize;
    for (line_num, line) in file_content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: JsonMemoryRecord = serde_json::from_str(line).map_err(|e| {
            DuDuClawError::Memory(format!("JSONL parse error on line {}: {e}", line_num + 1))
        })?;
        let tags = record.tags.unwrap_or_default();
        let importance = record.importance.unwrap_or(5.0).clamp(0.0, 10.0);
        let entry = build_memory_entry(agent_id, record.content.trim(), &tags, importance);
        engine.store(agent_id, entry).await?;
        count += 1;
    }

    info!(agent_id, count, path = %path.display(), "JSONL import complete");
    Ok(count)
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_import_csv_memory() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "content,tags").unwrap();
        writeln!(file, "\"Remember user prefers dark mode\",\"ui,preference\"").unwrap();
        writeln!(file, "\"User timezone is UTC+8\",\"timezone\"").unwrap();
        file.flush().unwrap();

        let count = import_csv(&engine, "test-agent", file.path(), "memory")
            .await
            .unwrap();
        assert_eq!(count, 2);

        let entries = engine.list_recent("test-agent", 10).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].layer, MemoryLayer::Semantic);
        assert_eq!(entries[0].source_event, "import");
    }

    #[tokio::test]
    async fn test_import_csv_faq() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "question,answer").unwrap();
        writeln!(file, "\"What is DuDuClaw?\",\"A multi-agent platform\"").unwrap();
        writeln!(file, "\"How to start?\",\"Run duduclaw onboard\"").unwrap();
        file.flush().unwrap();

        let count = import_csv(&engine, "test-agent", file.path(), "faq")
            .await
            .unwrap();
        assert_eq!(count, 2);

        let entries = engine.list_recent("test-agent", 10).await.unwrap();
        assert!(entries.iter().any(|e| e.content.contains("Q:") && e.content.contains("A:")));
        assert!(entries.iter().all(|e| e.tags.contains(&"faq".to_string())));
    }

    #[tokio::test]
    async fn test_import_json() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"[
            {{"content": "First memory", "tags": ["a", "b"], "importance": 8.0}},
            {{"content": "Second memory"}}
        ]"#
        )
        .unwrap();
        file.flush().unwrap();

        let count = import_json(&engine, "test-agent", file.path()).await.unwrap();
        assert_eq!(count, 2);

        let entries = engine.list_recent("test-agent", 10).await.unwrap();
        assert_eq!(entries.len(), 2);

        // Find the entry with importance 8.0
        let high = entries.iter().find(|e| e.content == "First memory").unwrap();
        assert!((high.importance - 8.0).abs() < f64::EPSILON);
        assert_eq!(high.tags, vec!["a".to_string(), "b".to_string()]);

        // The second entry should have default importance
        let default = entries.iter().find(|e| e.content == "Second memory").unwrap();
        assert!((default.importance - 5.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_import_jsonl() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"content": "Line one", "tags": ["x"]}}"#).unwrap();
        writeln!(file, r#"{{"content": "Line two", "importance": 3.0}}"#).unwrap();
        writeln!(file).unwrap(); // blank line should be skipped
        file.flush().unwrap();

        let count = import_jsonl(&engine, "test-agent", file.path()).await.unwrap();
        assert_eq!(count, 2);

        let entries = engine.list_recent("test-agent", 10).await.unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_import_csv_empty_file() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "content,tags").unwrap();
        file.flush().unwrap();

        let count = import_csv(&engine, "test-agent", file.path(), "memory")
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_import_json_empty_array() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "[]").unwrap();
        file.flush().unwrap();

        let count = import_json(&engine, "test-agent", file.path()).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_build_memory_entry_fields() {
        let entry = build_memory_entry("agent-1", "test content", &["tag1".to_string()], 7.5);
        assert_eq!(entry.agent_id, "agent-1");
        assert_eq!(entry.content, "test content");
        assert_eq!(entry.tags, vec!["tag1".to_string()]);
        assert!((entry.importance - 7.5).abs() < f64::EPSILON);
        assert_eq!(entry.layer, MemoryLayer::Semantic);
        assert_eq!(entry.source_event, "import");
        assert!(!entry.id.is_empty());
    }

    #[tokio::test]
    async fn test_import_json_clamps_importance() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"[{{"content": "over", "importance": 99.0}}, {{"content": "under", "importance": -5.0}}]"#
        )
        .unwrap();
        file.flush().unwrap();

        let count = import_json(&engine, "test-agent", file.path()).await.unwrap();
        assert_eq!(count, 2);

        let entries = engine.list_recent("test-agent", 10).await.unwrap();
        let over = entries.iter().find(|e| e.content == "over").unwrap();
        assert!((over.importance - 10.0).abs() < f64::EPSILON);
        let under = entries.iter().find(|e| e.content == "under").unwrap();
        assert!((under.importance - 0.0).abs() < f64::EPSILON);
    }
}
