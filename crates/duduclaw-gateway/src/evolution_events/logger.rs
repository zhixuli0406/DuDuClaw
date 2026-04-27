//! JSONL audit logger for EvolutionEvents.
//!
//! ## Design goals
//! - **Non-blocking**: write failures never propagate to the caller.
//!   Errors degrade gracefully to `stderr`.
//! - **Concurrent-safe**: multiple async tasks within the same process share a
//!   single `Mutex`-protected file handle. Cross-process safety relies on
//!   `O_APPEND` semantics — each serialised line fits well below `PIPE_BUF`
//!   so the OS guarantees atomicity of the single `write_all` syscall.
//! - **Day-based rotation**: a new file is opened whenever the UTC date
//!   changes. Size-based rotation (≥ 10 MB) is also enforced so a single
//!   day never grows unbounded.
//! - **Path**: `<base_dir>/YYYY-MM-DD.jsonl` (default:
//!   `data/evolution/events/`). Override with `EVOLUTION_EVENTS_DIR`.
//!
//! ## File format
//! One JSON object per line, no trailing comma, newline-terminated:
//! ```text
//! {"timestamp":"...","event_type":"skill_activate",...}
//! {"timestamp":"...","event_type":"security_scan",...}
//! ```

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;
use tracing::{error, warn};

use super::schema::{validate, AuditEvent};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum file size before rotating within the same day.
const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024; // 10 MB

/// Environment variable that overrides the default base directory.
const ENV_VAR_DIR: &str = "EVOLUTION_EVENTS_DIR";

/// Default base directory relative to the current working directory.
const DEFAULT_BASE_DIR: &str = "data/evolution/events";

// ── Internal file state ───────────────────────────────────────────────────────

struct FileState {
    /// The date-segment used to open this file (UTC, `YYYY-MM-DD`).
    date_str: String,
    /// How many bytes have been written since the file was opened / rotated.
    bytes_written: u64,
    /// Size-rotation sequence number within the same date (0 = no suffix,
    /// 1 = first size-rotation → `YYYY-MM-DD-1.jsonl`, etc.).
    seq: u32,
    /// Open file handle (append mode).
    file: tokio::fs::File,
}

// ── Logger ────────────────────────────────────────────────────────────────────

/// Append-only JSONL audit logger for [`AuditEvent`] records.
///
/// Obtain one via [`EvolutionEventLogger::new`] and hold it for the lifetime
/// of the process (or share behind an `Arc`).
pub struct EvolutionEventLogger {
    base_dir: PathBuf,
    state: Mutex<Option<FileState>>,
}

