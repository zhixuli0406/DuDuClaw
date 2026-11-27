//! Atomic Google sign-in decisions and passwordless employee provisioning.
//! The gateway verifies tokens and policy; this module owns all users.db decisions.
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::google_signin::{valid_google_sub, NewGoogleSigninLink, SigninRequestStatus,
    SIGNIN_REJECT_COOLDOWN_SECS};
use crate::members::valid_member_email;
use crate::user_row::{bump_acl_generation, load_user};
use crate::UserDb;

const STORAGE: &str = "signin_storage_unavailable";
const SYSTEM_ACTOR: &str = "system:google_signin";

/// Must only be constructed from a Google ID token whose signature and claims
/// were verified by the gateway. `hd` is already normalized to lowercase.
#[derive(Debug, Clone)]
pub struct VerifiedGoogleIdentity {
    pub sub: String,
    pub email: String,
    pub hd: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigninApproval { Admin, Direct }

pub struct SigninPolicy {
    pub approval: SigninApproval,
    pub daily_cap: u32,
    pub settings_version: String,
    pub ack_by: String,
    pub ack_at: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SigninOutcome {
    Ok { user_id: String, credential_epoch: u64, created: bool, linked_via: Option<&'static str> },
    AccountDisabled,
    ExistingAccount,
    Rejected,
    DailyLimit,
    PendingFull,
    PendingApproval { request_id: String },
}

fn validated_identity(id: &VerifiedGoogleIdentity) -> Result<VerifiedGoogleIdentity, &'static str> {
    if !valid_google_sub(&id.sub) { return Err("signin_invalid_sub"); }
    if !valid_member_email(&id.email) { return Err("signin_invalid_email"); }
    if id.hd.is_empty() || !id.hd.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-')) {
        return Err("signin_invalid_hd");
    }
    let clean: String = id.display_name.chars().filter(|c| !c.is_control()).collect();
    let name = if clean.is_empty() { id.email.split('@').next().ok_or("signin_invalid_email")? } else { &clean };
    Ok(VerifiedGoogleIdentity { display_name: duduclaw_core::truncate_chars(name, 200), ..id.clone() })
}

fn timestamp(now: i64) -> Result<String, &'static str> {
    chrono::DateTime::from_timestamp(now, 0).map(|t| t.to_rfc3339()).ok_or("signin_invalid_timestamp")
}

pub(crate) fn live_admin(tx: &Transaction<'_>, admin_id: &str, admin_epoch: u64) -> Result<(), &'static str> {
    let authorized: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active'
         AND must_change_password=0 AND credential_epoch=?2)",
        params![admin_id, admin_epoch], |row| row.get(0),
    ).map_err(|_| STORAGE)?;
    if !authorized { return Err("signin_admin_required"); }
    Ok(())
}

fn email_exists(tx: &Transaction<'_>, email: &str) -> Result<bool, &'static str> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE email=?1 COLLATE NOCASE)",
        [email], |r| r.get(0)).map_err(|_| STORAGE)
}

fn insert_link(tx: &Transaction<'_>, user_id: &str, id: &VerifiedGoogleIdentity,
    actor: &str, linked_via: &str, now: i64) -> Result<(), &'static str> {
    UserDb::insert_google_signin_link_tx(tx, &NewGoogleSigninLink {
        user_id: user_id.into(), google_sub: id.sub.clone(), hd: id.hd.clone(),
        email_at_link: id.email.clone(), linked_via: linked_via.into(), linked_by: actor.into(), linked_at: now,
    }).map_err(|_| STORAGE)
}

