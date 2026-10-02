//! Screenshot audit storage for browser automation actions.
//!
//! Appends audit entries to a JSONL file and stores screenshots under
//! `~/.duduclaw/audit/browser/screenshots/{agent_id}/`.
//!
//! Each JSONL line includes a `_prev_hash` field that chains entries via
//! SHA-256 so tampering with any line breaks the chain and is detectable.
//!
//! Rotation: when `audit.jsonl` grows past [`BROWSER_AUDIT_ROTATION_MAX_BYTES`]
//! (the same 16 MiB as `tool_calls.jsonl`) it is renamed to `audit.jsonl.old`
//! (replacing any previous one) under the append lock. The chain continues
//! across the boundary: the first line of the new file carries the hash of
//! the rotated file's last line, so [`BrowserAuditLog::verify_chain`] starts
//! from `audit.jsonl.old`'s last line when that file exists (from 64 zeros
//! otherwise).
//!
//! Screenshots are kept at most [`MAX_SCREENSHOTS_PER_AGENT`] files /
//! [`MAX_SCREENSHOT_BYTES_PER_AGENT`] bytes per employee (oldest deleted
//! first) and for the log's retention period (`cleanup_expired`, run by the
//! computer-use sweep).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e.to_string())
    }
}

impl From<serde_json::Error> for AuditError {
    fn from(e: serde_json::Error) -> Self {
        Self::ParseError(e.to_string())
    }
}

/// Size past which `audit.jsonl` is rotated to `audit.jsonl.old`; the same
/// threshold as `tool_calls.jsonl`.
pub const BROWSER_AUDIT_ROTATION_MAX_BYTES: u64 = duduclaw_security::audit::TOOL_CALLS_ROTATION_MAX_BYTES;
/// Screenshots kept per employee at most (oldest deleted first).
pub const MAX_SCREENSHOTS_PER_AGENT: usize = 500;
/// Bytes of screenshots kept per employee at most (oldest deleted first).
pub const MAX_SCREENSHOT_BYTES_PER_AGENT: u64 = 200 * 1024 * 1024;
/// Bytes read per step when looking for the last line from the end.
const TAIL_READ_WINDOW: u64 = 8 * 1024;

/// The hash a chain starts from when nothing precedes it.
fn genesis_hash() -> String {
    "0".repeat(64)
}

/// The last non-blank line of `path` (without its line ending), read from
/// the end of the file in [`TAIL_READ_WINDOW`] steps so the cost does not
/// grow with the file. `Ok(None)` when the file is missing or has no
/// non-blank line. A line longer than the window is read whole.
fn last_nonblank_line(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut pos = file.metadata()?.len();
    // `buf` holds the bytes from `pos` to the end of the file.
    let mut buf: Vec<u8> = Vec::new();
    let mut window = TAIL_READ_WINDOW;
    loop {
        let step = window.min(pos);
        // Grow the step so a very long last line costs O(n), not O(n²).
        window = window.saturating_mul(2);
        pos -= step;
        file.seek(SeekFrom::Start(pos))?;
        let mut chunk = vec![0u8; step as usize];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        // Lines that are complete: every segment after a '\n' in `buf`, and
        // the first segment too once the start of the file is reached.
        let segments: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
        let first_complete = if pos == 0 { 0 } else { 1 };
        for segment in segments[first_complete..].iter().rev() {
            let line = segment.strip_suffix(b"\r").unwrap_or(segment);
            if line.iter().any(|b| !b.is_ascii_whitespace()) {
                return Ok(Some(line.to_vec()));
            }
        }
        if pos == 0 {
            return Ok(None);
        }
        // Only the (possibly partial) first segment is still undecided; keep
        // just it, since everything after it was blank.
        let keep = segments.first().map(|s| s.len()).unwrap_or(0);
        buf.truncate(keep);
    }
}

/// SHA-256 (hex) of `path`'s last non-blank line, or the genesis hash.
fn tail_hash(path: &Path) -> std::io::Result<String> {
    Ok(match last_nonblank_line(path)? {
        Some(line) => hex::encode(Sha256::digest(&line)),
        None => genesis_hash(),
    })
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
    rotate_bytes: u64,
}

impl BrowserAuditLog {
    /// Create a new audit log rooted at `home_dir/audit/browser/`.
    pub fn new(home_dir: &Path, retention_days: u32) -> Self {
        Self {
            audit_dir: home_dir.join("audit").join("browser"),
            retention_days,
            rotate_bytes: BROWSER_AUDIT_ROTATION_MAX_BYTES,
        }
    }

