//! Gateway-authenticated job provenance, bound to persisted execution material.
use super::*;
use duduclaw_core::delegation_policy::{DelegationContext, evaluate_delegation, read_delegation_config};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::future::Future;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    kind: String,
    id: String,
    digest: String,
    sender: String,
    target: String,
    account: String,
    channel: Option<(String, String)>,
}
fn signature(home: &Path) -> Result<Hmac<Sha256>, OwnerRoleLookupError> {
    // This is the gateway identity key. Receipts provide integrity only; a
    // process that can read this key is already inside the trust boundary,
    // just as it is for agent tokens.
    let key = duduclaw_core::load_identity_key(home).ok_or(OwnerRoleLookupError)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| OwnerRoleLookupError)?;
    mac.update(b"duduclaw/delegation-initiator/v1\n");
    Ok(mac)
}
fn database(home: &Path, write: bool) -> Result<crate::fs_safe::sqlite::Connection, OwnerRoleLookupError> {
    #[cfg(unix)]
    let db = if write {
        crate::fs_safe::sqlite::open(home, "delegation-initiators.db")
    } else {
        crate::fs_safe::sqlite::open_existing(home, "delegation-initiators.db", crate::fs_safe::sqlite::Access::ReadOnly)
    }.map_err(|_| OwnerRoleLookupError)?;
    #[cfg(not(unix))]
    // Windows pins the protected secure-db directory and returns a guarded
    // connection. The secure opener refuses every other non-Unix platform.
    let db = crate::fs_safe::sqlite::open(home, "delegation-initiators.db")
        .map_err(|_| OwnerRoleLookupError)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| OwnerRoleLookupError)?;
    if write {
        db.execute_batch("CREATE TABLE IF NOT EXISTS initiators (
            kind TEXT NOT NULL, id TEXT NOT NULL, target TEXT NOT NULL, digest TEXT NOT NULL,
            body BLOB NOT NULL, mac BLOB NOT NULL, PRIMARY KEY(kind, id, target, digest))")
            .map_err(|_| OwnerRoleLookupError)?;
    }
    Ok(db)
}

/// Remove every version and target before durable work loses its authority.
pub(crate) fn revoke(home: &Path, kind: &str, id: &str) -> Result<(), OwnerRoleLookupError> {
    let path = crate::fs_safe::sqlite::database_path(home, "delegation-initiators.db");
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(OwnerRoleLookupError),
        Ok(_) => {},
    }
    let db = database(home, true)?;
    db.execute("DELETE FROM initiators WHERE kind=?1 AND id=?2", rusqlite::params![kind, id])
        .map_err(|_| OwnerRoleLookupError)?;
    Ok(())
}

/// A receipt-store failure must never prevent stopping the actual work.
pub(crate) fn revoke_before_write(home: &Path, kind: &str, id: &str) {
    if revoke(home, kind, id).is_err() {
        tracing::error!(kind, id, "delegation initiator revocation failed");
        crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
            "delegation_initiator_revoke_failed", "system", duduclaw_security::audit::Severity::Critical,
            serde_json::json!({"kind": kind, "id": id}),
        ));
    }
}

