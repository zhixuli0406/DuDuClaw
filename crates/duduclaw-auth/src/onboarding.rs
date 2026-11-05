//! First-login checklist: progress is per human and grants no capabilities.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use crate::{UserDb, UserStatus};

pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS user_onboarding (
        user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        dismissed INTEGER NOT NULL DEFAULT 0,
        channel TEXT NOT NULL DEFAULT 'pending',
        google TEXT NOT NULL DEFAULT 'pending',
        example TEXT NOT NULL DEFAULT 'pending',
        capabilities TEXT NOT NULL DEFAULT 'pending'
    );").map_err(|_| "onboarding_unavailable".to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus { Pending, Done, Skipped }
impl StepStatus {
    fn parse(s: String) -> rusqlite::Result<Self> {
        match s.as_str() {
            "pending" => Ok(Self::Pending), "done" => Ok(Self::Done), "skipped" => Ok(Self::Skipped),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
    fn as_str(self) -> &'static str {
        match self { Self::Pending => "pending", Self::Done => "done", Self::Skipped => "skipped" }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Steps { pub channel: StepStatus, pub google: StepStatus, pub example: StepStatus, pub capabilities: StepStatus }
#[derive(Debug, Clone, Serialize)]
pub struct Progress { pub enabled: bool, pub dismissed: bool, pub password_done: bool, pub steps: Steps }
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressUpdate { pub step: Option<String>, pub status: Option<StepStatus>, pub dismissed: Option<bool> }

fn live_user(conn: &Connection, id: &str, epoch: u64) -> Result<crate::User, String> {
    let user = crate::user_row::load_user(conn, id)?.ok_or("permission_denied")?;
    if user.status != UserStatus::Active || user.must_change_password || user.credential_epoch != epoch { return Err("permission_denied".into()); }
    Ok(user)
}

fn read(conn: &Connection, id: &str, password_done: bool) -> Result<Progress, String> {
    let row = conn.query_row("SELECT dismissed,channel,google,example,capabilities FROM user_onboarding WHERE user_id=?1", [id], |r| {
        Ok(Progress { enabled: true, dismissed: r.get(0)?, password_done, steps: Steps {
            channel: StepStatus::parse(r.get(1)?)?, google: StepStatus::parse(r.get(2)?)?,
            example: StepStatus::parse(r.get(3)?)?, capabilities: StepStatus::parse(r.get(4)?)?,
        }})
    }).optional().map_err(|_| "onboarding_unavailable".to_owned())?;
    Ok(row.unwrap_or(Progress { enabled: false, dismissed: false, password_done, steps: Steps {
        channel: StepStatus::Pending, google: StepStatus::Pending, example: StepStatus::Pending, capabilities: StepStatus::Pending,
    }}))
}

impl UserDb {
    pub fn onboarding_progress(&self, id: &str, epoch: u64) -> Result<Progress, String> {
        let conn = self.conn();
        let user = live_user(&conn, id, epoch)?;
        let has_password: bool = conn.query_row("SELECT password_hash != '' FROM users WHERE id=?1", [id], |r| r.get(0))
            .map_err(|_| "onboarding_unavailable".to_owned())?;
        read(&conn, id, !user.must_change_password && has_password)
    }

    pub fn update_onboarding(&self, id: &str, epoch: u64, update: &ProgressUpdate) -> Result<Progress, String> {
        let step = match (update.step.as_deref(), update.status, update.dismissed) {
            (None, None, Some(true)) => None,
            (Some("channel" | "google"), Some(StepStatus::Skipped), None) => update.step.as_deref(),
            (Some("example" | "capabilities"), Some(StepStatus::Done), None) => update.step.as_deref(),
            _ => return Err("invalid_onboarding_update".into()),
        };
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| "onboarding_unavailable".to_owned())?;
        live_user(&tx, id, epoch)?;
        // The column is chosen by the exact allowlist above, never user SQL.
        let changed = if let Some(step) = step {
            tx.execute(&format!("UPDATE user_onboarding SET {step}=?2 WHERE user_id=?1"), params![id, update.status.unwrap().as_str()])
        } else { tx.execute("UPDATE user_onboarding SET dismissed=1 WHERE user_id=?1", [id]) }
            .map_err(|_| "onboarding_unavailable".to_owned())?;
        if changed != 1 { return Err("onboarding_not_enabled".into()); }
        tx.execute("INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp) VALUES(?1,'onboarding.updated',?1,?2,?3)",
            params![id, step.unwrap_or("dismissed"), chrono::Utc::now().to_rfc3339()]).map_err(|_| "onboarding_unavailable".to_owned())?;
        tx.commit().map_err(|_| "onboarding_unavailable".to_owned())?;
        drop(conn);
        self.onboarding_progress(id, epoch)
    }

    /// Connection completion is observed by the gateway, never claimed by a client.
    pub fn observe_onboarding_connections(&self, id: &str, epoch: u64, google: bool) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| "onboarding_unavailable".to_owned())?;
        live_user(&tx, id, epoch)?;
        tx.execute("UPDATE user_onboarding SET
            channel=CASE WHEN EXISTS(SELECT 1 FROM channel_identities WHERE user_id=?1 AND verified=1) THEN 'done' ELSE channel END,
            google=CASE WHEN ?2 THEN 'done' ELSE google END WHERE user_id=?1", params![id, google])
            .map_err(|_| "onboarding_unavailable".to_owned())?;
        tx.commit().map_err(|_| "onboarding_unavailable".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn onboarding_is_per_user_persistent_and_rejects_forged_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        let a = db.create_user("a@test.invalid", "A", "password123", crate::UserRole::Employee).unwrap();
        let b = db.create_user("b@test.invalid", "B", "password123", crate::UserRole::Employee).unwrap();
        assert!(!db.onboarding_progress(&a.id, a.credential_epoch).unwrap().enabled);
        db.conn().execute("INSERT INTO user_onboarding(user_id) VALUES(?1)", [&a.id]).unwrap();
        let skip = ProgressUpdate { step: Some("channel".into()), status: Some(StepStatus::Skipped), dismissed: None };
        assert!(db.update_onboarding(&a.id, a.credential_epoch + 1, &skip).is_err());
        db.update_onboarding(&a.id, a.credential_epoch, &skip).unwrap();
        assert!(db.update_onboarding(&b.id, b.credential_epoch, &skip).is_err());
        let forged = ProgressUpdate { step: Some("google".into()), status: Some(StepStatus::Done), dismissed: None };
        assert!(db.update_onboarding(&a.id, a.credential_epoch, &forged).is_err());
        assert!(!db.onboarding_progress(&b.id, b.credential_epoch).unwrap().enabled);
        drop(db);
        let db = UserDb::new(&path).unwrap();
        assert_eq!(db.onboarding_progress(&a.id, a.credential_epoch).unwrap().steps.channel, StepStatus::Skipped);
        db.observe_onboarding_connections(&a.id, a.credential_epoch, true).unwrap();
        assert_eq!(db.onboarding_progress(&a.id, a.credential_epoch).unwrap().steps.google, StepStatus::Done);
        let dismiss = ProgressUpdate { step: None, status: None, dismissed: Some(true) };
        assert!(db.update_onboarding(&a.id, a.credential_epoch, &dismiss).unwrap().dismissed);
    }
}
