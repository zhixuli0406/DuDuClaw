//! WP7 — LINE OA B2C credit metering.
//!
//! DuduCloud hands a customer their own LINE Official Account plus a credit
//! balance; their end-users chat with the agent as an AI 客服 and each reply
//! burns credit. This module is the metering ledger: a per-`(oa, line_user)`
//! points balance with an append-only event trail, atomic deduction, and a
//! fail-closed gate (balance ≤ 0 ⇒ refuse before calling the LLM).
//!
//! Billing settlement (topping up with real money via PayUni) is a separate,
//! operator-gated concern — this module only meters and lets an operator grant.

use std::path::Path;

use rusqlite::{params, Connection};
use serde::Serialize;

/// A credit ledger backed by SQLite (`credits.db`, WAL).
pub struct CreditLedger {
    conn: std::sync::Mutex<Connection>,
}

/// A single account's balance snapshot.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CreditBalance {
    pub oa_name: String,
    pub line_user_id: String,
    pub balance_points: i64,
    pub updated_at: String,
}

/// One ledger event (grant / deduct).
#[derive(Debug, Clone, Serialize)]
pub struct CreditEvent {
    pub oa_name: String,
    pub line_user_id: String,
    pub delta_points: i64,
    pub reason: String,
    pub created_at: String,
}

impl CreditLedger {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(db_path).map_err(|e| format!("open credits.db: {e}"))?;
        Self::init(&conn)?;
        Ok(Self { conn: std::sync::Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        Self::init(&conn)?;
        Ok(Self { conn: std::sync::Mutex::new(conn) })
    }

    fn init(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;
            CREATE TABLE IF NOT EXISTS credit_accounts (
                oa_name TEXT NOT NULL,
                line_user_id TEXT NOT NULL,
                balance_points INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (oa_name, line_user_id)
            );
            CREATE TABLE IF NOT EXISTS credit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                oa_name TEXT NOT NULL,
                line_user_id TEXT NOT NULL,
                delta_points INTEGER NOT NULL,
                reason TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_credit_events_acct
                ON credit_events(oa_name, line_user_id, id);",
        )
        .map_err(|e| format!("init credits schema: {e}"))
    }

    /// Convert output tokens to points using an OA's `credit_rate` (points per
    /// 1K output tokens). Rounds up so a sub-1K reply still costs ≥1 point when
    /// the rate is positive. Rate 0 ⇒ 0 (metering off).
    pub fn tokens_to_points(output_tokens: u64, credit_rate: f64) -> i64 {
        if credit_rate <= 0.0 || output_tokens == 0 {
            return 0;
        }
        (output_tokens as f64 / 1000.0 * credit_rate).ceil() as i64
    }

    /// Operator grants (positive) or adjusts (negative) points. Returns the new
    /// balance. Records a `credit_events` row.
    pub fn grant(&self, oa: &str, user: &str, points: i64, reason: &str) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO credit_accounts (oa_name, line_user_id, balance_points, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(oa_name, line_user_id)
             DO UPDATE SET balance_points = balance_points + ?3, updated_at = ?4",
            params![oa, user, points, now],
        )
        .map_err(|e| format!("grant: {e}"))?;
        conn.execute(
            "INSERT INTO credit_events (oa_name, line_user_id, delta_points, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![oa, user, points, reason, now],
        )
        .map_err(|e| format!("grant event: {e}"))?;
        Self::balance_locked(&conn, oa, user)
    }

    /// Deduct `points` (must be ≥0). Single-writer transaction so concurrent
    /// deductions can't race. Returns the new balance (may go negative if the
    /// reply already happened — the gate below prevents starting when ≤0).
    pub fn deduct(&self, oa: &str, user: &str, points: i64, reason: &str) -> Result<i64, String> {
        if points <= 0 {
            return self.balance(oa, user);
        }
        self.grant(oa, user, -points, reason)
    }

    /// Current balance (0 if the account was never seen).
    pub fn balance(&self, oa: &str, user: &str) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        Self::balance_locked(&conn, oa, user)
    }

    fn balance_locked(conn: &Connection, oa: &str, user: &str) -> Result<i64, String> {
        let r: std::result::Result<i64, _> = conn.query_row(
            "SELECT balance_points FROM credit_accounts WHERE oa_name = ?1 AND line_user_id = ?2",
            params![oa, user],
            |row| row.get(0),
        );
        match r {
            Ok(b) => Ok(b),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(format!("balance: {e}")),
        }
    }

    /// Fail-closed gate: may this user's reply proceed? False when metering is
    /// on (rate > 0) and the balance is ≤ 0. Rate 0 ⇒ always allowed.
    pub fn can_proceed(&self, oa: &str, user: &str, credit_rate: f64) -> Result<bool, String> {
        if credit_rate <= 0.0 {
            return Ok(true);
        }
        Ok(self.balance(oa, user)? > 0)
    }

    /// Recent events for an account, newest first.
    pub fn history(&self, oa: &str, user: &str, limit: u32) -> Result<Vec<CreditEvent>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT oa_name, line_user_id, delta_points, reason, created_at
                 FROM credit_events WHERE oa_name = ?1 AND line_user_id = ?2
                 ORDER BY id DESC LIMIT ?3",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![oa, user, limit], |row| {
                Ok(CreditEvent {
                    oa_name: row.get(0)?,
                    line_user_id: row.get(1)?,
                    delta_points: row.get(2)?,
                    reason: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_to_points_rounds_up_and_respects_zero_rate() {
        assert_eq!(CreditLedger::tokens_to_points(0, 1.0), 0);
        assert_eq!(CreditLedger::tokens_to_points(500, 0.0), 0); // metering off
        assert_eq!(CreditLedger::tokens_to_points(500, 1.0), 1); // <1K rounds up
        assert_eq!(CreditLedger::tokens_to_points(2000, 2.0), 4);
    }

    #[test]
    fn grant_deduct_balance_gate() {
        let l = CreditLedger::open_in_memory().unwrap();
        // Unknown account starts at 0, and metering-on gate blocks it.
        assert_eq!(l.balance("oa1", "u1").unwrap(), 0);
        assert!(!l.can_proceed("oa1", "u1", 1.0).unwrap());
        // Rate 0 always allowed regardless of balance.
        assert!(l.can_proceed("oa1", "u1", 0.0).unwrap());

        // Grant 5, spend down.
        assert_eq!(l.grant("oa1", "u1", 5, "topup").unwrap(), 5);
        assert!(l.can_proceed("oa1", "u1", 1.0).unwrap());
        assert_eq!(l.deduct("oa1", "u1", 3, "reply").unwrap(), 2);
        assert_eq!(l.deduct("oa1", "u1", 2, "reply").unwrap(), 0);
        // Balance 0 ⇒ gate closes.
        assert!(!l.can_proceed("oa1", "u1", 1.0).unwrap());

        // Accounts are isolated per (oa, user).
        assert_eq!(l.balance("oa1", "u2").unwrap(), 0);
        assert_eq!(l.balance("oa2", "u1").unwrap(), 0);

        // History newest-first.
        let h = l.history("oa1", "u1", 10).unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].reason, "reply");
        assert_eq!(h[2].reason, "topup");
    }
}
