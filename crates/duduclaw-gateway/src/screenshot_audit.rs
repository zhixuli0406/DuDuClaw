//! Screenshot audit storage for browser automation actions.
//!
//! Appends audit entries to a JSONL file and stores screenshots under
//! `~/.duduclaw/audit/browser/screenshots/{agent_id}/`.
//!
//! Each JSONL line includes a `_prev_hash` field that chains entries via
//! SHA-256 so tampering with any line breaks the chain and is detectable.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Errors produced by browser audit operations.
#[derive(Debug)]
pub enum AuditError {
    IoError(String),
    ParseError(String),
}

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoError(msg) => write!(f, "audit I/O error: {msg}"),
            Self::ParseError(msg) => write!(f, "audit parse error: {msg}"),
        }
    }
}

impl std::error::Error for AuditError {}

impl From<std::io::Error> for AuditError {
    fn from(e: std::io::Error) -> Self { Self::IoError(e.to_string()) }
}

impl From<serde_json::Error> for AuditError {
    fn from(e: serde_json::Error) -> Self { Self::ParseError(e.to_string()) }
}

/// A single browser automation audit record, serialised as one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub agent_id: String,
    /// Browser tier: "L1"..."L5"
    pub tier: String,
    /// Action performed: "fetch", "extract", "click", "screenshot", etc.
    pub action: String,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub screenshot_path: Option<PathBuf>,
    pub details: serde_json::Value,
}

/// Append-only JSONL audit log with screenshot storage.
pub struct BrowserAuditLog {
    audit_dir: PathBuf,
    retention_days: u32,
}

impl BrowserAuditLog {
    /// Create a new audit log rooted at `home_dir/audit/browser/`.
    pub fn new(home_dir: &Path, retention_days: u32) -> Self {
        Self {
            audit_dir: home_dir.join("audit").join("browser"),
            retention_days,
        }
    }

    /// Path to the JSONL audit file.
    fn jsonl_path(&self) -> PathBuf {
        self.audit_dir.join("audit.jsonl")
    }

    /// Directory for a specific agent's screenshots.
    ///
    /// SEC2-M6: Sanitise `agent_id` to prevent path traversal.
    /// Characters `/`, `\`, `.`, and `\0` are replaced with `_` so that
    /// values like `../../../etc` cannot escape the screenshots root.
    fn screenshots_dir(&self, agent_id: &str) -> PathBuf {
        let safe_id = agent_id.replace(['/', '\\', '.', '\0'], "_");
        self.audit_dir.join("screenshots").join(safe_id)
    }

