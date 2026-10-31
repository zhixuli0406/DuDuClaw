//! Independent enrollment proof, scoped to one first-factor capability.
use super::*;

pub(super) fn proof_verified(tx: &Connection, token: &str) -> Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM mfa_enrollment_proofs WHERE token_hash=?1 AND verified=1)",
        [token_hash(token)], |r| r.get(0)).map_err(|e|e.to_string())
}

impl UserDb {
    /// Administrative review only; leave existing channel identity JSON unchanged.
    pub fn mfa_channel_binding_times(&self, user: &str) -> Result<Vec<serde_json::Value>> {
        let conn=self.conn();
        let mut q=conn.prepare("SELECT channel,channel_user_id,verified,verified_at FROM channel_identities WHERE user_id=?1 ORDER BY channel,channel_user_id").map_err(|e|e.to_string())?;
        q.query_map([user],|r|Ok(serde_json::json!({"channel":r.get::<_,String>(0)?,"channel_user_id":r.get::<_,String>(1)?,"verified":r.get::<_,bool>(2)?,"verified_at":r.get::<_,Option<i64>>(3)?})))
            .map_err(|e|e.to_string())?.collect::<std::result::Result<_,_>>().map_err(|e|e.to_string())
    }

    /// A dashboard step-up capability is never redeemable on the HTTP route.
    /// Channel-first login also needs the independent password at enrollment.
    pub fn mfa_http_enrollment(&self, token: &str, password: Option<&str>, now: i64) -> Result<()> {
        let (user, first, session): (String,String,Option<String>) = {
            let conn=self.conn();
            let user=challenge_user(&conn,token,"enroll",now)?;
            let (first,session)=conn.query_row("SELECT c.first_factor,p.session FROM mfa_challenges c JOIN mfa_enrollment_proofs p USING(token_hash) WHERE c.token_hash=?1",[token_hash(token)],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|DENIED)?;
            (user,first,session)
        };
        if session.is_some() { return Err(DENIED.into()); }
        if first=="channel_otp" {
            let password=password.filter(|p| !p.is_empty() && p.len()<=4096).ok_or(DENIED)?;
            let account=self.get_user(&user)?.ok_or(DENIED)?;
            if self.verify_password(&account.email,password)?.id != user { return Err(DENIED.into()); }
        }
        Ok(())
    }

    /// Called on the blocking pool after a fresh password verification. The
    /// session identity comes from the transport, never from RPC parameters.
    pub fn mfa_password_challenge(&self, user: &str, password: &str, session: &str, now: i64) -> Result<String> {
        if session.is_empty() || password.is_empty() || password.len()>4096 { return Err(DENIED.into()); }
        let account = self.get_user(user)?.ok_or(DENIED)?;
        let password_snapshot: String=self.conn().query_row("SELECT password_hash FROM users WHERE id=?1",[user],|r|r.get(0)).map_err(|_|DENIED)?;
        let verified = self.verify_password(&account.email, password)?;
        if verified.id != user { return Err(DENIED.into()); }
        let token = self.mfa_challenge(user, FirstFactor::Password, true, now)?;
        let mut connection = self.conn();
        let conn = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        // A password change between the slow verification and the write must
        // not turn the old password into a new enrollment grant.
        let current = crate::user_row::load_user(&conn, user)?.ok_or(DENIED)?;
        let current_hash:String=conn.query_row("SELECT password_hash FROM users WHERE id=?1",[user],|r|r.get(0)).map_err(|_|DENIED)?;
        if !credential_epoch_matches(verified.credential_epoch, current.credential_epoch) || current_hash != password_snapshot
            || challenge_user(&conn,&token,"enroll",now)? != user { return Err(DENIED.into()); }
        conn.execute("INSERT INTO mfa_enrollment_proofs(token_hash,session,kind) VALUES(?1,?2,'pending')",
            params![token_hash(&token), session]).map_err(|e|e.to_string())?;
        conn.commit().map_err(|e|e.to_string())?;
        Ok(token)
    }

