//! Person-to-person reporting lines and the agent access they derive.
//!
//! A dashboard user may have one direct manager (`users.manager_user_id`).
//! A manager can *call* the AI employees bound to anyone below them, at
//! `min(subordinate level, Operator)`; that never reaches Owner, so it never
//! opens SOUL/skills/config editing. [`crate::UserContext::has_agent_access`]
//! keeps looking at direct bindings only, so memory, history and settings
//! RPCs do not widen either; only [`crate::UserContext::can_call_agent`] and
//! [`crate::UserContext::can_assign_to`] read the team grants.
//!
//! Pure SQLite, shared by the gateway and the CLI. SQLite does not enforce
//! the self-reference, so every traversal uses `UNION` plus a depth cap and
//! terminates even on a cycle written by hand.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use rusqlite::Connection;
use serde::Serialize;
use tracing::warn;

use crate::acl::UserContext;
use crate::db::UserDb;
use crate::models::{AccessLevel, User, UserRole, UserStatus};
use crate::user_row::{read_acl_generation, user_from_row, USER_COLUMNS};

/// Longest reporting chain, counted in people including the person at the
/// top (Q14): a chain may hold at most 8 people, so someone can be at most 7
/// levels below the top. Traversals below a manager stop after this many
/// levels, which is more than any accepted tree needs and is what bounds a
/// cycle written by hand.
pub const MAX_ORG_DEPTH: u8 = 8;

/// Stable error codes (the gateway passes them through; the dashboard keys
/// on them).
pub const MANAGER_CYCLE: &str = "manager_cycle";
pub const MANAGER_TOO_DEEP: &str = "manager_too_deep";
pub const MANAGER_ROLE: &str = "manager_role";
pub const MANAGER_HAS_REPORTS: &str = "manager_has_reports";
pub const MANAGER_NOT_FOUND: &str = "manager_not_found";
pub const MANAGER_INACTIVE: &str = "manager_inactive";
pub const TRANSFER_TARGET_INVALID: &str = "transfer_target_invalid";

/// Why a reporting-line write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgError {
    UserNotFound,
    SelfManager,
    ManagerNotFound,
    ManagerInactive,
    ManagerRoleTooLow,
    Cycle,
    TooDeep,
    HasReports,
    TransferTargetInvalid,
    Db(String),
}

impl OrgError {
    /// Machine-readable code. A self-assignment is the shortest cycle.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UserNotFound => "user_not_found",
            Self::SelfManager | Self::Cycle => MANAGER_CYCLE,
            Self::ManagerNotFound => MANAGER_NOT_FOUND,
            Self::ManagerInactive => MANAGER_INACTIVE,
            Self::ManagerRoleTooLow => MANAGER_ROLE,
            Self::TooDeep => MANAGER_TOO_DEEP,
            Self::HasReports => MANAGER_HAS_REPORTS,
            Self::TransferTargetInvalid => TRANSFER_TARGET_INVALID,
            Self::Db(_) => "internal_error",
        }
    }
}

impl std::fmt::Display for OrgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "{e}"),
            other => write!(f, "{}", other.code()),
        }
    }
}

impl From<String> for OrgError {
    fn from(e: String) -> Self {
        Self::Db(e)
    }
}

/// Access to an agent that comes from a subordinate's binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamGrant {
    /// Never above [`AccessLevel::Operator`].
    pub level: AccessLevel,
    /// The subordinate whose binding this comes from.
    pub via_user: String,
    /// 1 = direct report.
    pub depth: u8,
}

/// One active report with a direct Operator/Owner binding on an employee.
/// Kept separate from `TeamGrant`: oversight needs every qualifying person,
/// and must not mistake a derived Operator grant for a direct Owner binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverseenReport {
    pub user_id: String,
    pub display_name: String,
    pub depth: u8,
    pub level: AccessLevel,
}

/// Where a caller's access to one agent comes from. `Default` and `Public`
/// are routing-level sources (the conversation's default employee and
/// `[channels] public_trigger`); [`EffectiveAccess::level`] never returns
/// them, the channel router adds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantSource {
    Admin,
    Direct(AccessLevel),
    Team { via_user: String, depth: u8 },
    Default,
    Public,
}

impl GrantSource {
    /// Audit label: `admin`, `binding`, `team:<user>:<depth>`, `default`, `public`.
    pub fn label(&self) -> String {
        match self {
            Self::Admin => "admin".to_string(),
            Self::Direct(_) => "binding".to_string(),
            Self::Team { via_user, depth } => format!("team:{via_user}:{depth}"),
            Self::Default => "default".to_string(),
            Self::Public => "public".to_string(),
        }
    }
}

/// One caller's agent access at one ACL generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveAccess {
    pub user_id: String,
    pub role: UserRole,
    pub direct: BTreeMap<String, AccessLevel>,
    pub team: BTreeMap<String, TeamGrant>,
    pub generation: u64,
}

