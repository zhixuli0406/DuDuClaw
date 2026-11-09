//! Live authority checks at SQLite admission and final publication.
use super::*;

pub(super) fn verify_live_io() -> Result<(), String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())
}

pub(super) fn begin_live_write(conn: &mut Connection) -> Result<rusqlite::Transaction<'_>, String> {
    verify_live_io()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("begin service write: {e}"))?;
    verify_live_io()?;
    Ok(tx)
}

pub(super) fn commit_live_write(tx: rusqlite::Transaction<'_>) -> Result<(), String> {
    verify_live_io()?;
    tx.commit().map_err(|e| format!("commit service write: {e}"))
}
