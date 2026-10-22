//! Who a request is for, and how that becomes the human side of the ACL.
//!
//! Only the gateway knows the person behind a turn, so it resolves a
//! [`RequesterIdentity`] and this module turns it into an [`HumanCtx`]:
//!
//! | requester | grants that count | 1:1? |
//! |---|---|---|
//! | `DashboardUser` (dashboard, WebChat) | user, department, (Admin: all) | yes |
//! | `ChannelUser` with a verified binding | user, department, channel | per session |
//! | `ChannelUser` without binding | channel only | per session |
//! | `System` on behalf of a user | that user's, group rules | no |
//! | `System` (cron, heartbeat, dispatch), registered by the gateway | public / internal collections granted to the employee | no |
//! | `Unknown` (no registered turn) | none | — |
//!
//! Anything that cannot be resolved (an unknown or disabled user, a lookup
//! error) serves nobody: user-scoped collections are denied.

use crate::acl::{AgentCtx, AgentNarrowing, HumanCtx, HumanMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequesterIdentity {
    /// A signed-in dashboard user (dashboard pages, WebChat).
    DashboardUser { user_id: String },
    /// A messaging-app sender. `user_id` only from a verified binding.
    ChannelUser {
        channel: String,
        chat: String,
        sender: String,
        user_id: Option<String>,
    },
    /// No person in the loop, as registered by the gateway (cron,
    /// heartbeat, dispatch).
    System {
        kind: String,
        on_behalf_of: Option<String>,
    },
    /// Nothing the gateway registered matches: serves nobody.
    Unknown,
}

impl RequesterIdentity {
    /// `access_log.requester_kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            RequesterIdentity::DashboardUser { .. } => "dashboard_user",
            RequesterIdentity::ChannelUser {
                user_id: Some(_), ..
            } => "channel_user",
            RequesterIdentity::ChannelUser { user_id: None, .. } => "channel_unbound",
            RequesterIdentity::System { .. } => "system",
            RequesterIdentity::Unknown => "unknown",
        }
    }

    /// `access_log.requester_id`: a user id, or a digest for an unbound
    /// sender (their channel id is not stored).
    pub fn audit_id(&self) -> Option<String> {
        match self {
            RequesterIdentity::DashboardUser { user_id } => Some(user_id.clone()),
            RequesterIdentity::ChannelUser {
                user_id: Some(u), ..
            } => Some(u.clone()),
            RequesterIdentity::ChannelUser {
                channel,
                sender,
                user_id: None,
                ..
            } => (!sender.is_empty()).then(|| {
                format!(
                    "sha256:{}",
                    &crate::sha256_hex(format!("{channel}\n{sender}").as_bytes())[..16]
                )
            }),
            RequesterIdentity::System { on_behalf_of, .. } => on_behalf_of.clone(),
            RequesterIdentity::Unknown => None,
        }
    }

    pub fn channel(&self) -> Option<&str> {
        match self {
            RequesterIdentity::ChannelUser { channel, .. } => Some(channel),
            _ => None,
        }
    }
}

/// A resolved dashboard account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUser {
    pub user_id: String,
    pub department: Option<String>,
    pub is_admin: bool,
}

/// Account lookup (the gateway's user database). `Ok(None)` for an unknown
/// or inactive account.
pub trait UserDirectory {
    fn lookup(&self, user_id: &str) -> Result<Option<ResolvedUser>, String>;
}

fn lookup(dir: &dyn UserDirectory, user_id: &str) -> Option<ResolvedUser> {
    match dir.lookup(user_id) {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(error = %e, "knowledge: requester lookup failed; denying user-scoped collections");
            None
        }
    }
}

/// Human side of the ACL for `req`.
pub fn human_ctx(req: &RequesterIdentity, dir: &dyn UserDirectory) -> HumanCtx {
    match req {
        RequesterIdentity::DashboardUser { user_id } => match lookup(dir, user_id) {
            Some(u) => HumanCtx::dashboard(&u.user_id, u.department.as_deref(), u.is_admin),
            None => HumanCtx::nobody(),
        },
        RequesterIdentity::ChannelUser {
            channel,
            chat,
            sender,
            user_id,
        } => {
            let session = format!("{channel}:{chat}");
            let private = duduclaw_core::sensitivity::is_private_session(&session, sender);
            let user = user_id.as_deref().and_then(|u| lookup(dir, u));
            HumanCtx {
                mode: if user.is_some() {
                    HumanMode::Full
                } else {
                    HumanMode::ChannelOnly
                },
                user_id: user.as_ref().map(|u| u.user_id.clone()),
                user_department: user.as_ref().and_then(|u| u.department.clone()),
                channel: Some(channel.clone()),
                channel_chat: Some(session),
                is_admin: user.as_ref().is_some_and(|u| u.is_admin),
                is_private: private,
            }
        }
        RequesterIdentity::System {
            on_behalf_of: Some(uid),
            ..
        } => match lookup(dir, uid) {
            Some(u) => HumanCtx {
                is_private: false,
                ..HumanCtx::dashboard(&u.user_id, u.department.as_deref(), u.is_admin)
            },
            None => HumanCtx::nobody(),
        },
        RequesterIdentity::Unknown => HumanCtx::nobody(),
        RequesterIdentity::System {
            on_behalf_of: None, ..
        } => HumanCtx {
            mode: HumanMode::InternalOnly,
            ..HumanCtx::nobody()
        },
    }
}

/// Agent side from an employee's `[knowledge]` scope.
pub fn agent_ctx(
    agent_id: &str,
    department: Option<&str>,
    scope: &duduclaw_core::knowledge_agent::KnowledgeScope,
) -> AgentCtx {
    use duduclaw_core::knowledge_agent::KnowledgeScope;
    AgentCtx {
        agent_id: agent_id.to_string(),
        department: department.map(str::to_string).filter(|d| !d.is_empty()),
        narrowing: match scope {
            KnowledgeScope::All => AgentNarrowing::All,
            KnowledgeScope::Only(list) => AgentNarrowing::Only(list.clone()),
            KnowledgeScope::Unreadable => AgentNarrowing::Only(Vec::new()),
        },
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;

    pub(crate) struct Users(pub HashMap<String, ResolvedUser>);

    impl UserDirectory for Users {
        fn lookup(&self, user_id: &str) -> Result<Option<ResolvedUser>, String> {
            if user_id == "broken" {
                return Err("db down".into());
            }
            Ok(self.0.get(user_id).cloned())
        }
    }

    pub(crate) fn users(list: &[(&str, Option<&str>, bool)]) -> Users {
        Users(
            list.iter()
                .map(|(id, dept, admin)| {
                    (
                        id.to_string(),
                        ResolvedUser {
                            user_id: id.to_string(),
                            department: dept.map(str::to_string),
                            is_admin: *admin,
                        },
                    )
                })
                .collect(),
        )
    }
}
