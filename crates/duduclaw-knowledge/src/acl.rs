//! Collection permissions. Default deny; exact-match comparison only.
//!
//! A grant is `(principal kind, principal id, role)`:
//!
//! | kind | id | roles | who |
//! |---|---|---|---|
//! | `user` | dashboard user id | viewer / editor / owner | that person |
//! | `department` | department name | viewer / editor | people and AI employees of that department |
//! | `agent` | AI employee id | viewer | that AI employee |
//! | `channel` | `line` or `line:<chat>` | viewer | **anyone** talking in that channel / chat |
//!
//! [`effective_collections`] is the one entry point every read path uses. It
//! answers `AgentSide ∩ HumanSide ∩ SensitivityFilter ∩ Narrowing`, computed
//! in SQL; any SQL error is returned as an error and callers deny.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{KbError, KbResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    User,
    Agent,
    Department,
    Channel,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::User => "user",
            PrincipalKind::Agent => "agent",
            PrincipalKind::Department => "department",
            PrincipalKind::Channel => "channel",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "user" => PrincipalKind::User,
            "agent" => PrincipalKind::Agent,
            "department" => PrincipalKind::Department,
            "channel" => PrincipalKind::Channel,
            _ => return None,
        })
    }

    /// Roles this kind of principal may hold.
    pub fn allows(self, role: Role) -> bool {
        match self {
            PrincipalKind::User => true,
            PrincipalKind::Department => role != Role::Owner,
            PrincipalKind::Agent | PrincipalKind::Channel => role == Role::Viewer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Editor,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "viewer" => Role::Viewer,
            "editor" => Role::Editor,
            "owner" => Role::Owner,
            _ => return None,
        })
    }
}

/// One grant as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclEntry {
    pub kind: PrincipalKind,
    pub id: String,
    pub role: Role,
    #[serde(default)]
    pub granted_by: String,
    #[serde(default)]
    pub granted_at: String,
}

/// How the human side of a request is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HumanMode {
    /// User, department and channel grants count.
    Full,
    /// Only channel grants count (unidentified sender).
    ChannelOnly,
    /// No person: public / internal collections the agent side allows.
    InternalOnly,
}

/// The person (or absence of one) a request acts for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanCtx {
    pub mode: HumanMode,
    pub user_id: Option<String>,
    pub user_department: Option<String>,
    /// Channel name (`line`) — matches `channel:line` grants.
    pub channel: Option<String>,
    /// `<channel>:<chat>` — matches `channel:line:<chat>` grants.
    pub channel_chat: Option<String>,
    pub is_admin: bool,
    /// A 1:1 conversation (or a dashboard session). `false` excludes
    /// personal and restricted collections.
    pub is_private: bool,
}

impl HumanCtx {
    /// A dashboard user acting directly.
    pub fn dashboard(user_id: &str, department: Option<&str>, is_admin: bool) -> Self {
        Self {
            mode: HumanMode::Full,
            user_id: Some(user_id.to_string()),
            user_department: department.map(str::to_string).filter(|d| !d.is_empty()),
            channel: None,
            channel_chat: None,
            is_admin,
            is_private: true,
        }
    }

    /// Nobody may be served: every human-side test fails.
    pub fn nobody() -> Self {
        Self {
            mode: HumanMode::ChannelOnly,
            user_id: None,
            user_department: None,
            channel: None,
            channel_chat: None,
            is_admin: false,
            is_private: false,
        }
    }
}

/// `[knowledge] collections` of an AI employee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentNarrowing {
    /// No `[knowledge]` section or no `collections` key: every collection
    /// granted to the employee.
    All,
    /// Only these slugs (unknown ones are ignored). Empty = none.
    Only(Vec<String>),
}

/// The AI employee side of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCtx {
    pub agent_id: String,
    pub department: Option<String>,
    pub narrowing: AgentNarrowing,
}

/// The collections a request may read, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedSet {
    ids: Vec<String>,
}

