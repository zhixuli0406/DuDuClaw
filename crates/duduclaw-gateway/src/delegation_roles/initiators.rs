//! One-shot evidence for an attended message, used only at the C1 queue gate.
use super::*;
use duduclaw_core::delegation_policy::{DelegationContext, evaluate_delegation, read_delegation_config};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const MAX_AGE_SECS: u64 = 24 * 60 * 60;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    id: String,
    sender: String,
    target: String,
    account: String,
    channel: Option<(String, String)>,
    created_at: u64,
}
fn now() -> Result<u64, OwnerRoleLookupError> {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).map_err(|_| OwnerRoleLookupError)
}
fn signature(home: &Path) -> Result<Hmac<Sha256>, OwnerRoleLookupError> {
    // Receipts provide integrity inside the gateway identity-key boundary.
    let key = duduclaw_core::load_identity_key(home).ok_or(OwnerRoleLookupError)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| OwnerRoleLookupError)?;
    mac.update(b"duduclaw/delegation-initiator/v2\n");
    Ok(mac)
}
fn database(home: &Path, write: bool) -> Result<crate::fs_safe::sqlite::Connection, OwnerRoleLookupError> {
    #[cfg(unix)]
    let db = if write {
        crate::fs_safe::sqlite::open(home, "delegation-initiators.db")
    } else {
        crate::fs_safe::sqlite::open_existing(home, "delegation-initiators.db", crate::fs_safe::sqlite::Access::ReadWrite)
    }.map_err(|_| OwnerRoleLookupError)?;
    #[cfg(not(unix))]
    // Windows retains its guarded secure-db connection; other platforms refuse.
    let db = crate::fs_safe::sqlite::open(home, "delegation-initiators.db")
        .map_err(|_| OwnerRoleLookupError)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| OwnerRoleLookupError)?;
    if write {
        // Never reinterpret the old durable-work initiators table.
        db.execute_batch("CREATE TABLE IF NOT EXISTS message_initiators (
            id TEXT PRIMARY KEY, sender TEXT NOT NULL, target TEXT NOT NULL,
            created_at INTEGER NOT NULL, body BLOB NOT NULL, mac BLOB NOT NULL)")
            .map_err(|_| OwnerRoleLookupError)?;
    }
    Ok(db)
}

pub(crate) fn record(home: &Path, kind: &str, sender: &str, target: &str, id: &str, requester: &Requester)
    -> Result<(), OwnerRoleLookupError> {
    if kind != "message"
        || id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
        || !duduclaw_core::is_valid_agent_id(sender) || duduclaw_core::is_reserved_agent_id(sender)
        || !duduclaw_core::is_valid_agent_id(target) || duduclaw_core::is_reserved_agent_id(target) {
        return Err(OwnerRoleLookupError);
    }
    if requester.background { return Ok(()); }
    let roles = GatewayOwnerRoles::new(home, requester.clone());
    let read = read_delegation_config(home);
    // Sender-only eligibility probe: pairs/modes cannot mint receipts. The
    // MCP permission gate already checks can_send_cross_agent for each tool.
    if evaluate_delegation(&read.rules, &duduclaw_core::MapOrgView::new(), sender,
        "__delegation_eligibility__", &DelegationContext {
            overrides: &read.overrides, owner_roles: Some(&roles), extra_system_senders: &[],
            boundaries: DelegationBoundaries { personal_target_allowed: true,
                main_target_allowed: true, can_send_cross_agent: true },
        }) != Ok(DelegationAllowSource::UnrestrictedRole) { return Ok(()); }
    let Some((account, false)) = roles.requester_id()? else { return Ok(()); };
    let channel = match &requester.who {
        RequesterIdentity::ChannelUser { channel, sender, .. } => Some((channel.clone(), sender.clone())),
        _ => None,
    };
    let created_at = now()?;
    let receipt = Receipt { id: id.into(), sender: sender.into(), target: target.into(),
        account, channel, created_at };
    let body = serde_json::to_vec(&receipt).map_err(|_| OwnerRoleLookupError)?;
    let mut mac = signature(home)?;
    mac.update(&body);
    let tag = mac.finalize().into_bytes().to_vec();
    let db = database(home, true)?;
    db.execute("DELETE FROM message_initiators WHERE created_at<=?1",
        [created_at.saturating_sub(MAX_AGE_SECS)]).map_err(|_| OwnerRoleLookupError)?;
    // First initiator wins; a repeated record cannot refresh the signed lifetime.
    db.execute("INSERT OR IGNORE INTO message_initiators(id,sender,target,created_at,body,mac) VALUES(?1,?2,?3,?4,?5,?6)",
        rusqlite::params![id, sender, target, created_at, body, tag]).map_err(|_| OwnerRoleLookupError)?;
    let (stored, stored_tag) = read_receipt(&db, id, sender, target).ok_or(OwnerRoleLookupError)?;
    let existing = verify(home, &stored, &stored_tag, id, sender, target, Some(&format!("user:{}", receipt.account)))
        .ok_or(OwnerRoleLookupError)?;
    if existing.channel != receipt.channel { return Err(OwnerRoleLookupError); }
    Ok(())
}

