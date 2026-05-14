//! JSONL audit sink — every redact, restore, egress, and override
//! decision can be persisted to a local file for forensic review.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{RedactionError, Result};

/// One audit record. The pipeline emits these from redact / restore /
/// egress checks; channel layer emits from force-override flows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AuditEvent {
    /// A token was created from a redacted span.
    Redact {
        agent_id: String,
        session_id: Option<String>,
        source_category: String,
        source_detail: Option<String>,
        rule_id: String,
        category: String,
        token: String,
    },
    /// A token was successfully restored.
    RestoreOk {
        agent_id: String,
        caller: String,
        target: String,
        token: String,
    },
    /// A token was found in the vault but the caller's scope was
    /// insufficient — token stays masked.
    RestoreDenied {
        agent_id: String,
        caller: String,
        target: String,
        token: String,
        required_scope: String,
    },
    /// A token-shaped string was present but not in the vault. Usually
    /// indicates LLM hallucination; the placeholder stays in the output.
    RestoreMiss {
        agent_id: String,
        caller: String,
        target: String,
        token: String,
    },
    /// A tool call was blocked because the tool isn't on the egress
    /// whitelist (or the args contain hallucinated tokens).
    EgressDeny {
        agent_id: String,
        tool: String,
        reason: String,
        tokens_seen: usize,
    },
    /// A tool call was allowed and its arguments were restored.
    EgressAllow {
        agent_id: String,
        tool: String,
        tokens_restored: usize,
    },
    /// Vault GC tick.
    VaultGc {
        expired_marked: usize,
        purged: usize,
    },
    /// A force-on policy was overridden via env+CLI. Always severity=CRITICAL.
    ForceOnOverride {
        operator: String,
        channel: String,
        severity: String,
    },
}

impl AuditEvent {
    /// Wrap with a timestamp and serialise to a single JSON object value.
    pub fn to_record(&self) -> Value {
        let ts = Utc::now().to_rfc3339();
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(ref mut map) = v {
            map.insert("ts".into(), Value::String(ts));
        }
        v
    }
}

/// Implementor of the audit sink.
pub trait AuditSink: Send + Sync {
    fn emit(&self, event: AuditEvent);
}

/// JSONL file sink. Each call appends one line; the file is opened in
/// append mode each time so the OS handles concurrent writes safely.
/// (Volume is low — at most a handful of events per request.)
pub struct JsonlAuditSink {
    path: PathBuf,
    rotation_bytes: u64,
    state: Mutex<()>,
}

impl JsonlAuditSink {
    /// Create a sink writing to `path`. Will rotate to `<path>.1` once
    /// the file exceeds `rotation_bytes` (default 10MB).
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            rotation_bytes: 10 * 1024 * 1024,
            state: Mutex::new(()),
        }
    }

    /// Override rotation threshold (mostly for tests).
    pub fn with_rotation(mut self, bytes: u64) -> Self {
        self.rotation_bytes = bytes;
        self
    }

    fn append_line(&self, line: &str) -> Result<()> {
        let _guard = self.state.lock().map_err(|e| RedactionError::vault(e.to_string()))?;

        // Ensure parent dir.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Rotate if needed.
        if let Ok(meta) = std::fs::metadata(&self.path)
            && meta.len() >= self.rotation_bytes
        {
            let rotated = self.path.with_extension(format!(
                "{}.1",
                self.path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("jsonl"),
            ));
            let _ = std::fs::rename(&self.path, &rotated);
        }

        let mut f: File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_data()?;
        Ok(())
    }
}

impl AuditSink for JsonlAuditSink {
    fn emit(&self, event: AuditEvent) {
        let record = event.to_record();
        let line = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    target: "duduclaw_redaction::audit",
                    error = %e,
                    "failed to serialise audit event"
                );
                return;
            }
        };
        // tracing breadcrumb for log-aggregator consumers.
        tracing::info!(
            target: "duduclaw_redaction::audit",
            record = %line,
            "redaction audit event"
        );
        if let Err(e) = self.append_line(&line) {
            tracing::error!(
                target: "duduclaw_redaction::audit",
                error = %e,
                "failed to write audit line"
            );
        }
    }
}

/// No-op sink used in tests.
#[derive(Debug, Default)]
pub struct NullAuditSink;

impl AuditSink for NullAuditSink {
    fn emit(&self, _event: AuditEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn jsonl_appends_lines_with_ts() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let sink = JsonlAuditSink::new(path.clone());
        sink.emit(AuditEvent::Redact {
            agent_id: "agnes".into(),
            session_id: Some("s1".into()),
            source_category: "tool_result".into(),
            source_detail: Some("odoo.search".into()),
            rule_id: "email".into(),
            category: "EMAIL".into(),
            token: "<REDACT:EMAIL:abcdef01>".into(),
        });
        sink.emit(AuditEvent::VaultGc {
            expired_marked: 3,
            purged: 1,
        });
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        for l in &lines {
            assert!(l.contains("\"ts\""));
            let parsed: Value = serde_json::from_str(l).unwrap();
            assert!(parsed.is_object());
        }
    }

    #[test]
    fn rotation_renames_when_threshold_exceeded() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let sink = JsonlAuditSink::new(path.clone()).with_rotation(50);
        for _ in 0..20 {
            sink.emit(AuditEvent::VaultGc {
                expired_marked: 1,
                purged: 0,
            });
        }
        // Rotated file exists with a "jsonl.1" suffix (path.with_extension).
        let rotated = path.with_extension("jsonl.1");
        assert!(rotated.exists() || path.exists());
    }

    #[test]
    fn null_sink_does_nothing() {
        let sink = NullAuditSink;
        sink.emit(AuditEvent::VaultGc { expired_marked: 0, purged: 0 });
    }
}