impl AllowedSet {
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.binary_search_by(|x| x.as_str().cmp(id)).is_ok()
    }

    /// JSON array for `json_each(?)` in SQL.
    pub fn json(&self) -> String {
        serde_json::to_string(&self.ids).unwrap_or_else(|_| "[]".to_string())
    }

    pub(crate) fn from_ids(ids: impl IntoIterator<Item = String>) -> Self {
        let set: BTreeSet<String> = ids.into_iter().collect();
        Self {
            ids: set.into_iter().collect(),
        }
    }
}

/// Collection ids for slugs (exact). Unknown slugs are reported back.
pub fn ids_for_slugs(conn: &Connection, slugs: &[String]) -> KbResult<(Vec<String>, Vec<String>)> {
    let mut ids = Vec::new();
    let mut unknown = Vec::new();
    let mut stmt = conn.prepare("SELECT id FROM collections WHERE slug = ?1")?;
    for slug in slugs {
        match stmt
            .query_row(params![slug], |r| r.get::<_, String>(0))
            .optional()?
        {
            Some(id) => ids.push(id),
            None => unknown.push(slug.clone()),
        }
    }
    Ok((ids, unknown))
}

const EFFECTIVE_SQL: &str = "
WITH agent_ok AS (
  SELECT collection_id FROM collection_acl
   WHERE (principal_kind = 'agent' AND principal_id = ?1 AND role = 'viewer')
      OR (principal_kind = 'department' AND ?2 IS NOT NULL AND principal_id = ?2
          AND role IN ('viewer','editor'))),
human_ok AS (
  SELECT collection_id FROM collection_acl
   WHERE role IN ('viewer','editor','owner')
     AND (?9 = 'full' AND ((principal_kind = 'user' AND ?3 IS NOT NULL AND principal_id = ?3)
       OR (principal_kind = 'department' AND ?4 IS NOT NULL AND principal_id = ?4 AND role IN ('viewer','editor'))))
  UNION
  SELECT collection_id FROM collection_acl
   WHERE principal_kind = 'channel' AND role = 'viewer'
     AND ((?5 IS NOT NULL AND principal_id = ?5) OR (?6 IS NOT NULL AND principal_id = ?6)))
SELECT c.id FROM collections c
 WHERE (?1 IS NULL OR c.id IN agent_ok)
   AND ((?7 = 1 AND ?9 = 'full') OR c.id IN human_ok
        OR (?9 = 'internal_only' AND c.sensitivity IN ('public','internal')))
   AND (?8 = 1 OR c.sensitivity IN ('public','internal'))
 ORDER BY c.id";

/// Narrowing from a tool argument or a dashboard selection: only shrinks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Narrowing {
    /// Slugs or ids; `None` = no narrowing.
    pub only: Option<Vec<String>>,
}

/// The single permission entry point: which collections may this request
/// read? Errors mean "deny".
pub fn effective_collections(
    conn: &Connection,
    human: &HumanCtx,
    agent: Option<&AgentCtx>,
    narrowing: &Narrowing,
) -> KbResult<AllowedSet> {
    let mode = match human.mode {
        HumanMode::Full => "full",
        HumanMode::ChannelOnly => "none",
        HumanMode::InternalOnly => "internal_only",
    };
    let agent_id = agent.map(|a| a.agent_id.as_str());
    let agent_dept = agent
        .and_then(|a| a.department.as_deref())
        .filter(|d| !d.is_empty());
    let mut stmt = conn.prepare_cached(EFFECTIVE_SQL)?;
    let rows = stmt.query_map(
        params![
            agent_id,
            agent_dept,
            human.user_id.as_deref(),
            human.user_department.as_deref().filter(|d| !d.is_empty()),
            human.channel.as_deref(),
            human.channel_chat.as_deref(),
            human.is_admin as i64,
            human.is_private as i64,
            mode,
        ],
        |r| r.get::<_, String>(0),
    )?;
    let mut ids: Vec<String> = rows.collect::<Result<_, _>>()?;
    if let Some(AgentCtx {
        narrowing: AgentNarrowing::Only(slugs),
        ..
    }) = agent
    {
        let (allowed, unknown) = ids_for_slugs(conn, slugs)?;
        if !unknown.is_empty() {
            tracing::warn!(
                agent = agent_id.unwrap_or(""),
                ?unknown,
                "knowledge: unknown collection slugs in [knowledge] collections ignored"
            );
        }
        ids.retain(|id| allowed.contains(id));
    }
    if let Some(only) = narrowing.only.as_ref() {
        let (by_slug, _) = ids_for_slugs(conn, only)?;
        ids.retain(|id| by_slug.contains(id) || only.contains(id));
    }
    Ok(AllowedSet::from_ids(ids))
}