fn read_receipt(db: &Connection, id: &str, sender: &str, target: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    db.query_row("SELECT body,mac FROM message_initiators WHERE id=?1 AND sender=?2 AND target=?3
        AND length(body)<=4096 AND length(mac)=32", rusqlite::params![id, sender, target],
        |r| Ok((r.get(0)?, r.get(1)?))).optional().ok()?
}
fn verify(home: &Path, body: &[u8], tag: &[u8], id: &str, sender: &str, target: &str,
    principal: Option<&str>) -> Option<Receipt> {
    let mut mac = signature(home).ok()?;
    mac.update(body);
    mac.verify_slice(tag).ok()?;
    let receipt: Receipt = serde_json::from_slice(body).ok()?;
    if receipt.id != id || receipt.sender != sender || receipt.target != target
        || now().ok()?.checked_sub(receipt.created_at)? >= MAX_AGE_SECS { return None; }
    let named = duduclaw_core::HumanPrincipal::parse(principal?).ok()?;
    if named.user_id() != Some(receipt.account.as_str()) { return None; }
    Some(receipt)
}
fn requester(receipt: Receipt) -> Requester {
    let who = match receipt.channel {
        Some((channel, sender)) => RequesterIdentity::ChannelUser { channel, sender,
            user_id: Some(receipt.account), chat: String::new(), attested_private: false },
        None => RequesterIdentity::DashboardUser { user_id: receipt.account },
    };
    // Evidence of the creating turn, never installed into the dispatched turn.
    Requester { who, background: false }
}

/// Lock, verify, evaluate all gates, then delete and commit before returning an
/// unrestricted allow. A denied decision or failed commit leaves no authority.
pub(crate) fn consume_if(home: &Path, id: &str, sender: &str, target: &str, principal: Option<&str>,
    allow: impl FnOnce(Requester) -> Option<DelegationAllowSource>) -> Option<DelegationAllowSource> {
    let mut db = database(home, false).ok()?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).ok()?;
    let (body, tag) = read_receipt(&tx, id, sender, target)?;
    let receipt = verify(home, &body, &tag, id, sender, target, principal)?;
    let source = allow(requester(receipt))?;
    if source != DelegationAllowSource::UnrestrictedRole { return None; }
    if tx.execute("DELETE FROM message_initiators WHERE id=?1", [id]).ok()? != 1 { return None; }
    tx.commit().ok()?;
    Some(source)
}

#[cfg(test)]
pub(crate) fn lookup(home: &Path, kind: &str, id: &str, sender: Option<&str>, target: &str,
    principal: Option<&str>) -> Option<Requester> {
    if kind != "message" { return None; }
    let sender = sender?;
    let db = database(home, false).ok()?;
    let (body, tag) = read_receipt(&db, id, sender, target)?;
    verify(home, &body, &tag, id, sender, target, principal).map(requester)
}

#[cfg(test)]
pub(super) fn set_created_at(home: &Path, id: &str, created_at: u64) {
    let db = database(home, false).unwrap();
    let body: Vec<u8> = db.query_row("SELECT body FROM message_initiators WHERE id=?1", [id], |r| r.get(0)).unwrap();
    let mut receipt: Receipt = serde_json::from_slice(&body).unwrap();
    receipt.created_at = created_at;
    let body = serde_json::to_vec(&receipt).unwrap();
    let mut mac = signature(home).unwrap();
    mac.update(&body);
    let tag = mac.finalize().into_bytes().to_vec();
    db.execute("UPDATE message_initiators SET body=?2,mac=?3,created_at=?4 WHERE id=?1",
        rusqlite::params![id, body, tag, created_at]).unwrap();
}
