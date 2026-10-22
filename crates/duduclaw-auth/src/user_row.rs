//! Shared `users` row mapping and the ACL generation counter.
//!
//! Every reader of a full [`User`] goes through [`USER_COLUMNS`] and
//! [`user_from_row`], so a new column is added in one place. The generation
//! helpers live here because every authorization-relevant write in
//! [`crate::db`], [`crate::otp`] and [`crate::org_write`] bumps it inside the
//! same transaction as the write itself.

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row};

use crate::models::{User, UserRole, UserStatus};

/// Column list matching [`user_from_row`]; keep the two in sync.
pub(crate) const USER_COLUMNS: &str = "id, email, display_name, role, status, created_at, \
     updated_at, last_login, must_change_password, department, manager_user_id";

/// `auth_meta` key of the counter that invalidates every cached live ACL.
pub(crate) const ACL_GENERATION_KEY: &str = "acl_generation";

fn corrupt(idx: usize, msg: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, msg.into())
}

/// Map one row selected with [`USER_COLUMNS`] (in that order, from index 0).
/// A role or status the enums do not know is a conversion error, never a
/// silent default.
pub(crate) fn user_from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    let role_str: String = row.get(3)?;
    let status_str: String = row.get(4)?;
    let role: UserRole = role_str
        .parse()
        .map_err(|e: String| corrupt(3, format!("corrupt role in DB: {e}")))?;
    let status: UserStatus = status_str
        .parse()
        .map_err(|e: String| corrupt(4, format!("corrupt status in DB: {e}")))?;
    Ok(User {
        id: row.get(0)?,
        email: row.get(1)?,
        display_name: row.get(2)?,
        role,
        status,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        last_login: row.get(7)?,
        must_change_password: row.get::<_, i64>(8)? != 0,
        department: row.get(9)?,
        manager_user_id: row.get(10)?,
    })
}

/// Read one user by id on an already-held connection or transaction.
pub(crate) fn load_user(conn: &Connection, user_id: &str) -> Result<Option<User>, String> {
    conn.query_row(
        &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
        [user_id],
        user_from_row,
    )
    .optional()
    .map_err(|e| format!("query error: {e}"))
}

/// Increment the ACL generation. Call inside the transaction that performs
/// the authorization-relevant write, so the bump commits (or rolls back)
/// with it. The upsert recreates a missing row instead of failing.
pub(crate) fn bump_acl_generation(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO auth_meta (key, value) VALUES (?1, 2)
         ON CONFLICT(key) DO UPDATE SET value = value + 1",
        [ACL_GENERATION_KEY],
    )?;
    Ok(())
}

/// Current ACL generation. A missing row is an error (callers then skip the
/// cache and recompute), not generation 0.
pub(crate) fn read_acl_generation(conn: &Connection) -> Result<u64, String> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT value FROM auth_meta WHERE key = ?1",
            [ACL_GENERATION_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("acl generation read failed: {e}"))?;
    match value {
        Some(v) if v >= 0 => Ok(v as u64),
        Some(_) => Err("acl generation is negative".to_string()),
        None => Err("acl generation missing".to_string()),
    }
}

/// `true` when `table` already has column `col` (idempotent migrations).
pub(crate) fn has_column(conn: &Connection, table: &str, col: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| format!("failed to inspect {table} table: {e}"))?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .and_then(|rows| rows.collect::<Result<Vec<String>, _>>())
        .map_err(|e| format!("failed to inspect {table} table: {e}"))?;
    Ok(cols.iter().any(|c| c == col))
}
