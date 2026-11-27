//! Authoritative member avatar assignments; dashboard read policy lives in the gateway.
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use crate::{UserDb, user_row::{bump_acl_generation, has_column}};

#[derive(Debug, serde::Serialize)]
pub struct AvatarConflict {
    pub agent_id: String,
    pub user_ids: Vec<String>,
}

/// Authority for publishing a new Personal avatar; never accepted from client JSON.
pub enum AvatarActor<'a> {
    Admin(&'a str),
    SigninPolicy {
        revision: &'a str,
        /// Re-read strict gateway settings and require sign-in enabled at this
        /// revision. Runs under the user DB transaction; must not re-enter it.
        policy_check: &'a dyn Fn() -> Result<(), String>,
    },
}

/// The column and backfill commit together. Reopening never assigns an avatar
/// that an administrator deliberately cleared after the upgrade.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    if !has_column(&tx, "users", "avatar_agent_id")? {
        tx.execute("ALTER TABLE users ADD COLUMN avatar_agent_id TEXT", []).map_err(|e| e.to_string())?;
        tx.execute("UPDATE users SET avatar_agent_id = primary_agent_id
            WHERE status = 'active' AND primary_agent_id IS NOT NULL
            AND (SELECT COUNT(*) FROM users other WHERE other.status = 'active'
                 AND other.primary_agent_id = users.primary_agent_id) = 1
            AND EXISTS(SELECT 1 FROM user_agent_bindings b WHERE b.user_id = users.id
                 AND b.agent_name = users.primary_agent_id AND b.access_level IN ('owner', 'operator'))",
            [],
        ).map_err(|e| e.to_string())?;
    }
    tx.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_users_avatar
        ON users(avatar_agent_id) WHERE avatar_agent_id IS NOT NULL", []).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Resolve a CURRENT avatar from RM-212 authority, never from a profile flag or
/// the legacy primary preference. Callers must fall back to stricter rules on
/// errors. A Personal avatar must belong to the same member as its owner.
pub fn current_personal_avatar_member_in(conn: &Connection, agent: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT u.id FROM users u
        JOIN isolation_personal_agents p ON p.agent_name = u.avatar_agent_id AND p.owner_user_id = u.id
        WHERE u.avatar_agent_id = ?1 AND u.status = 'active'
        AND (SELECT COUNT(*) FROM users a WHERE a.avatar_agent_id = ?1) = 1
        AND EXISTS(SELECT 1 FROM user_agent_bindings b WHERE b.user_id = u.id
            AND b.agent_name = ?1 AND b.access_level = 'owner')",
        [agent], |r| r.get(0)).optional()
}

