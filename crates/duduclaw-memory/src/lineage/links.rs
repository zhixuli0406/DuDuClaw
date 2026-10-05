//! Turn ↔ user-message links (P2-B third review F1, F6): which conversation
//! turn a user message started, recorded when one write carried both. Kept
//! across namespaces and forgets, so a forget of the message in any
//! namespace can find the turn's key.

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{Connection, OptionalExtension, params};

use super::db::{OriginRow, Role};

fn mem_err(e: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(e.to_string())
}

/// One-time fill of `turn_message_links` from lineage written before the
/// table existed (rows whose direct sources include both keys).
pub(crate) fn backfill_turn_links(conn: &Connection) -> Result<()> {
    let done = conn
        .query_row(
            "SELECT 1 FROM memory_meta WHERE key = 'turn_links_backfilled'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(mem_err)?
        .is_some();
    if done {
        return Ok(());
    }
    conn.execute_batch(
        "INSERT OR IGNORE INTO turn_message_links (source_session, user_message, turn_message)
         SELECT DISTINCT t.source_session, c.source_message, t.source_message
         FROM memory_origins t JOIN memory_origins c
           ON c.memory_store = t.memory_store AND c.memory_id = t.memory_id
          AND c.agent_id = t.agent_id AND c.source_session = t.source_session
         WHERE t.source_kind = 'mcp_turn' AND t.role = 'direct'
           AND c.source_kind = 'channel_message' AND c.role = 'direct';
         INSERT OR IGNORE INTO memory_meta (key, value) VALUES ('turn_links_backfilled', '1');",
    )
    .map_err(|e| DuDuClawError::Memory(format!("turn link backfill: {e}")))
}

/// Record the turn ↔ user-message links of one write: every `mcp_turn` and
/// `channel_message` pair among `rows` (that write's own sources, i.e. its
/// `direct` rows) in the same session.
pub(crate) fn record_turn_links(conn: &Connection, rows: &[OriginRow]) -> Result<()> {
    let turns: Vec<&OriginRow> = rows
        .iter()
        .filter(|r| r.role == Role::Direct && r.kind == "mcp_turn")
        .collect();
    if turns.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO turn_message_links
                (source_session, user_message, turn_message) VALUES (?1, ?2, ?3)",
        )
        .map_err(mem_err)?;
    for t in turns {
        for c in rows.iter().filter(|c| {
            c.role == Role::Direct && c.kind == "channel_message" && c.session == t.session
        }) {
            stmt.execute(params![t.session, c.message, t.message])
                .map_err(mem_err)?;
        }
    }
    Ok(())
}
