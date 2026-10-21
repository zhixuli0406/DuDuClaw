//! Who the sender of a message is, for deciding which employees they may
//! call. Every lookup failure narrows (fail closed): a broken identity store
//! never grants more than an unknown sender gets.

use std::collections::BTreeSet;
use std::path::Path;

use duduclaw_agent::registry::LoadedAgent;
use duduclaw_auth::{UserDb, UserRole, UserStatus};

/// The human behind a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// An active dashboard Admin: may call everyone.
    Admin,
    /// An active dashboard account with these employee bindings.
    Account {
        user_id: String,
        bound: BTreeSet<String>,
    },
    /// Nobody we can name: a messaging-app sender without a verified
    /// identity, a public web-widget visitor, or a failed lookup.
    Unmapped,
}

/// A sender plus where they speak from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderAccess {
    pub sender: Sender,
    /// True for messaging apps: a mapped account also keeps what an unmapped
    /// sender gets there (the default employee and `public_trigger` ones), so
    /// linking an account never takes access away. False for the dashboard
    /// WebChat, where only the account's own bindings count.
    pub channel: bool,
}

impl SenderAccess {
    /// An unmapped messaging-app sender.
    pub fn unmapped() -> Self {
        Self {
            sender: Sender::Unmapped,
            channel: true,
        }
    }

    /// From a role and binding set already loaded (e.g. WebChat's ACL cache).
    pub fn from_role(
        user_id: &str,
        role: UserRole,
        bound: BTreeSet<String>,
        channel: bool,
    ) -> Self {
        let sender = if role == UserRole::Admin {
            Sender::Admin
        } else {
            Sender::Account {
                user_id: user_id.to_string(),
                bound,
            }
        };
        Self { sender, channel }
    }

    /// May this sender call `agent` in a conversation whose default employee
    /// is `fallback`?
    pub fn may_call(&self, agent: &LoadedAgent, fallback: Option<&str>) -> bool {
        let id = agent.config.agent.name.as_str();
        let open_here = || fallback == Some(id) || is_public_trigger(agent);
        match &self.sender {
            Sender::Admin => true,
            Sender::Account { bound, .. } => bound.contains(id) || (self.channel && open_here()),
            Sender::Unmapped => open_here(),
        }
    }

    /// Stable, non-identifying label for logs and the audit trail.
    pub fn audit_label(&self, channel: &str, channel_sender: &str) -> String {
        match &self.sender {
            Sender::Admin => "admin".to_string(),
            Sender::Account { user_id, .. } => format!("user:{user_id}"),
            Sender::Unmapped => format!(
                "unmapped:{}:{}",
                channel,
                duduclaw_core::truncate_bytes(
                    &crate::channel_ingress::digest(&[channel, channel_sender]),
                    12
                )
            ),
        }
    }
}

/// `[channels] public_trigger = true` on this employee.
pub fn is_public_trigger(agent: &LoadedAgent) -> bool {
    agent
        .config
        .channels
        .as_ref()
        .is_some_and(|c| c.public_trigger)
}

/// A dashboard account (WebChat). Unknown, inactive or unreadable accounts
/// may call nobody.
pub fn for_dashboard_user(db: &UserDb, user_id: &str) -> SenderAccess {
    match load_account(db, user_id) {
        Ok(Some((role, bound))) => SenderAccess::from_role(user_id, role, bound, false),
        Ok(None) | Err(_) => SenderAccess {
            sender: Sender::Account {
                user_id: user_id.to_string(),
                bound: BTreeSet::new(),
            },
            channel: false,
        },
    }
}

/// A messaging-app sender: mapped through a **verified** `channel_identities`
/// row to an active dashboard account, else unmapped. An absent or unreadable
/// identity store, an unverified row or an inactive account all mean
/// unmapped (the least an app sender can have).
pub fn for_channel_sender(home: &Path, channel: &str, channel_user_id: &str) -> SenderAccess {
    if channel_user_id.is_empty() {
        return SenderAccess::unmapped();
    }
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else {
        return SenderAccess::unmapped();
    };
    let Ok(Some(user_id)) = db.find_verified_user_id_by_channel(channel, channel_user_id) else {
        return SenderAccess::unmapped();
    };
    match load_account(&db, &user_id) {
        Ok(Some((role, bound))) => SenderAccess::from_role(&user_id, role, bound, true),
        _ => SenderAccess::unmapped(),
    }
}

/// Role and bindings of an active account; `None` when missing or inactive.
fn load_account(
    db: &UserDb,
    user_id: &str,
) -> Result<Option<(UserRole, BTreeSet<String>)>, String> {
    let Some(user) = db.get_user(user_id)? else {
        return Ok(None);
    };
    if user.status != UserStatus::Active {
        return Ok(None);
    }
    let bound = db
        .get_user_agents(user_id)?
        .into_iter()
        .map(|b| b.agent_name)
        .collect();
    Ok(Some((user.role, bound)))
}
