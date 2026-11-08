//! F6h (native Linux isolation v1.81, F6 design r5 §3.3, H4): a process-wide
//! write gate on memory namespaces, installed once by the gateway.
//!
//! The gateway decides whether a namespace may be written (a tombstoned or
//! quarantined protected employee's label may not). This crate cannot depend
//! on the gateway, so the decision is a plain function pointer installed at
//! gateway boot. No gate installed (CLI, tests, every non-gateway process):
//! nothing changes. The gate receives the database file of the connection
//! (`None` for an in-memory or temporary database) and the namespace, and
//! must itself return `Ok` outside protected mode.
use std::path::Path;
use std::sync::OnceLock;

/// `gate(database_file, namespace)`; `Err(reason)` refuses the write.
pub type LabelWriteGate = fn(Option<&Path>, &str) -> Result<(), String>;

static GATE: OnceLock<LabelWriteGate> = OnceLock::new();

/// Install the gate (once per process). `false` when one is already installed
/// (the first one stays; a gate is never replaced or removed).
pub fn install_label_write_gate(gate: LabelWriteGate) -> bool {
    GATE.set(gate).is_ok()
}

/// Run the installed gate for one write on `conn` into `namespace`.
pub(crate) fn check(conn: &rusqlite::Connection, namespace: &str) -> duduclaw_core::Result<()> {
    let Some(gate) = GATE.get() else { return Ok(()) };
    let file = conn.path().filter(|path| !path.is_empty()).map(Path::new);
    gate(file, namespace).map_err(duduclaw_core::error::DuDuClawError::Memory)
}
