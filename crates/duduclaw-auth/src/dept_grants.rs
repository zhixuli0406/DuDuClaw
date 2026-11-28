//! Department grants are explicit authority, independent of users.department.
//! Callers authenticate the actor and authorize the operation; the gateway also
//! validates departments against its authoritative organization snapshot.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use rusqlite::{params, Connection, Row, TransactionBehavior};

use crate::user_row::{bump_acl_generation, load_user};
use crate::{AccessLevel, User, UserDb, UserRole, UserStatus};

pub const MAIN_AGENT_USERS_HASH_CONFLICT: &str = "main-agent access hash conflict";

pub const ACKNOWLEDGEMENT_REQUIRED: &str = "acknowledgement required";

/// Fresh database facts for display only; never an authorization context.
pub struct DisplayCreatorAccess {
    pub user: User,
    pub bindings: BTreeMap<String, AccessLevel>,
    pub grants: Vec<(String, Vec<GrantGroup>)>,
}

/// One batch projection for the dashboard; granter identities are never names.
pub struct MainAgentUserDisplay {
    pub user: User,
    pub granted_by_name: Option<String>,
    pub granted_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachChange {
    Gains,
    Loses,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReachLine {
    pub user_id: String,
    pub display_name: String,
    /// The grant's department, never the manager's profile department.
    pub department: String,
    pub groups: Vec<GrantGroup>,
    pub change: ReachChange,
}

/// Net group changes for one employee's move. Read grants and live users in
/// one SQLite snapshot, including on a read-only connection. No profile,
/// prefix, case folding or mirror is an authority source here.
pub fn reach_change(db: &UserDb, from_dept: Option<&str>, to_dept: Option<&str>)
    -> Result<Vec<ReachLine>, String> {
    let from = from_dept.filter(|d| !d.is_empty());
    let to = to_dept.filter(|d| !d.is_empty());
    if from == to { return Ok(Vec::new()); }
    let mut conn = db.conn();
    let tx = conn.transaction().map_err(store_error)?;
    let mut groups: BTreeMap<String, (BTreeSet<GrantGroup>, BTreeSet<GrantGroup>)> = BTreeMap::new();
    for grant in read_grants(&tx, None)? {
        if Some(grant.department.as_str()) == from {
            groups.entry(grant.user_id).or_default().0.extend(grant.groups);
        } else if Some(grant.department.as_str()) == to {
            groups.entry(grant.user_id).or_default().1.extend(grant.groups);
        }
    }
    let mut lines = Vec::new();
    for (user_id, (before, after)) in groups {
        let Some(user) = load_user(&tx, &user_id)? else { continue; };
        if user.role != UserRole::Manager || user.status != UserStatus::Active { continue; }
        for (change, department, net) in [
            (ReachChange::Gains, to, after.difference(&before).copied().collect::<Vec<_>>()),
            (ReachChange::Loses, from, before.difference(&after).copied().collect::<Vec<_>>()),
        ] {
            if !net.is_empty() {
                lines.push(ReachLine {
                    user_id: user_id.clone(), display_name: user.display_name.clone(),
                    department: department.expect("nonempty difference has a department").to_owned(),
                    groups: net, change,
                });
            }
        }
    }
    lines.sort_by(|a, b| (&a.user_id, &a.department, &a.groups, a.change)
        .cmp(&(&b.user_id, &b.department, &b.groups, b.change)));
    Ok(lines)
}

/// Canonical JSON tuples, sorted independently of caller order. Display names
/// are deliberately excluded: a rename does not change authority.
pub fn reach_digest(lines: &[ReachLine]) -> String {
    let mut canonical: Vec<_> = lines.iter().map(|line| {
        let mut groups: Vec<_> = line.groups.iter().map(|g| g.as_str()).collect();
        groups.sort_unstable();
        groups.dedup();
        (&line.user_id, &line.department, groups, line.change)
    }).collect();
    canonical.sort();
    let bytes = serde_json::to_vec(&canonical).expect("reach tuples are JSON serializable");
    // ring is already an auth dependency; no additional hash dependency.
    ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref().iter()
        .map(|byte| format!("{byte:02x}")).collect()
}

/// Org-change cards currently use zh-TW only.
pub fn reach_sentence_zh(lines: &[ReachLine]) -> Option<String> {
    if lines.is_empty() { return None; }
    let clauses: Vec<_> = lines.iter().map(|line| {
        let mut groups = line.groups.clone();
        groups.sort();
        groups.dedup();
        let labels: Vec<_> = groups.iter().map(|group| match group {
            GrantGroup::Schedules => "排程",
            GrantGroup::Automation => "自動化規則",
            GrantGroup::Skills => "技能",
            GrantGroup::Cost => "用量",
            GrantGroup::EgressReview => "對外傳送審核",
        }).collect();
        let label = match labels.split_last() {
            Some((last, [])) => (*last).to_owned(),
            Some((last, rest)) => format!("{}與{last}", rest.join("、")),
            None => String::new(),
        };
        let name = if line.display_name.trim().is_empty() { "部門主管" } else { &line.display_name };
        let verb = match line.change { ReachChange::Gains => "將可管理", ReachChange::Loses => "將不再能管理" };
        format!("{}（{}）{verb}這位 AI 員工的{label}",
            name.replace(['\r', '\n'], " "), line.department.replace(['\r', '\n'], " "))
    }).collect();
    Some(format!("核准後，{}。", clauses.join("；")))
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

// Hash the exact stored grant evidence, ordered by the primary key. JSON tuple
// encoding avoids delimiter ambiguity for legacy values containing separators.
fn read_main_users_hash(conn: &Connection) -> Result<String, String> {
    let mut stmt = conn.prepare("SELECT user_id, granted_by, granted_at FROM main_agent_users ORDER BY user_id")
        .map_err(store_error)?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .map_err(store_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(store_error)?;
    let bytes = serde_json::to_vec(&rows).map_err(|e| e.to_string())?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    Ok(digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_main_users_display(conn: &Connection) -> Result<Vec<MainAgentUserDisplay>, String> {
    let columns = crate::user_row::USER_COLUMNS.split(',')
        .map(|column| format!("u.{}", column.trim())).collect::<Vec<_>>().join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT {columns}, g.display_name AS granted_by_name, m.granted_at
         FROM main_agent_users m LEFT JOIN users u ON u.id=m.user_id
         LEFT JOIN users g ON g.id=m.granted_by ORDER BY m.user_id"
    )).map_err(store_error)?;
    let rows = stmt.query_map([], |row| Ok(MainAgentUserDisplay {
        // A stale member row still fails the whole RPC, as before.
        user: crate::user_row::user_from_row(row)?,
        granted_by_name: row.get("granted_by_name")?,
        granted_at: row.get("granted_at")?,
    })).map_err(store_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
}

impl UserDb {
    /// Replace a Manager's groups for one exact department. An unchanged set
    /// (including reordered/duplicate input) writes no audit and does not bump
    /// the ACL generation. Target validation still runs for unchanged sets.
    /// Any added group requires acknowledgement, compared within the same
    /// IMMEDIATE transaction as the write and audit (no re-authentication).
    pub fn set_dept_grants(&self, actor_user_id: &str, target_user_id: &str,
        department: &str, groups: &[GrantGroup], acknowledged: bool) -> Result<(), String> {
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
        // Compare under the IMMEDIATE write transaction, before any mutation.
        if !acknowledged && !after.is_subset(&before) {
            return Err(ACKNOWLEDGEMENT_REQUIRED.into());
        }
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
        let detail = serde_json::json!({"department": department, "before": before, "after": after, "acknowledged": acknowledged}).to_string();
        tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, ip, timestamp)
            VALUES (?1, 'dept_grant_changed', ?2, ?3, NULL, ?4)",
            params![actor_user_id, target_user_id, detail, now]).map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    /// DISPLAY ONLY. Read one creator's account, direct bindings and grants in
    /// one SQLite snapshot. A corrupt binding is unknown, not a missing grant.
    pub fn display_creator_access(&self, user_id: &str) -> Result<Option<DisplayCreatorAccess>, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(store_error)?;
        let Some(user) = load_user(&tx, user_id)? else { return Ok(None); };
        let mut bindings = BTreeMap::new();
        if user.status == UserStatus::Active && user.role != UserRole::Admin {
            let mut stmt = tx.prepare(
                "SELECT agent_name, access_level FROM user_agent_bindings WHERE user_id=?1",
            ).map_err(store_error)?;
            let rows = stmt.query_map([user_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }).map_err(store_error)?;
            for row in rows {
                let (agent, level) = row.map_err(store_error)?;
                bindings.insert(agent, level.parse::<AccessLevel>()?);
            }
        }
        let grants = if user.status == UserStatus::Active && user.role == UserRole::Manager {
            read_grants(&tx, Some(user_id))?.into_iter()
                .map(|grant| (grant.department, grant.groups)).collect()
        } else { Vec::new() };
        tx.commit().map_err(store_error)?;
        Ok(Some(DisplayCreatorAccess { user, bindings, grants }))
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
    /// Any added user requires acknowledgement in the same write transaction.
    pub fn set_main_agent_users(&self, actor: &str, user_ids: &[String], acknowledged: bool) -> Result<(), String> {
        self.replace_main_agent_users(actor, user_ids, acknowledged, None)
    }

    /// Compare the client snapshot inside the same IMMEDIATE transaction as
    /// replacement, ACL generation and audit. Conflicts mutate nothing.
    pub fn set_main_agent_users_if_unchanged(&self, actor: &str, user_ids: &[String],
        acknowledged: bool, expected_hash: &str) -> Result<(), String> {
        self.replace_main_agent_users(actor, user_ids, acknowledged, Some(expected_hash))
    }

    fn replace_main_agent_users(&self, actor: &str, user_ids: &[String], acknowledged: bool,
        expected_hash: Option<&str>) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        if let Some(expected_hash) = expected_hash {
            if read_main_users_hash(&tx)? != expected_hash {
                return Err(MAIN_AGENT_USERS_HASH_CONFLICT.into());
            }
        }
        let after: BTreeSet<String> = user_ids.iter().cloned().collect();
        for id in &after {
            let user = load_user(&tx, id)?.ok_or("user not found")?;
            if user.status != UserStatus::Active { return Err("main-agent access requires an active user".into()); }
        }
        let before: BTreeSet<String> = read_main_users(&tx)?.into_iter().collect();
        // Compare under the IMMEDIATE write transaction, before any mutation.
        if !acknowledged && !after.is_subset(&before) {
            return Err(ACKNOWLEDGEMENT_REQUIRED.into());
        }
        if before == after { return Ok(()); }
        tx.execute("DELETE FROM main_agent_users", []).map_err(store_error)?;
        let now = Utc::now().to_rfc3339();
        for id in &after {
            tx.execute("INSERT INTO main_agent_users(user_id, granted_by, granted_at) VALUES (?1, ?2, ?3)",
                params![id, actor, now]).map_err(store_error)?;
        }
        bump_acl_generation(&tx).map_err(store_error)?;
        let detail = serde_json::json!({"before": before, "after": after, "acknowledged": acknowledged}).to_string();
        tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, ip, timestamp)
            VALUES (?1, 'main_access_changed', 'main_agent_users', ?2, NULL, ?3)",
            params![actor, detail, now]).map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    pub fn main_agent_users(&self) -> Result<Vec<String>, String> {
        read_main_users(&self.conn())
    }

    /// Read members and grant evidence in a single statement/snapshot.
    pub fn main_agent_users_display(&self) -> Result<Vec<MainAgentUserDisplay>, String> {
        read_main_users_display(&self.conn())
    }

    /// Display and hash must describe one SQLite read snapshot, including all
    /// grant evidence even when the granter no longer has a display name.
    pub fn main_agent_users_display_with_hash(&self) -> Result<(Vec<MainAgentUserDisplay>, String), String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(store_error)?;
        let entries = read_main_users_display(&tx)?;
        let hash = read_main_users_hash(&tx)?;
        tx.commit().map_err(store_error)?;
        Ok((entries, hash))
    }

    pub fn is_main_agent_user(&self, user_id: &str) -> Result<bool, String> {
        self.conn().query_row("SELECT EXISTS(SELECT 1 FROM main_agent_users WHERE user_id=?1)",
            [user_id], |row| row.get(0)).map_err(store_error)
    }
}

#[cfg(test)]
#[path = "dept_grants_tests.rs"]
mod tests;