fn material_digest(home: &Path, kind: &str, id: &str, target: &str, sender: Option<&str>)
    -> Result<String, OwnerRoleLookupError> {
    if kind == "message" { return Ok(String::new()); }
    if !matches!(kind, "cron" | "reminder" | "task" | "task_spec") { return Err(OwnerRoleLookupError); }
    let material_id = if kind == "task_spec" { format!("{target}:{id}:step:0") } else { id.into() };
    let material = crate::isolation_background::material::read_current(home, kind, &material_id)
        .map_err(|_| OwnerRoleLookupError)?;
    let target_field = if kind == "task" { "assigned_to" } else { "agent_id" };
    if material.get(target_field).and_then(serde_json::Value::as_str) != Some(target) {
        return Err(OwnerRoleLookupError);
    }
    if let Some(sender) = sender {
        let creator = match kind {
            "reminder" => material.get("sender_agent").and_then(serde_json::Value::as_str),
            "task" => {
                // MCP stores an agent id; dashboard stores the exact account
                // id, while synthetic creators use system/goal:/a2a: names.
                let creator = material.get("created_by").and_then(serde_json::Value::as_str)
                    .ok_or(OwnerRoleLookupError)?;
                let account: bool = Connection::open_with_flags(home.join("users.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|_| OwnerRoleLookupError)?.query_row(
                        "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1)", [creator], |r| r.get(0))
                    .map_err(|_| OwnerRoleLookupError)?;
                (!account && duduclaw_core::is_valid_agent_id(creator)
                    && !duduclaw_core::is_reserved_agent_id(creator)).then_some(creator)
            },
            // Cron material has no creating-agent field.
            _ => None,
        };
        if creator.is_some_and(|creator| creator != sender) { return Err(OwnerRoleLookupError); }
    }
    let bytes = serde_json::to_vec(&material).map_err(|_| OwnerRoleLookupError)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn record(home: &Path, kind: &str, sender: &str, target: &str, id: &str, requester: &Requester)
    -> Result<(), OwnerRoleLookupError> {
    if !matches!(kind, "message" | "cron" | "reminder" | "task" | "task_spec")
        || id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
        || !duduclaw_core::is_valid_agent_id(sender) || duduclaw_core::is_reserved_agent_id(sender)
        || !duduclaw_core::is_valid_agent_id(target) || duduclaw_core::is_reserved_agent_id(target) {
        return Err(OwnerRoleLookupError);
    }
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
    let Some((account, _)) = roles.requester_id()? else { return Ok(()); };
    let digest = material_digest(home, kind, id, target, Some(sender))?;
    let channel = match &requester.who {
        RequesterIdentity::ChannelUser { channel, sender, .. } => Some((channel.clone(), sender.clone())),
        _ => None,
    };
    let receipt = Receipt { kind: kind.into(), id: id.into(), digest: digest.clone(),
        sender: sender.into(), target: target.into(), account, channel };
    let body = serde_json::to_vec(&receipt).map_err(|_| OwnerRoleLookupError)?;
    let mut mac = signature(home)?;
    mac.update(&body);
    let tag = mac.finalize().into_bytes().to_vec();
    let db = database(home, true)?;
    // First initiator wins for each exact version of the work.
    db.execute("INSERT OR IGNORE INTO initiators(kind,id,target,digest,body,mac) VALUES(?1,?2,?3,?4,?5,?6)",
        rusqlite::params![kind, id, target, digest, body, tag]).map_err(|_| OwnerRoleLookupError)?;
    let stored: Vec<u8> = db.query_row("SELECT body FROM initiators WHERE kind=?1 AND id=?2 AND target=?3 AND digest=?4",
        rusqlite::params![kind, id, target, digest], |r| r.get(0)).map_err(|_| OwnerRoleLookupError)?;
    if stored != body { return Err(OwnerRoleLookupError); }
    Ok(())
}

pub(crate) fn lookup(home: &Path, kind: &str, id: &str, sender: Option<&str>, target: &str,
    principal: Option<&str>) -> Option<Requester> {
    let digest = material_digest(home, kind, id, target, None).ok()?;
    let db = database(home, false).ok()?;
    let (body, tag): (Vec<u8>, Vec<u8>) = db.query_row(
        "SELECT body,mac FROM initiators WHERE kind=?1 AND id=?2 AND target=?3 AND digest=?4 AND length(body)<=4096 AND length(mac)=32",
        rusqlite::params![kind, id, target, digest], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok()??;
    let mut mac = signature(home).ok()?;
    mac.update(&body);
    mac.verify_slice(&tag).ok()?;
    let receipt: Receipt = serde_json::from_slice(&body).ok()?;
    if receipt.kind != kind || receipt.id != id || receipt.target != target || receipt.digest != digest
        || sender.is_some_and(|s| s != receipt.sender) { return None; }
    let named = duduclaw_core::HumanPrincipal::parse(principal?).ok()?;
    if named.user_id() != Some(receipt.account.as_str()) { return None; }
    let who = match receipt.channel {
        Some((channel, sender)) => RequesterIdentity::ChannelUser { channel, sender,
            user_id: Some(receipt.account), chat: String::new(), attested_private: false },
        None => RequesterIdentity::DashboardUser { user_id: receipt.account },
    };
    Some(Requester { who, background: true })
}

tokio::task_local! { static INITIATOR: (String, Option<Requester>); }
pub(crate) fn current(agent: &str) -> Option<Option<Requester>> {
    INITIATOR.try_with(|(target, requester)| if target == agent { requester.clone() } else { None }).ok()
}
pub(crate) fn scope<F: Future>(target: &str, requester: Option<Requester>, future: F) -> impl Future<Output = F::Output> + use<F> {
    INITIATOR.scope((target.into(), requester), Box::pin(future))
}
