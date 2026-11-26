//! D4: narrow, audited Admin reads of CURRENT Personal avatars. This is not
//! a role-wide ACL exception: only named read methods install request grants.
use super::{DataCategory, USERS_DB};
use std::path::Path;
use crate::fs_safe::sqlite::{open_existing, Access};

pub(crate) const AVATAR_READ_ACTION: &str = "isolation.avatar_data_read";

/// Categories also contain writes; never grant by category or prefix alone.
pub(crate) fn avatar_read_category(method: &str) -> Option<DataCategory> {
    Some(match method {
        "memory.search" | "memory.browse" | "memory.key_facts" | "memory.decay_overview"
        | "memory.history" | "memory.at" | "memory.get_at" | "memory.graph"
        | "push:memory.changed" => DataCategory::Memory,
        "chat.sessions.list" | "chat.sessions.history" | "search.query"
        | "push:chat.sessions.updated" => DataCategory::Sessions,
        _ => return None,
    })
}

fn member_in(conn: &rusqlite::Connection, user: &str, agent: &str) -> rusqlite::Result<Option<String>> {
    if user.is_empty() || user == "system" { return Ok(None); }
    let admin: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM users
        WHERE id = ?1 AND role = 'admin' AND status = 'active')", [user], |r| r.get(0))?;
    if !admin { return Ok(None); }
    duduclaw_auth::avatar::current_personal_avatar_member_in(conn, agent)
}

pub(crate) fn current_admin_avatar_member(home: &Path, user: &str, agent: &str) -> Option<String> {
    let conn = open_existing(home, USERS_DB, Access::ReadOnly).ok()?;
    member_in(&conn, user, agent).ok().flatten()
}

/// Ok(None) means use the old rules (including any authority read failure).
/// The authority check and mandatory audit commit in one transaction. A failed
/// audit never grants access. No content, search text or credentials are logged.
pub(crate) fn audit_avatar_read(home: &Path, user: &str, agent: &str, method: &str)
    -> Result<Option<String>, &'static str> {
    let Some(category) = avatar_read_category(method) else { return Ok(None); };
    let Ok(mut conn) = open_existing(home, USERS_DB, Access::ReadWrite) else { return Ok(None); };
    let Ok(tx) = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) else { return Ok(None); };
    let Some(member) = member_in(&tx, user, agent).ok().flatten() else { return Ok(None); };
    let detail = serde_json::json!({
        "admin_user_id": user, "employee": agent, "avatar_agent_id": agent,
        "owner_user_id": member, "member_user_id": member,
        "category": category.as_str(), "method": method, "authorization": "current_avatar",
    }).to_string();
    const FAILED: &str = "無法寫入稽核紀錄，已拒絕讀取。";
    tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp)
        VALUES(?1, ?2, ?3, ?4, ?5)", rusqlite::params![user, AVATAR_READ_ACTION, agent, detail,
            chrono::Utc::now().to_rfc3339()]).map_err(|_| FAILED)?;
    tx.commit().map_err(|_| FAILED)?;
    Ok(Some(member))
}
