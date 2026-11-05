//! Five-minute migration authorization; never accepts client-supplied MFA state.
use super::*;

pub struct MigrationConfirmationSession {
    pub verified_at: u64,
    pub csrf: [u8; 32],
}

fn current_admin(conn: &Connection, user: &str, epoch: u64) -> Result<crate::User> {
    let user = crate::user_row::load_user(conn, user)?.ok_or(DENIED)?;
    if user.status != crate::UserStatus::Active || user.role != UserRole::Admin
        || user.credential_epoch != epoch {
        return Err(DENIED.into());
    }
    Ok(user)
}

impl UserDb {
    /// Password and an enrolled TOTP are both required. Failure counters and TOTP
    /// replay prevention share the same transaction as the persisted proof.
    #[allow(clippy::too_many_arguments)]
    pub fn migration_step_up(&self, user: &str, session: &str, epoch: u64,
        password: &str, code: &str, cipher: &dyn MfaCipher, now: i64,
    ) -> Result<MigrationConfirmationSession> {
        if session.is_empty() || session.len() > 128 || password.len() > 4096 || now < 0 {
            return Err(DENIED.into());
        }
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| DENIED)?;
        current_admin(&tx, user, epoch)?;
        let (window, attempts): (i64, u32) = tx.query_row(
            "SELECT window,attempts FROM migration_step_up_attempts WHERE user_id=?1",
            [user], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|_| DENIED)?.unwrap_or((now, 0));
        if window > now || (now - window < 60 && attempts >= 5) { return Err(DENIED.into()); }
        tx.execute("INSERT INTO migration_step_up_attempts VALUES(?1,?2,?3)
            ON CONFLICT(user_id) DO UPDATE SET window=excluded.window,attempts=excluded.attempts",
            params![user, if now-window >= 60 {now} else {window},
                if now-window >= 60 {1} else {attempts+1}]).map_err(|_| DENIED)?;
        let password_hash: String = tx.query_row("SELECT password_hash FROM users WHERE id=?1", [user], |r| r.get(0)).map_err(|_| DENIED)?;
        if verify_password_hash(password, &password_hash).is_err() {
            audit(&tx, user, "migration.step_up_failed", None)?;
            tx.commit().map_err(|_| DENIED)?;
            return Err(DENIED.into());
        }
        if check_factor(&tx, user, code, false, cipher, now).is_err() {
            audit(&tx, user, "migration.step_up_failed", None)?;
            tx.commit().map_err(|_| DENIED)?;
            return Err(DENIED.into());
        }
        tx.execute("DELETE FROM migration_dashboard_proofs WHERE verified_at<?1 OR verified_at>?2",
            params![now - TTL, now]).map_err(|_| DENIED)?;
        let count: u32 = tx.query_row("SELECT COUNT(*) FROM migration_dashboard_proofs WHERE user_id=?1", [user], |r| r.get(0)).map_err(|_| DENIED)?;
        if count >= 20 { return Err(DENIED.into()); }
        let mut csrf = [0; 32];
        SystemRandom::new().fill(&mut csrf).map_err(|_| DENIED)?;
        tx.execute("INSERT INTO migration_dashboard_proofs VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(user_id,session_hash) DO UPDATE SET epoch=excluded.epoch,
            verified_at=excluded.verified_at,csrf=excluded.csrf",
            params![user, token_hash(session), epoch, now, csrf.as_slice()]).map_err(|_| DENIED)?;
        audit(&tx, user, "migration.step_up", None)?;
        tx.commit().map_err(|_| DENIED)?;
        Ok(MigrationConfirmationSession { verified_at: now as u64, csrf })
    }

    pub fn migration_confirmation_session(&self, user: &str, session: &str,
        epoch: u64, now: i64,
    ) -> Result<Option<MigrationConfirmationSession>> {
        let conn = self.conn();
        current_admin(&conn, user, epoch)?;
        if session.is_empty() || session.len() > 128 || now < 0 { return Err(DENIED.into()); }
        let row: Option<(i64, Vec<u8>)> = conn.query_row(
            "SELECT verified_at,csrf FROM migration_dashboard_proofs
             WHERE user_id=?1 AND session_hash=?2 AND epoch=?3 AND verified_at<=?4
             AND verified_at>=?5 AND EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1)",
            params![user, token_hash(session), epoch, now, now - TTL],
            |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|_| DENIED)?;
        row.map(|(at, csrf)| Ok(MigrationConfirmationSession {
            verified_at: at as u64, csrf: csrf.try_into().map_err(|_| DENIED)?,
        })).transpose()
    }
}
