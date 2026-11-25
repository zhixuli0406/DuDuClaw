//! Seat-changing identity writes share the filesystem admission lock. The
//! recount reads the pending SQLite transaction so refusal rolls back its
//! status, bindings, avatar, audit and ACL generation together.
use duduclaw_core::agent_seats::{SeatAdmission, seat_holders, seat_holders_in, AVATAR_SEAT_REFUSAL};
use crate::UserDb;

pub type SeatLimit<'a> = Option<(&'a SeatAdmission, usize)>;

pub(crate) struct SeatChange<'a> {
    owned: Option<SeatAdmission>,
    borrowed: SeatLimit<'a>,
    before: usize,
    offline: bool,
}
impl<'a> SeatChange<'a> {
    pub(crate) fn begin(db: &UserDb, limit: SeatLimit<'a>) -> Result<Self, String> {
        if let Some((admission, _)) = limit {
            if db.home.as_deref() != Some(admission.home()) {
                return Err("分身名額資料來源不一致，未儲存。".into());
            }
        }
        let owned = if limit.is_none() {
            db.home.as_deref().map(SeatAdmission::acquire).transpose().map_err(|e| e.to_string())?
        } else { None };
        let before = db.home.as_deref().map(seat_holders).transpose()
            .map_err(|e| e.to_string())?.map_or(0, |rows| rows.len());
        Ok(Self { owned, borrowed: limit, before, offline: limit.is_none() })
    }

    /// Borrow an existing admission even without a resolved license cap. An
    /// unknown cap keeps the offline no-growth rule and never re-acquires.
    pub(crate) fn begin_admitted(db: &UserDb, admission: &'a SeatAdmission,
        cap: Option<usize>) -> Result<Self, String> {
        let mut change = Self::begin(db, Some((admission, cap.unwrap_or(0))))?;
        if cap.is_none() { change.offline = true; }
        Ok(change)
    }

    pub(crate) fn check(&self, conn: &rusqlite::Connection) -> Result<(), String> {
        let admission = self.borrowed.map(|(a, _)| a).or(self.owned.as_ref());
        let Some(admission) = admission else { return Ok(()) }; // in-memory tests only
        let after = seat_holders_in(admission.home(), conn).map_err(|e| e.to_string())?.len();
        // Without a caller-resolved license cap, never consume an additional
        // seat. Offline callers must use the explicit _under variant to do so.
        let allowed = match self.borrowed.filter(|_| !self.offline) {
            Some((_, max)) => max == 0 || after <= max || after <= self.before,
            None => after <= self.before,
        };
        if allowed { Ok(()) } else { Err(AVATAR_SEAT_REFUSAL.into()) }
    }
}

