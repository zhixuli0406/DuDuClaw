//! Email second factors. Only a successful code proof verifies an address.
use super::*;

pub const EMAIL_TTL: i64 = 600;

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS mfa_email_credentials (
        user_id TEXT PRIMARY KEY,email TEXT NOT NULL,verified_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS mfa_email_codes (
        token_hash TEXT PRIMARY KEY,user_id TEXT NOT NULL,email TEXT NOT NULL,
        code_hash TEXT NOT NULL,expires_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS mfa_email_sends (user_id TEXT NOT NULL,sent_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_mfa_email_sends ON mfa_email_sends(user_id,sent_at);
        CREATE TABLE IF NOT EXISTS mfa_factor_preferences (user_id TEXT PRIMARY KEY,last_used TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS mfa_factor_failures (user_id TEXT PRIMARY KEY,attempts INTEGER NOT NULL,window INTEGER NOT NULL);
        CREATE TRIGGER IF NOT EXISTS mfa_email_address_changed AFTER UPDATE OF email ON users
        WHEN OLD.email != NEW.email BEGIN
            DELETE FROM mfa_email_credentials WHERE user_id=NEW.id;
            DELETE FROM mfa_email_codes WHERE user_id=NEW.id;
            DELETE FROM mfa_challenges WHERE user_id=NEW.id;
            UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=NEW.id;
            UPDATE auth_meta SET value=value+1 WHERE key='acl_generation';
        END;
        CREATE TRIGGER IF NOT EXISTS mfa_email_user_deleted AFTER DELETE ON users BEGIN
            DELETE FROM mfa_email_credentials WHERE user_id=OLD.id;
            DELETE FROM mfa_email_codes WHERE user_id=OLD.id;
            DELETE FROM mfa_email_sends WHERE user_id=OLD.id;
            DELETE FROM mfa_factor_preferences WHERE user_id=OLD.id;
            DELETE FROM mfa_factor_failures WHERE user_id=OLD.id;
        END;") .map_err(|e|e.to_string())
}

fn numeric_code() -> Result<String> {
    loop {
        let mut bytes = [0u8;4];
        SystemRandom::new().fill(&mut bytes).map_err(|_|DENIED)?;
        let value = u32::from_be_bytes(bytes);
        if value < 4_294_000_000 { return Ok(format!("{:06}",value % 1_000_000)); }
    }
}
fn email_hash(token: &str, code: &str) -> String {
    // The random challenge is not persisted, preventing an offline six-digit dictionary.
    token_hash(&format!("email:{token}:{code}"))
}

impl UserDb {
    pub fn mfa_admin_reset_reauth(&self, actor: &str, user: &str, token: &str, session: &str, now: i64) -> Result<()> {
        if actor==user {return Err("不能重設自己的兩步驟驗證。".into());}
        let mut conn=self.conn();
        let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        check_reauth(&tx,actor,token,session,now)?;
        let admin:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active')",[actor],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !admin {return Err(DENIED.into());}
        active_user(&tx,user)?;
        clear(&tx,user)?;
        tx.execute("UPDATE mfa_challenges SET consumed=1 WHERE token_hash=?1",[token_hash(token)]).map_err(|e|e.to_string())?;
        audit(&tx,user,"mfa.reset",Some(actor))?;
        tx.commit().map_err(|e|e.to_string())
    }

    pub fn mfa_factors(&self, user: &str) -> Result<serde_json::Value> {
        let conn = self.conn();
        active_user(&conn,user)?;
        let (totp,email,passkey): (bool,bool,bool) = conn.query_row("SELECT
            EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1),
            EXISTS(SELECT 1 FROM mfa_email_credentials e JOIN users u ON u.id=e.user_id WHERE e.user_id=?1 AND e.email=u.email),
            EXISTS(SELECT 1 FROM mfa_passkeys WHERE user_id=?1)", [user], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))
            .map_err(|e|e.to_string())?;
        let last: Option<String> = conn.query_row("SELECT last_used FROM mfa_factor_preferences WHERE user_id=?1", [user], |r|r.get(0))
            .optional().map_err(|e|e.to_string())?;
        let last = last.filter(|v| match v.as_str() {"totp"=>totp,"email"=>email,"passkey"=>passkey,_=>false})
            .unwrap_or_else(|| if totp {"totp"} else if passkey {"passkey"} else {"email"}.to_owned());
        Ok(serde_json::json!({"factors":{"totp":totp,"email":email,"passkey":passkey},"last_used":last}))
    }

    /// Fresh password capability scoped to this exact dashboard JWT session.
    pub fn mfa_factor_password_challenge(&self, user: &str, password: &str, session: &str, now: i64) -> Result<String> {
        if session.is_empty() || password.is_empty() || password.len()>4096 { return Err(DENIED.into()); }
        let account = self.get_user(user)?.ok_or(DENIED)?;
        let snapshot: String = self.conn().query_row("SELECT password_hash FROM users WHERE id=?1", [user], |r|r.get(0)).map_err(|_|DENIED)?;
        let verified = self.verify_password(&account.email,password)?;
        if verified.id != user { return Err(DENIED.into()); }
        let mut conn=self.conn();
        let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        active_user(&tx,user)?;
        let current=crate::user_row::load_user(&tx,user)?.ok_or(DENIED)?;
        let hash:String=tx.query_row("SELECT password_hash FROM users WHERE id=?1", [user], |r|r.get(0)).map_err(|_|DENIED)?;
        if hash!=snapshot || current.credential_epoch!=verified.credential_epoch { return Err(DENIED.into()); }
        let recent:i64=tx.query_row("SELECT COUNT(*) FROM mfa_challenges WHERE user_id=?1 AND purpose='enroll' AND expires_at>?2",params![user,now+TTL-60],|r|r.get(0)).map_err(|e|e.to_string())?;
        if recent>=3 { return Err("嘗試次數過多，請稍後再試。".into()); }
        let token=random_hex(32)?;
        tx.execute("INSERT INTO mfa_challenges(token_hash,user_id,first_factor,purpose,expires_at) VALUES(?1,?2,'password','enroll',?3)",params![token_hash(&token),user,now+TTL]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO mfa_enrollment_proofs(token_hash,session,kind,verified) VALUES(?1,?2,'password',1)", params![token_hash(&token),session]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(token)
    }

    /// Returns private delivery data; callers must never include the code in HTTP/RPC output.
    pub fn mfa_email_send(&self, token: &str, enrollment: bool, now: i64) -> Result<(String,String)> {
        let mut conn=self.conn();
        let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let user=challenge_user(&tx,token,if enrollment {"enroll"} else {"login"},now)?;
        active_user(&tx,&user)?;
        let email:String=tx.query_row("SELECT email FROM users WHERE id=?1",[&user],|r|r.get(0)).map_err(|_|DENIED)?;
        if !enrollment {
            let verified:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mfa_email_credentials WHERE user_id=?1 AND email=?2)",params![user,email],|r|r.get(0)).map_err(|e|e.to_string())?;
            if !verified { return Err(DENIED.into()); }
        }
        tx.execute("DELETE FROM mfa_email_sends WHERE sent_at<=?1",[now-3600]).map_err(|e|e.to_string())?;
        let (count,last):(i64,Option<i64>)=tx.query_row("SELECT COUNT(*),MAX(sent_at) FROM mfa_email_sends WHERE user_id=?1",[&user],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
        if count>=5 || last.is_some_and(|v|v>now-60) { return Err("請稍後再寄送驗證碼。".into()); }
        let code=numeric_code()?;
        tx.execute("INSERT INTO mfa_email_codes(token_hash,user_id,email,code_hash,expires_at) VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(token_hash) DO UPDATE SET email=excluded.email,code_hash=excluded.code_hash,expires_at=excluded.expires_at",
            params![token_hash(token),user,email,email_hash(token,&code),now+EMAIL_TTL]).map_err(|e|e.to_string())?;
        tx.execute("UPDATE mfa_challenges SET expires_at=?2 WHERE token_hash=?1",params![token_hash(token),now+EMAIL_TTL]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO mfa_email_sends(user_id,sent_at) VALUES(?1,?2)",params![user,now]).map_err(|e|e.to_string())?;
        audit(&tx,&user,"mfa.email_sent",None)?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok((email,code))
    }

    pub fn mfa_email_delivery_failed(&self, token: &str, code: &str) -> Result<()> {
        self.conn().execute("DELETE FROM mfa_email_codes WHERE token_hash=?1 AND code_hash=?2",params![token_hash(token),email_hash(token,code)])
            .map_err(|e|e.to_string())?;
        Ok(())
    }

    /// Login success consumes the whole challenge. Enrollment and replacement
    /// token issuance share the same transaction, including recovery codes.
    pub fn mfa_email_verify_and_issue<T>(&self, token: &str, code: &str, enrollment: bool, now: i64,
        issue: impl FnOnce(&crate::User,&[(String,crate::AccessLevel)],Vec<String>)->Result<T>) -> Result<T> {
        let mut conn=self.conn();
        let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let user=challenge_user(&tx,token,if enrollment {"enroll"} else {"login"},now)?;
        active_user(&tx,&user)?;
        tx.execute("UPDATE mfa_challenges SET attempts=attempts+1 WHERE token_hash=?1",[token_hash(token)]).map_err(|e|e.to_string())?;
        let record:Option<(String,String)>=tx.query_row("SELECT c.email,c.code_hash FROM mfa_email_codes c JOIN users u ON u.id=c.user_id
            WHERE c.token_hash=?1 AND c.user_id=?2 AND c.email=u.email AND c.expires_at>?3",params![token_hash(token),user,now],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        let valid=record.as_ref().is_some_and(|(_,hash)| code.len()==6 && code.bytes().all(|b|b.is_ascii_digit()) && bool::from(hash.as_bytes().ct_eq(email_hash(token,code).as_bytes())));
        if !valid {
            audit(&tx,&user,"mfa.verification_failed",None)?;
            tx.commit().map_err(|e|e.to_string())?;
            return Err(DENIED.into());
        }
        let (email,_)=record.ok_or(DENIED)?;
        let codes=if enrollment {
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mfa_email_credentials WHERE user_id=?1 AND email=?2)",params![user,email],|r|r.get(0)).map_err(|e|e.to_string())?;
            if exists { return Err("Email 驗證碼已啟用。".into()); }
            let had_factor=any_factor(&tx,&user)?;
            tx.execute("INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,?2,?3)
                ON CONFLICT(user_id) DO UPDATE SET email=excluded.email,verified_at=excluded.verified_at",params![user,email,now]).map_err(|e|e.to_string())?;
            bump_epoch(&tx,&user)?;
            audit(&tx,&user,"mfa.email_added",None)?;
            if had_factor {Vec::new()} else {replace_recovery(&tx,&user)?}
        } else {
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mfa_email_credentials WHERE user_id=?1 AND email=?2)",params![user,email],|r|r.get(0)).map_err(|e|e.to_string())?;
            if !exists { return Err(DENIED.into()); }
            audit(&tx,&user,"mfa.login",None)?;
            Vec::new()
        };
        tx.execute("DELETE FROM mfa_email_codes WHERE token_hash=?1",[token_hash(token)]).map_err(|e|e.to_string())?;
        tx.execute("UPDATE mfa_challenges SET consumed=1 WHERE token_hash=?1",[token_hash(token)]).map_err(|e|e.to_string())?;
        if enrollment { tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1",[&user]).map_err(|e|e.to_string())?; }
        set_last_factor(&tx,&user,"email")?;
        let result=issue_mfa_result(&tx,&user,codes,issue)?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(result)
    }

    pub fn mfa_email_remove_and_issue<T>(&self,user:&str,token:&str,session:&str,now:i64,policy_exempt:bool,
        issue:impl FnOnce(&crate::User,&[(String,crate::AccessLevel)],Vec<String>)->Result<T>)->Result<T> {
        let mut conn=self.conn();
        let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        check_reauth(&tx,user,token,session,now)?;
        if !policy_exempt && policy_enforced(&tx)? && factor_count(&tx,user)?<=1 { return Err("管理員已要求兩步驟驗證，請先新增其他驗證方式。".into()); }
        if tx.execute("DELETE FROM mfa_email_credentials WHERE user_id=?1",[user]).map_err(|e|e.to_string())?!=1 { return Err(DENIED.into()); }
        tx.execute("DELETE FROM mfa_email_codes WHERE user_id=?1",[user]).map_err(|e|e.to_string())?;
        tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1",[user]).map_err(|e|e.to_string())?;
        if !any_factor(&tx,user)? {tx.execute("DELETE FROM mfa_recovery_codes WHERE user_id=?1",[user]).map_err(|e|e.to_string())?;}
        bump_epoch(&tx,user)?;
        audit(&tx,user,"mfa.email_removed",None)?;
        let result=issue_mfa_result(&tx,user,Vec::new(),issue)?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