    #[cfg(test)]
    fn with_rotation_bytes(mut self, bytes: u64) -> Self {
        self.rotate_bytes = bytes;
        self
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

    /// Path the JSONL file is rotated to.
    fn rotated_path(&self) -> PathBuf {
        self.audit_dir.join("audit.jsonl.old")
    }

    /// Append an audit entry as one JSONL line.
    ///
    /// Each line is augmented with a `_prev_hash` field containing the
    /// SHA-256 of the previous line (or 64 zeros for the first entry ever).
    /// This creates a hash chain so any tampering with historical entries
    /// is detectable by re-computing the chain. The previous line is read
    /// from the end of the file, so an append costs the same however large
    /// the log is. Synchronous file I/O: async callers run it on a blocking
    /// thread.
    pub fn log_action(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        fs::create_dir_all(&self.audit_dir)?;

        let mut record = serde_json::to_value(entry)?;
        // Reading the chain tail, rotating and appending happen under one
        // advisory lock (a sidecar `.lock` file, so the rename below does not
        // invalidate it), so two writers (two tool-driven sessions, or two
        // gateways on one home) cannot fork the hash chain
        // or interleave lines.
        let path = self.jsonl_path();
        duduclaw_core::with_file_lock(&path, || {
            let prev_hash = tail_hash(&path)?;
            let oversized = fs::metadata(&path)
                .map(|m| m.len() > self.rotate_bytes)
                .unwrap_or(false);
            if oversized {
                // The new file's first line chains from the rotated file's
                // last line (`prev_hash`, read above).
                fs::rename(&path, self.rotated_path())?;
            }
            record["_prev_hash"] = serde_json::Value::String(prev_hash);

            let line = serde_json::to_string(&record).map_err(std::io::Error::other)?;
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(file, "{line}")
        })?;

        info!(
            agent_id = %entry.agent_id,
            tier = %entry.tier,
            action = %entry.action,
            "browser audit logged"
        );
        Ok(())
    }

    /// Save a PNG screenshot and return its path.
    pub fn save_screenshot(&self, agent_id: &str, png_data: &[u8]) -> Result<PathBuf, AuditError> {
        let dir = self.screenshots_dir(agent_id);
        fs::create_dir_all(&dir)?;

        let filename = format!("{}.png", Utc::now().format("%Y%m%dT%H%M%S%.3fZ"));
        let path = dir.join(filename);
        fs::write(&path, png_data)?;

        info!(agent_id, path = %path.display(), "screenshot saved");
        let pruned = prune_screenshots(&dir, MAX_SCREENSHOTS_PER_AGENT, MAX_SCREENSHOT_BYTES_PER_AGENT);
        if pruned > 0 {
            info!(agent_id, pruned, "oldest screenshots removed (per-employee cap)");
        }
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

    /// Delete screenshot files older than `retention_days`. Returns count
    /// removed. A file that cannot be inspected or removed is skipped (and
    /// retried on the next run), never aborting the whole cleanup.
    pub fn cleanup_expired(&self) -> Result<u32, AuditError> {
        let screenshots_root = self.audit_dir.join("screenshots");
        if !screenshots_root.exists() {
            return Ok(0);
        }

        let cutoff = Utc::now() - chrono::Duration::days(i64::from(self.retention_days));
        let mut removed: u32 = 0;

        for agent_dir in fs::read_dir(&screenshots_root)?.flatten() {
            if !agent_dir.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(files) = fs::read_dir(agent_dir.path()) else {
                continue;
            };
            for file in files.flatten() {
                let Ok(modified) = file.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                let modified: DateTime<Utc> = modified.into();
                if modified < cutoff && fs::remove_file(file.path()).is_ok() {
                    removed += 1;
                }
            }
        }

        if removed > 0 {
            info!(
                removed,
                retention_days = self.retention_days,
                "expired screenshots cleaned"
            );
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
        let content =
            std::fs::read_to_string(&path).map_err(|e| AuditError::IoError(e.to_string()))?;

        // A rotated predecessor anchors the first line (see the module doc).
        let mut expected_prev = tail_hash(&self.rotated_path())?;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record: serde_json::Value =
                serde_json::from_str(line).map_err(|e| AuditError::ParseError(e.to_string()))?;

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

/// Delete the oldest `.png` files of one employee's screenshot directory
/// until at most `max_files` remain and they total at most `max_bytes`. File
/// names are UTC timestamps, so name order is age order. The newest file is
/// always kept. Returns how many were deleted; errors skip that file.
fn prune_screenshots(dir: &Path, max_files: usize, max_bytes: u64) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(PathBuf, u64)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .map(|e| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        .collect();
    files.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    let mut total: u64 = files.iter().map(|(_, len)| *len).sum();
    let mut count = files.len();
    let mut removed = 0;
    for (path, len) in &files {
        if count <= 1 || (count <= max_files && total <= max_bytes) {
            break;
        }
        if fs::remove_file(path).is_ok() {
            removed += 1;
        }
        // Counted as gone either way, so one undeletable file cannot pin
        // the loop; the next save retries it.
        count -= 1;
        total = total.saturating_sub(*len);
    }
    removed
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

    fn sha_hex(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn tail_read_finds_the_last_line_whatever_its_length() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.jsonl");
        assert_eq!(tail_hash(&path).unwrap(), genesis_hash(), "missing file");
        fs::write(&path, b"\n \n").unwrap();
        assert_eq!(tail_hash(&path).unwrap(), genesis_hash(), "only blank lines");
        fs::write(&path, b"only-line").unwrap();
        assert_eq!(tail_hash(&path).unwrap(), sha_hex(b"only-line"), "no trailing newline");
        // A last line much longer than the read window, trailing blank lines
        // and a CRLF ending.
        let long: String = "x".repeat(TAIL_READ_WINDOW as usize * 5 + 17);
        let content = format!("first\n{long}\r\n\n  \n");
        fs::write(&path, content.as_bytes()).unwrap();
        assert_eq!(tail_hash(&path).unwrap(), sha_hex(long.as_bytes()));
        // Exactly at a window boundary.
        let edge = "y".repeat(TAIL_READ_WINDOW as usize - 1);
        fs::write(&path, format!("prev\n{edge}\n").as_bytes()).unwrap();
        assert_eq!(tail_hash(&path).unwrap(), sha_hex(edge.as_bytes()));
        // Multi-byte text is hashed as bytes, never cut.
        fs::write(&path, "a\n電腦操作\n".as_bytes()).unwrap();
        assert_eq!(tail_hash(&path).unwrap(), sha_hex("電腦操作".as_bytes()));
    }

    #[test]
    fn chain_stays_valid_across_appends_and_rotation() {
        let tmp = TempDir::new().unwrap();
        let log = BrowserAuditLog::new(tmp.path(), 7).with_rotation_bytes(1000);
        for i in 0..12 {
            log.log_action(&make_entry(&format!("bot{i}"), "L5a", "click")).unwrap();
        }
        assert!(log.rotated_path().exists(), "the small threshold rotated");
        assert!(log.verify_chain().unwrap());
        // The first line of the current file chains from the rotated file.
        let first = fs::read_to_string(log.jsonl_path()).unwrap().lines().next().unwrap().to_string();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["_prev_hash"], tail_hash(&log.rotated_path()).unwrap());
        // Tampering is still detected.
        let content = fs::read_to_string(log.jsonl_path()).unwrap();
        assert!(content.lines().count() >= 2, "{content}");
        fs::write(log.jsonl_path(), content.replacen("click", "klick", 1)).unwrap();
        assert!(!log.verify_chain().unwrap());
    }

    #[test]
    fn screenshots_are_capped_per_employee_oldest_first() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("shots");
        fs::create_dir_all(&dir).unwrap();
        for i in 0..7 {
            fs::write(dir.join(format!("2026010{i}T000000.000Z.png")), vec![0u8; 10]).unwrap();
        }
        fs::write(dir.join("notes.txt"), b"kept").unwrap();
        assert_eq!(prune_screenshots(&dir, 5, u64::MAX), 2);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.first().unwrap(), "20260102T000000.000Z.png", "the two oldest went");
        assert!(left.contains(&"notes.txt".to_string()), "non-PNG files are never touched");
        // The byte cap: 5 × 10 bytes over a 25-byte cap leaves 2 files.
        assert_eq!(prune_screenshots(&dir, 500, 25), 3);
        // The newest file is kept even when it alone is over the cap.
        assert_eq!(prune_screenshots(&dir, 500, 1), 1);
        assert!(dir.join("20260106T000000.000Z.png").exists());
        // Through save_screenshot with the real constants nothing is removed.
        let log = BrowserAuditLog::new(tmp.path(), 7);
        log.save_screenshot("bot1", b"png").unwrap();
        assert_eq!(fs::read_dir(log.screenshots_dir("bot1")).unwrap().count(), 1);
        assert_eq!(MAX_SCREENSHOTS_PER_AGENT, 500);
        assert_eq!(MAX_SCREENSHOT_BYTES_PER_AGENT, 200 * 1024 * 1024);
    }

    #[test]
    fn entries_for_agent_filters_correctly() {
        let tmp = TempDir::new().unwrap();
        let log = BrowserAuditLog::new(tmp.path(), 7);

        log.log_action(&make_entry("bot1", "L1", "fetch")).unwrap();
        log.log_action(&make_entry("bot2", "L2", "extract"))
            .unwrap();
        log.log_action(&make_entry("bot1", "L3", "click")).unwrap();

        let entries = log.entries_for_agent("bot1", 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.agent_id == "bot1"));

        let entries = log.entries_for_agent("bot2", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "extract");
    }
}
