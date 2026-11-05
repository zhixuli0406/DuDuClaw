use super::*;

/// A corrupt/missing policy is bounded by the default, never unlimited.
pub(super) fn days(conn: &Connection) -> u16 {
    read_days(conn).unwrap_or(DEFAULT_DAYS)
}

fn read_days(conn: &Connection) -> rusqlite::Result<u16> {
    let mut statement = conn.prepare("SELECT id,days FROM remembered_policy_state")?;
    let mut rows = statement.query([])?;
    let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let id: i64 = row.get(0)?;
    let days: u16 = row.get(1)?;
    if id != 1 || !(1..=365).contains(&days) || rows.next()?.is_some() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(days)
}

fn admin(conn: &Connection, actor: &str, epoch: u64) -> Result<()> {
    let user = crate::user_row::load_user(conn, actor)?.ok_or(DENIED)?;
    if user.role != crate::UserRole::Admin || user.status != crate::UserStatus::Active
        || user.credential_epoch != epoch || user.must_change_password {
        return Err(DENIED.into());
    }
    Ok(())
}

impl UserDb {
    pub fn remember_days(&self) -> u16 { days(&self.conn()) }

    /// Read current authority and policy in the same identity-store snapshot.
    pub fn remember_admin_policy(&self, actor: &str, epoch: u64, value: Option<u16>) -> Result<u16> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let check = admin(&tx, actor, epoch).and_then(|()| {
            if value.is_some_and(|v| !(1..=365).contains(&v)) { Err("保持登入天數必須介於 1 至 365 天。".into()) } else { Ok(()) }
        });
        if let Err(error) = check {
            audit(&tx, actor, "session.policy_refused", "remember_days")?;
            tx.commit().map_err(|e|e.to_string())?;
            return Err(error);
        }
        if let Some(value) = value {
            tx.execute("INSERT INTO remembered_policy_state(id,days) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET days=excluded.days", [value]).map_err(|e|e.to_string())?;
            if read_days(&tx).map_err(|e|e.to_string())? != value {
                return Err("無法儲存登入設定。".into());
            }
            audit(&tx, actor, "session.policy_updated", &value.to_string())?;
        }
        let result = days(&tx);
        tx.commit().map_err(|e|e.to_string())?;
        Ok(result)
    }

    /// Epoch invalidation covers ordinary JWTs and already-connected sockets;
    /// remembered revocation is explicit as well as enforced by the trigger.
    pub fn remember_admin_revoke_all(&self, actor: &str, epoch: u64, target: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let target_user = if target.is_empty() || target.len() > 256 { None }
            else { crate::user_row::load_user(&tx, target)? };
        let check = admin(&tx, actor, epoch).and_then(|()| {
            target_user.as_ref().ok_or_else(|| "找不到此帳號。".to_owned()).map(|_| ())
        });
        if let Err(error) = check {
            let audit_target = target_user.as_ref().map(|u|u.id.as_str()).unwrap_or("invalid_target");
            audit(&tx, actor, "session.admin_revoke_refused", audit_target)?;
            tx.commit().map_err(|e|e.to_string())?;
            return Err(error);
        }
        tx.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [target]).map_err(|e|e.to_string())?;
        tx.execute("UPDATE remembered_families SET revoked=1 WHERE user_id=?1", [target]).map_err(|e|e.to_string())?;
        crate::user_row::bump_acl_generation(&tx).map_err(|e|e.to_string())?;
        audit(&tx, actor, "session.admin_revoked", target)?;
        tx.commit().map_err(|e|e.to_string())
    }
}