    /// Read the SHA-256 hash of the last line written to the JSONL file.
    /// Returns a sentinel of 64 zeros when the file does not yet exist.
    fn last_line_hash(&self) -> String {
        let path = self.jsonl_path();
        if !path.exists() {
            return "0".repeat(64);
        }
        // Walk to the last non-empty line without loading the whole file.
        let Ok(file) = fs::File::open(&path) else {
            return "0".repeat(64);
        };
        let reader = BufReader::new(file);
        let mut last = String::new();
        for line in reader.lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                last = line;
            }
        }
        if last.is_empty() {
            return "0".repeat(64);
        }
        let mut hasher = Sha256::new();
        hasher.update(last.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Append an audit entry as one JSONL line.
    ///
    /// Each line is augmented with a `_prev_hash` field containing the
    /// SHA-256 of the previous line (or 64 zeros for the first entry).
    /// This creates a hash chain so any tampering with historical entries
    /// is detectable by re-computing the chain.
    pub fn log_action(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        fs::create_dir_all(&self.audit_dir)?;

        // Build the record with the chain link included before hashing.
        let prev_hash = self.last_line_hash();
        let mut record = serde_json::to_value(entry)?;
        record["_prev_hash"] = serde_json::Value::String(prev_hash);

        let line = serde_json::to_string(&record)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.jsonl_path())?;
        writeln!(file, "{line}")?;

        info!(
            agent_id = %entry.agent_id,
            tier = %entry.tier,
            action = %entry.action,
            "browser audit logged"
        );
        Ok(())
    }

    /// Save a PNG screenshot and return its path.
    pub fn save_screenshot(
        &self,
        agent_id: &str,
        png_data: &[u8],
    ) -> Result<PathBuf, AuditError> {
        let dir = self.screenshots_dir(agent_id);
        fs::create_dir_all(&dir)?;

        let filename = format!("{}.png", Utc::now().format("%Y%m%dT%H%M%S%.3fZ"));
        let path = dir.join(filename);
        fs::write(&path, png_data)?;

        info!(agent_id, path = %path.display(), "screenshot saved");
        Ok(path)
    }

    /// Read the last `limit` entries from the JSONL file.
    pub fn recent_entries(&self, limit: usize) -> Result<Vec<AuditEntry>, AuditError> {
        let entries = self.read_all_entries()?;
        let start = entries.len().saturating_sub(limit);
        Ok(entries[start..].to_vec())
    }

    /// Read entries filtered by `agent_id`, returning the last `limit` matches.
    pub fn entries_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, AuditError> {
        let entries = self.read_all_entries()?;
        let filtered: Vec<AuditEntry> = entries
            .into_iter()
            .filter(|e| e.agent_id == agent_id)
            .collect();
        let start = filtered.len().saturating_sub(limit);
        Ok(filtered[start..].to_vec())
    }

    /// Delete screenshot files older than `retention_days`. Returns count removed.
    pub fn cleanup_expired(&self) -> Result<u32, AuditError> {
        let screenshots_root = self.audit_dir.join("screenshots");
        if !screenshots_root.exists() {
            return Ok(0);
        }

        let cutoff = Utc::now() - chrono::Duration::days(i64::from(self.retention_days));
        let mut removed: u32 = 0;

        for agent_dir in fs::read_dir(&screenshots_root)? {
            let agent_dir = agent_dir?;
            if !agent_dir.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(agent_dir.path())? {
                let file = file?;
                let metadata = file.metadata()?;
                let modified: DateTime<Utc> = metadata
                    .modified()
                    .map_err(|e| AuditError::IoError(e.to_string()))?
                    .into();
                if modified < cutoff {
                    fs::remove_file(file.path())?;
                    removed += 1;
                }
            }
        }

        if removed > 0 {
            info!(removed, retention_days = self.retention_days, "expired screenshots cleaned");
        }
        Ok(removed)
    }

    /// Verify the hash chain integrity of the audit log.
    ///
    /// Returns `Ok(true)` if the chain is intact, `Ok(false)` if any link is broken,
    /// or an error if the file cannot be read.
    pub fn verify_chain(&self) -> Result<bool, AuditError> {
        let path = self.jsonl_path();
        if !path.exists() {
            return Ok(true);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| AuditError::IoError(e.to_string()))?;

        let mut expected_prev = "0".repeat(64);
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| AuditError::ParseError(e.to_string()))?;

            let stored_prev = record
                .get("_prev_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if stored_prev != expected_prev {
                return Ok(false); // Chain broken
            }

            // Hash this line to use as the expected_prev for the next entry
            let mut hasher = Sha256::new();
            hasher.update(line.as_bytes());
            expected_prev = hex::encode(hasher.finalize());
        }

        Ok(true)
    }

    fn read_all_entries(&self) -> Result<Vec<AuditEntry>, AuditError> {
        let path = self.jsonl_path();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<AuditEntry>(&line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    warn!(error = %e, "skipping malformed audit line");
                }
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_entry(agent_id: &str, tier: &str, action: &str) -> AuditEntry {
        AuditEntry {
            timestamp: Utc::now(),
            agent_id: agent_id.to_owned(),
            tier: tier.to_owned(),
            action: action.to_owned(),
            url: Some("https://example.com".to_owned()),
            domain: Some("example.com".to_owned()),
            screenshot_path: None,
            details: serde_json::json!({}),
        }
    }

    #[test]
    fn log_and_read_back() {
        let tmp = TempDir::new().unwrap();
        let log = BrowserAuditLog::new(tmp.path(), 7);

        log.log_action(&make_entry("bot1", "L1", "fetch")).unwrap();
        log.log_action(&make_entry("bot2", "L3", "click")).unwrap();

        let entries = log.recent_entries(10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].agent_id, "bot1");
        assert_eq!(entries[1].tier, "L3");
    }

    #[test]
    fn recent_entries_respects_limit() {
        let tmp = TempDir::new().unwrap();
        let log = BrowserAuditLog::new(tmp.path(), 7);

        for i in 0..5 {
            log.log_action(&make_entry(&format!("bot{i}"), "L1", "fetch"))
                .unwrap();
        }

        let entries = log.recent_entries(2).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].agent_id, "bot3");
        assert_eq!(entries[1].agent_id, "bot4");
    }

    #[test]
    fn save_screenshot_creates_file() {
        let tmp = TempDir::new().unwrap();
        let log = BrowserAuditLog::new(tmp.path(), 7);

        let png_data = b"\x89PNG fake data";
        let path = log.save_screenshot("bot1", png_data).unwrap();

        assert!(path.exists());
        assert_eq!(fs::read(&path).unwrap(), png_data);
        assert!(path.to_string_lossy().contains("bot1"));
    }

    #[test]
    fn cleanup_expired_removes_old_files() {
        let tmp = TempDir::new().unwrap();
        // retention_days = 0 so everything is "expired"
        let log = BrowserAuditLog::new(tmp.path(), 0);

        log.save_screenshot("bot1", b"old").unwrap();

        let removed = log.cleanup_expired().unwrap();
        assert_eq!(removed, 1);

        // Second run should find nothing to remove
        let removed = log.cleanup_expired().unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn entries_for_agent_filters_correctly() {
        let tmp = TempDir::new().unwrap();
        let log = BrowserAuditLog::new(tmp.path(), 7);

        log.log_action(&make_entry("bot1", "L1", "fetch")).unwrap();
        log.log_action(&make_entry("bot2", "L2", "extract")).unwrap();
        log.log_action(&make_entry("bot1", "L3", "click")).unwrap();

        let entries = log.entries_for_agent("bot1", 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.agent_id == "bot1"));

        let entries = log.entries_for_agent("bot2", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "extract");
    }
}