/// The employee side alone: collections granted to the employee or its
/// department, narrowed by its `[knowledge] collections`. Decides whether the
/// knowledge tools exist for the employee at all (listed and callable iff
/// non-empty); every read still goes through [`effective_collections`].
pub fn agent_side_collections(conn: &Connection, agent: &AgentCtx) -> KbResult<AllowedSet> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT collection_id FROM collection_acl
          WHERE (principal_kind = 'agent' AND principal_id = ?1 AND role = 'viewer')
             OR (principal_kind = 'department' AND ?2 IS NOT NULL AND principal_id = ?2
                 AND role IN ('viewer','editor'))
          ORDER BY collection_id",
    )?;
    let dept = agent.department.as_deref().filter(|d| !d.is_empty());
    let mut ids: Vec<String> = stmt
        .query_map(params![agent.agent_id, dept], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if let AgentNarrowing::Only(slugs) = &agent.narrowing {
        let (allowed, _) = ids_for_slugs(conn, slugs)?;
        ids.retain(|id| allowed.contains(id));
    }
    Ok(AllowedSet::from_ids(ids))
}

/// The role a dashboard user holds on a collection (Admin ⇒ owner of all).
/// Malformed rows are ignored, never upgraded.
pub fn user_role(
    conn: &Connection,
    collection_id: &str,
    user_id: &str,
    department: Option<&str>,
    is_admin: bool,
) -> KbResult<Option<Role>> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM collections WHERE id = ?1",
            params![collection_id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(None);
    }
    if is_admin {
        return Ok(Some(Role::Owner));
    }
    let mut stmt = conn.prepare_cached(
        "SELECT principal_kind, role FROM collection_acl WHERE collection_id = ?1
          AND ((principal_kind = 'user' AND principal_id = ?2)
            OR (principal_kind = 'department' AND ?3 IS NOT NULL AND principal_id = ?3))",
    )?;
    let rows = stmt.query_map(
        params![collection_id, user_id, department.filter(|d| !d.is_empty())],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    let mut best: Option<Role> = None;
    for row in rows {
        let (kind, role) = row?;
        let (Some(kind), Some(role)) = (PrincipalKind::parse(&kind), Role::parse(&role)) else {
            continue;
        };
        if !kind.allows(role) {
            continue;
        }
        best = best.max(Some(role));
    }
    Ok(best)
}

/// Require at least `min` on `collection_id`; anything else is
/// [`KbError::PermissionDenied`] (including a missing collection).
pub fn require_role(
    conn: &Connection,
    collection_id: &str,
    user_id: &str,
    department: Option<&str>,
    is_admin: bool,
    min: Role,
) -> KbResult<Role> {
    match user_role(conn, collection_id, user_id, department, is_admin)? {
        Some(r) if r >= min => Ok(r),
        _ => Err(KbError::PermissionDenied),
    }
}