impl EffectiveAccess {
    /// The strongest grant for `agent`. A direct binding wins a tie.
    pub fn level(&self, agent: &str) -> Option<(AccessLevel, GrantSource)> {
        if self.role == UserRole::Admin {
            return Some((AccessLevel::Owner, GrantSource::Admin));
        }
        let direct = self.direct.get(agent).copied();
        let team = self.team.get(agent);
        match (direct, team) {
            (Some(d), Some(t)) if t.level.level() > d.level() => Some((t.level, team_source(t))),
            (Some(d), _) => Some((d, GrantSource::Direct(d))),
            (None, Some(t)) => Some((t.level, team_source(t))),
            (None, None) => None,
        }
    }

    /// May talk to `agent` (any level).
    pub fn may_call(&self, agent: &str) -> bool {
        self.level(agent).is_some()
    }

    /// May hand `agent` a task (Operator or above).
    pub fn may_assign(&self, agent: &str) -> bool {
        self.level(agent)
            .is_some_and(|(level, _)| level.at_least(AccessLevel::Operator))
    }
}

fn team_source(t: &TeamGrant) -> GrantSource {
    GrantSource::Team {
        via_user: t.via_user.clone(),
        depth: t.depth,
    }
}

/// What a manager gets from a subordinate's binding: `min(level, Operator)`.
pub fn derived_team_level(subordinate: AccessLevel) -> AccessLevel {
    if subordinate.at_least(AccessLevel::Operator) {
        AccessLevel::Operator
    } else {
        subordinate
    }
}

/// Everyone below `?1`, at most `?2` levels down, with the depth of each
/// visit. Offboarded people are not part of the tree; the walk only goes on
/// through people whose role can manage (a stray Employee with reports, which
/// writes refuse, relays nothing). Suspended managers are walked through.
const TEAM_CTE: &str = "WITH RECURSIVE team(id, depth) AS (
    SELECT u.id, 1 FROM users u
     WHERE u.manager_user_id = ?1 AND u.id <> ?1 AND u.status <> 'offboarded'
    UNION
    SELECT u.id, t.depth + 1 FROM team t
      JOIN users p ON p.id = t.id
      JOIN users u ON u.manager_user_id = t.id
     WHERE t.depth < ?2 AND u.id <> ?1 AND u.status <> 'offboarded'
       AND p.role IN ('manager', 'admin'))";

/// Fold the team's bindings into one grant per agent: highest derived level,
/// then the nearest subordinate, then the smallest user id (deterministic).
///
/// Public for read-only callers that hold their own connection (the
/// gateway's task-content reader opens `users.db` read-only). The caller
/// decides whether `manager_id` may have a team at all (Active and role at
/// least Manager); this function only walks the tree. On `Err` the caller
/// must narrow.
pub fn team_grants_on(
    conn: &Connection,
    manager_id: &str,
) -> Result<BTreeMap<String, TeamGrant>, String> {
    let sql = format!(
        "{TEAM_CTE}
         SELECT t.id, MIN(t.depth), b.agent_name, b.access_level
           FROM team t JOIN user_agent_bindings b ON b.user_id = t.id
          GROUP BY t.id, b.agent_name"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("team query error: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![manager_id, MAX_ORG_DEPTH], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| format!("team query error: {e}"))?;
    let mut grants: BTreeMap<String, TeamGrant> = BTreeMap::new();
    for row in rows {
        let (via_user, depth, agent, level_str) = row.map_err(|e| format!("team row error: {e}"))?;
        let Ok(level) = level_str.parse::<AccessLevel>() else {
            // Narrow: a binding we cannot read grants nothing.
            warn!(agent = %agent, "skipping team binding with a corrupt access level");
            continue;
        };
        let grant = TeamGrant {
            level: derived_team_level(level),
            via_user,
            depth: u8::try_from(depth).unwrap_or(MAX_ORG_DEPTH),
        };
        match grants.entry(agent) {
            Entry::Vacant(slot) => {
                slot.insert(grant);
            }
            Entry::Occupied(mut slot) => {
                if grant_rank(&grant) < grant_rank(slot.get()) {
                    slot.insert(grant);
                }
            }
        }
    }
    Ok(grants)
}

fn grant_rank(g: &TeamGrant) -> (std::cmp::Reverse<u8>, u8, String) {
    (std::cmp::Reverse(g.level.level()), g.depth, g.via_user.clone())
}

/// Active reports who hold a direct Operator or Owner binding on `agent`.
/// This is a bounded database query, not an authorization decision: callers
/// must first verify the viewer's live status, role, edition and settings.
/// A corrupt binding grants no visibility; a database read failure is an error.
pub fn overseen_reports_on(
    conn: &Connection,
    manager_id: &str,
    agent: &str,
) -> Result<Vec<OverseenReport>, String> {
    let sql = format!(
        "{TEAM_CTE}
         SELECT t.id, u.display_name, MIN(t.depth), b.access_level
           FROM team t JOIN users u ON u.id = t.id
           JOIN user_agent_bindings b ON b.user_id = t.id
          WHERE u.status = 'active' AND b.agent_name = ?3
          GROUP BY t.id, u.display_name, b.access_level
          ORDER BY MIN(t.depth), t.id"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("oversight query error: {e}"))?;
    let rows = stmt.query_map(rusqlite::params![manager_id, MAX_ORG_DEPTH, agent], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u8>(2)?,
            row.get::<_, String>(3)?,
        ))
    }).map_err(|e| format!("oversight query error: {e}"))?;
    let mut reports = Vec::new();
    for row in rows {
        let (user_id, display_name, depth, raw_level) =
            row.map_err(|e| format!("oversight row error: {e}"))?;
        let Ok(level) = raw_level.parse::<AccessLevel>() else {
            continue;
        };
        if level.at_least(AccessLevel::Operator) {
            reports.push(OverseenReport { user_id, display_name, depth, level });
        }
    }
    Ok(reports)
}

