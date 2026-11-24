//! Owner/avatar-only imports. Every authority read is repeated under the
//! source insertion's IMMEDIATE transaction, including the attached users DB.
use std::path::Path;
use duduclaw_knowledge::{KbError, KbResult};
use duduclaw_knowledge::requester::RequesterIdentity;
use rusqlite::{Connection, Transaction, params};

pub const HOURLY_CAP: i64 = 10;

#[derive(Clone)]
pub(crate) struct Turn {
    pub agent: String,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
}
impl Turn {
    pub fn requester(&self) -> RequesterIdentity {
        super::turn_requesters::resolve_with_run(&self.agent, self.turn_id.as_deref(), self.run_id.as_deref())
    }
}

pub(crate) fn human_id(who: &RequesterIdentity) -> Option<&str> {
    match who {
        RequesterIdentity::DashboardUser { user_id }
        | RequesterIdentity::ChannelUser { user_id: Some(user_id), .. } => Some(user_id),
        _ => None,
    }
}

pub(crate) fn attach_users(conn: &Connection, home: &Path) -> KbResult<()> {
    // Never create an empty identity database as a side effect of authorization.
    let path = home.join("users.db");
    if !path.is_file() { return Err(KbError::PermissionDenied); }
    conn.execute("ATTACH DATABASE ?1 AS self_add_users", [path.to_string_lossy().as_ref()])?;
    Ok(())
}

pub(crate) fn is_direct_owner(conn: &Connection, cid: &str, uid: &str) -> KbResult<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM collection_acl
        WHERE collection_id=?1 AND principal_kind='user' AND principal_id=?2 AND role='owner')",
        params![cid, uid], |r| r.get(0))?)
}

/// Administrator status never substitutes for the collection's direct Owner.
pub(crate) fn may_set_opt_in(tx: &Transaction<'_>, home: &Path, cid: &str, uid: &str) -> KbResult<()> {
    let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM self_add_users.users WHERE id=?1 AND status='active')",
        [uid], |r| r.get(0))?;
    if !active || !duduclaw_knowledge::limits::KnowledgeConfig::from_home(home).owner_self_add
        || !is_direct_owner(tx, cid, uid)? { return Err(KbError::PermissionDenied); }
    Ok(())
}

fn exact_shape(conn: &Connection, cid: &str, uid: &str, agent: &str) -> KbResult<bool> {
    Ok(conn.query_row("SELECT COUNT(*)=2 AND SUM(CASE
        WHEN principal_kind='user' AND principal_id=?2 AND role='owner' THEN 1
        WHEN principal_kind='agent' AND principal_id=?3 AND role='viewer' THEN 1 ELSE 0 END)=2
        FROM collection_acl WHERE collection_id=?1", params![cid, uid, agent], |r| r.get(0))?)
}

/// Reserve the rate slot and audit in the source transaction. Any read error
/// propagates to the caller, which files an ordinary approval instead.
pub(crate) fn authorize(
    tx: &Transaction<'_>, home: &Path, turn: &Turn, cid: &str, summary: &str,
) -> KbResult<()> {
    let cfg = duduclaw_knowledge::limits::KnowledgeConfig::from_home(home);
    if !cfg.enabled || !cfg.owner_self_add { return Err(KbError::PermissionDenied); }
    let who = turn.requester();
    let uid = human_id(&who).ok_or(KbError::PermissionDenied)?;
    let opted: bool = tx.query_row("SELECT owner_self_add=1 FROM collections WHERE id=?1", [cid], |r| r.get(0))?;
    if !opted || !exact_shape(tx, cid, uid, &turn.agent)? { return Err(KbError::PermissionDenied); }
    let avatar: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM self_add_users.users u
        WHERE u.id=?1 AND u.status='active' AND u.avatar_agent_id=?2
        AND EXISTS(SELECT 1 FROM self_add_users.user_agent_bindings b
            WHERE b.user_id=u.id AND b.agent_name=?2 AND b.access_level IN ('owner','operator'))
        AND NOT EXISTS(SELECT 1 FROM self_add_users.users other WHERE other.id<>u.id AND other.avatar_agent_id=?2))",
        params![uid, turn.agent], |r| r.get(0))?;
    if !avatar { return Err(KbError::PermissionDenied); }
    // Re-resolve a channel binding as well as the registered turn's snapshot.
    if let RequesterIdentity::ChannelUser { channel, sender, .. } = &who {
        let mapped = crate::decision_notify::open_user_db_read_only(home).ok().flatten()
            .and_then(|db| crate::googlechat_identity::verified_user_id(home, &db, channel, sender).ok().flatten());
        if mapped.as_deref() != Some(uid) { return Err(KbError::PermissionDenied); }
    }
    let now = chrono::Utc::now().timestamp();
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM owner_self_add_log WHERE collection_id=?1 AND imported_at>?2",
        params![cid, now - 3600], |r| r.get(0))?;
    if count >= HOURLY_CAP { return Err(KbError::PermissionDenied); }
    tx.execute("INSERT INTO owner_self_add_log(collection_id, actor_ai, owner_id, source_summary, imported_at)
        VALUES(?1, ?2, ?3, ?4, ?5)", params![cid, turn.agent, uid, duduclaw_core::truncate_chars(summary, 200), now])?;
    Ok(())
}

#[cfg(test)]
#[path = "owner_self_add_tests.rs"]
mod tests;