/// Current grants of a collection.
pub fn get_acl(conn: &Connection, collection_id: &str) -> KbResult<(Vec<AclEntry>, i64)> {
    let revision: i64 = conn
        .query_row(
            "SELECT acl_revision FROM collections WHERE id = ?1",
            params![collection_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(KbError::PermissionDenied)?;
    let mut stmt = conn.prepare(
        "SELECT principal_kind, principal_id, role, granted_by, granted_at FROM collection_acl
          WHERE collection_id = ?1 ORDER BY principal_kind, principal_id",
    )?;
    let rows = stmt.query_map(params![collection_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get(3)?,
            r.get(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, id, role, granted_by, granted_at) = row?;
        if let (Some(kind), Some(role)) = (PrincipalKind::parse(&kind), Role::parse(&role)) {
            out.push(AclEntry {
                kind,
                id,
                role,
                granted_by,
                granted_at,
            });
        }
    }
    Ok((out, revision))
}

fn valid_channel_principal(id: &str) -> bool {
    let (channel, chat) = match id.split_once(':') {
        Some((c, rest)) => (c, Some(rest)),
        None => (id, None),
    };
    let channel_ok = !channel.is_empty()
        && channel.len() <= 32
        && channel
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    let chat_ok =
        chat.is_none_or(|c| !c.is_empty() && c.len() <= 256 && !c.chars().any(char::is_control));
    channel_ok && chat_ok
}

/// Validate one grant before it is written.
pub fn validate_entry(kind: PrincipalKind, id: &str, role: Role) -> KbResult<()> {
    if !kind.allows(role) {
        return Err(KbError::Invalid(match kind {
            PrincipalKind::Department => "部門只能設為檢視者或編輯者。".to_string(),
            _ => "AI 員工與通道只能設為檢視者。".to_string(),
        }));
    }
    let ok = match kind {
        PrincipalKind::User => {
            !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
        }
        PrincipalKind::Agent => duduclaw_core::is_valid_agent_id(id),
        PrincipalKind::Department => duduclaw_core::department::is_valid_department(id),
        PrincipalKind::Channel => valid_channel_principal(id),
    };
    if ok {
        Ok(())
    } else {
        Err(KbError::Invalid("授權對象的名稱格式不正確。".to_string()))
    }
}

/// Replace a collection's grants. `expected_revision` must match (else
/// [`KbError::Conflict`]); at least one user owner must remain.
pub fn set_acl(
    conn: &mut Connection,
    collection_id: &str,
    entries: &[(PrincipalKind, String, Role)],
    expected_revision: i64,
    actor: &str,
) -> KbResult<i64> {
    for (kind, id, role) in entries {
        validate_entry(*kind, id, *role)?;
    }
    let mut seen = BTreeSet::new();
    for (kind, id, _) in entries {
        if !seen.insert((*kind, id.as_str())) {
            return Err(KbError::Invalid("同一個授權對象不能出現兩次。".to_string()));
        }
    }
    if !entries
        .iter()
        .any(|(k, _, r)| *k == PrincipalKind::User && *r == Role::Owner)
    {
        return Err(KbError::Invalid("至少要保留一位擁有者。".to_string()));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let revision: i64 = tx
        .query_row(
            "SELECT acl_revision FROM collections WHERE id = ?1",
            params![collection_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(KbError::PermissionDenied)?;
    if revision != expected_revision {
        return Err(KbError::Conflict(
            "權限已被其他人修改，請重新載入後再儲存。".to_string(),
        ));
    }
    let now = crate::now_rfc3339();
    tx.execute(
        "DELETE FROM collection_acl WHERE collection_id = ?1",
        params![collection_id],
    )?;
    for (kind, id, role) in entries {
        tx.execute(
            "INSERT INTO collection_acl(collection_id, principal_kind, principal_id, role, granted_by, granted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![collection_id, kind.as_str(), id, role.as_str(), actor, now],
        )?;
    }
    tx.execute(
        "UPDATE collections SET acl_revision = acl_revision + 1, updated_at = ?2 WHERE id = ?1",
        params![collection_id, now],
    )?;
    tx.commit()?;
    Ok(revision + 1)
}

#[cfg(test)]
#[path = "acl_tests.rs"]
mod tests;
