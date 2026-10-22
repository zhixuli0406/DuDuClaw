//! Writes to the reporting lines: `set_manager`, the demotion guard and the
//! offboarding reparent. Each runs in one `BEGIN IMMEDIATE` transaction that
//! also bumps the ACL generation, so a refused write changes nothing and an
//! accepted one invalidates every cached live context at once.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::db::UserDb;
use crate::models::{AccessLevel, UserRole, UserStatus};
use crate::org::{OrgError, MAX_ORG_DEPTH};
use crate::user_row::{bump_acl_generation, load_user};

/// One report moved to a new manager when their manager left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reparent {
    pub user_id: String,
    /// `None` ⇒ moved to the top of the tree.
    pub to: Option<String>,
}

/// What `offboard_user` did besides setting the status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OffboardOutcome {
    pub transferred_agents: Vec<String>,
    pub reparented: Vec<Reparent>,
}

fn db_err(e: rusqlite::Error) -> OrgError {
    OrgError::Db(format!("org write error: {e}"))
}

/// Direct reports that are still part of the tree (not Offboarded).
pub(crate) fn reports_count(conn: &Connection, user_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM users
          WHERE manager_user_id = ?1 AND id <> ?1 AND status <> 'offboarded'",
        [user_id],
        |row| row.get(0),
    )
    .map_err(|e| format!("reports query error: {e}"))
}

fn direct_reports(conn: &Connection, user_id: &str) -> Result<Vec<String>, OrgError> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM users
              WHERE manager_user_id = ?1 AND id <> ?1 AND status <> 'offboarded'
              ORDER BY created_at, id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([user_id], |row| row.get::<_, String>(0))
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