pub(crate) fn audit(tx: &Transaction<'_>, actor: &str, action: &str, target: &str, detail: &Value, now: i64) -> Result<(), &'static str> {
    tx.execute("INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp) VALUES(?1,?2,?3,?4,?5)",
        params![actor, action, target, detail.to_string(), timestamp(now)?]).map_err(|_| STORAGE)?;
    Ok(())
}

fn create_signin_account_tx(tx: &Transaction<'_>, identity: &VerifiedGoogleIdentity,
    actor: &str, linked_via: &str, audit_detail: &Value, now: i64) -> Result<String, String> {
    let user_id = Uuid::new_v4().to_string();
    let at = timestamp(now)?;
    // Both primary_agent_id and avatar_agent_id are nullable. No bindings or
    // avatar grants are inserted, so provisioning cannot change seat usage.
    tx.execute("INSERT INTO users(id,email,display_name,password_hash,role,status,created_at,updated_at,
        must_change_password,primary_agent_id,avatar_agent_id)
        VALUES(?1,?2,?3,'','employee','active',?4,?4,0,NULL,NULL)",
        params![user_id, identity.email, identity.display_name, at]).map_err(|_| STORAGE)?;
    insert_link(tx, &user_id, identity, actor, linked_via, now)?;
    tx.execute("INSERT INTO user_onboarding(user_id,source,avatar,google,google_chat,odoo,mfa)
        VALUES(?1,'google_signin','pending','pending','pending','pending','pending')", [&user_id]).map_err(|_| STORAGE)?;
    audit(tx, actor, "user.create_member_signin", &user_id, audit_detail, now)?;
    bump_acl_generation(tx).map_err(|_| STORAGE)?;
    let user = load_user(tx, &user_id).map_err(|_| STORAGE)?.ok_or(STORAGE)?;
    Ok(user.id)
}

fn pending(tx: &Transaction<'_>, identity: &VerifiedGoogleIdentity, request_id: &str, now: i64) -> Result<SigninOutcome, String> {
    match UserDb::upsert_pending_signin_request_tx(tx, request_id, &identity.sub, &identity.email,
        &identity.hd, &identity.display_name, now) {
        Ok(()) => Ok(SigninOutcome::PendingApproval { request_id: request_id.into() }),
        Err(code) if code == "signin_pending_full" => Ok(SigninOutcome::PendingFull),
        Err(_) => Err(STORAGE.into()),
    }
}

fn resolve_tx(tx: &Transaction<'_>, id: &VerifiedGoogleIdentity, policy: &SigninPolicy, now: i64,
    personal_sub: &dyn Fn(&str) -> Result<Option<String>, String>) -> Result<SigninOutcome, String> {
    // 1. An immutable subject link always wins, even if the token email changed.
    let linked: Option<String> = tx.query_row("SELECT user_id FROM google_signin_links WHERE google_sub=?1",
        [&id.sub], |r| r.get(0)).optional().map_err(|_| STORAGE)?;
    if let Some(user_id) = linked {
        let (status, credential_epoch): (String, u64) = tx.query_row("SELECT status,credential_epoch FROM users WHERE id=?1",
            [&user_id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|_| STORAGE)?;
        if status != "active" { return Ok(SigninOutcome::AccountDisabled); }
        tx.execute("UPDATE google_signin_links SET last_signin_at=?2 WHERE user_id=?1", params![user_id, now]).map_err(|_| STORAGE)?;
        return Ok(SigninOutcome::Ok { user_id, credential_epoch, created: false, linked_via: None });
    }
    // 2. Read provenance before the attestation helper consumes it, under the
    // same transaction. Never take an attestation from an already-linked user.
    let attestations = {
        let mut stmt = tx.prepare("SELECT u.id,u.status,a.attested_by,u.credential_epoch FROM users u
            JOIN google_signin_attestations a ON a.user_id=u.id
            WHERE u.email=?1 COLLATE NOCASE
            AND NOT EXISTS(SELECT 1 FROM google_signin_links l WHERE l.user_id=u.id)").map_err(|_| STORAGE)?;
        let rows = stmt.query_map([&id.email], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, u64>(3)?)))
            .map_err(|_| STORAGE)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| STORAGE)?
    };
    for (user_id, status, attested_by, credential_epoch) in attestations {
        if personal_sub(&user_id)?.is_some_and(|sub| sub != id.sub) {
            return Ok(SigninOutcome::ExistingAccount);
        }
        if UserDb::take_valid_google_signin_attestation_tx(tx, &user_id, &id.email, now).map_err(|_| STORAGE)? {
            if status != "active" { return Ok(SigninOutcome::AccountDisabled); }
            insert_link(tx, &user_id, id, &attested_by, "admin_attest", now)?;
            audit(tx, SYSTEM_ACTOR, "user.google_signin_linked", &user_id,
                &json!({"linked_via": "admin_attest", "hd": id.hd}), now)?;
            return Ok(SigninOutcome::Ok { user_id, credential_epoch, created: false, linked_via: Some("admin_attest") });
        }
    }
    // 3. Email is only a collision guard, never an identity proof.
    if email_exists(tx, &id.email)? { return Ok(SigninOutcome::ExistingAccount); }
    let request = UserDb::signin_request_by_sub_tx(tx, &id.sub).map_err(|_| STORAGE)?;
    // 4. A malformed rejection timestamp denies rather than skipping cooldown.
    if let Some(r) = request.as_ref().filter(|r| r.status == SigninRequestStatus::Rejected) {
        let until = r.decided_at.ok_or(STORAGE)?.checked_add(SIGNIN_REJECT_COOLDOWN_SECS).ok_or("signin_invalid_timestamp")?;
        if until > now { return Ok(SigninOutcome::Rejected); }
    }
    // 5. Refreshes in Admin mode are already counted and keep the original TTL.
    if policy.approval == SigninApproval::Admin {
        if let Some(r) = request.as_ref().filter(|r| r.status == SigninRequestStatus::Pending) {
            return pending(tx, id, &r.id, now);
        }
    }
    let since = now.checked_sub(86400).ok_or("signin_invalid_timestamp")?;
    if policy.daily_cap == 0 || UserDb::count_signin_jit_since_tx(tx, since).map_err(|_| STORAGE)? >= policy.daily_cap {
        return Ok(SigninOutcome::DailyLimit);
    }
    match policy.approval {
        // 6. The storage helper enforces the separate global pending limit.
        SigninApproval::Admin => pending(tx, id, &Uuid::new_v4().to_string(), now),
        // 7. Accounts are always employees, regardless of policy metadata.
        SigninApproval::Direct => {
            let detail = json!({"linked_via": "jit_direct", "hd": id.hd, "settings_version": policy.settings_version,
                "ack_by": policy.ack_by, "ack_at": policy.ack_at});
            let user_id = create_signin_account_tx(tx, id, SYSTEM_ACTOR, "jit_direct", &detail, now)?;
            let credential_epoch = tx.query_row("SELECT credential_epoch FROM users WHERE id=?1",
                [&user_id], |r| r.get::<_, u64>(0)).map_err(|_| STORAGE)?;
            Ok(SigninOutcome::Ok { user_id, credential_epoch, created: true, linked_via: Some("jit_direct") })
        }
    }
}

