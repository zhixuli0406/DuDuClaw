//! Persistent activation boundaries for read-only team history.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::UserDb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryWindow {
    pub enabled: bool,
    /// Unix microseconds. No content before this activation may be released.
    pub since_micros: Option<i64>,
}

fn meta(conn: &Connection, key: &str) -> Result<Option<i64>, String> {
    conn.query_row("SELECT value FROM auth_meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(|e| e.to_string())
}

impl UserDb {
    /// Persist an observed off period before attempting its audit. If that
    /// append fails, a later enable must first complete the pending disable.
    pub fn require_team_history_reset(&self) -> Result<(), String> {
        self.conn()
            .execute(
                "INSERT INTO auth_meta(key,value) VALUES ('team_history_reset_required',1)
            ON CONFLICT(key) DO UPDATE SET value=1",
                [],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn team_history_reset_required(&self) -> Result<bool, String> {
        match meta(&self.conn(), "team_history_reset_required")? {
            None | Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err("invalid team history reset state".into()),
        }
    }

    pub fn clear_team_history_reset(&self) -> Result<(), String> {
        self.conn()
            .execute(
                "DELETE FROM auth_meta WHERE key='team_history_reset_required'",
                [],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Observe the effective edition/config switch in the same transaction
    /// that advances its activation boundary. A failed audit callback rolls
    /// the transition back. The callback must only record the transition,
    /// never call back into this UserDb while its connection is locked.
    /// This does not authorize the viewer; the gateway checks the live user
    /// and reporting tree independently before releasing any content.
    pub fn observe_team_history<F>(
        &self,
        enabled: bool,
        now_micros: i64,
        audit: F,
    ) -> Result<HistoryWindow, String>
    where
        F: FnOnce(Option<bool>, bool, Option<i64>) -> Result<(), String>,
    {
        if now_micros <= 0 {
            return Err("invalid team history observation time".to_string());
        }
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let previous = match meta(&tx, "team_history_enabled")? {
            None => None,
            Some(0) => Some(false),
            Some(1) => Some(true),
            Some(_) => return Err("invalid team history setting state".to_string()),
        };
        let stored_since = meta(&tx, "team_history_since")?;
        if stored_since.is_some_and(|n| n <= 0)
            || (previous == Some(true) && stored_since.is_none())
        {
            return Err("invalid team history activation boundary".to_string());
        }
        if previous == Some(enabled) {
            let window = HistoryWindow {
                enabled,
                since_micros: enabled.then_some(stored_since).flatten(),
            };
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(window);
        }
        let since = enabled.then(|| now_micros.max(stored_since.unwrap_or(0).saturating_add(1)));
        tx.execute(
            "INSERT INTO auth_meta(key, value) VALUES ('team_history_enabled', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [i64::from(enabled)],
        )
        .map_err(|e| e.to_string())?;
        if let Some(since) = since {
            tx.execute(
                "INSERT INTO auth_meta(key, value) VALUES ('team_history_since', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [since],
            )
            .map_err(|e| e.to_string())?;
        }
        audit(previous, enabled, since)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(HistoryWindow {
            enabled,
            since_micros: since,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::UserDb;

    #[test]
    fn observing_a_setting_retains_the_window_until_reenabled() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = UserDb::new(file.path()).unwrap();
        let mut events = Vec::new();
        let a = db
            .observe_team_history(true, 100, |old, new, since| {
                events.push((old, new, since));
                Ok(())
            })
            .unwrap();
        assert_eq!(a.since_micros, Some(100));
        let b = db
            .observe_team_history(true, 200, |_, _, _| {
                panic!("unchanged setting must not audit twice")
            })
            .unwrap();
        assert_eq!(b.since_micros, Some(100));
        let off = db
            .observe_team_history(false, 300, |old, new, since| {
                events.push((old, new, since));
                Ok(())
            })
            .unwrap();
        assert!(!off.enabled);
        assert_eq!(off.since_micros, None);
        let c = db
            .observe_team_history(true, 400, |old, new, since| {
                events.push((old, new, since));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            c.since_micros,
            Some(400),
            "messages from the earlier window stay outside"
        );
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn failed_activation_audit_rolls_back_the_window() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = UserDb::new(file.path()).unwrap();
        assert!(
            db.observe_team_history(true, 100, |_, _, _| Err("audit unavailable".into()))
                .is_err()
        );
        let window = db
            .observe_team_history(true, 200, |_, _, _| Ok(()))
            .unwrap();
        assert_eq!(window.since_micros, Some(200));
        assert!(
            db.observe_team_history(false, 300, |_, _, _| Err("audit unavailable".into()))
                .is_err()
        );
        let kept = db
            .observe_team_history(true, 400, |_, _, _| panic!("failed disable must roll back"))
            .unwrap();
        assert_eq!(kept.since_micros, Some(200));
    }

    #[test]
    fn malformed_state_and_invalid_time_do_not_enable_visibility() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = UserDb::new(file.path()).unwrap();
        assert!(db.observe_team_history(true, 0, |_, _, _| Ok(())).is_err());
        db.conn()
            .execute(
                "INSERT INTO auth_meta(key, value) VALUES ('team_history_enabled', 7)",
                [],
            )
            .unwrap();
        assert!(
            db.observe_team_history(true, 200, |_, _, _| Ok(()))
                .is_err()
        );
    }
}
