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
//!
//! F6fb (fix round, item 2): the explicit [`check`] calls covered only some
//! writers. Every connection a store opens now also carries
//! [`install_write_triggers`]: a per-connection SQL function
//! ([`GATE_FUNCTION`]) and TEMP triggers on every table that has a label
//! column ([`LABEL_COLUMNS`]) — and, for a store whose child rows are keyed
//! by a parent id, on every child table — so every INSERT and UPDATE that
//! carries a label is gated, including writers added later:
//! - INSERT of a refused label: `RAISE(ABORT, REFUSED)` (the writer sees an
//!   error);
//! - UPDATE with the old or the new label refused that *widens*: sets any
//!   column other than the store's closing columns to a new non-NULL value,
//!   or raises a lowering-only column ([`GateRules`]): the row is skipped
//!   (`RAISE(IGNORE)`), so a sweep over every agent still runs for the
//!   others and never changes (or re-keys into or out of) a quarantined
//!   label's content. Narrowing writes keep working on such a label: expiry
//!   and close-out ([`MEMORY_CLOSING_COLUMNS`]), clearing a value to NULL
//!   (forget / revocation blanking), lowering trust or confidence.
//!
//! Not gated: DELETE (it cannot plant state for a later holder of the label,
//! a right-to-erasure delete must never be skipped, and a delete under a
//! quarantined label changes its store marks, which a same-id restore then
//! refuses `STORES_CHANGED`), and the store's narrowing tables (forget
//! tombstones, revocation and fence records, [`MEMORY_NARROWING_TABLES`]),
//! whose inserts revoke rather than plant. The gateway's own re-key / purge /
//! tombstone connections are opened separately and carry no triggers.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rusqlite::functions::FunctionFlags;
use rusqlite::types::ValueRef;
use rusqlite::Connection;

/// `gate(database_file, namespace)`; `Err(reason)` refuses the write.
pub type LabelWriteGate = fn(Option<&Path>, &str) -> Result<(), String>;

static GATE: OnceLock<LabelWriteGate> = OnceLock::new();

/// Columns that hold an agent label. Same list as the gateway's
/// `isolation_label_stores::AGENT_COLUMNS` (a gateway test keeps them equal).
pub const LABEL_COLUMNS: [&str; 6] = ["agent_id", "assigned_to", "owner_agent_id", "agent", "agent_name", "agent_label"];
/// The per-connection SQL function the TEMP triggers call: `1` = writable.
pub const GATE_FUNCTION: &str = "duduclaw_label_writable";
/// Prefix of every gate trigger (in `sqlite_temp_master`).
pub const TRIGGER_PREFIX: &str = "duduclaw_label_gate_";
/// The error text of an INSERT a trigger refused.
pub const REFUSED: &str = "label write refused: this employee is tombstoned or quarantined (F6 H4 writer gate)";
/// How long one connection reuses a gate answer for a label (bounds the
/// lifecycle reads of a sweep over many rows).
const CACHE_TTL: Duration = Duration::from_millis(250);
const CACHE_MAX: usize = 256;

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

/// The process-wide gate (allows everything until one is installed).
pub fn installed_gate(file: Option<&Path>, namespace: &str) -> Result<(), String> {
    match GATE.get() {
        Some(gate) => gate(file, namespace),
        None => Ok(()),
    }
}

/// A child table gated through its parent's label (e.g. sessions.db
/// `session_messages.session_id` → `sessions.id` → `sessions.agent_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentKey {
    pub table: &'static str,
    pub id: &'static str,
    pub label: &'static str,
    pub child: &'static str,
}

fn quoted(name: &str) -> String { format!("\"{}\"", name.replace('"', "\"\"")) }

/// memory.db columns that only close, expire or count a row: an UPDATE may
/// change them freely on a refused label (expiry, supersession close-out).
pub const MEMORY_CLOSING_COLUMNS: [&str; 6] =
    ["valid_until", "invalidated_by_event", "invalidated_at", "superseded_by", "access_count", "last_accessed"];
/// memory.db columns an UPDATE may lower but not raise on a refused label
/// (a revoked source lowers the trust of what cites it).
pub const MEMORY_LOWERING_COLUMNS: [&str; 2] = ["origin_trust", "confidence"];
/// memory.db tables that only record forgets, revocations, fences and their
/// delivery: never gated (their writes revoke, never plant).
pub const MEMORY_NARROWING_TABLES: [&str; 15] = ["forgotten_sources", "forgotten_memories", "memory_forget_plans",
    "memory_forget_steps", "memory_fence", "forgotten_connectors", "memory_forget_apply_reports", "disconnect_forget_batches",
    "disconnect_forget_batch_plans", "forget_receipts", "disconnected_connectors", "causal_ccr_revocation_outbox",
    "causal_ccr_revoking", "causal_ccr_delivery_leases", "causal_ccr_outbox_meta"];

/// Per-store exceptions of [`install_write_triggers`]. A table or column not
/// listed is gated (a writer or column added later is covered by default).
#[derive(Debug, Clone, Copy, Default)]
pub struct GateRules<'a> {
    /// Tables never gated (narrowing records only).
    pub exempt_tables: &'a [&'a str],
    /// Columns an UPDATE may change freely.
    pub closing: &'a [&'a str],
    /// Columns an UPDATE may lower (or clear), never raise.
    pub lowering: &'a [&'a str],
}

/// The memory engine's rules.
pub const MEMORY_RULES: GateRules<'static> =
    GateRules { exempt_tables: &MEMORY_NARROWING_TABLES, closing: &MEMORY_CLOSING_COLUMNS, lowering: &MEMORY_LOWERING_COLUMNS };