impl UserDb {
    pub fn avatar_agent(&self, user_id: &str) -> Result<Option<String>, String> {
        self.conn().query_row("SELECT avatar_agent_id FROM users WHERE id = ?1", [user_id], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    /// Prefer a valid bound avatar, then exactly the previous bound-primary
    /// rule. A failed read is an error; callers must refuse instead of guessing.
    pub fn member_avatar_or_primary(&self, user_id: &str) -> Result<Option<String>, String> {
        self.conn().query_row("SELECT CASE
            WHEN avatar_agent_id IS NOT NULL AND EXISTS(SELECT 1 FROM user_agent_bindings b
                WHERE b.user_id = users.id AND b.agent_name = avatar_agent_id
                AND b.access_level IN ('owner', 'operator')) THEN avatar_agent_id
            WHEN EXISTS(SELECT 1 FROM user_agent_bindings b
                WHERE b.user_id = users.id AND b.agent_name = primary_agent_id
                AND b.access_level IN ('owner', 'operator')) THEN primary_agent_id
            ELSE NULL END FROM users WHERE id = ?1 AND status = 'active'",
            [user_id], |r| r.get(0),
        ).optional().map(|v| v.flatten()).map_err(|e| e.to_string())
    }

    /// The gateway also checks that the agent exists in its live registry.
    /// This write never creates or elevates a binding.
    pub fn set_avatar_as(&self, actor: &str, user_id: &str, agent_id: Option<&str>) -> Result<(), String> {
        self.set_avatar_as_under(actor, user_id, agent_id, None)
    }

    pub fn set_avatar_as_under(&self, actor: &str, user_id: &str, agent_id: Option<&str>, limit: crate::avatar_seats::SeatLimit<'_>) -> Result<(), String> {
        let seats = crate::avatar_seats::SeatChange::begin(self, limit)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let admin: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users
            WHERE id = ?1 AND role = 'admin' AND status = 'active')", [actor], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if !admin { return Err("avatar_admin_required".into()); }
        if let Some(agent) = agent_id { check_available(&tx, user_id, agent)?; }
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute("UPDATE users SET avatar_agent_id = ?1,
            primary_agent_id = CASE WHEN ?1 IS NULL THEN primary_agent_id ELSE ?1 END,
            updated_at = ?2 WHERE id = ?3 AND status = 'active'
            AND (?1 IS NULL OR EXISTS(SELECT 1 FROM user_agent_bindings b
                WHERE b.user_id = users.id AND b.agent_name = ?1 AND b.access_level IN ('owner', 'operator')))",
            params![agent_id, now, user_id],
        ).map_err(|e| e.to_string())?;
        if changed != 1 { return Err("avatar_binding_required".into()); }
        tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp)
            VALUES(?1, 'user.set_avatar', ?2, ?3, ?4)",
            params![actor, user_id, serde_json::json!({"avatar_agent_id": agent_id}).to_string(), now],
        ).map_err(|e| e.to_string())?;
        bump_acl_generation(&tx).map_err(|e| e.to_string())?;
        seats.check(&tx)?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn avatar_conflicts(&self) -> Result<Vec<AvatarConflict>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT primary_agent_id, id FROM users
            WHERE status = 'active' AND primary_agent_id IS NOT NULL
            AND primary_agent_id IN (SELECT primary_agent_id FROM users WHERE status = 'active'
                AND primary_agent_id IS NOT NULL GROUP BY primary_agent_id HAVING COUNT(*) > 1)
            AND primary_agent_id IN (SELECT primary_agent_id FROM users WHERE status = 'active'
                AND avatar_agent_id IS NULL)
            ORDER BY primary_agent_id, id").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut conflicts: Vec<AvatarConflict> = Vec::new();
        for row in rows {
            let (agent_id, user_id) = row.map_err(|e| e.to_string())?;
            if let Some(last) = conflicts.last_mut().filter(|c| c.agent_id == agent_id) {
                last.user_ids.push(user_id);
            } else { conflicts.push(AvatarConflict { agent_id, user_ids: vec![user_id] }); }
        }
        Ok(conflicts)
    }
}

