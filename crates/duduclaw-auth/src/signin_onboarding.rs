//! Transactional Google sign-in avatar jobs. These functions grant no creation authority.
use rusqlite::{params, Transaction, TransactionBehavior};
use crate::UserDb;

const STORAGE: &str = "signin_avatar_storage_unavailable";
const STATE: &str = "signin_avatar_wrong_state";

// Keep the read-only button eligibility and the retry mutation in step.
const ADMIN_RETRY_SQL: &str = "source='google_signin' AND avatar='waiting_admin'";

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct SigninAvatarSummary {
    pub state: &'static str,
    pub reason: Option<String>,
    pub retry_allowed: bool,
}

#[derive(Debug, Clone)]
pub struct SigninAvatarJob {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub avatar_state: String,
    pub avatar_agent_name: Option<String>,
    pub attempts: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(state: &str, source: Option<&str>) -> (tempfile::TempDir, UserDb) {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        db.conn().execute("INSERT INTO users(id,email,display_name,password_hash,created_at,updated_at)
            VALUES('member','member@company.test','Member','','now','now')", []).unwrap();
        db.conn().execute("INSERT INTO user_onboarding(user_id,source,avatar) VALUES('member',?1,?2)",
            params![source, state]).unwrap();
        (home, db)
    }

    fn row(db: &UserDb) -> (String, Option<String>, i64, Option<i64>, Option<String>) {
        db.conn().query_row("SELECT avatar,avatar_agent_name,avatar_attempts,avatar_next_at,avatar_error
            FROM user_onboarding WHERE user_id='member'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap()
    }

    #[test]
    fn signin_onboarding_batch_closed_fields_and_retry_match_mutation() {
        for source in [None, Some("other"), Some("google_signin")] {
            for (raw, expected) in [("pending", "pending"), ("reserved", "creating"),
                ("creating", "creating"), ("retry", "retrying"), ("waiting_admin", "waiting_admin"),
                ("done", "done"), ("unexpected", "waiting_admin")] {
                let (_home, db) = fixture(raw, source);
                db.conn().execute("UPDATE user_onboarding SET avatar_error='seats_full'", []).unwrap();
                let summary = db.signin_avatar_summaries().unwrap().remove("member").unwrap();
                assert_eq!(summary.state, expected);
                assert_eq!(summary.reason.as_deref(), Some("seats_full"));
                assert_eq!(summary.retry_allowed, db.signin_avatar_admin_retry("member").is_ok());
            }
        }
        let (_home, db) = fixture("waiting_admin", Some("google_signin"));
        for code in ["done", "name_taken", "policy_changed", "failed", "protected_mode",
            "template_changed", "template_invalid", "seats_full", "not_eligible", "orphan:private-name"] {
            db.conn().execute("UPDATE user_onboarding SET avatar_error=?1", [code]).unwrap();
            let summaries = db.signin_avatar_summaries().unwrap();
            assert_eq!(summaries["member"].reason.as_deref(), Some(if code == "orphan:private-name" { "unknown" } else { code }));
        }
        db.conn().execute("UPDATE user_onboarding SET avatar_error=NULL", []).unwrap();
        assert_eq!(db.signin_avatar_summaries().unwrap()["member"].reason, None);
        db.conn().execute("DELETE FROM user_onboarding", []).unwrap();
        assert!(db.signin_avatar_summaries().unwrap().is_empty());
        db.conn().execute("DROP TABLE user_onboarding", []).unwrap();
        assert!(db.signin_avatar_summaries().is_err());
    }

    #[test]
    fn signin_onboarding_transitions_and_admin_retry() {
        let (_home, db) = fixture("pending", Some("google_signin"));
        db.signin_avatar_reserve("member", "avatar-member-ab12").unwrap();
        assert_eq!(row(&db).0, "reserved");
        assert!(db.signin_avatar_creating("member", "wrong").is_err());
        db.signin_avatar_creating("member", "avatar-member-ab12").unwrap();
        assert_eq!(row(&db).0, "creating");
        db.signin_avatar_wait_admin("member", "protected_mode").unwrap();
        assert_eq!(row(&db).4.as_deref(), Some("protected_mode"));
        db.signin_avatar_admin_retry("member").unwrap();
        assert_eq!(row(&db), ("pending".into(), None, 0, None, None));
        assert_eq!(db.signin_avatar_admin_retry("member").unwrap_err(), "signin_avatar_not_waiting");
        assert!(db.signin_avatar_mark_done_if_present("member").is_err());
        db.conn().execute("UPDATE users SET avatar_agent_id='existing' WHERE id='member'", []).unwrap();
        db.signin_avatar_mark_done_if_present("member").unwrap();
        db.signin_avatar_mark_done_if_present("member").unwrap();
        assert_eq!(row(&db).0, "done");
    }

    #[test]
    fn signin_onboarding_retry_waits_after_three_failures() {
        let (_home, db) = fixture("pending", Some("google_signin"));
        for (attempt, next) in [(1,60),(2,660),(3,4260)] {
            db.signin_avatar_reserve("member", "avatar-member-ab12").unwrap();
            db.signin_avatar_creating("member", "avatar-member-ab12").unwrap();
            db.signin_avatar_retry("member", next, "failed").unwrap();
            assert_eq!(row(&db).2, attempt);
            assert_eq!(row(&db).0, if attempt == 3 { "waiting_admin" } else { "retry" });
            assert_eq!(row(&db).3, if attempt == 3 { None } else { Some(next) });
        }
        assert_eq!(row(&db).4.as_deref(), Some("failed"));
        assert!(db.signin_avatar_retry("member", 9999, "failed").is_err());
    }

    #[test]
    fn signin_onboarding_wrong_states_and_legacy_untouched() {
        for source in [None, Some("other"), Some("google_signin")] {
            for state in ["pending","reserved","creating","retry","waiting_admin","done","unknown"] {
                let (_home, db) = fixture(state, source);
                let before = row(&db);
                if source != Some("google_signin") || !matches!(state, "pending" | "retry") {
                    assert!(db.signin_avatar_reserve("member", "avatar-member-ab12").is_err());
                    assert_eq!(row(&db), before);
                }
                let (_home, db) = fixture(state, source);
                if source != Some("google_signin") || state != "reserved" {
                    assert!(db.signin_avatar_creating("member", "avatar-member-ab12").is_err());
                }
                if source != Some("google_signin") || state != "creating" {
                    assert!(db.signin_avatar_retry("member", 60, "failed").is_err());
                }
                if source != Some("google_signin") || !matches!(state, "pending" | "reserved" | "creating" | "retry") {
                    assert!(db.signin_avatar_wait_admin("member", "seats_full").is_err());
                }
                if source != Some("google_signin") || state != "waiting_admin" {
                    assert_eq!(db.signin_avatar_admin_retry("member").unwrap_err(), "signin_avatar_not_waiting");
                }
                if source != Some("google_signin") || !matches!(state, "reserved" | "creating") {
                    assert!(db.signin_avatar_requeue("member", Some("orphan:old")).is_err());
                }
                if source != Some("google_signin") {
                    assert!(db.signin_avatar_due(0, 20).unwrap().is_empty());
                    db.conn().execute("UPDATE users SET avatar_agent_id='existing' WHERE id='member'", []).unwrap();
                    assert!(db.signin_avatar_mark_done_if_present("member").is_err());
                    assert_eq!(row(&db), before);
                }
            }
        }
    }

    #[test]
    fn signin_onboarding_due_and_completion_ignore_backoff_when_avatar_present() {
        let (_home, db) = fixture("pending", Some("google_signin"));
        assert!(db.signin_avatar_due(0, 0).unwrap().is_empty());
        assert_eq!(db.signin_avatar_due(0, 20).unwrap().len(), 1);
        db.conn().execute("UPDATE user_onboarding SET avatar_next_at=100", []).unwrap();
        assert!(db.signin_avatar_due(99, 20).unwrap().is_empty());
        assert_eq!(db.signin_avatar_due(100, 20).unwrap().len(), 1);
        db.conn().execute("UPDATE users SET status='disabled'", []).unwrap();
        assert!(db.signin_avatar_due(100, 20).unwrap().is_empty());
        db.conn().execute("UPDATE users SET status='active',avatar_agent_id='existing'", []).unwrap();
        db.conn().execute("UPDATE user_onboarding SET avatar='waiting_admin',avatar_next_at=999", []).unwrap();
        assert_eq!(db.signin_avatar_due(0, 20).unwrap().len(), 1);
        db.signin_avatar_mark_done_if_present("member").unwrap();
        assert!(db.signin_avatar_due(1000, 20).unwrap().is_empty());
    }

    #[test]
    fn signin_onboarding_requeue_retains_orphan_without_counting_failure() {
        let (_home, db) = fixture("pending", Some("google_signin"));
        db.signin_avatar_reserve("member", "old").unwrap();
        db.signin_avatar_creating("member", "old").unwrap();
        db.signin_avatar_requeue("member", Some("orphan:old")).unwrap();
        assert_eq!(row(&db), ("pending".into(), None, 0, None, Some("orphan:old".into())));
        db.signin_avatar_reserve("member", "new").unwrap();
        db.signin_avatar_creating("member", "new").unwrap();
        db.signin_avatar_requeue("member", None).unwrap();
        assert_eq!(row(&db).4.as_deref(), Some("orphan:old"));
    }
}

impl UserDb {
    pub fn signin_avatar_summaries(&self) -> Result<std::collections::HashMap<String, SigninAvatarSummary>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT user_id,avatar,avatar_error,
            CASE WHEN {ADMIN_RETRY_SQL} THEN 1 ELSE 0 END FROM user_onboarding"))
            .map_err(|_| STORAGE)?;
        let rows = stmt.query_map([], |r| {
            let raw: Option<String> = r.get(1)?;
            let error: Option<String> = r.get(2)?;
            let state = match raw.as_deref() {
                Some("pending") => "pending",
                Some("reserved" | "creating") => "creating",
                Some("retry") => "retrying",
                Some("done") => "done",
                _ => "waiting_admin",
            };
            let reason = error.map(|code| match code.as_str() {
                "done" | "name_taken" | "policy_changed" | "failed" | "protected_mode" |
                "template_changed" | "template_invalid" | "seats_full" | "not_eligible" => code,
                _ => "unknown".to_owned(),
            });
            Ok((r.get(0)?, SigninAvatarSummary { state, reason, retry_allowed: r.get(3)? }))
        }).map_err(|_| STORAGE)?;
        rows.collect::<rusqlite::Result<_>>().map_err(|_| STORAGE.to_owned())
    }

    fn signin_avatar_transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, String>) -> Result<T, String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        let value = f(&tx)?;
        tx.commit().map_err(|_| STORAGE)?;
        Ok(value)
    }

    pub fn signin_avatar_due(&self, now: i64, limit: usize) -> Result<Vec<SigninAvatarJob>, String> {
        let limit = i64::try_from(limit).map_err(|_| STORAGE)?;
        self.signin_avatar_transaction(|tx| {
            let mut query = tx.prepare("SELECT u.id,u.email,u.display_name,o.avatar,o.avatar_agent_name,o.avatar_attempts
                FROM user_onboarding o JOIN users u ON u.id=o.user_id
                WHERE o.source='google_signin' AND u.status='active' AND u.role='employee' AND (
                    (u.avatar_agent_id IS NULL AND o.avatar IN ('pending','reserved','creating','retry')
                        AND (o.avatar_next_at IS NULL OR o.avatar_next_at<=?1))
                    OR (u.avatar_agent_id IS NOT NULL AND o.avatar IS NOT 'done'))
                ORDER BY COALESCE(o.avatar_next_at,0),u.id LIMIT ?2").map_err(|_| STORAGE)?;
            let rows = query.query_map(params![now, limit], |r| Ok(SigninAvatarJob {
                user_id: r.get(0)?, email: r.get(1)?, display_name: r.get(2)?,
                avatar_state: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                avatar_agent_name: r.get(4)?, attempts: r.get(5)?,
            })).map_err(|_| STORAGE)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| STORAGE.to_owned())
        })
    }

    pub fn signin_avatar_reserve(&self, user_id: &str, agent_name: &str) -> Result<(), String> {
        self.signin_avatar_transaction(|tx| {
            if !duduclaw_core::is_valid_agent_id(agent_name) { return Err("signin_avatar_invalid_name".into()); }
            changed(tx.execute("UPDATE user_onboarding SET avatar='reserved',avatar_agent_name=?2,avatar_next_at=NULL
                WHERE user_id=?1 AND source='google_signin' AND avatar IN ('pending','retry')",
                params![user_id, agent_name]), STATE)
        })
    }

    pub fn signin_avatar_creating(&self, user_id: &str, agent_name: &str) -> Result<(), String> {
        self.signin_avatar_transaction(|tx| changed(tx.execute("UPDATE user_onboarding SET avatar='creating'
            WHERE user_id=?1 AND source='google_signin' AND avatar='reserved' AND avatar_agent_name=?2",
            params![user_id, agent_name]), STATE))
    }

    pub fn signin_avatar_retry(&self, user_id: &str, next_at: i64, error_code: &str) -> Result<(), String> {
        self.signin_avatar_transaction(|tx| changed(tx.execute("UPDATE user_onboarding SET
            avatar_attempts=avatar_attempts+1,
            avatar=CASE WHEN avatar_attempts+1>=3 THEN 'waiting_admin' ELSE 'retry' END,
            avatar_next_at=CASE WHEN avatar_attempts+1>=3 THEN NULL ELSE ?2 END,avatar_error=?3
            WHERE user_id=?1 AND source='google_signin' AND avatar='creating'",
            params![user_id, next_at, error_code]), STATE))
    }

    pub fn signin_avatar_wait_admin(&self, user_id: &str, reason_code: &str) -> Result<(), String> {
        self.signin_avatar_transaction(|tx| changed(tx.execute("UPDATE user_onboarding SET
            avatar='waiting_admin',avatar_next_at=NULL,avatar_error=?2
            WHERE user_id=?1 AND source='google_signin' AND avatar IN ('pending','reserved','creating','retry')",
            params![user_id, reason_code]), STATE))
    }

    pub fn signin_avatar_mark_done_if_present(&self, user_id: &str) -> Result<(), String> {
        self.signin_avatar_transaction(|tx| changed(tx.execute("UPDATE user_onboarding SET
            avatar='done',avatar_next_at=NULL,
            avatar_error=CASE WHEN substr(avatar_error,1,7)='orphan:' THEN avatar_error ELSE NULL END
            WHERE user_id=?1 AND source='google_signin'
                AND EXISTS(SELECT 1 FROM users WHERE id=?1 AND avatar_agent_id IS NOT NULL)",
            [user_id]), STATE))
    }

    pub fn signin_avatar_admin_retry(&self, user_id: &str) -> Result<(), String> {
        self.signin_avatar_transaction(|tx| changed(tx.execute(&format!("UPDATE user_onboarding SET
            avatar='pending',avatar_attempts=0,avatar_next_at=NULL,avatar_error=NULL,avatar_agent_name=NULL
            WHERE user_id=?1 AND {ADMIN_RETRY_SQL}"),
            [user_id]), "signin_avatar_not_waiting"))
    }

    /// A collision/policy change is not a failed creation attempt. Preserve
    /// orphan evidence while releasing the reservation for a fresh name.
    pub fn signin_avatar_requeue(&self, user_id: &str, orphan: Option<&str>) -> Result<(), String> {
        self.signin_avatar_transaction(|tx| changed(tx.execute("UPDATE user_onboarding SET
            avatar='pending',avatar_agent_name=NULL,avatar_next_at=NULL,avatar_error=COALESCE(?2,avatar_error)
            WHERE user_id=?1 AND source='google_signin' AND avatar IN ('reserved','creating')",
            params![user_id, orphan]), STATE))
    }
}

fn changed(result: rusqlite::Result<usize>, wrong_state: &str) -> Result<(), String> {
    match result {
        Ok(1) => Ok(()),
        Ok(_) => Err(wrong_state.to_owned()),
        Err(_) => Err(STORAGE.to_owned()),
    }
}