/// The memory engine's connection: [`install_write_triggers`] with the
/// process-wide gate over [`LABEL_COLUMNS`].
pub(crate) fn install_on_engine(conn: &Connection) -> rusqlite::Result<usize> {
    install_write_triggers(conn, installed_gate, &LABEL_COLUMNS, None, MEMORY_RULES)
}

/// Register [`GATE_FUNCTION`] on `conn` (calling `gate` with the
/// connection's database file) and create the TEMP gate triggers on every
/// table of the main schema that has one of `columns` (and, with `parent`,
/// on every other table that has the parent's child column), except
/// `rules.exempt_tables`. Virtual tables and their shadow tables are skipped.
/// Idempotent; returns the number of triggers for the gated tables. Call
/// after the schema exists.
pub fn install_write_triggers(conn: &Connection, gate: LabelWriteGate, columns: &[&str], parent: Option<ParentKey>,
    rules: GateRules<'_>) -> rusqlite::Result<usize> {
    let file: Option<PathBuf> = conn.path().filter(|path| !path.is_empty()).map(PathBuf::from);
    let mut cache: HashMap<String, (Instant, bool)> = HashMap::new();
    conn.create_scalar_function(GATE_FUNCTION, 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
        let label = match ctx.get_raw(0) {
            ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) => text.to_owned(),
                Err(_) => return Ok(0i64),
            },
            // No label on the row (NULL, a number): nothing to gate.
            _ => return Ok(1i64),
        };
        if let Some((at, writable)) = cache.get(&label) {
            if at.elapsed() < CACHE_TTL { return Ok(i64::from(*writable)); }
        }
        let writable = gate(file.as_deref(), &label).is_ok();
        if cache.len() >= CACHE_MAX { cache.clear(); }
        cache.insert(label, (Instant::now(), writable));
        Ok(i64::from(writable))
    })?;
    let tables: Vec<(String, Option<String>)> = {
        let mut query = conn.prepare("SELECT name, sql FROM main.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
        let rows = query.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let virtual_tables: Vec<&str> = tables.iter()
        .filter(|(_, sql)| sql.as_deref().is_some_and(|sql| sql.trim_start().to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE")))
        .map(|(name, _)| name.as_str()).collect();
    let mut created = 0usize;
    for (table, _) in &tables {
        if virtual_tables.iter().any(|v| table == v || table.starts_with(&format!("{v}_"))) { continue; }
        if rules.exempt_tables.contains(&table.as_str()) { continue; }
        let table_columns: Vec<String> = {
            let mut query = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
            let rows = query.query_map([table], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut exprs: Vec<(String, String, String)> = Vec::new();
        for column in table_columns.iter().filter(|column| columns.contains(&column.as_str())) {
            let c = quoted(column);
            exprs.push((format!("c_{column}"), format!("NEW.{c}"), format!("OLD.{c}")));
        }
        if let Some(parent) = parent {
            if table != parent.table && table_columns.iter().any(|column| column == parent.child) {
                let lookup = |row: &str| format!("(SELECT {} FROM {} WHERE {} = {row}.{})", quoted(parent.label), quoted(parent.table),
                    quoted(parent.id), quoted(parent.child));
                exprs.push((format!("p_{}", parent.child), lookup("NEW"), lookup("OLD")));
            }
        }
        // A widening change: a non-closing column set to a new non-NULL value,
        // a lowering-only column raised (a column added later is gated).
        let changed: Vec<String> = table_columns.iter().filter(|column| !rules.closing.contains(&column.as_str()))
            .map(|column| {
                let c = quoted(column);
                if rules.lowering.contains(&column.as_str()) {
                    format!("(NEW.{c} IS NOT OLD.{c} AND NEW.{c} IS NOT NULL AND (OLD.{c} IS NULL OR NEW.{c} > OLD.{c}))")
                } else {
                    format!("(NEW.{c} IS NOT OLD.{c} AND NEW.{c} IS NOT NULL)")
                }
            }).collect();
        for (key, new, old) in exprs {
            let name = |kind: &str| quoted(&format!("{TRIGGER_PREFIX}{kind}_{table}_{key}"));
            let on = quoted(table);
            conn.execute_batch(&format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS {ins} BEFORE INSERT ON {on} WHEN {GATE_FUNCTION}({new}) = 0
                     BEGIN SELECT RAISE(ABORT, '{REFUSED}'); END;", ins = name("i")))?;
            created += 1;
            if changed.is_empty() { continue; }
            conn.execute_batch(&format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS {upd} BEFORE UPDATE ON {on}
                     WHEN ({GATE_FUNCTION}({old}) = 0 OR {GATE_FUNCTION}({new}) = 0) AND ({changed})
                     BEGIN SELECT RAISE(IGNORE); END;", upd = name("u"), changed = changed.join(" OR ")))?;
            created += 1;
        }
    }
    Ok(created)
}

/// Tables of `conn` (main schema, not virtual) that have a label column,
/// with the gate trigger names present for each (diagnostics and tests).
pub fn gated_tables(conn: &Connection) -> rusqlite::Result<Vec<(String, Vec<String>)>> {
    let mut query = conn.prepare(
        "SELECT tbl_name, name FROM sqlite_temp_master WHERE type='trigger' AND name LIKE ?1 ORDER BY tbl_name, name")?;
    let rows: Vec<(String, String)> = query.query_map([format!("{TRIGGER_PREFIX}%")], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (table, name) in rows {
        match out.last_mut() {
            Some((last, names)) if *last == table => names.push(name),
            _ => out.push((table, vec![name])),
        }
    }
    Ok(out)
}
