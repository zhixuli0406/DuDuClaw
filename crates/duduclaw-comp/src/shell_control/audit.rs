// WP-comp-shell-ipc — shell-control's own audit trail.
//
// Deliberately a SEPARATE file/struct from `codrive::audit::AuditLog`, not
// a shared writer — this is the whole point of the task brief's "落獨立的
// shell-action 稽核，與 agent 稽核分明": a reader who wants to know "did a
// HUMAN move focus, or did the AGENT" must be able to answer that from
// WHICH FILE a line came from, not from a field inside a shared file that
// could itself be the thing under dispute. `codrive::audit::AuditLog` also
// carries a `frozen: bool` column baked into every row — meaningless here
// (this channel has no freeze concept at all, see `mod.rs`'s module doc),
// so reusing that struct's `record()` signature would force either a fake
// value or an API change to a channel this one has nothing to do with.
//
// Same `Mutex<File>` concurrency reasoning as `codrive::audit::AuditLog`'s
// own doc comment: one writer PROCESS (this compositor binary) for this
// file's whole lifetime, so a `Mutex` is sufficient — no
// `duduclaw_core::with_file_lock` cross-process dependency needed.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

#[derive(Debug, Serialize)]
struct ShellControlAuditEvent {
    ts_ms: u128,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

pub struct ShellControlAuditLog {
    file: Mutex<File>,
}

impl ShellControlAuditLog {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        // Best-effort tighten, same non-fatal-on-failure posture as
        // `codrive::audit::AuditLog::open` — this file holds window titles/
        // app_ids, not secrets, but there's no reason to leave it group/
        // world readable.
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(error = %e, path = %path.display(), "shell_control: could not chmod 0600 the audit log");
        }
        Ok(Self { file: Mutex::new(file) })
    }

    /// Fail-open on the audit trail itself (same repo convention #4 split
    /// as `codrive::audit::AuditLog::record`: a broken audit log must never
    /// become a reason to block or crash the actual list/focus operation).
    pub fn record(&self, kind: &'static str, detail: Option<String>) {
        let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let event = ShellControlAuditEvent { ts_ms, kind, detail };

        let line = match serde_json::to_string(&event) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "shell_control: failed to serialize audit event — dropping this line");
                return;
            }
        };

        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(e) = writeln!(guard, "{line}") {
            tracing::error!(error = %e, "shell_control: failed to append audit log line");
            return;
        }
        let _ = guard.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    #[test]
    fn record_appends_one_json_line_with_kind_and_detail() {
        let path = std::env::temp_dir()
            .join(format!("duduclaw-shell-control-audit-test-{}-{}.jsonl", std::process::id(), line!()));
        let _ = std::fs::remove_file(&path);
        let log = ShellControlAuditLog::open(&path).expect("open must succeed");
        log.record("focus_window", Some("query=\"foot-A\" matched_app_id=\"foot-A\"".to_string()));
        log.record("focus_window_failed", Some("query=\"nope\"".to_string()));

        let contents = std::fs::read(&path).expect("audit file must exist");
        let lines: Vec<String> = contents.lines().map(|l| l.unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""kind":"focus_window""#));
        assert!(lines[0].contains("foot-A"));
        assert!(lines[1].contains(r#""kind":"focus_window_failed""#));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_file_is_created_mode_0600() {
        let path = std::env::temp_dir()
            .join(format!("duduclaw-shell-control-audit-perm-test-{}-{}.jsonl", std::process::id(), line!()));
        let _ = std::fs::remove_file(&path);
        let _log = ShellControlAuditLog::open(&path).expect("open must succeed");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(&path);
    }
}
