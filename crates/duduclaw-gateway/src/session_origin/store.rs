//! `sessions` origin columns: idempotent migration, the stamping
//! `get_or_create_with_origin`, and a read accessor.
//!
//! Stamping rule: the first non-NULL `origin_kind` wins and is never
//! overwritten. A row created before the columns existed (NULL) is stamped by
//! the next live turn on it (a WebChat resume is only allowed for the
//! session's owner, so that turn's origin is the row's) or by the
//! [`super::backfill`].

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{Connection, OptionalExtension, params};

use super::{OriginKind, SessionOrigin};
use crate::session::{Session, SessionManager};

/// Add the origin columns and their index. Errors from `ALTER TABLE` mean the
/// column already exists (same convention as the other `sessions`
/// migrations); the index statement is idempotent.
pub(crate) fn migrate(conn: &Connection) {
    for sql in [
        // webchat_user | webchat_visitor | channel | public_visitor | internal | unknown
        "ALTER TABLE sessions ADD COLUMN origin_kind TEXT",
        // only for webchat_user
        "ALTER TABLE sessions ADD COLUMN owner_user_id TEXT",
        // when the origin was stamped (RFC 3339)
        "ALTER TABLE sessions ADD COLUMN origin_at TEXT",
    ] {
        let _ = conn.execute(sql, []);
    }
    if let Err(e) = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sessions_agent_origin
            ON sessions(agent_id, origin_kind, owner_user_id, last_active)",
        [],
    ) {
        tracing::warn!("sessions origin index not created: {e}");
    }
}

/// Insert a session row with `origin` (no-op when the row exists), then
/// (when `stamp_existing`) stamp `origin` onto an existing row of the same
/// agent whose origin is still NULL.
pub(crate) fn insert_or_stamp(
    conn: &Connection,
    session_id: &str,
    agent_id: &str,
    origin: &SessionOrigin,
    stamp_existing: bool,
    now: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO sessions
             (id, agent_id, summary, total_tokens, last_active, model, created_at,
              origin_kind, owner_user_id, origin_at)
         VALUES (?1, ?2, '', 0, ?3, 'auto', ?3, ?4, ?5, ?3)",
        params![
            session_id,
            agent_id,
            now,
            origin.kind().as_str(),
            origin.owner_user_id()
        ],
    )?;
    if stamp_existing {
        conn.execute(
            "UPDATE sessions SET origin_kind = ?3, owner_user_id = ?4, origin_at = ?5
             WHERE id = ?1 AND agent_id = ?2 AND origin_kind IS NULL",
            params![
                session_id,
                agent_id,
                origin.kind().as_str(),
                origin.owner_user_id(),
                now
            ],
        )?;
    }
    Ok(())
}

fn select_session(conn: &Connection, session_id: &str) -> Result<Session> {
    conn.query_row(
        "SELECT id, agent_id, summary, total_tokens, last_active, model,
                COALESCE(lineage, 1), created_at
         FROM sessions WHERE id = ?1",
        params![session_id],
        |row| {
            Ok(Session {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                summary: row.get(2)?,
                total_tokens: row.get(3)?,
                last_active: row.get(4)?,
                model: row.get(5)?,
                lineage: row.get::<_, i64>(6)?.max(1) as u32,
                created_at: row.get(7)?,
            })
        },
    )
    .map_err(|e| DuDuClawError::Gateway(e.to_string()))
}

/// Read the stored origin. `Ok(None)` when the row does not exist or its
/// origin is still NULL (not yet stamped or backfilled).
pub(crate) fn read_origin(conn: &Connection, session_id: &str) -> Result<Option<SessionOrigin>> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT origin_kind, owner_user_id FROM sessions WHERE id = ?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
    Ok(row.and_then(|(kind, owner)| {
        let kind = OriginKind::parse(kind.as_deref()?);
        Some(match (kind, owner) {
            (OriginKind::WebchatUser, Some(owner)) => SessionOrigin::webchat_user(&owner),
            // A webchat_user row without an owner cannot be attributed.
            (OriginKind::WebchatUser, None) => SessionOrigin::unknown(),
            (kind, _) => SessionOrigin::of_kind(kind),
        })
    }))
}

impl SessionManager {
    /// Get an existing session or create a new one, recording where it comes
    /// from. The first recorded origin wins: a row that already has one keeps
    /// it; a legacy row (NULL origin) of the same agent takes `origin`.
    ///
    /// Same race-free INSERT OR IGNORE + SELECT and the same cross-agent
    /// check as [`SessionManager::get_or_create`].
    pub async fn get_or_create_with_origin(
        &self,
        session_id: &str,
        agent_id: &str,
        origin: &SessionOrigin,
    ) -> Result<Session> {
        self.create_inner(session_id, agent_id, origin, true).await
    }

    /// Shared body of `get_or_create` (origin `unknown` on insert only, never
    /// stamping a legacy row so the backfill can still classify it) and
    /// [`Self::get_or_create_with_origin`].
    ///
    /// One IMMEDIATE transaction: the returned [`Session::created_at`] is the
    /// identity of exactly the row this call created or loaded (the turn
    /// generation — no clear can slip in between). RM-173: a user clear left
    /// at this id ([`crate::session::STUCK_USER_CLEAR`]) is moved away first,
    /// so the turn starts a fresh conversation without the cleared context.
    pub(crate) async fn create_inner(
        &self,
        session_id: &str,
        agent_id: &str,
        origin: &SessionOrigin,
        stamp_existing: bool,
    ) -> Result<Session> {
        let mut guard = self.acquire().await;
        // F6h writer gate (protected mode only): no session for a tombstoned
        // label — checked before RM-173's stuck-clear repair writes anything.
        crate::isolation_label_stores::check_label_writable_at(&guard, agent_id).map_err(DuDuClawError::Gateway)?;
        let now = chrono::Utc::now().to_rfc3339();
        let tx = guard
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;
        if let Some(moved) = SessionManager::repair_stuck_clear(&tx, session_id)? {
            tracing::info!(
                session_id, cleared_to = %moved,
                "user clear left at its original id moved before a new turn"
            );
        }
        insert_or_stamp(&tx, session_id, agent_id, origin, stamp_existing, &now)
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let session = select_session(&tx, session_id)?;
        // Verify the session belongs to the expected agent to prevent session
        // hijacking across agents (e.g. two agents racing with the same session_id).
        if session.agent_id != agent_id {
            return Err(DuDuClawError::Gateway(format!(
                "Session '{session_id}' belongs to agent '{}', not '{agent_id}'",
                session.agent_id
            )));
        }
        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
        Ok(session)
    }

    /// The stored origin of `session_id` (`None`: no row, or not stamped yet).
    pub async fn session_origin(&self, session_id: &str) -> Result<Option<SessionOrigin>> {
        let conn = self.acquire().await;
        read_origin(&conn, session_id)
    }
}