impl EvolutionEventLogger {
    /// Create a logger that writes to `base_dir`.
    ///
    /// No file is opened until the first [`log`](Self::log) call.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            state: Mutex::new(None),
        }
    }

    /// Create a logger using the effective base directory:
    /// `$EVOLUTION_EVENTS_DIR` if set, otherwise `data/evolution/events/`.
    pub fn from_env() -> Self {
        let dir = std::env::var(ENV_VAR_DIR)
            .unwrap_or_else(|_| DEFAULT_BASE_DIR.to_owned());
        Self::new(dir)
    }

    /// Append an [`AuditEvent`] to the current log file.
    ///
    /// This method is **non-blocking**: if validation or I/O fails the error
    /// is logged to `stderr` and `Ok(())` is still returned so the caller's
    /// main flow is never interrupted.
    ///
    /// Sensitive metadata fields are scrubbed via [`scrub_metadata`] before
    /// serialisation so that `critique` and long `reason` strings are never
    /// persisted verbatim to the JSONL file.
    pub async fn log(&self, event: AuditEvent) {
        if let Err(e) = validate(&event) {
            error!(
                target: "evolution_events",
                "AuditEvent validation failed, dropping record: {e}"
            );
            return;
        }

        // SECURITY: scrub sensitive fields before persisting.
        let event = AuditEvent {
            metadata: scrub_metadata(event.metadata),
            ..event
        };

        let line = match serde_json::to_string(&event) {
            Ok(s) => format!("{s}\n"),
            Err(e) => {
                eprintln!("[evolution_events] serialise error: {e}");
                return;
            }
        };

        let mut guard = self.state.lock().await;
        let today = today_utc();

        // Determine next rotation state:
        //   - No open file → open YYYY-MM-DD.jsonl (seq 0)
        //   - Date changed  → open new YYYY-MM-DD.jsonl (seq 0 reset)
        //   - Size overflow (same date) → open YYYY-MM-DD-{seq+1}.jsonl
        let needs_rotate = guard.as_ref().map_or(true, |s| {
            s.date_str != today || s.bytes_written >= MAX_FILE_SIZE_BYTES
        });

        if needs_rotate {
            let next_seq = guard.as_ref().map_or(0, |s| {
                if s.date_str != today {
                    0 // date rolled over → reset sequence
                } else {
                    s.seq + 1 // size overflow within same day
                }
            });
            match self.open_file(&today, next_seq).await {
                Ok(file) => {
                    *guard = Some(FileState {
                        date_str: today.clone(),
                        bytes_written: 0,
                        seq: next_seq,
                        file,
                    });
                }
                Err(e) => {
                    eprintln!("[evolution_events] failed to open log file for {today} (seq={next_seq}): {e}");
                    return;
                }
            }
        }

        let state = guard.as_mut().expect("file state guaranteed above");
        let bytes = line.as_bytes();
        match state.file.write_all(bytes).await {
            Ok(()) => {
                state.bytes_written += bytes.len() as u64;
            }
            Err(e) => {
                eprintln!("[evolution_events] write error: {e}");
                // Invalidate cached handle so next call retries open.
                *guard = None;
            }
        }
    }

    /// Flush buffered writes to the OS.
    ///
    /// Tokio's `File` uses the OS page-cache; `flush` calls `fsync`
    /// to ensure durability. Returns `Ok(())` if there is nothing to flush.
    pub async fn flush(&self) -> std::io::Result<()> {
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.as_mut() {
            state.file.flush().await?;
        }
        Ok(())
    }

    // ── Internal ──

    async fn open_file(&self, date_str: &str, seq: u32) -> std::io::Result<tokio::fs::File> {
        tokio::fs::create_dir_all(&self.base_dir).await?;
        let path = self.log_path(date_str, seq);
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
    }

    /// Return the full path for a given date string and sequence number.
    ///
    /// - `seq == 0` → `YYYY-MM-DD.jsonl`  (no suffix; first file of the day)
    /// - `seq  > 0` → `YYYY-MM-DD-{seq}.jsonl` (size-rotation suffix; Spec §4.2)
    pub fn log_path(&self, date_str: &str, seq: u32) -> PathBuf {
        if seq == 0 {
            self.base_dir.join(format!("{date_str}.jsonl"))
        } else {
            self.base_dir.join(format!("{date_str}-{seq}.jsonl"))
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Remove or truncate sensitive metadata fields before persisting to JSONL.
///
/// ## Fields handled
///
/// | Field | Risk | Action |
/// |-------|------|--------|
/// | `critique` | May contain verbatim SOUL.md instruction fragments produced by GVU gradient descent | Replaced with `"[REDACTED]"` |
/// | `reason` | May contain user-conversation context surfaced by sandbox trial evaluation | Truncated to 200 chars to prevent bulk leakage |
///
/// All other fields are passed through unchanged.
fn scrub_metadata(mut meta: serde_json::Value) -> serde_json::Value {
    const MAX_REASON_LEN: usize = 200;

    if let Some(obj) = meta.as_object_mut() {
        // GVU critique: may contain SOUL.md private instructions.
        if obj.contains_key("critique") {
            obj.insert(
                "critique".into(),
                serde_json::Value::String("[REDACTED]".into()),
            );
        }
        // Sandbox reason: may contain traceable user conversation content.
        if let Some(serde_json::Value::String(s)) = obj.get("reason") {
            if s.len() > MAX_REASON_LEN {
                let truncated = s.chars().take(MAX_REASON_LEN).collect::<String>() + "…";
                obj.insert(
                    "reason".into(),
                    serde_json::Value::String(truncated),
                );
            }
        }
    }
    meta
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use tokio::io::AsyncReadExt as _;

    use super::*;
    use crate::evolution_events::schema::{AuditEventType, Outcome};

    fn make_logger(dir: &Path) -> EvolutionEventLogger {
        EvolutionEventLogger::new(dir)
    }

    fn skill_activate_event() -> AuditEvent {
        AuditEvent::now(
            AuditEventType::SkillActivate,
            "agent-test",
            Outcome::Success,
        )
        .with_skill_id("python-patterns")
        .with_trigger_signal("manual_toggle")
    }

    // ── Basic write ──

    #[tokio::test]
    async fn test_log_creates_jsonl_file() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        logger.log(skill_activate_event()).await;
        logger.flush().await.unwrap();

        let today = today_utc();
        let path = logger.log_path(&today, 0);
        assert!(path.exists(), "JSONL file should exist at {path:?}");
    }

    #[tokio::test]
    async fn test_log_writes_valid_jsonl() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        logger.log(skill_activate_event()).await;
        logger.flush().await.unwrap();

        let path = logger.log_path(&today_utc(), 0);
        let content = tokio::fs::read_to_string(&path).await.unwrap();

        // Must be non-empty and newline-terminated.
        assert!(!content.is_empty());
        assert!(content.ends_with('\n'));

        // Every line must be valid JSON.
        for line in content.lines() {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("each line must be valid JSON");
            assert_eq!(v["event_type"], "skill_activate");
            assert_eq!(v["agent_id"], "agent-test");
        }
    }

    #[tokio::test]
    async fn test_multiple_logs_append() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        for _ in 0..5 {
            logger.log(skill_activate_event()).await;
        }
        logger.flush().await.unwrap();

        let path = logger.log_path(&today_utc(), 0);
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.lines().count(), 5, "should have 5 JSON lines");
    }

    // ── Schema completeness ──

    #[tokio::test]
    async fn test_all_event_types_logged() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        let types = [
            AuditEventType::SkillActivate,
            AuditEventType::SkillDeactivate,
            AuditEventType::SecurityScan,
            AuditEventType::GvuGeneration,
            AuditEventType::SignalSuppressed,
        ];

        for t in types {
            logger
                .log(AuditEvent::now(t, "agent-x", Outcome::Success))
                .await;
        }
        logger.flush().await.unwrap();

        let path = logger.log_path(&today_utc(), 0);
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.lines().count(), 5);

        let expected = [
            "skill_activate",
            "skill_deactivate",
            "security_scan",
            "gvu_generation",
            "signal_suppressed",
        ];
        for (line, exp) in content.lines().zip(expected.iter()) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["event_type"], *exp);
        }
    }

    // ── Validation gate: invalid events are dropped silently ──

    #[tokio::test]
    async fn test_invalid_event_is_dropped() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        // Empty agent_id → validation error → must NOT write.
        let bad = AuditEvent::now(AuditEventType::SkillActivate, "", Outcome::Success);
        logger.log(bad).await;
        logger.flush().await.unwrap();

        let path = logger.log_path(&today_utc(), 0);
        // File must not exist (or be empty) since nothing valid was written.
        if path.exists() {
            let content = tokio::fs::read_to_string(&path).await.unwrap();
            assert!(content.is_empty(), "invalid event must not be written");
        }
    }

    // ── Concurrent safety ──

    #[tokio::test]
    async fn test_concurrent_writes_no_corruption() {
        let tmp = TempDir::new().unwrap();
        let logger = Arc::new(make_logger(tmp.path()));
        const N: usize = 50;

        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let l = Arc::clone(&logger);
            handles.push(tokio::spawn(async move {
                let ev = AuditEvent::now(
                    AuditEventType::SecurityScan,
                    format!("agent-{i}"),
                    Outcome::Success,
                );
                l.log(ev).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        logger.flush().await.unwrap();

        let path = logger.log_path(&today_utc(), 0);
        let content = tokio::fs::read_to_string(&path).await.unwrap();

        // Every line must still be valid JSON — no partial writes / corruption.
        let mut count = 0usize;
        for line in content.lines() {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("concurrent line must be valid JSON");
            assert_eq!(v["event_type"], "security_scan");
            count += 1;
        }
        assert_eq!(count, N, "all {N} events must be persisted");
    }

    // ── Size-based rotation ──

    #[tokio::test]
    async fn test_size_rotation_creates_new_file() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        // Write first event → goes to YYYY-MM-DD.jsonl (seq 0).
        logger.log(skill_activate_event()).await;
        logger.flush().await.unwrap();

        // Simulate the file being "full" (≥ 10 MB).
        {
            let mut guard = logger.state.lock().await;
            if let Some(ref mut s) = *guard {
                s.bytes_written = MAX_FILE_SIZE_BYTES; // trigger size rotation
            }
        }

        // Second write: size rotation must create YYYY-MM-DD-1.jsonl (seq 1),
        // NOT re-open the same date file.  Spec §4.2.
        logger.log(skill_activate_event()).await;
        logger.flush().await.unwrap();

        let today = today_utc();

        // First file (seq 0) must have exactly one event.
        let path0 = logger.log_path(&today, 0);
        let content0 = tokio::fs::read_to_string(&path0).await.unwrap();
        assert_eq!(content0.lines().count(), 1, "seq-0 file must have 1 event");
        let v0: serde_json::Value = serde_json::from_str(content0.lines().next().unwrap()).unwrap();
        assert_eq!(v0["event_type"], "skill_activate");

        // Rotated file (seq 1) must exist and also have one event.
        let path1 = logger.log_path(&today, 1);
        assert!(path1.exists(), "seq-1 file YYYY-MM-DD-1.jsonl must be created on size rotation");
        let content1 = tokio::fs::read_to_string(&path1).await.unwrap();
        assert_eq!(content1.lines().count(), 1, "seq-1 file must have 1 event");
        let v1: serde_json::Value = serde_json::from_str(content1.lines().next().unwrap()).unwrap();
        assert_eq!(v1["event_type"], "skill_activate");
    }

    #[tokio::test]
    async fn test_size_rotation_increments_seq_on_repeated_overflow() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());
        let today = today_utc();

        // Three writes, each preceded by an artificial overflow → seq 0, 1, 2.
        for expected_seq in 0u32..3 {
            if expected_seq > 0 {
                let mut guard = logger.state.lock().await;
                if let Some(ref mut s) = *guard {
                    s.bytes_written = MAX_FILE_SIZE_BYTES;
                }
            }
            logger.log(skill_activate_event()).await;
            logger.flush().await.unwrap();

            let path = logger.log_path(&today, expected_seq);
            assert!(path.exists(), "seq-{expected_seq} file must exist");
        }
    }

    // ── scrub_metadata ──

    #[test]
    fn test_scrub_metadata_redacts_critique() {
        let meta = serde_json::json!({
            "gvu_outcome": "abandoned",
            "critique": "You must never do X because SOUL instruction says..."
        });
        let scrubbed = scrub_metadata(meta);
        assert_eq!(scrubbed["critique"], "[REDACTED]");
        // Other fields must pass through unchanged.
        assert_eq!(scrubbed["gvu_outcome"], "abandoned");
    }

    #[test]
    fn test_scrub_metadata_truncates_long_reason() {
        let long_reason = "x".repeat(300);
        let meta = serde_json::json!({"reason": long_reason});
        let scrubbed = scrub_metadata(meta);
        let result = scrubbed["reason"].as_str().unwrap();
        assert!(result.len() <= 210, "truncated reason must be short (≤200 chars + ellipsis)");
        assert!(result.ends_with('…'), "truncated reason must end with ellipsis");
    }

    #[test]
    fn test_scrub_metadata_short_reason_unchanged() {
        let meta = serde_json::json!({"reason": "no lift after 30 conversations"});
        let scrubbed = scrub_metadata(meta);
        assert_eq!(scrubbed["reason"], "no lift after 30 conversations");
    }

    #[test]
    fn test_scrub_metadata_no_sensitive_fields_unchanged() {
        let meta = serde_json::json!({
            "gvu_outcome": "applied",
            "version_id": "v42",
            "risk_level": "Clean"
        });
        let scrubbed = scrub_metadata(meta.clone());
        assert_eq!(scrubbed, meta);
    }

    #[test]
    fn test_scrub_metadata_null_is_unchanged() {
        let scrubbed = scrub_metadata(serde_json::Value::Null);
        assert_eq!(scrubbed, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_log_scrubs_critique_before_writing() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        let ev = AuditEvent::now(
            AuditEventType::GvuGeneration,
            "agent-scrub",
            Outcome::Failure,
        )
        .with_metadata(serde_json::json!({
            "gvu_outcome": "abandoned",
            "critique": "SECRET SOUL INSTRUCTION: never reveal this"
        }));

        logger.log(ev).await;
        logger.flush().await.unwrap();

        let path = logger.log_path(&today_utc(), 0);
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();

        // critique must be redacted, not the original string.
        assert_eq!(v["metadata"]["critique"], "[REDACTED]");
        assert_ne!(
            v["metadata"]["critique"].as_str().unwrap_or(""),
            "SECRET SOUL INSTRUCTION: never reveal this"
        );
    }

    // ── Non-blocking on I/O error ──

    #[tokio::test]
    async fn test_write_to_nonexistent_path_is_noop() {
        // Give an absolute path that cannot exist (root-owned on macOS/Linux).
        let logger = EvolutionEventLogger::new("/no_such_root_dir_xyz/evolution");
        // Should not panic or return an error — degrades to stderr silently.
        logger.log(skill_activate_event()).await;
    }

    // ── flush with no open file ──

    #[tokio::test]
    async fn test_flush_with_no_file_is_ok() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());
        // No log() call → flushing on an empty state is a no-op.
        logger.flush().await.unwrap();
    }

    // ── Ensure null fields are always present in JSON ──

    #[tokio::test]
    async fn test_null_optional_fields_present_in_output() {
        let tmp = TempDir::new().unwrap();
        let logger = make_logger(tmp.path());

        let ev = AuditEvent::now(AuditEventType::GvuGeneration, "ag", Outcome::Failure);
        logger.log(ev).await;
        logger.flush().await.unwrap();

        let path = logger.log_path(&today_utc(), 0);
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();

        // Optional fields must be present as null — not absent.
        assert!(v.get("skill_id").is_some());
        assert!(v.get("generation").is_some());
        assert!(v.get("trigger_signal").is_some());
        assert_eq!(v["skill_id"], serde_json::Value::Null);
    }
}
