//! RM-143 self-service proof: dashboard start -> authenticated chat -> dashboard confirm.
//!
//! Callers authenticate the dashboard user/session and the chat event. In
//! particular, `is_private` must come from the verified adapter event, never
//! user/model input. This store does not inspect bot configuration or write audit
//! events: the gateway owns those checks and the confirm/reject audit records.
//! All timestamps are Unix seconds except channel_identities.verified_at (micros).

use chrono::Utc;
use ring::{hmac, rand::{SecureRandom, SystemRandom}};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{UserDb, otp::{ChatApprovalProof, mask_target}};
use crate::user_row::bump_acl_generation;

type Result<T> = std::result::Result<T, String>;

pub const CHANNELS: &[&str] = &["telegram", "googlechat"];
pub const TTL_SECS: i64 = 600;
const WINDOW_SECS: i64 = 600;
const RETENTION_SECS: i64 = 86_400;
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const DENIED: &str = "chat link unavailable";

/// Opaque normalized secret; intentionally has no Debug/Serialize implementation.
pub struct NormalizedCode(String);

/// The code is returned once for display and must never be logged.
#[derive(Serialize)]
pub struct StartedHandshake {
    pub handshake_id: String,
    pub code: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeStatus { Waiting, Matched, Confirmed, Rejected, Expired }

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Handshake {
    pub handshake_id: String,
    pub channel: String,
    pub status: HandshakeStatus,
    pub expires_at: i64,
    pub matched_principal: Option<String>,
    pub masked_target: Option<String>,
    pub matched_account: Option<String>,
    pub matched_dm: Option<String>,
    /// Untrusted chat text; the presentation layer must escape this for its UI.
    pub matched_display: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOutcome {
    Matched,
    MustUsePrivateChat,
    InvalidOrExpired,
    /// Adapters must swallow the message without replying or forwarding to AI.
    Throttled,
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS chat_link_handshakes (
        id TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        channel TEXT NOT NULL,
        code_digest BLOB NOT NULL,
        session_hash TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('waiting','matched','confirmed','rejected','expired')),
        expires_at INTEGER NOT NULL,
        matched_principal TEXT, matched_account TEXT, matched_dm TEXT, matched_display TEXT,
        created_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS chat_link_user_created ON chat_link_handshakes(user_id,created_at);
        CREATE INDEX IF NOT EXISTS chat_link_waiting ON chat_link_handshakes(channel,status,expires_at);
        CREATE TABLE IF NOT EXISTS chat_link_attempts (
        sender_key BLOB NOT NULL, window_start INTEGER NOT NULL,
        failures INTEGER NOT NULL CHECK(failures > 0), blocked_until INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(sender_key,window_start));")
        .map_err(|e| e.to_string())
}

/// Strict whole-message parser. Only surrounding whitespace is ignored;
/// Crockford aliases apply only to the eight payload characters, not the prefix.
pub fn parse_code(text: &str) -> Option<NormalizedCode> {
    let mut parts = text.trim().split('-');
    if !parts.next()?.eq_ignore_ascii_case("DUDU") { return None; }
    let mut normalized = String::from("DUDU");
    for _ in 0..2 {
        let part = parts.next()?;
        if part.chars().count() != 4 { return None; }
        normalized.push('-');
        for ch in part.chars() {
            let ch = match ch.to_ascii_uppercase() {
                'O' => '0', 'I' | 'L' => '1', other => other,
            };
            if !ch.is_ascii() || !ALPHABET.iter().any(|b| char::from(*b) == ch) { return None; }
            normalized.push(ch);
        }
    }
    if parts.next().is_some() { return None; }
    Some(NormalizedCode(normalized))
}

fn code_message(code: &NormalizedCode) -> Vec<u8> {
    let mut data = b"chat_link_code\0".to_vec();
    data.extend_from_slice(code.0.as_bytes());
    data
}

fn sender_key(db: &UserDb, channel: &str, principal: &str) -> Vec<u8> {
    // Length framing prevents ambiguous concatenations, including embedded NULs.
    let mut context = hmac::Context::with_key(&db.remember_key);
    context.update(b"chat_link_sender\0");
    for part in [channel, principal] {
        context.update(&(part.len() as u64).to_be_bytes());
        context.update(part.as_bytes());
    }
    context.sign().as_ref().to_vec()
}

fn global_key(db: &UserDb) -> Vec<u8> {
    hmac::sign(&db.remember_key, b"chat_link_global\0").as_ref().to_vec()
}

fn require_admin(conn: &Connection, user_id: &str) -> Result<()> {
    let admin: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active')",
        [user_id], |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    if !admin { return Err(DENIED.into()); }
    let enforced = crate::mfa::read_enforcement(conn)?;
    if enforced && !crate::mfa::any_factor(conn, user_id)? { return Err(DENIED.into()); }
    Ok(())
}

fn expire(conn: &Connection, now: i64) -> Result<()> {
    conn.execute("UPDATE chat_link_handshakes SET status='expired'
        WHERE status IN ('waiting','matched') AND expires_at <= ?1", [now])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Start only for a live Active Admin meeting the current MFA policy. The caller
/// supplies the authenticated dashboard session hash, not a request-body value.
pub fn start(db: &UserDb, user_id: &str, channel: &str, session_hash: &str) -> Result<StartedHandshake> {
    if !CHANNELS.contains(&channel) || session_hash.is_empty() { return Err(DENIED.into()); }
    let mut conn = db.conn();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let now = Utc::now().timestamp();
    require_admin(&tx, user_id)?;
    expire(&tx, now)?;
    let (waiting, starts): (i64, i64) = tx.query_row(
        "SELECT COUNT(CASE WHEN status='waiting' THEN 1 END),
            COUNT(CASE WHEN created_at > ?2 THEN 1 END)
         FROM chat_link_handshakes WHERE user_id=?1",
        params![user_id, now - 3600], |r| Ok((r.get(0)?, r.get(1)?)),
    ).map_err(|e| e.to_string())?;
    if waiting >= 2 || starts >= 6 { return Err("chat link start limit reached".into()); }
    // Eight independent low five bits of CSPRNG bytes: exactly 40 uniform bits.
    // Avoid a collision with any retained handshake, including consumed codes.
    let (code, digest) = loop {
        let mut random = [0u8; 8];
        SystemRandom::new().fill(&mut random).map_err(|_| "chat link randomness unavailable")?;
        let mut code = String::from("DUDU");
        for (i, byte) in random.iter().enumerate() {
            if i % 4 == 0 { code.push('-'); }
            code.push(char::from(ALPHABET[usize::from(byte & 31)]));
        }
        let message = code_message(&NormalizedCode(code.clone()));
        let mut stmt = tx.prepare("SELECT code_digest FROM chat_link_handshakes").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).map_err(|e| e.to_string())?;
        let mut collision = false;
        for row in rows {
            let stored = row.map_err(|e| e.to_string())?;
            collision |= hmac::verify(&db.remember_key, &message, &stored).is_ok();
        }
        if !collision { break (code, hmac::sign(&db.remember_key, &message)); }
    };
    let handshake_id = Uuid::new_v4().to_string();
    let expires_at = now + TTL_SECS;
    tx.execute("INSERT INTO chat_link_handshakes
        (id,user_id,channel,code_digest,session_hash,status,expires_at,created_at)
        VALUES(?1,?2,?3,?4,?5,'waiting',?6,?7)",
        params![handshake_id, user_id, channel, digest.as_ref(), session_hash, expires_at, now])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(StartedHandshake { handshake_id, code, expires_at })
}

fn throttled(conn: &Connection, key: &[u8], now: i64) -> Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM chat_link_attempts
        WHERE sender_key=?1 AND blocked_until > ?2)", params![key, now], |r| r.get(0))
        .map_err(|e| e.to_string())
}

fn failure(conn: &Connection, key: &[u8], limit: i64, silence: i64, now: i64) -> Result<()> {
    // Per-second buckets give a rolling window rather than a resettable fixed
    // window. blocked_until is separate because sender silence outlives it.
    conn.execute("INSERT INTO chat_link_attempts(sender_key,window_start,failures)
        VALUES(?1,?2,1) ON CONFLICT(sender_key,window_start)
        DO UPDATE SET failures=failures+1", params![key, now]).map_err(|e| e.to_string())?;
    let failures: i64 = conn.query_row("SELECT COALESCE(SUM(failures),0) FROM chat_link_attempts
        WHERE sender_key=?1 AND window_start > ?2", params![key, now - WINDOW_SECS], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if failures >= limit {
        conn.execute("UPDATE chat_link_attempts SET blocked_until=?3
            WHERE sender_key=?1 AND window_start=?2", params![key, now, now + silence])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Process an adapter-authenticated sender. Throttling precedes every code lookup;
/// wrong, expired, reused, other-channel and conflicting-owner codes share one result.
/// `Matched` creates no binding: dashboard confirmation is mandatory.
#[allow(clippy::too_many_arguments)]
pub fn match_code(
    db: &UserDb, channel: &str, code: &NormalizedCode, principal: &str,
    account: &str, dm: &str, display_name: &str, is_private: bool,
) -> Result<MatchOutcome> {
    let mut conn = db.conn();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let now = Utc::now().timestamp();
    let sender = sender_key(db, channel, principal);
    let global = global_key(db);
    if throttled(&tx, &sender, now)? || throttled(&tx, &global, now)? {
        return Ok(MatchOutcome::Throttled);
    }
    expire(&tx, now)?;
    let mut found = None;
    if CHANNELS.contains(&channel) {
        let message = code_message(code);
        let mut stmt = tx.prepare("SELECT id,user_id,code_digest FROM chat_link_handshakes
            WHERE channel=?1 AND status='waiting' AND expires_at > ?2").map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![channel, now], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?,
        ))).map_err(|e| e.to_string())?;
        for row in rows {
            let (id, user, digest) = row.map_err(|e| e.to_string())?;
            if hmac::verify(&db.remember_key, &message, &digest).is_ok() { found = Some((id, user)); }
        }
    }
    let outcome = if let Some((id, user)) = found {
        if !is_private {
            tx.execute("UPDATE chat_link_handshakes SET status='rejected' WHERE id=?1", [&id])
                .map_err(|e| e.to_string())?;
            MatchOutcome::MustUsePrivateChat
        } else {
            let conflict: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM channel_identities
                WHERE channel=?1 AND channel_user_id=?2 AND user_id != ?3 AND verified != 0)",
                params![channel, principal, user], |r| r.get(0)).map_err(|e| e.to_string())?;
            if conflict || principal.is_empty() || account.is_empty() || dm.is_empty() {
                MatchOutcome::InvalidOrExpired
            } else {
                tx.execute("UPDATE chat_link_handshakes SET status='matched',matched_principal=?2,
                    matched_account=?3,matched_dm=?4,matched_display=?5 WHERE id=?1 AND status='waiting'",
                    params![id, principal, account, dm, duduclaw_core::truncate_chars(display_name, 200)])
                    .map_err(|e| e.to_string())?;
                MatchOutcome::Matched
            }
        }
    } else { MatchOutcome::InvalidOrExpired };
    if outcome == MatchOutcome::InvalidOrExpired {
        failure(&tx, &sender, 5, 3600, now)?;
        failure(&tx, &global, 50, 600, now)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(outcome)
}

/// Owner-scoped dashboard state, with enough data to present the matched account.
/// No code, digest, or session identifier is returned.
pub fn status(db: &UserDb, user_id: &str, handshake_id: &str) -> Result<Option<Handshake>> {
    let mut conn = db.conn();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    expire(&tx, Utc::now().timestamp())?;
    let result = tx.query_row("SELECT channel,status,expires_at,matched_principal,
        matched_account,matched_dm,matched_display FROM chat_link_handshakes WHERE id=?1 AND user_id=?2",
        params![handshake_id, user_id], |r| {
            let raw: String = r.get(1)?;
            let state = match raw.as_str() {
                "waiting" => HandshakeStatus::Waiting, "matched" => HandshakeStatus::Matched,
                "confirmed" => HandshakeStatus::Confirmed, "rejected" => HandshakeStatus::Rejected,
                "expired" => HandshakeStatus::Expired,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let principal: Option<String> = r.get(3)?;
            Ok(Handshake {
                handshake_id: handshake_id.to_owned(), channel: r.get(0)?, status: state,
                expires_at: r.get(2)?, masked_target: principal.as_deref().map(mask_target),
                matched_principal: principal, matched_account: r.get(4)?,
                matched_dm: r.get(5)?, matched_display: r.get(6)?,
            })
        }).optional().map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

/// Owner-scoped pending handshake IDs for the dashboard. Status reads still
/// perform expiry and expose only the public projection chosen by the gateway.
pub fn pending_ids(db: &UserDb, user_id: &str) -> Result<Vec<String>> {
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT id FROM chat_link_handshakes
        WHERE user_id=?1 AND status IN ('waiting','matched') AND expires_at > ?2
        ORDER BY created_at,id").map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![user_id, Utc::now().timestamp()], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Confirm using the same authenticated user/session as start. Rechecks live
/// Admin/MFA eligibility and identity ownership inside the binding transaction.
/// The gateway must audit successful confirmation (`channel_identity_proven`).
pub fn confirm(db: &UserDb, user_id: &str, handshake_id: &str, session_hash: &str) -> Result<ChatApprovalProof> {
    let mut conn = db.conn();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let now = Utc::now().timestamp();
    expire(&tx, now)?;
    let matched = tx.query_row("SELECT channel,session_hash,matched_principal,matched_account,matched_dm
        FROM chat_link_handshakes WHERE id=?1 AND user_id=?2 AND status='matched' AND expires_at > ?3",
        params![handshake_id, user_id, now], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
            r.get::<_, String>(3)?, r.get::<_, String>(4)?,
        ))).optional().map_err(|e| e.to_string())?;
    let Some((channel, session, principal, account, dm)) = matched else {
        tx.commit().map_err(|e| e.to_string())?;
        return Err(DENIED.into());
    };
    if session_hash.is_empty() || !bool::from(session.as_bytes().ct_eq(session_hash.as_bytes()))
        || !CHANNELS.contains(&channel.as_str()) || principal.is_empty() || account.is_empty() || dm.is_empty() {
        return Err(DENIED.into());
    }
    require_admin(&tx, user_id)?;
    let conflict: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM channel_identities
        WHERE channel=?1 AND channel_user_id=?2 AND user_id != ?3 AND verified != 0)",
        params![channel, principal, user_id], |r| r.get(0)).map_err(|e| e.to_string())?;
    if conflict { return Err(DENIED.into()); }
    tx.execute("INSERT INTO channel_identities
        (user_id,channel,channel_user_id,verified,created_at,verified_at,
         proof_method,proof_user_id,proven_at,proof_account,proof_dm)
        VALUES(?1,?2,?3,1,?4,?5,'chat_handshake',?1,?6,?7,?8)
        ON CONFLICT(channel,channel_user_id) DO UPDATE SET user_id=excluded.user_id,
            verified=1,verified_at=excluded.verified_at,proof_method=excluded.proof_method,
            proof_user_id=excluded.proof_user_id,proven_at=excluded.proven_at,
            proof_account=excluded.proof_account,proof_dm=excluded.proof_dm",
        params![user_id, channel, principal, Utc::now().to_rfc3339(), Utc::now().timestamp_micros(), now, account, dm])
        .map_err(|e| e.to_string())?;
    bump_acl_generation(&tx).map_err(|e| e.to_string())?;
    tx.execute("UPDATE chat_link_handshakes SET status='confirmed' WHERE id=?1", [handshake_id])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ChatApprovalProof { channel, channel_user_id: principal, proof_account: account, proof_dm: dm, proven_at: now })
}

