//! Fresh isolation authority from the existing users database. A tenant is
//! this database's installation domain, not a caller-selected tenant claim.
use rusqlite::{Connection, params};
use crate::{UserContext, UserDb};

const TENANT_KEY: &str = "isolation_tenant_id";

pub(crate) fn initialize_tenant(conn: &Connection) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "isolation authority initialization unavailable".to_string())?;
    let (tenant_exists, epoch_exists): (bool, bool) = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_meta WHERE key=?1),
                EXISTS(SELECT 1 FROM auth_meta WHERE key=?2)",
        params![TENANT_KEY, duduclaw_core::isolation_policy::POLICY_EPOCH_KEY],
        |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|_| "isolation authority initialization unavailable".to_string())?;
    match (tenant_exists, epoch_exists) {
        (false, false) => {
            // The existing non-STRICT INTEGER-affinity value column preserves
            // canonical UUID text. Both keys are born in the same transaction.
            tx.execute("INSERT INTO auth_meta(key,value) VALUES(?1,?2)",
                params![TENANT_KEY, uuid::Uuid::new_v4().to_string()])
                .map_err(|_| "isolation tenant unavailable".to_string())?;
            tx.execute("INSERT INTO auth_meta(key,value) VALUES(?1,1)",
                [duduclaw_core::isolation_policy::POLICY_EPOCH_KEY])
                .map_err(|_| "isolation policy epoch unavailable".to_string())?;
        }
        (true, true) => {}
        _ => return Err("isolation authority initialization is incomplete".into()),
    }
    // Never repair one missing key: resetting an epoch could revive a run.
    read_tenant(&tx)?;
    read_policy_epoch(&tx)?;
    tx.commit().map_err(|_| "isolation authority initialization unavailable".to_string())
}

fn read_policy_epoch(conn: &Connection) -> Result<u64, String> {
    conn.query_row("SELECT value FROM auth_meta WHERE key=?1 AND typeof(value)='integer' AND value>0",
        [duduclaw_core::isolation_policy::POLICY_EPOCH_KEY], |row| row.get(0))
        .map_err(|_| "isolation policy epoch unavailable".to_string())
}

fn read_tenant(conn: &Connection) -> Result<String, String> {
    let value: String = conn.query_row("SELECT value FROM auth_meta WHERE key=?1", [TENANT_KEY], |row| row.get(0))
        .map_err(|_| "isolation tenant unavailable".to_string())?;
    let id = uuid::Uuid::parse_str(&value).map_err(|_| "invalid isolation tenant".to_string())?;
    if id.is_nil() || id.get_version_num() != 4 || id.get_variant() != uuid::Variant::RFC4122 || id.to_string() != value {
        return Err("invalid isolation tenant".into());
    }
    Ok(value)
}

/// The installation tenant alone, validated exactly as in
/// [`read_isolation_authority`]. Never initializes or repairs a missing key.
pub fn read_isolation_tenant(conn: &Connection) -> Result<String, String> {
    read_tenant(conn)
}

#[derive(Debug, Clone)]
pub struct IsolationAuthoritySnapshot {
    pub tenant_id: String,
    pub acl_generation: u64,
    pub policy_epoch: u64,
    /// None is a background run. It never creates or impersonates a user.
    pub human: Option<UserContext>,
    /// Owner recorded in the `isolation_personal_agents` mirror; None means
    /// the agent has no Personal mirror row. Evidence only: callers compare it
    /// with the confirmed employee lifecycle and never trust it alone.
    pub personal_owner: Option<String>,
    /// Number of users bound to the agent (any access level), read in the
    /// same transaction as every other field.
    pub agent_member_count: u32,
}

/// Read-only, including when the supplied connection was opened read-only.
/// A human needs a completed login and current Operator-or-higher access.
/// Callers separately prove the authenticated session/credential epoch and
/// immutable employee allocation; these database facts never authenticate a
/// user ID supplied by an employee.
pub fn read_isolation_authority(conn: &mut Connection, user_id: Option<&str>, agent: &str)
    -> Result<IsolationAuthoritySnapshot, String> {
    let tx = conn.transaction().map_err(|_| "isolation authority unavailable".to_string())?;
    let snapshot = read_isolation_authority_in(&tx, user_id, agent)?;
    tx.finish().map_err(|_| "isolation authority unavailable".to_string())?;
    Ok(snapshot)
}

/// [`read_isolation_authority`] inside a transaction the caller already
/// holds, so further reads (e.g. a Personal owner's account row) share the
/// same snapshot. Refuses when `tx` is in autocommit mode (no transaction):
/// separate statements would then see different database states.
pub fn read_isolation_authority_in(tx: &Connection, user_id: Option<&str>, agent: &str)
    -> Result<IsolationAuthoritySnapshot, String> {
    if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
        || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
        return Err("invalid isolation agent".into());
    }
    if tx.is_autocommit() { return Err("isolation authority unavailable".into()); }
    let tenant_id = read_tenant(tx)?;
    let acl_generation = crate::user_row::read_acl_generation(tx)?;
    let policy_epoch = read_policy_epoch(tx)?;
    // A missing/unreadable mirror is unavailable, never "not Personal".
    let personal_owner = crate::db::personal_agent_owner(tx, agent)
        .map_err(|_| "isolation authority unavailable".to_string())?;
    let agent_member_count: i64 = tx.query_row(
        "SELECT count(*) FROM user_agent_bindings WHERE agent_name=?1", [agent], |row| row.get(0))
        .map_err(|_| "isolation authority unavailable".to_string())?;
    let agent_member_count = u32::try_from(agent_member_count)
        .map_err(|_| "isolation authority unavailable".to_string())?;
    let human = if let Some(user_id) = user_id {
        let (user, access) = crate::org::load_access_on(tx, user_id)?
            .ok_or_else(|| "isolation principal denied".to_string())?;
        if user.must_change_password || user.last_login.as_ref().is_none_or(|v| v.is_empty())
            || !access.may_assign(agent) {
            return Err("isolation principal denied".into());
        }
        Some(UserContext::from_live(&user, &access))
    } else { None };
    Ok(IsolationAuthoritySnapshot { tenant_id, acl_generation, policy_epoch, human,
        personal_owner, agent_member_count })
}

impl UserDb {
    pub fn isolation_authority_snapshot(&self, user_id: Option<&str>, agent: &str)
        -> Result<IsolationAuthoritySnapshot, String> {
        read_isolation_authority(&mut self.conn(), user_id, agent)
    }
}

#[cfg(test)]
#[path = "isolation_tests.rs"]
mod tests;