pub(crate) fn check_available(conn: &Connection, user_id: &str, agent: &str) -> Result<(), String> {
    let taken: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM users
        WHERE avatar_agent_id = ?1 AND id <> ?2)", params![agent, user_id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if taken { Err("avatar_agent_in_use".into()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessLevel, User, UserRole};

    fn legacy() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE users(id TEXT PRIMARY KEY, status TEXT, primary_agent_id TEXT);
            CREATE TABLE user_agent_bindings(user_id TEXT, agent_name TEXT, access_level TEXT);").unwrap();
        conn
    }
    fn legacy_user(conn: &Connection, id: &str, primary: Option<&str>, level: &str) {
        conn.execute("INSERT INTO users VALUES(?1, 'active', ?2)", params![id, primary]).unwrap();
        if let Some(agent) = primary {
            conn.execute("INSERT INTO user_agent_bindings VALUES(?1, ?2, ?3)", params![id, agent, level]).unwrap();
        }
    }
    fn avatar(conn: &Connection, id: &str) -> Option<String> {
        conn.query_row("SELECT avatar_agent_id FROM users WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }
    #[test]
    fn migration_without_primary_is_idempotent() {
        let conn = legacy();
        legacy_user(&conn, "no-primary", None, "operator");
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(avatar(&conn, "no-primary"), None);
    }
    #[test]
    fn migration_backfills_only_single_active_bound_primary_once() {
        let conn = legacy();
        legacy_user(&conn, "single", Some("solo"), "operator");
        legacy_user(&conn, "viewer", Some("read-only"), "viewer");
        legacy_user(&conn, "unbound", Some("missing"), "operator");
        conn.execute("DELETE FROM user_agent_bindings WHERE user_id = 'unbound'", []).unwrap();
        legacy_user(&conn, "inactive", Some("solo"), "operator");
        conn.execute("UPDATE users SET status = 'suspended' WHERE id = 'inactive'", []).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(avatar(&conn, "single").as_deref(), Some("solo"));
        for id in ["viewer", "unbound", "inactive"] { assert_eq!(avatar(&conn, id), None); }
        conn.execute("UPDATE users SET avatar_agent_id = NULL WHERE id = 'single'", []).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(avatar(&conn, "single"), None, "do not restore a deliberately cleared avatar");
    }
    #[test]
    fn migration_shared_primary_succeeds_and_leaves_both_unassigned() {
        let conn = legacy();
        legacy_user(&conn, "a", Some("shared"), "operator");
        legacy_user(&conn, "b", Some("shared"), "operator");
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(avatar(&conn, "a"), None);
        assert_eq!(avatar(&conn, "b"), None);
        conn.execute("UPDATE users SET avatar_agent_id = 'shared' WHERE id = 'a'", []).unwrap();
        assert!(conn.execute("UPDATE users SET avatar_agent_id = 'shared' WHERE id = 'b'", []).is_err());
    }

    fn fixture() -> (tempfile::TempDir, UserDb, User, User, User) {
        let dir = tempfile::tempdir().unwrap();
        let db = UserDb::new(&dir.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@avatar.test", "Admin", "password-123", UserRole::Admin).unwrap();
        let a = db.create_user("a@avatar.test", "A", "password-123", UserRole::Employee).unwrap();
        let b = db.create_user("b@avatar.test", "B", "password-123", UserRole::Employee).unwrap();
        for user in [&a, &b] {
            db.bind_agent(&user.id, "shared", AccessLevel::Operator).unwrap();
            db.bind_agent(&user.id, "other", AccessLevel::Operator).unwrap();
        }
        (dir, db, admin, a, b)
    }
    #[test]
    fn set_avatar_requires_admin_and_enforces_unique_binding_and_sync() {
        let (_dir, db, admin, a, b) = fixture();
        assert_eq!(db.set_avatar_as(&a.id, &a.id, Some("shared")).unwrap_err(), "avatar_admin_required");
        assert_eq!(db.set_avatar_as(&admin.id, &a.id, Some("unbound")).unwrap_err(), "avatar_binding_required");
        let generation = db.acl_generation().unwrap();
        db.set_avatar_as(&admin.id, &a.id, Some("shared")).unwrap();
        assert!(db.acl_generation().unwrap() > generation);
        assert_eq!(db.get_user(&a.id).unwrap().unwrap().avatar_agent_id.as_deref(), Some("shared"));
        assert_eq!(db.configured_primary_agent(&a.id).unwrap().as_deref(), Some("shared"));
        let generation = db.acl_generation().unwrap();
        assert_eq!(db.set_avatar_as(&admin.id, &b.id, Some("shared")).unwrap_err(), "avatar_agent_in_use");
        assert_eq!(db.acl_generation().unwrap(), generation);
        assert_eq!(db.avatar_agent(&b.id).unwrap(), None);
        db.set_avatar_as(&admin.id, &a.id, None).unwrap();
        assert!(db.acl_generation().unwrap() > generation);
        assert_eq!(db.configured_primary_agent(&a.id).unwrap().as_deref(), Some("shared"));
        db.set_avatar_as(&admin.id, &b.id, Some("shared")).unwrap();
    }
    #[test]
    fn primary_change_is_locked_with_avatar_and_unchanged_without_one() {
        let (_dir, db, admin, a, _) = fixture();
        db.set_primary_agent_as(&a.id, &a.id, Some("other")).unwrap();
        assert_eq!(db.configured_primary_agent(&a.id).unwrap().as_deref(), Some("other"));
        db.set_avatar_as(&admin.id, &a.id, Some("shared")).unwrap();
        for target in [Some("other"), None] {
            assert_eq!(db.set_primary_agent_as(&a.id, &a.id, target).unwrap_err(), "avatar_primary_locked");
        }
        assert_eq!(db.configured_primary_agent(&a.id).unwrap().as_deref(), Some("shared"));
        db.set_avatar_as(&admin.id, &a.id, None).unwrap();
        db.set_primary_agent_as(&a.id, &a.id, None).unwrap();
        assert_eq!(db.configured_primary_agent(&a.id).unwrap(), None);
    }
    #[test]
    fn member_avatar_or_primary_prefers_avatar_falls_back_and_fails_closed() {
        let (_dir, db, admin, a, _) = fixture();
        db.set_primary_agent(&a.id, Some("other")).unwrap();
        assert_eq!(db.member_avatar_or_primary(&a.id).unwrap().as_deref(), Some("other"));
        db.set_avatar_as(&admin.id, &a.id, Some("shared")).unwrap();
        // Model a stale legacy preference independently from the avatar.
        db.conn().execute("UPDATE users SET primary_agent_id = 'other' WHERE id = ?1", [&a.id]).unwrap();
        assert_eq!(db.member_avatar_or_primary(&a.id).unwrap().as_deref(), Some("shared"));
        db.unbind_agent(&a.id, "shared").unwrap();
        assert_eq!(db.member_avatar_or_primary(&a.id).unwrap().as_deref(), Some("other"));
        db.unbind_agent(&a.id, "other").unwrap();
        assert_eq!(db.member_avatar_or_primary(&a.id).unwrap(), None);
        db.conn().execute("ALTER TABLE users RENAME COLUMN avatar_agent_id TO unreadable_avatar", []).unwrap();
        assert!(db.member_avatar_or_primary(&a.id).is_err());
    }
    #[test]
    fn offboard_clears_avatar_preserves_transfer_and_frees_ai() {
        let (_dir, db, admin, a, b) = fixture();
        db.bind_agent(&a.id, "owned", AccessLevel::Owner).unwrap();
        db.set_avatar_as(&admin.id, &a.id, Some("owned")).unwrap();
        db.offboard_user(&a.id, Some(&b.id), Some(&admin.id)).unwrap();
        assert_eq!(db.avatar_agent(&a.id).unwrap(), None);
        assert_eq!(db.member_avatar_or_primary(&a.id).unwrap(), None);
        db.set_avatar_as(&admin.id, &b.id, Some("owned")).unwrap();
    }
    #[test]
    fn conflicts_list_shared_active_primaries_until_all_are_resolved() {
        let (_dir, db, admin, a, b) = fixture();
        db.set_primary_agent(&a.id, Some("shared")).unwrap();
        db.set_primary_agent(&b.id, Some("shared")).unwrap();
        let conflicts = db.avatar_conflicts().unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].agent_id, "shared");
        assert_eq!(conflicts[0].user_ids.len(), 2);
        db.set_avatar_as(&admin.id, &a.id, Some("shared")).unwrap();
        assert_eq!(db.avatar_conflicts().unwrap().len(), 1);
        db.set_avatar_as(&admin.id, &b.id, Some("other")).unwrap();
        assert!(db.avatar_conflicts().unwrap().is_empty());
    }
}

