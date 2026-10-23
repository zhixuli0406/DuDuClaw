//! The human facts behind personal Google identity. Credentials stay in
//! the gateway; this database holds only Owner bindings and self-consent.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::user_row::{bump_acl_generation, has_column, load_user};
#[cfg(test)]
use crate::{AccessLevel, UserRole};
use crate::{User, UserDb, UserStatus};

pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    if !has_column(conn, "user_agent_bindings", "is_primary")? {
        conn.execute("ALTER TABLE user_agent_bindings ADD COLUMN is_primary INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1))", [])
            .map_err(|e| format!("primary owner migration: {e}"))?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_bindings_primary ON user_agent_bindings(agent_name) WHERE is_primary = 1;
         CREATE TRIGGER IF NOT EXISTS google_primary_owner_insert BEFORE INSERT ON user_agent_bindings
           WHEN NEW.is_primary = 1 AND NEW.access_level != 'owner' BEGIN SELECT RAISE(ABORT, 'primary binding must be owner'); END;
         CREATE TRIGGER IF NOT EXISTS google_primary_owner_update BEFORE UPDATE ON user_agent_bindings
           WHEN NEW.is_primary = 1 AND NEW.access_level != 'owner' BEGIN SELECT RAISE(ABORT, 'primary binding must be owner'); END;
         CREATE TABLE IF NOT EXISTS google_identity_consents (
           user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
           agent_name TEXT NOT NULL,
           state TEXT NOT NULL CHECK (state IN ('allowed', 'declined')),
           decided_at TEXT NOT NULL,
           decided_by TEXT NOT NULL,
           CHECK (decided_by = user_id),
           PRIMARY KEY (user_id, agent_name)
         );"
    ).map_err(|e| format!("Google consent migration: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerError {
    Missing,
    Ambiguous,
    Store(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct GoogleConsent {
    pub agent_name: String,
    pub allowed: bool,
    pub decided_at: String,
}

impl UserDb {
    /// Owner selection is one database snapshot; no arbitrary first row.
    pub fn google_owner(&self, agent: &str) -> Result<User, OwnerError> {
        let mut conn = self.conn();
        let tx = conn
            .transaction()
            .map_err(|e| OwnerError::Store(e.to_string()))?;
        let mut statement = tx.prepare(
            "SELECT user_id, is_primary FROM user_agent_bindings WHERE agent_name = ?1 AND access_level = 'owner'"
        ).map_err(|e| OwnerError::Store(e.to_string()))?;
        let owners = statement
            .query_map([agent], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            })
            .map_err(|e| OwnerError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OwnerError::Store(e.to_string()))?;
        let user_id = match owners.as_slice() {
            [] => return Err(OwnerError::Missing),
            [(id, _)] => id,
            _ => {
                &owners
                    .iter()
                    .find(|(_, primary)| *primary)
                    .ok_or(OwnerError::Ambiguous)?
                    .0
            }
        };
        load_user(&tx, user_id)
            .map_err(OwnerError::Store)?
            .ok_or(OwnerError::Missing)
    }

    /// Gateway Admin route only. The selected binding must be an active
    /// Owner; the replacement and ACL invalidation commit atomically.
    pub fn set_google_primary_owner(
        &self,
        agent: &str,
        user_id: &str,
        actor: &str,
    ) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let owner: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM user_agent_bindings b JOIN users u ON u.id = b.user_id
             WHERE b.agent_name = ?1 AND b.user_id = ?2 AND b.access_level = 'owner' AND u.status = 'active')",
            params![agent, user_id], |r| r.get(0)
        ).map_err(|e| e.to_string())?;
        if !owner {
            return Err("主要負責人必須是這位員工的有效擁有者。".into());
        }
        tx.execute(
            "UPDATE user_agent_bindings SET is_primary = 0 WHERE agent_name = ?1",
            [agent],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE user_agent_bindings SET is_primary = 1 WHERE agent_name = ?1 AND user_id = ?2",
            params![agent, user_id],
        )
        .map_err(|e| e.to_string())?;
        bump_acl_generation(&tx).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp) VALUES (?1, 'agent.primary_owner_set', ?2, ?3, ?4)",
            params![actor, agent, serde_json::json!({"primary_user_id": user_id}).to_string(), chrono::Utc::now().to_rfc3339()]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn google_consent(&self, user_id: &str, agent: &str) -> Result<Option<bool>, String> {
        let state: Option<String> = self.conn().query_row(
            "SELECT state FROM google_identity_consents WHERE user_id = ?1 AND agent_name = ?2 AND decided_by = user_id",
            params![user_id, agent], |r| r.get(0)
        ).optional().map_err(|e| e.to_string())?;
        match state.as_deref() {
            Some("allowed") => Ok(Some(true)),
            Some("declined") => Ok(Some(false)),
            None => Ok(None),
            _ => Err("同意紀錄無法辨識。".into()),
        }
    }

    /// This invariant is checked in code and by the SQL CHECK constraint;
    /// an Admin may ask for consent but cannot grant it for somebody else.
    pub fn set_google_consent(
        &self,
        user_id: &str,
        agent: &str,
        allowed: bool,
        decided_by: &str,
    ) -> Result<(), String> {
        if user_id != decided_by {
            return Err("只有帳號本人可以同意或拒絕。".into());
        }
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let user = load_user(&tx, user_id)?
            .filter(|u| u.status == UserStatus::Active)
            .ok_or("帳號不存在或已停用。")?;
        if allowed {
            let owned: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM user_agent_bindings WHERE user_id = ?1 AND agent_name = ?2 AND access_level = 'owner')",
                params![user.id, agent], |r| r.get(0)
            ).map_err(|e| e.to_string())?;
            if !owned {
                return Err("你必須是這位員工的擁有者，才能開放自己的帳號。".into());
            }
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO google_identity_consents(user_id, agent_name, state, decided_at, decided_by) VALUES (?1, ?2, ?3, ?4, ?1)
             ON CONFLICT(user_id, agent_name) DO UPDATE SET state = excluded.state, decided_at = excluded.decided_at, decided_by = excluded.decided_by",
            params![user_id, agent, if allowed {"allowed"} else {"declined"}, now]
        ).map_err(|e| e.to_string())?;
        bump_acl_generation(&tx).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp) VALUES (?1, 'user.google_consent', ?2, ?3, ?4)",
            params![user_id, agent, serde_json::json!({"allowed": allowed}).to_string(), now]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn google_consents(&self, user_id: &str) -> Result<Vec<GoogleConsent>, String> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT agent_name, state, decided_at FROM google_identity_consents WHERE user_id = ?1 AND decided_by = user_id ORDER BY agent_name")
            .map_err(|e| e.to_string())?;
        statement
            .query_map([user_id], |r| {
                let state: String = r.get(1)?;
                if state != "allowed" && state != "declined" {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok(GoogleConsent {
                    agent_name: r.get(0)?,
                    allowed: state == "allowed",
                    decided_at: r.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[path = "google_identity_tests.rs"]
mod tests;
