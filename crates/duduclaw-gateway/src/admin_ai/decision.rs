//! Proposer-only decisions. Identity is supplied by authenticated transports.
use std::path::Path;
use rusqlite::{Connection, OpenFlags, params};
use crate::approval::{ApprovalRecord, ConversationAudience, RequestKind,
    chat_admin::{ledger, LedgerEntry}, decision_service::ChatAdminProof};

pub(crate) const REFUSAL: &str = "只有提出變更的管理員本人能確認或取消。";

pub(crate) fn may_decide(home: &Path, rec: &ApprovalRecord, user: &str) -> bool {
    if rec.action_kind != super::card::ACTION_KIND || rec.request_kind != RequestKind::Approval
        || rec.payload["admin_user_id"].as_str() != Some(user)
        || rec.payload["arguments"]["admin_user_id"].as_str() != Some(user)
        || !duduclaw_core::HumanPrincipal::user(user)
            .is_some_and(|p| rec.on_behalf_of.as_deref() == Some(p.to_string().as_str()))
        || rec.binding.as_ref().is_none_or(|b| b.actor_principal != rec.agent_id
            || b.run_origin_kind != "ingress" || b.resume_handler != "live_only_v1"
            || b.conversation_audience != ConversationAudience::Private)
    { return false; }
    let Ok(mut db) = Connection::open_with_flags(home.join("users.db"), OpenFlags::SQLITE_OPEN_READ_ONLY) else { return false; };
    let Ok(tx) = db.transaction() else { return false; };
    // Same account/avatar snapshot and SQL predicates as admin_turn.
    tx.query_row("SELECT EXISTS(SELECT 1 FROM users u
        WHERE u.id=?1 AND u.status='active' AND u.role='admin' AND u.must_change_password=0
        AND u.avatar_agent_id=?2
        AND EXISTS(SELECT 1 FROM user_agent_bindings b
            WHERE b.user_id=u.id AND b.agent_name=?2 AND b.access_level IN ('owner','operator'))
        AND NOT EXISTS(SELECT 1 FROM users other WHERE other.id<>u.id AND other.avatar_agent_id=?2))",
        params![user, rec.agent_id], |row| row.get::<_, bool>(0)).unwrap_or(false)
}

fn same_private_context(rec: &ApprovalRecord, channel: &str, account: &str, principal: &str, dm: &str) -> bool {
    rec.binding.as_ref().is_some_and(|b| {
        let c = &b.decision_context;
        b.conversation_audience == ConversationAudience::Private
            && c.channel == channel && c.account_id == account
            && c.principal_id == principal && c.conversation_id == dm
    })
}

pub(crate) fn may_decide_chat(home: &Path, rec: &ApprovalRecord, proof: &ChatAdminProof) -> bool {
    may_decide(home, rec, &proof.user_id)
        && same_private_context(rec, proof.channel.as_str(), &proof.account, &proof.principal, &proof.dm)
}

/// Recheck delivery coordinates inside the decision transaction, including
/// callers of the broker below the service. No connection lock is acquired.
pub(crate) fn receipt_matches(conn: &Connection, rec: &ApprovalRecord, entry: &LedgerEntry) -> bool {
    conn.query_row("SELECT account, principal, dm FROM chat_admin_cards
        WHERE card_id=?1 AND approval_id=?2 AND purpose='rm143' AND user_id=?3
        AND channel=?4 AND digest=?5 AND status='live'",
        params![entry.card_id, rec.id.as_str(), entry.user_id.to_string(), entry.channel.as_str(), entry.digest],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .is_ok_and(|(account, principal, dm)| same_private_context(rec, entry.channel.as_str(), &account, &principal, &dm))
}

/// Called under the grant's consume transaction. A string alone is never a
/// chat decision: it must name the proposer and have a matching durable ledger.
pub(crate) fn decided_by_admin(home: &Path, rec: &ApprovalRecord, conn: &Connection) -> bool {
    let Some(user) = rec.payload["admin_user_id"].as_str() else { return false; };
    if !may_decide(home, rec, user) { return false; }
    if rec.decided_by.as_deref() == Some(format!("dashboard:{user}").as_str()) { return true; }
    rec.decided_by.as_deref().and_then(|s| s.parse::<ledger::DecidedBy>().ok())
        .is_some_and(|by| by.user_id() == user
            && rec.binding.as_ref().is_some_and(|b| b.decision_context.channel == by.channel().as_str())
            && ledger::accepts(conn, rec))
}