pub(crate) fn direct_bindings(conn: &Connection, user_id: &str) -> Result<BTreeMap<String, AccessLevel>, String> {
    let mut stmt = conn
        .prepare("SELECT agent_name, access_level FROM user_agent_bindings WHERE user_id = ?1")
        .map_err(|e| format!("binding query error: {e}"))?;
    let rows = stmt
        .query_map([user_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("binding query error: {e}"))?;
    let mut direct = BTreeMap::new();
    for row in rows {
        let (agent, level_str) = row.map_err(|e| format!("binding row error: {e}"))?;
        match level_str.parse::<AccessLevel>() {
            Ok(level) => {
                direct.insert(agent, level);
            }
            Err(_) => warn!(agent = %agent, "skipping binding with a corrupt access level"),
        }
    }
    Ok(direct)
}

/// Read the user row, bindings, team grants and generation from one read
/// snapshot. `Ok(None)` when the user does not exist or is not Active.
fn load_access(conn: &mut Connection, user_id: &str) -> Result<Option<(User, EffectiveAccess)>, String> {
    let tx = conn.transaction().map_err(|e| format!("read error: {e}"))?;
    let generation = read_acl_generation(&tx)?;
    let Some(user) = crate::user_row::load_user(&tx, user_id)? else {
        return Ok(None);
    };
    if user.status != UserStatus::Active {
        return Ok(None);
    }
    let direct = direct_bindings(&tx, user_id)?;
    // Q2: only someone whose role can manage gets anything from a team.
    let team = if user.role.at_least(UserRole::Manager) {
        team_grants_on(&tx, user_id)?
    } else {
        BTreeMap::new()
    };
    tx.finish().map_err(|e| format!("read error: {e}"))?;
    let access = EffectiveAccess {
        user_id: user.id.clone(),
        role: user.role,
        direct,
        team,
        generation,
    };
    Ok(Some((user, access)))
}

impl UserDb {
    /// All qualifying people for a manager's read-only oversight of `agent`.
    pub fn overseen_reports(&self, manager_id: &str, agent: &str) -> Result<Vec<OverseenReport>, String> {
        overseen_reports_on(&self.conn(), manager_id, agent)
    }

    /// Current ACL generation (bumped by every authorization-relevant write).
    pub fn acl_generation(&self) -> Result<u64, String> {
        let conn = self.conn();
        read_acl_generation(&conn)
    }

    /// Effective agent access for `user_id`. `Ok(None)` for a missing or
    /// non-Active user. On `Err` callers must narrow (no bindings, not Admin).
    pub fn effective_access(&self, user_id: &str) -> Result<Option<EffectiveAccess>, String> {
        let mut conn = self.conn();
        Ok(load_access(&mut conn, user_id)?.map(|(_, access)| access))
    }

    /// A [`UserContext`] rebuilt from the database: role, direct bindings,
    /// team grants, forced-password-change flag and generation, all from one
    /// snapshot. `Ok(None)` for a missing or non-Active user.
    pub fn live_user_context(&self, user_id: &str) -> Result<Option<UserContext>, String> {
        let mut conn = self.conn();
        Ok(load_access(&mut conn, user_id)?.map(|(user, access)| UserContext::from_live(&user, &access)))
    }

    /// Everyone below `manager_id` (Offboarded excluded), at most
    /// `max_depth` levels down (clamped to [`MAX_ORG_DEPTH`]), with the
    /// nearest depth at which each was found, ordered by depth then name.
    pub fn team_members(&self, manager_id: &str, max_depth: u8) -> Result<Vec<(User, u8)>, String> {
        let depth_cap = max_depth.clamp(1, MAX_ORG_DEPTH);
        let conn = self.conn();
        let cols = USER_COLUMNS
            .split(", ")
            .map(|c| format!("u.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "{TEAM_CTE}
             SELECT {cols}, m.depth FROM
               (SELECT id, MIN(depth) AS depth FROM team GROUP BY id) m
               JOIN users u ON u.id = m.id
              ORDER BY m.depth, u.display_name, u.id"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("team query error: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![manager_id, depth_cap], |row| {
                Ok((user_from_row(row)?, row.get::<_, i64>(12)?))
            })
            .map_err(|e| format!("team query error: {e}"))?;
        let mut members = Vec::new();
        for row in rows {
            let (user, depth) = row.map_err(|e| format!("team row error: {e}"))?;
            members.push((user, u8::try_from(depth).unwrap_or(MAX_ORG_DEPTH)));
        }
        Ok(members)
    }
}

#[cfg(test)]
#[path = "org_tests.rs"]
mod tests;
