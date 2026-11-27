//! RM-194: proof of an administrator personally attending their own avatar.
//! Every input identifying the turn comes from the live gateway registry.
use std::path::Path;
use duduclaw_knowledge::requester::RequesterIdentity;
use rusqlite::{Connection, OpenFlags, params};
use crate::approval::{ConversationAudience, DecisionContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdminTurnLevel { Read, Write }

#[derive(Debug, Clone)]
pub(crate) struct AdminTurn {
    pub user_id: String,
    pub agent: String,
    pub session_id: String,
    pub turn_id: String,
    pub channel: String,
    pub audience: ConversationAudience,
    pub decision_context: Option<DecisionContext>,
}

/// Stable, parameter-free audit reasons, one per ordered proof check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdminTurnRefusal {
    FeatureDisabled,
    ProtectedExecution,
    InvalidContext,
    Unattended,
    UnverifiedRequester,
    IneligibleAccount,
    InvalidAvatar,
    TurnMismatch,
    PrivateAudienceRequired,
}
impl AdminTurnRefusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::FeatureDisabled => "admin_ai_disabled",
            Self::ProtectedExecution => "admin_ai_protected_execution",
            Self::InvalidContext => "admin_ai_invalid_context",
            Self::Unattended => "admin_ai_unattended",
            Self::UnverifiedRequester => "admin_ai_unverified_requester",
            Self::IneligibleAccount => "admin_ai_ineligible_account",
            Self::InvalidAvatar => "admin_ai_invalid_avatar",
            Self::TurnMismatch => "admin_ai_turn_mismatch",
            Self::PrivateAudienceRequired => "admin_ai_private_audience_required",
        }
    }
}

pub(crate) fn admin_turn(
    home: &Path, caller_agent: &str, context_ref: &str, level: AdminTurnLevel,
) -> Result<AdminTurn, AdminTurnRefusal> {
    use AdminTurnRefusal::*;
    // 1. Strict configuration: unreadable, malformed and absent all disable.
    if !duduclaw_core::admin_ai_settings::enabled_from_home(home) {
        return Err(FeatureDisabled);
    }
    // 2. Use the same host-owned isolation predicate as execution admission.
    if duduclaw_core::execution_gate::isolation_required() {
        return Err(ProtectedExecution);
    }
    // 3. No request-body run/task ids: only the registered runtime counts.
    let entry = super::context::admin_turn_context(home, caller_agent, context_ref)
        .ok_or(InvalidContext)?;
    if entry.run.is_some() || entry.task.is_some() { return Err(InvalidContext); }
    // 4. The proof belongs to this runtime and must still be live.
    let attendance = entry.attendance.as_ref()
        .filter(|call| call.valid_for(caller_agent)).ok_or(Unattended)?;
    // 5. RM-183 re-resolves channel bindings on every evaluation.
    let who = entry.requester.identity();
    match who {
        RequesterIdentity::DashboardUser { .. } => {},
        RequesterIdentity::ChannelUser { channel, user_id: Some(_), .. }
            if !duduclaw_knowledge::requester::is_public_visitor_channel(channel) => {},
        _ => return Err(UnverifiedRequester),
    }
    let user_id = entry.requester.verified_account_id(home)
        .map_err(|_| UnverifiedRequester)?.ok_or(UnverifiedRequester)?;
    // 6. No database creation or Personal-edition bypass. Keep the account
    // and avatar checks in one read transaction so they see one snapshot.
    let mut db = Connection::open_with_flags(home.join("users.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| IneligibleAccount)?;
    let tx = db.transaction().map_err(|_| IneligibleAccount)?;
    let eligible: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users
        WHERE id=?1 AND status='active' AND role='admin' AND must_change_password=0)",
        [&user_id], |row| row.get(0)).map_err(|_| IneligibleAccount)?;
    if !eligible { return Err(IneligibleAccount); }
    // 7. Match owner_self_add's binding and uniqueness semantics exactly.
    let avatar: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users u
        WHERE u.id=?1 AND u.status='active' AND u.avatar_agent_id=?2
        AND EXISTS(SELECT 1 FROM user_agent_bindings b
            WHERE b.user_id=u.id AND b.agent_name=?2 AND b.access_level IN ('owner','operator'))
        AND NOT EXISTS(SELECT 1 FROM users other WHERE other.id<>u.id AND other.avatar_agent_id=?2))",
        params![user_id, caller_agent], |row| row.get(0)).map_err(|_| InvalidAvatar)?;
    if !avatar { return Err(InvalidAvatar); }
    // 8. Compare independent host snapshots, never a turn supplied by a tool.
    let session = attendance.session().ok_or(TurnMismatch)?;
    if entry.turn.as_deref() != Some(attendance.turn())
        || entry.session.as_deref() != Some(session)
    { return Err(TurnMismatch); }
    let turn = attendance.turn();
    let decision_context = crate::turn_context::decision_context_for(caller_agent, turn);
    // 9. Unknown audience is MultiParty for reads and denies writes.
    let (channel, private) = match who {
        RequesterIdentity::DashboardUser { .. } => ("webchat".to_owned(),
            session.starts_with("webchat:") && attendance.verified_webchat()),
        RequesterIdentity::ChannelUser { channel, sender, .. } => {
            let private = crate::turn_context::target_for(caller_agent, turn).is_some_and(|target|
                target.conversation_audience == ConversationAudience::Private
                && decision_context.as_ref().is_some_and(|context|
                    context.principal_id == *sender && context.channel == *channel
                    && context == &target.context));
            (channel.clone(), private)
        }
        _ => return Err(UnverifiedRequester),
    };
    let audience = if private { ConversationAudience::Private } else { ConversationAudience::MultiParty };
    if level == AdminTurnLevel::Write && !private { return Err(PrivateAudienceRequired); }
    Ok(AdminTurn {
        user_id, agent: caller_agent.into(), session_id: session.into(), turn_id: turn.into(),
        channel, audience, decision_context,
    })
}
