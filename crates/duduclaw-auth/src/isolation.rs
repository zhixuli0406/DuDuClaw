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

#[derive(Debug, Clone)]
pub struct IsolationAuthoritySnapshot {
    pub tenant_id: String,
    pub acl_generation: u64,
    pub policy_epoch: u64,
    /// None is a background run. It never creates or impersonates a user.
    pub human: Option<UserContext>,
}

/// Read-only, including when the supplied connection was opened read-only.
/// A human needs a completed login and current Operator-or-higher access.
/// Callers separately prove the authenticated session/credential epoch and
/// immutable employee allocation; these database facts never authenticate a
/// user ID supplied by an employee.
pub fn read_isolation_authority(conn: &mut Connection, user_id: Option<&str>, agent: &str)
    -> Result<IsolationAuthoritySnapshot, String> {
    if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
        || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
        return Err("invalid isolation agent".into());
    }
    let tx = conn.transaction().map_err(|_| "isolation authority unavailable".to_string())?;
    let tenant_id = read_tenant(&tx)?;
    let acl_generation = crate::user_row::read_acl_generation(&tx)?;
    let policy_epoch = read_policy_epoch(&tx)?;
    let human = if let Some(user_id) = user_id {
        let (user, access) = crate::org::load_access_on(&tx, user_id)?
            .ok_or_else(|| "isolation principal denied".to_string())?;
        if user.must_change_password || user.last_login.as_ref().is_none_or(|v| v.is_empty())
            || !access.may_assign(agent) {
            return Err("isolation principal denied".into());
        }
        Some(UserContext::from_live(&user, &access))
    } else { None };
    tx.finish().map_err(|_| "isolation authority unavailable".to_string())?;
    Ok(IsolationAuthoritySnapshot { tenant_id, acl_generation, policy_epoch, human })
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