    pub fn mfa_check_session(&self, token: &str, user: &str, session: &str, now: i64) -> Result<()> {
        let conn = self.conn();
        if challenge_user(&conn, token, "enroll", now)? != user { return Err(DENIED.into()); }
        let matched: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM mfa_enrollment_proofs WHERE token_hash=?1 AND session=?2)",
            params![token_hash(token),session], |r|r.get(0)).map_err(|e|e.to_string())?;
        if matched { Ok(()) } else { Err(DENIED.into()) }
    }

    /// Prepare exactly one independent proof. Repeated calls cannot resend or
    /// replace it. Channel OTPs use otp.rs's generator, hashing and delivery DTO,
    /// but cannot be redeemed by the ordinary one-factor OTP login endpoint.
    pub fn mfa_prepare_enrollment(&self, token: &str, required: bool, now: i64) -> Result<Option<crate::otp::OtpChallenge>> {
        let user = self.mfa_enrollment_user(token, now)?;
        let code = crate::otp::generate_numeric_code();
        let hash = hash_password(&code)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        if challenge_user(&tx, token, "enroll", now)? != user { return Err(DENIED.into()); }
        let channel: Option<(String,String)> = tx.query_row(
            "SELECT channel,channel_user_id FROM channel_identities WHERE user_id=?1 AND verified=1
             AND verified_at < (SELECT enforced_at FROM mfa_policy_state WHERE id=1 AND enforced=1)
             ORDER BY channel,channel_user_id LIMIT 1", [&user], |r|Ok((r.get(0)?,r.get(1)?)))
            .optional().map_err(|e|e.to_string())?;
        let old: Option<String> = tx.query_row("SELECT kind FROM mfa_enrollment_proofs WHERE token_hash=?1", [token_hash(token)], |r|r.get(0)).optional().map_err(|e|e.to_string())?;
        if old.as_deref().is_some_and(|v|v!="pending") { return Err(DENIED.into()); }
        let kind = if !required { "password" } else if channel.is_some() { "channel" } else { "admin" };
        tx.execute("INSERT INTO mfa_enrollment_proofs(token_hash,code_hash,kind,verified) VALUES(?1,?2,?3,?4)
            ON CONFLICT(token_hash) DO UPDATE SET code_hash=excluded.code_hash,kind=excluded.kind,verified=excluded.verified",
            params![token_hash(token), if kind=="channel" {Some(hash)} else {None}, kind, !required]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(channel.filter(|_|required).map(|c|crate::otp::OtpChallenge {
            challenge_id: token.to_string(), code, user_id:user, channel:c.0,
            masked_target:crate::otp::mask_target(&c.1), channel_user_id:c.1,
        }))
    }

    /// A policy toggle must not grandfather an unconfirmed password-only setup.
    pub fn mfa_require_independent_proof(&self, token: &str, now: i64) -> Result<()> {
        let conn=self.conn();
        challenge_user(&conn,token,"enroll",now)?;
        let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM mfa_enrollment_proofs WHERE token_hash=?1 AND verified=1 AND kind IN ('channel','admin'))",
            [token_hash(token)],|r|r.get(0)).map_err(|e|e.to_string())?;
        if valid { Ok(()) } else { Err("請重新開始註冊並完成獨立身分驗證。".into()) }
    }

    pub fn mfa_prove_enrollment(&self, token: &str, code: &str, now: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let user = challenge_user(&tx, token, "enroll", now)?;
        let (kind, hash, verified): (String,Option<String>,bool) = tx.query_row(
            "SELECT kind,code_hash,verified FROM mfa_enrollment_proofs WHERE token_hash=?1", [token_hash(token)],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_|DENIED)?;
        if verified { return Ok(()); }
        tx.execute("UPDATE mfa_challenges SET attempts=attempts+1 WHERE token_hash=?1", [token_hash(token)]).map_err(|e|e.to_string())?;
        let valid = if kind=="channel" {
            hash.is_some_and(|h|verify_password_hash(code,&h).is_ok())
        } else if kind=="admin" {
            tx.query_row("SELECT code_hash FROM mfa_enrollment_codes WHERE user_id=?1 AND challenge=?2 AND expires_at>?3", params![user,token_hash(token),now], |r|r.get::<_,String>(0))
                .is_ok_and(|h|h.as_bytes().ct_eq(token_hash(code).as_bytes()).into())
        } else { false };
        if valid {
            if kind=="admin" { tx.execute("DELETE FROM mfa_enrollment_codes WHERE user_id=?1 AND challenge=?2", params![user,token_hash(token)]).map_err(|e|e.to_string())?; }
            tx.execute("UPDATE mfa_enrollment_proofs SET verified=1,code_hash=NULL WHERE token_hash=?1", [token_hash(token)]).map_err(|e|e.to_string())?;
            audit(&tx,&user,"mfa.enrollment_proved",None)?;
        } else { audit(&tx,&user,"mfa.verification_failed",None)?; }
        tx.commit().map_err(|e|e.to_string())?;
        if valid { Ok(()) } else { Err(DENIED.into()) }
    }

    pub fn mfa_issue_enrollment_code(&self, actor: Option<&str>, user: &str, challenge_ref: &str, now: i64) -> Result<String> {
        let code = random_hex(16)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        active_user(&tx,user)?;
        if let Some(actor)=actor {
            if actor==user { return Err("不能核發自己的註冊碼。".into()); }
            let admin: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active')", [actor], |r|r.get(0)).map_err(|e|e.to_string())?;
            if !admin { return Err(DENIED.into()); }
        }
        if challenge_ref.len()!=12 || !challenge_ref.bytes().all(|b|b.is_ascii_hexdigit()) { return Err(DENIED.into()); }
        let pending: Vec<String> = {
            let mut q=tx.prepare("SELECT c.token_hash FROM mfa_challenges c JOIN mfa_enrollment_proofs p USING(token_hash)
                WHERE c.user_id=?1 AND c.purpose='enroll' AND c.consumed=0 AND c.attempts<5 AND c.expires_at>?2
                AND p.kind='admin' AND p.verified=0 AND substr(c.token_hash,1,12)=?3").map_err(|e|e.to_string())?;
            q.query_map(params![user,now,challenge_ref], |r|r.get(0)).map_err(|e|e.to_string())?
                .collect::<std::result::Result<_,_>>().map_err(|e|e.to_string())?
        };
        if pending.len()!=1 { return Err(DENIED.into()); }
        tx.execute("INSERT INTO mfa_enrollment_codes(user_id,code_hash,expires_at,challenge) VALUES(?1,?2,?3,?4)
            ON CONFLICT(user_id) DO UPDATE SET code_hash=excluded.code_hash,expires_at=excluded.expires_at,challenge=excluded.challenge",
            params![user,token_hash(&code),now+TTL,pending[0]]).map_err(|e|e.to_string())?;
        audit(&tx,user,"mfa.enrollment_code_issued",Some(actor.unwrap_or("本機指令列（身分未驗證）")))?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(code)
    }
}