impl UserDb {
    /// The caller holds the personal grant lease; the lookup must not access users.db.
    pub fn resolve_google_signin(&self, id: &VerifiedGoogleIdentity, policy: &SigninPolicy, now: i64,
        personal_sub: &dyn Fn(&str) -> Result<Option<String>, String>) -> Result<SigninOutcome, String> {
        let identity = validated_identity(id)?;
        timestamp(now)?;
        // These writes create neither bindings nor avatars: no seat admission
        // is needed. Preserve authority lease -> IMMEDIATE transaction order.
        let _authority = self.google_authority_write().map_err(|_| STORAGE)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        let outcome = resolve_tx(&tx, &identity, policy, now, personal_sub)?;
        tx.commit().map_err(|_| STORAGE)?;
        Ok(outcome)
    }

    pub fn approve_signin_request(&self, admin_id: &str, admin_epoch: u64, request_id: &str,
        mfa_preauthorize: bool, now: i64) -> Result<String, &'static str> {
        timestamp(now)?;
        let _authority = self.google_authority_write().map_err(|_| STORAGE)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        live_admin(&tx, admin_id, admin_epoch)?;
        let request = Self::signin_request_by_id_tx(&tx, request_id).map_err(|_| STORAGE)?.ok_or("signin_request_not_found")?;
        if request.status != SigninRequestStatus::Pending { return Err("signin_request_not_pending"); }
        if request.expires_at <= now { return Err("signin_request_expired"); }
        let linked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM google_signin_links WHERE google_sub=?1)",
            [&request.google_sub], |r| r.get(0)).map_err(|_| STORAGE)?;
        if linked { return Err("signin_sub_linked"); }
        if email_exists(&tx, &request.email)? { return Err("signin_email_exists"); }
        let identity = validated_identity(&VerifiedGoogleIdentity { sub: request.google_sub, email: request.email,
            hd: request.hd, display_name: request.display_name })?;
        let detail = json!({"linked_via": "jit_approved", "hd": identity.hd, "request_id": request_id,
            "mfa_preauthorized": mfa_preauthorize});
        let user_id = create_signin_account_tx(&tx, &identity, admin_id, "jit_approved", &detail, now).map_err(|_| STORAGE)?;
        Self::set_signin_request_status_tx(&tx, request_id, SigninRequestStatus::Approved, admin_id, now,
            Some(&user_id), mfa_preauthorize).map_err(|_| STORAGE)?;
        tx.commit().map_err(|_| STORAGE)?;
        Ok(user_id)
    }

    pub fn reject_signin_request(&self, admin_id: &str, admin_epoch: u64, request_id: &str, now: i64) -> Result<(), &'static str> {
        timestamp(now)?;
        let _authority = self.google_authority_write().map_err(|_| STORAGE)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| STORAGE)?;
        live_admin(&tx, admin_id, admin_epoch)?;
        let request = Self::signin_request_by_id_tx(&tx, request_id).map_err(|_| STORAGE)?.ok_or("signin_request_not_found")?;
        if request.status != SigninRequestStatus::Pending { return Err("signin_request_not_pending"); }
        Self::set_signin_request_status_tx(&tx, request_id, SigninRequestStatus::Rejected, admin_id, now, None, false).map_err(|_| STORAGE)?;
        audit(&tx, admin_id, "user.signin_request_rejected", request_id, &json!({"request_id": request_id}), now)?;
        tx.commit().map_err(|_| STORAGE)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "signin_members_tests.rs"]
mod tests;