/// Protected kind/provenance, bound to an immutable employee incarnation.
/// Only provisioning and the one-time validated RM-212 migration issue it;
/// assigning an ordinary employee as an avatar never issues a grant.
pub fn grant_personal_incarnation(conn: &rusqlite::Connection, agent: &str, owner: &str) -> Result<(), String> {
    if conn.is_autocommit() { return Err("avatar grant requires a transaction".into()); }
    if crate::db::personal_agent_owner(conn, agent).map_err(|e| e.to_string())?.as_deref() != Some(owner) {
        return Err("avatar Personal owner mismatch".into());
    }
    let nonce = duduclaw_core::employee_incarnation::ensure(conn, agent).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO avatar_seat_grants(agent_name,owner_user_id,nonce) VALUES(?1,?2,?3)
        ON CONFLICT(agent_name) DO UPDATE SET owner_user_id=excluded.owner_user_id,nonce=excluded.nonce",
        rusqlite::params![agent, owner, nonce]).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn migrate_grants(conn: &rusqlite::Connection, home: Option<&std::path::Path>) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='avatar_seat_grants')",
        [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if exists { return Ok(()); }
    tx.execute_batch("CREATE TABLE avatar_seat_grants(agent_name TEXT PRIMARY KEY NOT NULL,
        owner_user_id TEXT NOT NULL, nonce TEXT NOT NULL);").map_err(|e| e.to_string())?;
    duduclaw_core::employee_incarnation::migrate(&tx).map_err(|e| e.to_string())?;
    let candidates = {
        let mut stmt = tx.prepare("SELECT p.agent_name,p.owner_user_id FROM isolation_personal_agents p
            JOIN users u ON u.id=p.owner_user_id AND u.status='active' AND u.avatar_agent_id=p.agent_name
            WHERE (SELECT COUNT(*) FROM users a WHERE a.avatar_agent_id=p.agent_name)=1
            AND (SELECT COUNT(*) FROM user_agent_bindings b WHERE b.agent_name=p.agent_name)=1
            AND EXISTS(SELECT 1 FROM user_agent_bindings b WHERE b.agent_name=p.agent_name
                AND b.user_id=u.id AND b.access_level='owner')").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())?
    };
    if let Some(home) = home {
        for (agent, owner) in candidates {
            let seat = duduclaw_core::agent_seats::resolve_agent_seat(home, &agent).map_err(|e| e.to_string())?;
            if seat.is_some_and(|s| !s.is_deleted() && s.status != duduclaw_core::agent_seats::UNREADABLE_STATUS) {
                grant_personal_incarnation(&tx, &agent, &owner)?;
            }
        }
        // Count with the pending grants; a migration only releases seats.
        seat_holders_in(home, &tx).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessLevel, UserRole};

    #[test]
    fn failed_migration_rolls_back_grants_and_can_retry() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        db.conn().execute("DROP TABLE avatar_seat_grants", []).unwrap();
        drop(db);
        std::fs::write(home.path().join("agents"), "not a directory").unwrap();
        assert!(UserDb::new(&path).is_err());
        let conn = rusqlite::Connection::open(&path).unwrap();
        let created: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='avatar_seat_grants')",
            [], |r| r.get(0)).unwrap();
        assert!(!created, "a failed recount must not commit partial migration grants");
        drop(conn);
        std::fs::remove_file(home.path().join("agents")).unwrap();
        let db = UserDb::new(&path).unwrap();
        assert!(db.conn().prepare("SELECT * FROM avatar_seat_grants").is_ok());
    }

    #[test]
    fn migration_validates_legacy_owner_and_never_regrants_replaced_ai() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        let admin = db.create_user("admin@seats.test", "Admin", "password123", UserRole::Admin).unwrap();
        let member = db.create_user("member@seats.test", "Member", "password123", UserRole::Employee).unwrap();
        let dir = home.path().join("agents/avatar");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname='avatar'\nstatus='active'\n").unwrap();
        {
            let mut conn = db.conn();
            let tx = conn.transaction().unwrap();
            crate::db::insert_personal_agent_in_transaction(&tx, "avatar", &member.id).unwrap();
            tx.commit().unwrap();
        }
        db.bind_agent(&member.id, "avatar", AccessLevel::Owner).unwrap();
        db.set_avatar_as(&admin.id, &member.id, Some("avatar")).unwrap();
        assert_eq!(seat_holders(home.path()).unwrap().len(), 1, "assignment alone never grants exemption");
        // Simulate the pre-D5 schema; the RM-212 assignment already exists.
        db.conn().execute("DROP TABLE avatar_seat_grants", []).unwrap();
        drop(db);
        let db = UserDb::new(&path).unwrap();
        assert!(seat_holders(home.path()).unwrap().is_empty());
        let admission = SeatAdmission::acquire(home.path()).unwrap();
        duduclaw_core::employee_incarnation::created(home.path(), "avatar").unwrap();
        assert_eq!(seat_holders(home.path()).unwrap().len(), 1, "replacement cannot inherit protected kind");
        drop(admission);
        db.set_avatar_as(&admin.id, &member.id, Some("avatar")).unwrap();
        drop(db);
        let _db = UserDb::new(&path).unwrap();
        assert_eq!(seat_holders(home.path()).unwrap().len(), 1, "reopening never regrants old identities");
    }

    #[test]
    fn unset_can_consume_a_free_seat_and_suspension_rolls_back_when_full() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@seats.test", "Admin", "password123", UserRole::Admin).unwrap();
        let member = db.create_user("member@seats.test", "Member", "password123", UserRole::Employee).unwrap();
        let admission = SeatAdmission::acquire(home.path()).unwrap();
        db.publish_new_avatar_under(&admin.id, &member.id, "avatar", &admission, 1, |conn| {
            let dir = home.path().join("agents/avatar");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("agent.toml"), "[agent]\nname='avatar'\nstatus='active'\n").map_err(|e| e.to_string())?;
            duduclaw_core::employee_incarnation::ensure(conn, "avatar").map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        db.set_avatar_as_under(&admin.id, &member.id, None, Some((&admission,1))).unwrap();
        assert_eq!(seat_holders(home.path()).unwrap().len(),1);
        db.set_avatar_as_under(&admin.id, &member.id, Some("avatar"), Some((&admission,1))).unwrap();
        let dir = home.path().join("agents/ordinary");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname='ordinary'\nstatus='active'\n").unwrap();
        let generation = db.acl_generation().unwrap();
        assert!(db.set_user_status_as_under(Some(&admin.id), &member.id, crate::UserStatus::Suspended,
            Some((&admission,1))).unwrap_err().contains("名額"));
        assert_eq!(db.acl_generation().unwrap(), generation);
        assert_eq!(db.get_user(&member.id).unwrap().unwrap().status, crate::UserStatus::Active);
    }
}
