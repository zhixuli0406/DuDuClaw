// CD-0 codrive spike — audit trail.
// See DESIGN-codrive-desktop-2026-08.md §3.3.3/§3.4 "稽核": every injected
// event and every freeze/resume/emergency-stop transition is one JSON line
// appended to `$XDG_RUNTIME_DIR/duduclaw-codrive-audit.jsonl`.
//
// Concurrency note (repo convention #3, "cross-process file appends must
// hold an advisory lock"): that convention targets files written by
// *multiple processes* (e.g. `bus_queue.jsonl` written by both the Rust
// gateway and Python adapters). This audit file has exactly one writer
// process — this compositor binary — for the whole lifetime of CD-0 (no
// dashboard/agent-side writer exists yet). Within that one process, writes
// come from two threads (the injection-socket thread and the calloop main
// thread), so a `std::sync::Mutex<File>` is enough to serialize them —
// reaching for `duduclaw_core::with_file_lock` here would add a path
// dependency on `duduclaw-core` (and whatever it transitively pulls) for a
// guarantee this crate already gets for free from `Mutex`. If a second
// process ever needs to append to this same file (e.g. a dashboard-side
// reader that also writes checkpoints), switch to the cross-process helper
// then — flagged here so it isn't forgotten.

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
struct AuditEvent {
    ts_ms: u128,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    op: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// Agent-seat freeze state *at the time this event was recorded* — lets
    /// a reader reconstruct the freeze timeline without cross-referencing
    /// separate "freeze"/"resume" rows for every other event kind too.
    frozen: bool,
}

pub struct AuditLog {
    file: Mutex<File>,
}

impl AuditLog {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        // Best-effort tighten: the audit trail holds coordinates/keycodes,
        // not secrets, but there's no reason to leave it group/world
        // readable either. Non-fatal if this fails (e.g. non-POSIX fs).
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(error = %e, path = %path.display(), "codrive: could not chmod 0600 the audit log");
        }
        Ok(Self { file: Mutex::new(file) })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        kind: &'static str,
        op: Option<&'static str>,
        x: Option<f64>,
        y: Option<f64>,
        detail: Option<String>,
        frozen: bool,
    ) {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        let event = AuditEvent { ts_ms, kind, op, x, y, detail, frozen };

        let line = match serde_json::to_string(&event) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "codrive: failed to serialize audit event — dropping this line (fail-open on the audit trail itself, never blocks the injection path)");
                return;
            }
        };

        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(e) = writeln!(guard, "{line}") {
            tracing::error!(error = %e, "codrive: failed to append audit log line");
            return;
        }
        let _ = guard.flush();
    }
}
