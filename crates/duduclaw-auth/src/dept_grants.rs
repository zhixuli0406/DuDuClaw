//! Department grants are explicit authority, independent of users.department.
//! Callers authenticate the actor and authorize the operation; the gateway also
//! validates departments against its authoritative organization snapshot.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use rusqlite::{params, Connection, Row, TransactionBehavior};

use crate::user_row::{bump_acl_generation, load_user};
use crate::{UserDb, UserRole, UserStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GrantGroup {
    Schedules,
    Automation,
    Skills,
    Cost,
    EgressReview,
}

impl GrantGroup {
    pub const ALL: [Self; 5] = [Self::Schedules, Self::Automation, Self::Skills, Self::Cost, Self::EgressReview];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedules => "schedules",
            Self::Automation => "automation",
            Self::Skills => "skills",
            Self::Cost => "cost",
            Self::EgressReview => "egress_review",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|group| group.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeptGrant {
    pub user_id: String,
    pub department: String,
    pub groups: Vec<GrantGroup>,
    pub granted_by: String,
    pub granted_at: String,
}

fn store_error(error: rusqlite::Error) -> String {
    format!("department grant store error: {error}")
}

// Unknown future groups contribute no authority. Conversion/read errors still fail.
fn grant_from_row(row: &Row<'_>) -> rusqlite::Result<Option<DeptGrant>> {
    let raw: String = row.get(2)?;
    let Some(group) = GrantGroup::parse(&raw) else { return Ok(None); };
    Ok(Some(DeptGrant {
        user_id: row.get(0)?, department: row.get(1)?, groups: vec![group],
        granted_by: row.get(3)?, granted_at: row.get(4)?,
    }))
}

fn read_grants(conn: &Connection, user: Option<&str>) -> Result<Vec<DeptGrant>, String> {
    let mut stmt = conn.prepare(
        "SELECT user_id, department, grp, granted_by, granted_at
         FROM department_manager_grants WHERE (?1 IS NULL OR user_id = ?1)
         ORDER BY user_id, department, granted_at DESC, grp",
    ).map_err(store_error)?;
    let rows = stmt.query_map([user], grant_from_row).map_err(store_error)?;
    let mut grants: BTreeMap<(String, String), DeptGrant> = BTreeMap::new();
    for row in rows {
        let Some(grant) = row.map_err(store_error)? else { continue; };
        let key = (grant.user_id.clone(), grant.department.clone());
        if let Some(existing) = grants.get_mut(&key) {
            existing.groups.extend(grant.groups);
        } else {
            grants.insert(key, grant);
        }
    }
    Ok(grants.into_values().map(|mut grant| {
        grant.groups.sort();
        grant
    }).collect())
}

fn read_main_users(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn.prepare("SELECT user_id FROM main_agent_users ORDER BY user_id").map_err(store_error)?;
    stmt.query_map([], |row| row.get(0)).map_err(store_error)?
        .collect::<rusqlite::Result<Vec<String>>>().map_err(store_error)
}

impl UserDb {
    /// Replace a Manager's groups for one exact department. An unchanged set
    /// (including reordered/duplicate input) writes no audit and does not bump
    /// the ACL generation. Target validation still runs for unchanged sets.
    pub fn set_dept_grants(&self, actor_user_id: &str, target_user_id: &str,
        department: &str, groups: &[GrantGroup]) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let user = load_user(&tx, target_user_id)?.ok_or("user not found")?;
        if user.status != UserStatus::Active || user.role != UserRole::Manager {
            return Err("department grants require an active Manager".into());
        }
        if !duduclaw_core::department::is_valid_department(department) {
            return Err("invalid department name".into());
        }
        // Preserve unknown stored names in the comparison/audit: replacing them
        // must revoke them even though readers never expose their authority.
        let before: BTreeSet<String> = {
            let mut stmt = tx.prepare("SELECT grp FROM department_manager_grants WHERE user_id=?1 AND department=?2")
                .map_err(store_error)?;
            stmt.query_map(params![target_user_id, department], |row| row.get(0)).map_err(store_error)?
                .collect::<rusqlite::Result<_>>().map_err(store_error)?
        };
        let after: BTreeSet<String> = groups.iter().map(|group| group.as_str().to_owned()).collect();
        if before == after { return Ok(()); }
        tx.execute("DELETE FROM department_manager_grants WHERE user_id=?1 AND department=?2",
            params![target_user_id, department]).map_err(store_error)?;
        let now = Utc::now().to_rfc3339();
        for group in &after {
            tx.execute("INSERT INTO department_manager_grants(user_id, department, grp, granted_by, granted_at)
                VALUES (?1, ?2, ?3, ?4, ?5)",
                params![target_user_id, department, group, actor_user_id, now]).map_err(store_error)?;
        }
        bump_acl_generation(&tx).map_err(store_error)?;
        let detail = serde_json::json!({"department": department, "before": before, "after": after}).to_string();
        tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, ip, timestamp)
            VALUES (?1, 'dept_grant_changed', ?2, ?3, NULL, ?4)",
            params![actor_user_id, target_user_id, detail, now]).map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    pub fn dept_grants_for_user(&self, user_id: &str) -> Result<Vec<(String, Vec<GrantGroup>)>, String> {
        Ok(read_grants(&self.conn(), Some(user_id))?.into_iter()
            .map(|grant| (grant.department, grant.groups)).collect())
    }

    /// Record a successful grant-backed mutation, without changing authority.
    pub fn record_dept_grant_use(&self, actor: &str, agent: &str, group: GrantGroup,
        action: &str, method: &str) -> Result<(), String> {
        let detail = serde_json::json!({
            "group": group.as_str(), "action": action, "method": method,
        }).to_string();
        self.conn().execute(
            "INSERT INTO auth_audit_log(user_id, action, target, detail, ip, timestamp)
             VALUES (?1, 'dept_grant_used', ?2, ?3, NULL, ?4)",
            params![actor, agent, detail, Utc::now().to_rfc3339()],
        ).map_err(store_error)?;
        Ok(())
    }

    pub fn list_dept_grants(&self) -> Result<Vec<DeptGrant>, String> {
        read_grants(&self.conn(), None)
    }

    /// Replace the complete main-agent access set. Like department grants,
    /// unchanged sets do not write an audit or bump the ACL generation.
    pub fn set_main_agent_users(&self, actor: &str, user_ids: &[String]) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let after: BTreeSet<String> = user_ids.iter().cloned().collect();
        for id in &after {
            let user = load_user(&tx, id)?.ok_or("user not found")?;
            if user.status != UserStatus::Active { return Err("main-agent access requires an active user".into()); }
        }
        let before: BTreeSet<String> = read_main_users(&tx)?.into_iter().collect();
        if before == after { return Ok(()); }
        tx.execute("DELETE FROM main_agent_users", []).map_err(store_error)?;
        let now = Utc::now().to_rfc3339();
        for id in &after {
            tx.execute("INSERT INTO main_agent_users(user_id, granted_by, granted_at) VALUES (?1, ?2, ?3)",
                params![id, actor, now]).map_err(store_error)?;
        }
        bump_acl_generation(&tx).map_err(store_error)?;
        let detail = serde_json::json!({"before": before, "after": after}).to_string();
        tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, ip, timestamp)
            VALUES (?1, 'main_access_changed', 'main_agent_users', ?2, NULL, ?3)",
            params![actor, detail, now]).map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    pub fn main_agent_users(&self) -> Result<Vec<String>, String> {
        read_main_users(&self.conn())
    }

    pub fn is_main_agent_user(&self, user_id: &str) -> Result<bool, String> {
        self.conn().query_row("SELECT EXISTS(SELECT 1 FROM main_agent_users WHERE user_id=?1)",
            [user_id], |row| row.get(0)).map_err(store_error)
    }
}

#[cfg(test)]
#[path = "dept_grants_tests.rs"]
mod tests;
