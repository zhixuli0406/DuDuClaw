//! Durable delivery receipts. Failure to read a receipt is never proof of delivery.

use rusqlite::{OptionalExtension, params};

use crate::approval::{ApprovalId, ApprovalStore, begin_live_write, commit_live_write, verify_live_io};

/// Times use RFC3339, matching the approvals table. purpose is reserved for
/// shared delivery infrastructure; RM-143 writes only the literal rm143.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub(crate) card_id: String,
    pub(crate) approval_id: String,
    pub(crate) purpose: String,
    pub(crate) user_id: String,
    pub(crate) channel: String,
    pub(crate) account: String,
    pub(crate) principal: String,
    pub(crate) dm: String,
    pub(crate) message_id: Option<String>,
    pub(crate) digest: String,
    pub(crate) tier: String,
    pub(crate) valid_until: String,
    pub(crate) status: String,
    pub(crate) sent_at: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TerminalStatus {
    Consumed,
    Retired,
    Expired,
}

impl TerminalStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Consumed => "consumed",
            Self::Retired => "retired",
            Self::Expired => "expired",
        }
    }
}

const COLUMNS: &str = "card_id, approval_id, purpose, user_id, channel, account, principal,
    dm, message_id, digest, tier, valid_until, status, sent_at";

pub(crate) async fn by_card_id(store: &ApprovalStore, card_id: &str) -> Result<Option<Receipt>, String> {
    let conn = store.conn.lock().await;
    verify_live_io()?;
    conn.query_row(&format!("SELECT {COLUMNS} FROM chat_admin_cards WHERE card_id=?1"),
        [card_id], from_row).optional().map_err(|e| format!("read chat-admin receipt: {e}"))
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Receipt> {
    Ok(Receipt {
        card_id: row.get(0)?, approval_id: row.get(1)?, purpose: row.get(2)?,
        user_id: row.get(3)?, channel: row.get(4)?, account: row.get(5)?,
        principal: row.get(6)?, dm: row.get(7)?, message_id: row.get(8)?,
        digest: row.get(9)?, tier: row.get(10)?, valid_until: row.get(11)?,
        status: row.get(12)?, sent_at: row.get(13)?,
    })
}

/// Only future delivery code may call this, after a successful channel send.
/// Identity/destination must come from the caller-verified proof, not payloads.
pub(crate) async fn insert(store: &ApprovalStore, receipt: &Receipt) -> Result<(), String> {
    if receipt.purpose != "rm143" || receipt.status != "live"
        || !matches!(receipt.tier.as_str(), "T1" | "T2")
        || uuid::Uuid::parse_str(&receipt.user_id).is_err()
        || super::ledger::parse_channel(&receipt.channel).is_err() {
        return Err("invalid chat-admin delivery receipt".into());
    }
    let mut conn = store.conn.lock().await;
    let tx = begin_live_write(&mut conn)?;
    tx.execute(
        &format!("INSERT INTO chat_admin_cards ({COLUMNS})
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"),
        params![receipt.card_id, receipt.approval_id, receipt.purpose, receipt.user_id,
            receipt.channel, receipt.account, receipt.principal, receipt.dm, receipt.message_id,
            receipt.digest, receipt.tier, receipt.valid_until, receipt.status, receipt.sent_at],
    ).map_err(|e| format!("insert chat-admin receipt: {e}"))?;
    commit_live_write(tx)
}

/// Exact delivery coordinates; callers must still check purpose, status and proof.
pub(crate) async fn lookup_message(
    store: &ApprovalStore, channel: &str, account: &str, dm: &str, message_id: &str,
) -> Result<Option<Receipt>, String> {
    let conn = store.conn.lock().await;
    verify_live_io()?;
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM chat_admin_cards
            WHERE channel = ?1 AND account = ?2 AND dm = ?3 AND message_id = ?4"),
        params![channel, account, dm, message_id], from_row,
    ).optional().map_err(|e| format!("lookup chat-admin receipt: {e}"))
}

/// LINE has no message ID. Return all exact matches rather than arbitrarily
/// choosing between multiple deliveries; callers must disambiguate by proof/card.
pub(crate) async fn lookup_without_message_id(
    store: &ApprovalStore, approval_id: &ApprovalId, user_id: &str, dm: &str,
) -> Result<Vec<Receipt>, String> {
    let conn = store.conn.lock().await;
    verify_live_io()?;
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM chat_admin_cards
        WHERE approval_id = ?1 AND user_id = ?2 AND dm = ?3 AND message_id IS NULL
        ORDER BY sent_at, card_id"))
        .map_err(|e| format!("prepare chat-admin receipt lookup: {e}"))?;
    let rows = stmt.query_map(params![approval_id.as_str(), user_id, dm], from_row)
        .map_err(|e| format!("lookup chat-admin receipts: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("read chat-admin receipts: {e}"))
}

pub(crate) async fn list_live(store: &ApprovalStore, approval_id: &ApprovalId) -> Result<Vec<Receipt>, String> {
    let conn = store.conn.lock().await;
    verify_live_io()?;
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM chat_admin_cards
        WHERE approval_id = ?1 AND status = 'live' ORDER BY sent_at, card_id"))
        .map_err(|e| format!("prepare live chat-admin receipts: {e}"))?;
    let rows = stmt.query_map([approval_id.as_str()], from_row)
        .map_err(|e| format!("list live chat-admin receipts: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("read live chat-admin receipts: {e}"))
}

/// A delivered card stays in the receipt-bound route even after retirement.
pub(crate) async fn has_for_approval(store: &ApprovalStore, approval_id: &ApprovalId) -> Result<bool, String> {
    let conn = store.conn.lock().await;
    verify_live_io()?;
    conn.query_row("SELECT EXISTS(SELECT 1 FROM chat_admin_cards WHERE approval_id=?1 AND purpose='rm143')",
        [approval_id.as_str()], |r| r.get(0))
        .map_err(|e| format!("read chat-admin receipt ownership: {e}"))
}

/// Only RM-143 owns these maintenance edits; never sweep another purpose.
pub(crate) async fn list_all_live(store: &ApprovalStore) -> Result<Vec<Receipt>, String> {
    let conn = store.conn.lock().await;
    verify_live_io()?;
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM chat_admin_cards
        WHERE purpose = 'rm143' AND status = 'live' ORDER BY sent_at, card_id"))
        .map_err(|e| format!("prepare live chat-admin card sweep: {e}"))?;
    let rows = stmt.query_map([], from_row)
        .map_err(|e| format!("list live chat-admin card sweep: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("read live chat-admin card sweep: {e}"))
}

/// Terminal transitions never revive a receipt. Successful approval decisions
/// use ledger::insert_and_consume instead, in the approval write transaction.
pub(crate) async fn mark(
    store: &ApprovalStore, card_id: &str, status: TerminalStatus,
) -> Result<bool, String> {
    let mut conn = store.conn.lock().await;
    let tx = begin_live_write(&mut conn)?;
    let n = tx.execute("UPDATE chat_admin_cards SET status = ?1 WHERE card_id = ?2 AND status = 'live'",
        params![status.as_str(), card_id])
        .map_err(|e| format!("mark chat-admin receipt: {e}"))?;
    commit_live_write(tx)?;
    Ok(n == 1)
}