fn manager_of(conn: &Connection, user_id: &str) -> Result<Option<String>, OrgError> {
    conn.query_row(
        "SELECT manager_user_id FROM users WHERE id = ?1",
        [user_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(db_err)
}

/// Levels below `user_id` (0 = nobody reports to them). Capped one past
/// [`MAX_ORG_DEPTH`] so a hand-written cycle still terminates.
fn subtree_height(conn: &Connection, user_id: &str) -> Result<u32, OrgError> {
    let height: Option<i64> = conn
        .query_row(
            "WITH RECURSIVE sub(id, depth) AS (
                SELECT id, 1 FROM users
                 WHERE manager_user_id = ?1 AND id <> ?1 AND status <> 'offboarded'
                UNION
                SELECT u.id, s.depth + 1 FROM sub s JOIN users u ON u.manager_user_id = s.id
                 WHERE s.depth < ?2 AND u.id <> ?1 AND u.status <> 'offboarded')
             SELECT MAX(depth) FROM sub",
            params![user_id, u32::from(MAX_ORG_DEPTH) + 1],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(height.unwrap_or(0).max(0) as u32)
}

/// May `manager_id` become the manager of `user_id` (`None` = a person who
/// does not exist yet and so has nobody below them)?
fn validate_manager(conn: &Connection, user_id: Option<&str>, manager_id: &str) -> Result<(), OrgError> {
    if user_id == Some(manager_id) {
        return Err(OrgError::SelfManager);
    }
    let manager = load_user(conn, manager_id)?.ok_or(OrgError::ManagerNotFound)?;
    if manager.status != UserStatus::Active {
        return Err(OrgError::ManagerInactive);
    }
    if !manager.role.at_least(UserRole::Manager) {
        return Err(OrgError::ManagerRoleTooLow);
    }
    // Walk up from the manager. Meeting the user means a cycle; running past
    // the depth cap means the chain (or a cycle above it) is already too deep.
    let mut level: u32 = 0;
    let mut cursor = Some(manager_id.to_string());
    while let Some(id) = cursor {
        if user_id == Some(id.as_str()) {
            return Err(OrgError::Cycle);
        }
        level += 1;
        if level > u32::from(MAX_ORG_DEPTH) {
            return Err(OrgError::TooDeep);
        }
        cursor = manager_of(conn, &id)?;
    }
    let below = match user_id {
        Some(u) => subtree_height(conn, u)?,
        None => 0,
    };
    if level + 1 + below > u32::from(MAX_ORG_DEPTH) {
        return Err(OrgError::TooDeep);
    }
    Ok(())
}

fn write_manager(conn: &Connection, user_id: &str, manager_id: Option<&str>) -> Result<(), OrgError> {
    conn.execute(
        "UPDATE users SET manager_user_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![manager_id, Utc::now().to_rfc3339(), user_id],
    )
    .map_err(db_err)?;
    Ok(())
}

fn audit(conn: &Connection, actor: Option<&str>, action: &str, target: &str, detail: &str) -> Result<(), OrgError> {
    conn.execute(
        "INSERT INTO auth_audit_log (user_id, action, target, detail, ip, timestamp)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
        params![actor, action, target, detail, Utc::now().to_rfc3339()],
    )
    .map_err(db_err)?;
    Ok(())
}

impl UserDb {
    /// Set (`Some`) or clear (`None`) a user's direct manager.
    ///
    /// Refuses, without writing: the user themself ([`OrgError::SelfManager`]),
    /// a manager that does not exist, is not Active, or is below the Manager
    /// role, a manager inside the user's own subtree ([`OrgError::Cycle`]),
    /// and any placement where the deepest person under the user would end
    /// up more than [`MAX_ORG_DEPTH`] levels from the top.
    pub fn set_manager(&self, user_id: &str, manager_id: Option<&str>) -> Result<(), OrgError> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        load_user(&tx, user_id)?.ok_or(OrgError::UserNotFound)?;
        if let Some(m) = manager_id {
            validate_manager(&tx, Some(user_id), m)?;
        }
        write_manager(&tx, user_id, manager_id)?;
        bump_acl_generation(&tx).map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Would `manager_id` be accepted as the manager of a brand-new user?
    /// `users.create` checks this before inserting, then calls
    /// [`Self::set_manager`], which checks again.
    pub fn check_manager_for_new_user(&self, manager_id: &str) -> Result<(), OrgError> {
        let conn = self.conn();
        validate_manager(&conn, None, manager_id)
    }

    /// `true` while someone still in the tree reports directly to `user_id`
    /// (the demotion guard behind [`crate::org::MANAGER_HAS_REPORTS`]).
    pub fn manager_has_reports(&self, user_id: &str) -> Result<bool, String> {
        let conn = self.conn();
        Ok(reports_count(&conn, user_id)? > 0)
    }

    /// Offboard `user_id` in one transaction: status `offboarded`, the
    /// leaver leaves the tree, their direct reports move to `transfer_to`
    /// (or, without it, to the leaver's own manager), and with `transfer_to`
    /// their agent bindings move too.
    ///
    /// Each move is checked like [`Self::set_manager`]. An explicit
    /// `transfer_to` that fails the check refuses the whole offboarding; the
    /// implicit fallback (the leaver's manager) that fails it puts the report
    /// at the top instead, so someone else's state can never block an
    /// offboarding. Every move writes a `user.reparent` audit row by `actor`.
    pub fn offboard_user(
        &self,
        user_id: &str,
        transfer_to: Option<&str>,
        actor: Option<&str>,
    ) -> Result<OffboardOutcome, OrgError> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let leaver = load_user(&tx, user_id)?.ok_or(OrgError::UserNotFound)?;
        if let Some(t) = transfer_to {
            let target = load_user(&tx, t)?;
            if t == user_id || !target.is_some_and(|u| u.status == UserStatus::Active) {
                return Err(OrgError::TransferTargetInvalid);
            }
        }
        tx.execute(
            "UPDATE users SET status = 'offboarded', manager_user_id = NULL, updated_at = ?1
              WHERE id = ?2",
            params![Utc::now().to_rfc3339(), user_id],
        )
        .map_err(db_err)?;

        let mut outcome = OffboardOutcome::default();
        for report in direct_reports(&tx, user_id)? {
            let to = reparent_target(&tx, &report, transfer_to, leaver.manager_user_id.as_deref())?;
            write_manager(&tx, &report, to.as_deref())?;
            let detail = format!("from={user_id} to={}", to.as_deref().unwrap_or("none"));
            audit(&tx, actor, "user.reparent", &report, &detail)?;
            outcome.reparented.push(Reparent { user_id: report, to });
        }

        if let Some(t) = transfer_to {
            outcome.transferred_agents = move_bindings(&tx, user_id, t)?;
        }
        bump_acl_generation(&tx).map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(outcome)
    }
}

/// The new manager of `report` after their manager left.
fn reparent_target(
    conn: &Connection,
    report: &str,
    transfer_to: Option<&str>,
    leaver_manager: Option<&str>,
) -> Result<Option<String>, OrgError> {
    // A report who is themself the transfer target takes the leaver's place
    // under the leaver's manager.
    let explicit = transfer_to.filter(|t| *t != report);
    if let Some(t) = explicit {
        validate_manager(conn, Some(report), t)?;
        return Ok(Some(t.to_string()));
    }
    match leaver_manager.filter(|m| *m != report) {
        Some(m) if validate_manager(conn, Some(report), m).is_ok() => Ok(Some(m.to_string())),
        _ => Ok(None),
    }
}

/// Move every binding of `from` to `to` at the same level (the pre-existing
/// `users.offboard` transfer semantics: the moved level replaces any binding
/// `to` already had for that agent).
fn move_bindings(conn: &Connection, from: &str, to: &str) -> Result<Vec<String>, OrgError> {
    let bindings: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT agent_name, access_level FROM user_agent_bindings WHERE user_id = ?1")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([from], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
    };
    let now = Utc::now().to_rfc3339();
    let mut moved = Vec::with_capacity(bindings.len());
    for (agent, level) in bindings {
        // Re-validate the stored level instead of copying an unknown string.
        let level: AccessLevel = level
            .parse()
            .map_err(|e: String| OrgError::Db(format!("corrupt access_level in DB: {e}")))?;
        conn.execute(
            "INSERT OR REPLACE INTO user_agent_bindings (user_id, agent_name, access_level, bound_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![to, agent, level.to_string(), now],
        )
        .map_err(db_err)?;
        conn.execute(
            "DELETE FROM user_agent_bindings WHERE user_id = ?1 AND agent_name = ?2",
            params![from, agent],
        )
        .map_err(db_err)?;
        moved.push(agent);
    }
    Ok(moved)
}