impl UserDb {
    /// Admin-only creation of a NEW Personal avatar. The callback publishes the
    /// staged directory under the already-held seat/org locks. The caller must
    /// remove its newly published directory if recount or commit fails.
    pub fn publish_new_avatar_under(
        &self, actor: &str, member: &str, agent: &str,
        admission: &duduclaw_core::agent_seats::SeatAdmission, max: usize,
        publish: impl FnOnce(&Connection) -> Result<(), String>,
    ) -> Result<(), String> {
        self.publish_new_avatar_under_as(AvatarActor::Admin(actor), member, agent, admission, max, publish)
    }

    /// Publish a NEW Personal avatar under live admin or Google onboarding
    /// authority. All identity changes and onboarding completion commit together.
    /// The caller must remove its newly published directory on recount/commit failure.
    pub fn publish_new_avatar_under_as(
        &self, actor: AvatarActor<'_>, member: &str, agent: &str,
        admission: &duduclaw_core::agent_seats::SeatAdmission, max: usize,
        publish: impl FnOnce(&Connection) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.home.as_deref() != Some(admission.home()) {
            return Err("分身名額資料來源不一致，未建立。".into());
        }
        if duduclaw_core::agent_seats::resolve_agent_seat(admission.home(), agent)
            .map_err(|e| e.to_string())?.is_some() {
            return Err("只能為成員建立新的 Personal AI，不能將一般員工改為免計名額分身。".into());
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        match &actor {
            AvatarActor::Admin(id) => {
                let admin: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1
                    AND role='admin' AND status='active')", [id], |r| r.get(0)).map_err(|e| e.to_string())?;
                if !admin { return Err("avatar_admin_required".into()); }
            }
            AvatarActor::SigninPolicy { .. } => {
                let signin: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM user_onboarding
                    WHERE user_id=?1 AND source='google_signin')
                    AND EXISTS(SELECT 1 FROM google_signin_links WHERE user_id=?1)",
                    [member], |r| r.get(0)).map_err(|e| e.to_string())?;
                if !signin { return Err("avatar_signin_required".into()); }
            }
        }
        let eligible: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1
            AND status='active' AND avatar_agent_id IS NULL)", [member], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !eligible { return Err("成員必須啟用且尚未設定 AI 分身。".into()); }
        if let AvatarActor::SigninPolicy { policy_check, .. } = &actor {
            policy_check()?;
        }
        let stale: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM user_agent_bindings WHERE agent_name=?1)
            OR EXISTS(SELECT 1 FROM isolation_personal_agents WHERE agent_name=?1)
            OR EXISTS(SELECT 1 FROM users WHERE avatar_agent_id=?1)", [agent], |r| r.get(0)).map_err(|e| e.to_string())?;
        if stale { return Err("此 AI 名稱仍有身分紀錄，請使用新的名稱。".into()); }
        publish(&tx)?;
        crate::db::insert_personal_agent_in_transaction(&tx, agent, member).map_err(|e| e.to_string())?;
        let now = Utc::now().to_rfc3339();
        crate::db::bind_agent_in_transaction(&tx, member, agent, crate::AccessLevel::Owner, &now)
            .map_err(|e| e.to_string())?;
        check_available(&tx, member, agent)?;
        tx.execute("UPDATE users SET avatar_agent_id=?1, primary_agent_id=?1, updated_at=?2 WHERE id=?3",
            params![agent, now, member]).map_err(|e| e.to_string())?;
        tx.execute("UPDATE user_onboarding SET avatar='done' WHERE user_id=?1 AND source='google_signin'",
            [member]).map_err(|e| e.to_string())?;
        let (audit_actor, detail) = match actor {
            AvatarActor::Admin(id) => (id, agent.to_owned()),
            AvatarActor::SigninPolicy { revision, .. } => ("system:google_signin",
                serde_json::json!({ "agent": agent, "revision": revision }).to_string()),
        };
        tx.execute("INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp)
            VALUES(?1,'user.create_avatar',?2,?3,?4)", params![audit_actor, member, detail, now]).map_err(|e| e.to_string())?;
        bump_acl_generation(&tx).map_err(|e| e.to_string())?;
        crate::avatar_seats::grant_personal_incarnation(&tx, agent, member)?;
        admission.recount_in(&tx, max)?;
        tx.commit().map_err(|e| e.to_string())
    }
}