/// Void the owner's pending handshake. The gateway writes `channel_link_rejected`
/// when rejecting a stolen/misidentified match. Terminal states are never rewritten.
pub fn reject(db: &UserDb, user_id: &str, handshake_id: &str) -> Result<bool> {
    let mut conn = db.conn();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    expire(&tx, Utc::now().timestamp())?;
    let changed = tx.execute("UPDATE chat_link_handshakes SET status='rejected'
        WHERE id=?1 AND user_id=?2 AND status IN ('waiting','matched')", params![handshake_id, user_id])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(changed == 1)
}

/// Cancellation uses the design's existing `rejected` terminal state.
pub fn cancel(db: &UserDb, user_id: &str, handshake_id: &str) -> Result<bool> {
    reject(db, user_id, handshake_id)
}

/// Expire pending handshakes and delete handshake/attempt rows older than a day.
/// Returns the total number of deleted rows. Does not delete identity proofs.
pub fn purge_stale(db: &UserDb) -> Result<usize> {
    let mut conn = db.conn();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let now = Utc::now().timestamp();
    expire(&tx, now)?;
    let handshakes = tx.execute("DELETE FROM chat_link_handshakes WHERE created_at < ?1",
        [now - RETENTION_SECS]).map_err(|e| e.to_string())?;
    let attempts = tx.execute("DELETE FROM chat_link_attempts WHERE window_start < ?1 AND blocked_until <= ?2",
        params![now - RETENTION_SECS, now]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(handshakes + attempts)
}

#[cfg(test)]
mod tests;
