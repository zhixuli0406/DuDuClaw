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

    /// RM-173: an Admin moves the activation boundary of an ENABLED window.
    /// Moving it earlier exposes older conversations, so the gateway requires
    /// a fresh step-up for that direction before calling this; this layer only
    /// guarantees the transition is atomic with its audit: `audit(old, new)`
    /// runs inside the same IMMEDIATE transaction and a failure rolls the
    /// change back. Unchanged ⇒ `Ok(old)` without auditing. Returns the old
    /// boundary. Refuses an off window or a pending reset (a later re-enable
    /// starts a fresh window at "now" anyway).
    ///
    /// `actor_user_id` must be, inside this same transaction, an Active Admin
    /// without a pending forced password change — for ANY direction. The
    /// shared-token `system` context (no user row) is always refused, so a
    /// cached role or a demotion racing the request can never move it.
    pub fn set_team_history_since<F>(
        &self,
        actor_user_id: &str,
        since_micros: i64,
        audit: F,
    ) -> Result<i64, String>
    where
        F: FnOnce(i64, i64) -> Result<(), String>,
    {
        if since_micros <= 0 {
            return Err("invalid team history boundary".to_string());
        }
        if actor_user_id.is_empty() || actor_user_id == "system" {
            return Err("permission denied".to_string());
        }
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let live_admin = crate::user_row::load_user(&tx, actor_user_id)?.is_some_and(|u| {
            u.status == crate::models::UserStatus::Active
                && u.role == crate::models::UserRole::Admin
                && !u.must_change_password
        });
        if !live_admin {
            return Err("permission denied".to_string());
        }
        if matches!(meta(&tx, "team_history_reset_required")?, Some(n) if n != 0) {
            return Err("team history is off".to_string());
        }
        if meta(&tx, "team_history_enabled")? != Some(1) {
            return Err("team history is off".to_string());
        }
        let old = match meta(&tx, "team_history_since")? {
            Some(n) if n > 0 => n,
            _ => return Err("invalid team history activation boundary".to_string()),
        };
        if old == since_micros {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(old);
        }
        tx.execute(
            "UPDATE auth_meta SET value = ?1 WHERE key = 'team_history_since'",
            [since_micros],
        )
        .map_err(|e| e.to_string())?;
        audit(old, since_micros)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(old)
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

    fn admin(db: &UserDb) -> String {
        db.create_user("boss@test.invalid", "boss", "isolated-test-password", crate::UserRole::Admin)
            .unwrap()
            .id
    }

    #[test]
    fn rm173_boundary_moves_only_on_an_enabled_window_and_rolls_back_with_audit() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = UserDb::new(file.path()).unwrap();
        let a = admin(&db);
        assert!(db.set_team_history_since(&a, 50, |_, _| Ok(())).is_err(), "off window");
        db.observe_team_history(true, 100, |_, _, _| Ok(())).unwrap();
        assert!(db.set_team_history_since(&a, 0, |_, _| Ok(())).is_err());
        assert!(db.set_team_history_since(&a, 40, |_, _| Err("audit unavailable".into())).is_err());
        let kept = db.observe_team_history(true, 500, |_, _, _| panic!("no transition")).unwrap();
        assert_eq!(kept.since_micros, Some(100), "failed audit rolls the change back");
        let mut seen = None;
        assert_eq!(db.set_team_history_since(&a, 40, |old, new| { seen = Some((old, new)); Ok(()) }).unwrap(), 100);
        assert_eq!(seen, Some((100, 40)));
        assert_eq!(db.set_team_history_since(&a, 40, |_, _| panic!("unchanged must not audit")).unwrap(), 40);
        let now = db.observe_team_history(true, 600, |_, _, _| panic!("no transition")).unwrap();
        assert_eq!(now.since_micros, Some(40));
        db.require_team_history_reset().unwrap();
        assert!(db.set_team_history_since(&a, 30, |_, _| Ok(())).is_err(), "pending reset");
    }

    #[test]
    fn rm173_boundary_move_requires_a_live_admin_in_the_transaction() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = UserDb::new(file.path()).unwrap();
        db.observe_team_history(true, 100, |_, _, _| Ok(())).unwrap();
        let mgr = db
            .create_user("m@test.invalid", "m", "isolated-test-password", crate::UserRole::Manager)
            .unwrap()
            .id;
        for actor in ["system", "", "no-such-user", mgr.as_str()] {
            // Either direction, and the audit never runs.
            for since in [200, 50] {
                assert_eq!(
                    db.set_team_history_since(actor, since, |_, _| panic!("refused before audit")),
                    Err("permission denied".to_string()),
                    "{actor:?}"
                );
            }
        }
        let kept = db.observe_team_history(true, 500, |_, _, _| panic!("no transition")).unwrap();
        assert_eq!(kept.since_micros, Some(100));
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