impl UserDb {
    /// Compensation after an avatar's newly-created configuration was removed
    /// because the protected proposal could not record its creation marker.
    pub fn discard_absent_avatar_under(&self, member: &str, agent: &str,
        admission: &duduclaw_core::agent_seats::SeatAdmission) -> Result<(), String> {
        if self.home.as_deref() != Some(admission.home())
            || duduclaw_core::agent_seats::resolve_agent_seat(admission.home(), agent)
                .map_err(|e| e.to_string())?.is_some() {
            return Err("avatar cleanup requires its absent configuration".into());
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        crate::db::remove_personal_agent_in_transaction(&tx, agent, member).map_err(|e| e.to_string())?;
        tx.execute("UPDATE users SET avatar_agent_id=CASE WHEN avatar_agent_id=?1 THEN NULL ELSE avatar_agent_id END,
            primary_agent_id=CASE WHEN primary_agent_id=?1 THEN NULL ELSE primary_agent_id END WHERE id=?2",
            params![agent,member]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM avatar_seat_grants WHERE agent_name=?1 AND owner_user_id=?2", params![agent,member])
            .map_err(|e| e.to_string())?;
        duduclaw_core::employee_incarnation::retire(&tx, agent).map_err(|e| e.to_string())?;
        bump_acl_generation(&tx).map_err(|e| e.to_string())?;
        admission.recount_in(&tx, 0)?;
        tx.commit().map_err(|e| e.to_string())
    }
}
