//! Idempotent invitation-only account creation for a confirmed migration proposal.
//! The caller authenticates the Admin and binds the immutable proposal first.
use crate::{UserDb, User, UserRole, UserStatus};
use crate::user_row::{load_user, bump_acl_generation};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl UserDb {
    /// The operation receipt and user are committed in the same transaction. A
    /// matching email is never considered an idempotency receipt.
    pub fn migration_invite_user(&self, operation: &str, digest: &[u8; 32],
        email: &str, display_name: &str, role: UserRole) -> Result<User, String> {
        validate(operation, email, display_name)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(error)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS migration_user_receipts (
            operation TEXT PRIMARY KEY, digest BLOB NOT NULL, user_id TEXT NOT NULL,
            disabled INTEGER NOT NULL DEFAULT 0)").map_err(error)?;
        let receipt: Option<(Vec<u8>, String, bool)> = tx.query_row(
            "SELECT digest,user_id,disabled FROM migration_user_receipts WHERE operation=?1",
            [operation], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)?;
        if let Some((stored, id, disabled)) = receipt {
            if stored != digest { return Err(error(())); }
            let mut user = load_user(&tx, &id).map_err(error)?.ok_or_else(|| error(()))?;
            if user.email != email || user.display_name != display_name || user.role != role
                || !user.must_change_password || (!disabled && user.status != UserStatus::Active) {
                return Err(error(()));
            }
            if disabled {
                let empty:bool=tx.query_row("SELECT password_hash='' FROM users WHERE id=?1",[&id],|r|r.get(0)).map_err(error)?;
                if !empty || user.status!=UserStatus::Suspended {return Err(error(()));}
                tx.execute("UPDATE users SET status='active' WHERE id=?1",[&id]).map_err(error)?;
                tx.execute("UPDATE migration_user_receipts SET disabled=0 WHERE operation=?1",[operation]).map_err(error)?;
                bump_acl_generation(&tx).map_err(error)?;
                user=load_user(&tx,&id).map_err(error)?.ok_or_else(||error(()))?;
                tx.commit().map_err(error)?;
            }
            return Ok(user);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        // Same invitation-only representation as create_invited_user_with:
        // empty hash cannot authenticate; activation requires the ordinary invite.
        tx.execute("INSERT INTO users (id,email,display_name,password_hash,role,status,
            created_at,updated_at,must_change_password) VALUES (?1,?2,?3,'',?4,'active',?5,?5,1)",
            params![id, email, display_name, role.to_string(), now]).map_err(error)?;
        tx.execute("INSERT INTO migration_user_receipts(operation,digest,user_id) VALUES(?1,?2,?3)",
            params![operation, digest.as_slice(), id]).map_err(error)?;
        bump_acl_generation(&tx).map_err(error)?;
        let user = load_user(&tx, &id).map_err(error)?.ok_or_else(|| error(()))?;
        tx.commit().map_err(error)?;
        Ok(user)
    }

    pub fn migration_user_readback(&self, operation: &str, digest: &[u8; 32]) -> Result<User, String> {
        let conn = self.conn();
        let (stored, id, disabled): (Vec<u8>, String, bool) = conn.query_row(
            "SELECT digest,user_id,disabled FROM migration_user_receipts WHERE operation=?1",
            [operation], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(error)?;
        if stored != digest || disabled { return Err(error(())); }
        load_user(&conn, &id).map_err(error)?.ok_or_else(|| error(()))
    }

    /// Never deletes the account. Suspension and credential-epoch fencing revoke
    /// even a partially activated account; its operation remains tombstoned.
    pub fn migration_user_disable(&self, operation: &str, digest: &[u8; 32]) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(error)?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='migration_user_receipts')",
            [], |r| r.get(0)).map_err(error)?;
        if !exists { return Ok(()); }
        let receipt: Option<(Vec<u8>, String, bool)> = tx.query_row(
            "SELECT digest,user_id,disabled FROM migration_user_receipts WHERE operation=?1",
            [operation], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)?;
        let Some((stored, id, disabled)) = receipt else { return Ok(()); };
        if stored != digest { return Err(error(())); }
        if !disabled {
            tx.execute("UPDATE users SET status='suspended',credential_epoch=credential_epoch+1 WHERE id=?1", [&id]).map_err(error)?;
            tx.execute("UPDATE migration_user_receipts SET disabled=1 WHERE operation=?1", [operation]).map_err(error)?;
            bump_acl_generation(&tx).map_err(error)?;
        }
        let user = load_user(&tx, &id).map_err(error)?.ok_or_else(|| error(()))?;
        if user.status != UserStatus::Suspended { return Err(error(())); }
        tx.commit().map_err(error)
    }
}
fn error<T>(_: T) -> String { "migration account operation refused".into() }
fn validate(operation: &str, email: &str, display: &str) -> Result<(), String> {
    if operation.is_empty() || operation.len() > 96 || !operation.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        || crate::db::validate_account_email(email).ok() != Some(email)
        || display.trim().is_empty() || display.chars().count() > 200 || display.chars().any(char::is_control) {
        return Err(error(()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_invite_concurrent_connections_return_one_identity() {
        let home=tempfile::tempdir().unwrap();let path=home.path().join("users.db");
        let a=UserDb::new(&path).unwrap();let b=UserDb::new(&path).unwrap();
        let (left,right)=std::thread::scope(|s| {
            let left=s.spawn(||a.migration_invite_user("abcd-1",&[3;32],"x@example.test","X",UserRole::Employee).unwrap().id);
            let right=s.spawn(||b.migration_invite_user("abcd-1",&[3;32],"x@example.test","X",UserRole::Employee).unwrap().id);
            (left.join().unwrap(),right.join().unwrap())
        });
        assert_eq!(left,right);assert_eq!(a.list_users().unwrap().len(),1);
    }
    #[test]
    fn migration_invite_is_atomic_bound_and_cannot_claim_existing_email() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let a = db.migration_invite_user("abcd-1", &[1;32], "a@example.test", "A", UserRole::Admin).unwrap();
        let b = db.migration_invite_user("abcd-1", &[1;32], "a@example.test", "A", UserRole::Admin).unwrap();
        assert_eq!(a.id, b.id);
        assert!(a.must_change_password);
        assert!(db.migration_invite_user("abcd-1", &[2;32], "a@example.test", "A", UserRole::Admin).is_err());
        assert!(db.migration_invite_user("abcd-2", &[1;32], "a@example.test", "A", UserRole::Admin).is_err());
        assert_eq!(db.list_users().unwrap().len(), 1);
        db.migration_user_disable("abcd-1", &[1;32]).unwrap();
        db.migration_user_disable("abcd-1", &[1;32]).unwrap();
        assert_eq!(db.get_user(&a.id).unwrap().unwrap().status, UserStatus::Suspended);
        assert!(db.migration_user_readback("abcd-1", &[1;32]).is_err());
        assert_eq!(db.migration_invite_user("abcd-1", &[1;32], "a@example.test", "A", UserRole::Admin).unwrap().id,a.id);
        db.conn().execute("UPDATE users SET password_hash='activated' WHERE id=?1",[&a.id]).unwrap();
        db.migration_user_disable("abcd-1", &[1;32]).unwrap();
        assert!(db.migration_invite_user("abcd-1", &[1;32], "a@example.test", "A", UserRole::Admin).is_err());
    }
}

impl UserDb {
    /// Shared ACL binding only. The gateway verifies the confirmed Shared
    /// employee allocation; no Personal principal is inferred from legacy data.
    /// Normal binding writer and durable receipt share one immediate transaction.
    pub fn migration_bind_shared(&self, operation:&str, digest:&[u8;32], user_id:&str,
        agent:&str, access:crate::AccessLevel) -> Result<(),String> {
        validate_binding(operation,agent)?;
        let mut conn=self.conn();let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(error)?;
        binding_schema(&tx)?;
        let user=load_user(&tx,user_id).map_err(error)?.ok_or_else(||error(()))?;
        if user.status!=UserStatus::Active || user.must_change_password || user.last_login.is_none() {return Err(error(()));}
        let receipt:Option<(Vec<u8>,String,String,String,String,bool)>=tx.query_row(
            "SELECT digest,user_id,agent,access,bound_at,disabled FROM migration_binding_receipts WHERE operation=?1",[operation],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(error)?;
        if let Some((stored,owner,name,level,bound,disabled))=receipt {
            if stored!=digest || owner!=user_id || name!=agent || level!=access.to_string() {return Err(error(()));}
            let current=binding_current(&tx,user_id,agent)?;
            if !disabled {return if current==Some((level,bound)) {Ok(())}else{Err(error(()))};}
            if current.is_some(){return Err(error(()));}
        } else if binding_current(&tx,user_id,agent)?.is_some(){return Err(error(()));}
        let now=chrono::Utc::now().to_rfc3339();
        crate::db::bind_agent_in_transaction(&tx,user_id,agent,access,&now)?;
        tx.execute("INSERT INTO migration_binding_receipts(operation,digest,user_id,agent,access,bound_at,disabled)
            VALUES (?1,?2,?3,?4,?5,?6,0) ON CONFLICT(operation) DO UPDATE SET bound_at=excluded.bound_at,disabled=0",
            params![operation,digest.as_slice(),user_id,agent,access.to_string(),now]).map_err(error)?;
        tx.commit().map_err(error)
    }
    pub fn migration_binding_readback(&self,operation:&str,digest:&[u8;32])->Result<crate::UserAgentBinding,String>{
        let conn=self.conn();
        let (stored,user,agent,level,bound,disabled):(Vec<u8>,String,String,String,String,bool)=conn.query_row(
            "SELECT digest,user_id,agent,access,bound_at,disabled FROM migration_binding_receipts WHERE operation=?1",[operation],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).map_err(error)?;
        if stored!=digest||disabled||binding_current(&conn,&user,&agent)?!=Some((level.clone(),bound.clone())) {return Err(error(()));}
        let owner=load_user(&conn,&user).map_err(error)?.ok_or_else(||error(()))?;
        if owner.status!=UserStatus::Active||owner.must_change_password||owner.last_login.is_none(){return Err(error(()));}
        let primary=conn.query_row("SELECT is_primary FROM user_agent_bindings WHERE user_id=?1 AND agent_name=?2",params![user,agent],|r|r.get(0)).map_err(error)?;
        Ok(crate::UserAgentBinding{user_id:user,agent_name:agent,access_level:level.parse().map_err(error)?,bound_at:bound,is_primary:primary})
    }
    pub fn migration_binding_disable(&self,operation:&str,digest:&[u8;32])->Result<(),String>{
        let mut conn=self.conn();let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(error)?;
        binding_schema(&tx)?;
        let receipt:Option<(Vec<u8>,String,String,String,String)>=tx.query_row(
            "SELECT digest,user_id,agent,access,bound_at FROM migration_binding_receipts WHERE operation=?1",[operation],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(error)?;
        let Some((stored,user,agent,level,bound))=receipt else{return Ok(());};
        if stored!=digest{return Err(error(()));}
        if let Some(current)=binding_current(&tx,&user,&agent)? {
            if current!=(level,bound){return Err(error(()));}
            tx.execute("DELETE FROM user_agent_bindings WHERE user_id=?1 AND agent_name=?2",params![user,agent]).map_err(error)?;
            bump_acl_generation(&tx).map_err(error)?;
        }
        tx.execute("UPDATE migration_binding_receipts SET disabled=1 WHERE operation=?1",[operation]).map_err(error)?;
        tx.commit().map_err(error)
    }
}
fn binding_schema(conn:&rusqlite::Connection)->Result<(),String>{conn.execute_batch("CREATE TABLE IF NOT EXISTS migration_binding_receipts(
    operation TEXT PRIMARY KEY,digest BLOB NOT NULL,user_id TEXT NOT NULL,agent TEXT NOT NULL,access TEXT NOT NULL,bound_at TEXT NOT NULL,disabled INTEGER NOT NULL DEFAULT 0)").map_err(error)}
fn binding_current(conn:&rusqlite::Connection,user:&str,agent:&str)->Result<Option<(String,String)>,String>{
    conn.query_row("SELECT access_level,bound_at FROM user_agent_bindings WHERE user_id=?1 AND agent_name=?2",params![user,agent],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(error)
}
fn validate_binding(operation:&str,agent:&str)->Result<(),String>{
    if operation.is_empty()||operation.len()>96||!operation.bytes().all(|b|b.is_ascii_hexdigit()||b==b'-')
        ||agent.is_empty()||agent.len()>64||!agent.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_')){return Err(error(()));}Ok(())
}
#[cfg(test)] mod binding_tests {
    use super::*;
    #[test]
    fn migration_binding_requires_fresh_login_and_atomic_bound_receipt(){
        let dir=tempfile::tempdir().unwrap();let db=UserDb::new(&dir.path().join("users.db")).unwrap();
        let user=db.migration_invite_user("abc-1",&[1;32],"new@example.test","New",UserRole::Employee).unwrap();
        assert!(db.migration_bind_shared("def-1",&[2;32],&user.id,"sales",crate::AccessLevel::Viewer).is_err());
        db.conn().execute("UPDATE users SET must_change_password=0,last_login='now' WHERE id=?1",[&user.id]).unwrap();
        for _ in 0..2 {db.migration_bind_shared("def-1",&[2;32],&user.id,"sales",crate::AccessLevel::Viewer).unwrap();}
        assert_eq!(db.get_user_agents(&user.id).unwrap().len(),1);
        assert!(db.migration_bind_shared("def-1",&[3;32],&user.id,"sales",crate::AccessLevel::Viewer).is_err());
        assert!(db.migration_bind_shared("def-2",&[2;32],&user.id,"sales",crate::AccessLevel::Viewer).is_err());
        assert_eq!(db.migration_binding_readback("def-1",&[2;32]).unwrap().access_level,crate::AccessLevel::Viewer);
        db.migration_binding_disable("def-1",&[2;32]).unwrap();db.migration_binding_disable("def-1",&[2;32]).unwrap();
        assert!(db.get_user_agents(&user.id).unwrap().is_empty());
        db.migration_bind_shared("def-1",&[2;32],&user.id,"sales",crate::AccessLevel::Viewer).unwrap();
        db.bind_agent(&user.id,"sales",crate::AccessLevel::Owner).unwrap();
        assert!(db.migration_binding_disable("def-1",&[2;32]).is_err());
        assert_eq!(db.get_user_agents(&user.id).unwrap()[0].access_level,crate::AccessLevel::Owner);
    }
}
