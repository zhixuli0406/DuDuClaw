//! Durable evidence for chat-admin decisions; a decided_by string alone is not authority.

use std::{fmt, path::Path, str::FromStr};

use rusqlite::{Connection, params};
use uuid::Uuid;

use super::{ChatChannel, Tier, tier};
use crate::approval::{ApprovalId, ApprovalRecord, ApprovalStatus, verify_live_io};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Tap,
    Web,
    Totp,
}

impl Step {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Tap => "tap",
            Self::Web => "web",
            Self::Totp => "totp",
        }
    }
}

pub(super) fn parse_channel(value: &str) -> Result<ChatChannel, &'static str> {
    match value {
        "telegram" => Ok(ChatChannel::Telegram),
        "googlechat" => Ok(ChatChannel::Googlechat),
        "line" => Ok(ChatChannel::Line),
        "slack" => Ok(ChatChannel::Slack),
        "discord" => Ok(ChatChannel::Discord),
        _ => Err("invalid chat-admin channel"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecidedBy {
    // Preserve the validated spelling so ledger comparisons use exact equality.
    user_id: String,
    channel: ChatChannel,
    step: Step,
}

impl DecidedBy {
    pub(crate) fn user_id(&self) -> &str { &self.user_id }

    pub(crate) fn channel(&self) -> ChatChannel { self.channel }

    pub(crate) fn new(user_id: Uuid, channel: ChatChannel, step: Step) -> Self {
        Self { user_id: user_id.to_string(), channel, step }
    }
}

impl FromStr for DecidedBy {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts: Vec<_> = value.split(':').collect();
        let ["chat-admin", user_id, channel, step] = parts.as_slice() else {
            return Err("invalid chat-admin decided_by shape");
        };
        Uuid::parse_str(user_id).map_err(|_| "invalid chat-admin user UUID")?;
        let step = match *step {
            "tap" => Step::Tap,
            "web" => Step::Web,
            "totp" => Step::Totp,
            _ => return Err("invalid chat-admin step"),
        };
        Ok(Self { user_id: (*user_id).to_string(), channel: parse_channel(channel)?, step })
    }
}

impl fmt::Display for DecidedBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chat-admin:{}:{}:{}", self.user_id, self.channel.as_str(), self.step.as_str())
    }
}

/// Only caller-verified identity and proof may populate this entry.
/// step_required describes this decision (a denial needs no step-up).
#[derive(Debug, Clone)]
pub(crate) struct LedgerEntry {
    pub(crate) user_id: Uuid,
    pub(crate) channel: ChatChannel,
    pub(crate) step: Step,
    pub(crate) step_required: bool,
    pub(crate) card_id: String,
    pub(crate) digest: String,
}

impl LedgerEntry {
    pub(crate) fn decided_by(&self) -> DecidedBy {
        DecidedBy::new(self.user_id, self.channel, self.step)
    }
}

/// Fail closed on absent tables, malformed rows, or any read/authority error.
pub(crate) fn accepts(conn: &Connection, rec: &ApprovalRecord) -> bool {
    if verify_live_io().is_err() || !matches!(tier(rec), Tier::T1 | Tier::T2) {
        return false;
    }
    let Some(by) = rec.decided_by.as_deref().and_then(|value| value.parse::<DecidedBy>().ok()) else {
        return false;
    };
    let approve = match rec.status {
        ApprovalStatus::Approved => true,
        ApprovalStatus::Denied => false,
        _ => return false,
    };
    conn.query_row(
        "SELECT user_id, channel, step, step_required, approve
         FROM chat_admin_decisions WHERE approval_id = ?1",
        [rec.id.as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
            row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?)),
    ).is_ok_and(|(user_id, channel, step, required, approved)| {
        user_id == by.user_id && channel == by.channel.as_str() && step == by.step.as_str()
            && approved == i64::from(approve)
            && (required == 0 || (required == 1 && by.step != Step::Tap))
    })
}

/// Blocking read-only lookup, with the same opening policy as get_readonly.
/// Async callers must use spawn_blocking; missing databases never get created.
pub(crate) fn accepts_readonly(home: &Path, rec: &ApprovalRecord) -> bool {
    match crate::review_evidence::audience::readonly_db(&home.join("approvals.db")) {
        Ok(conn) => accepts(&conn, rec),
        Err(_) => false,
    }
}

/// Called only inside the approval status write transaction. Any error must
/// propagate so neither a partial decision nor a consumed receipt can commit.
pub(in crate::approval) fn insert_and_consume(
    tx: &rusqlite::Transaction<'_>,
    id: &ApprovalId,
    approve: bool,
    entry: &LedgerEntry,
    decided_at: &str,
) -> Result<(), String> {
    let consumed = tx.execute(
        "UPDATE chat_admin_cards SET status = 'consumed'
         WHERE card_id = ?1 AND approval_id = ?2 AND purpose = 'rm143'
         AND user_id = ?3 AND channel = ?4 AND digest = ?5 AND status = 'live'
         AND julianday(valid_until) IS NOT NULL AND julianday(?6) < julianday(valid_until)",
        params![entry.card_id, id.as_str(), entry.user_id.to_string(), entry.channel.as_str(),
            entry.digest, decided_at],
    ).map_err(|e| format!("consume chat-admin receipt: {e}"))?;
    if consumed != 1 {
        return Err("chat-admin receipt missing, mismatched or no longer live".into());
    }
    tx.execute(
        "INSERT INTO chat_admin_decisions
         (approval_id, card_id, user_id, channel, step, step_required, digest, approve, decided_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![id.as_str(), entry.card_id, entry.user_id.to_string(), entry.channel.as_str(),
            entry.step.as_str(), entry.step_required, entry.digest, approve, decided_at],
    ).map_err(|e| format!("insert chat-admin decision: {e}"))?;
    Ok(())
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "consumer_test_support.rs"]
pub(crate) mod consumer_test_support;
