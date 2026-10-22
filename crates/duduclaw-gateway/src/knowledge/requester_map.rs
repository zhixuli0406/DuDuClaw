//! Who a channel reply turn is for, resolved once per turn before the reply
//! pipeline runs.
//!
//! | turn | requester |
//! |---|---|
//! | WebChat, signed-in dashboard account | `DashboardUser` (the authenticated account, never the session id) |
//! | WebChat public widget visitor (`widget-visitor:…`) | `ChannelUser` of channel `widget`, unbound |
//! | Odoo website Live Chat visitor (`odoolivechat:…`) | `ChannelUser` of channel `odoolivechat`, unbound |
//! | messaging app (LINE, Telegram, …) | `ChannelUser`; `user_id` only from a **verified** `channel_identities` binding of an active account |
//! | anything else (internal callers) | `System`, on behalf of nobody |
//!
//! Public visitors are never mapped to an account, so they can read only
//! collections opened to their channel, and never personal or restricted
//! ones (their conversations are not treated as 1:1). A failed identity
//! lookup means unbound (the least a sender can have).

use std::path::Path;

use duduclaw_knowledge::requester::RequesterIdentity;

/// Channel name for public WebChat widget visitors.
pub const WIDGET_CHANNEL: &str = "widget";
/// Channel name for Odoo website Live Chat visitors.
pub const ODOO_LIVECHAT_CHANNEL: &str = "odoolivechat";
const WIDGET_PRINCIPAL_PREFIX: &str = "widget-visitor:";

/// Map one turn. `webchat_principal` is the authenticated WebChat account
/// (the `AUTHENTICATED_WEBCHAT_PRINCIPAL` task-local) when the turn came
/// through WebChat; `verified(channel, sender)` returns the dashboard
/// account bound to a messaging-app sender.
pub fn map_requester(
    session_id: &str,
    sender: &str,
    webchat_principal: Option<&str>,
    verified: impl Fn(&str, &str) -> Option<String>,
) -> RequesterIdentity {
    if let Some(p) = webchat_principal.map(str::trim) {
        if let Some(visitor) = p.strip_prefix(WIDGET_PRINCIPAL_PREFIX) {
            return unbound(WIDGET_CHANNEL, visitor, p);
        }
        if p.is_empty() || p.chars().any(char::is_control) {
            return RequesterIdentity::Unknown;
        }
        return RequesterIdentity::DashboardUser {
            user_id: p.to_string(),
        };
    }
    let Some((channel, chat)) = session_id.split_once(':') else {
        return system();
    };
    if chat.is_empty() || session_id.chars().any(char::is_control) {
        return system();
    }
    if crate::odoo_livechat::is_public_visitor_session(session_id) {
        return unbound(ODOO_LIVECHAT_CHANNEL, chat, sender);
    }
    if !crate::channel_reply::is_messaging_session(session_id) {
        return system();
    }
    let user_id = (!sender.is_empty())
        .then(|| verified(channel, sender))
        .flatten();
    RequesterIdentity::ChannelUser {
        channel: channel.to_string(),
        chat: chat.to_string(),
        sender: sender.to_string(),
        user_id,
    }
}

fn unbound(channel: &str, chat: &str, sender: &str) -> RequesterIdentity {
    RequesterIdentity::ChannelUser {
        channel: channel.to_string(),
        chat: chat.to_string(),
        // A public visitor's conversation is never treated as 1:1: an empty
        // sender cannot prove it (`is_private_session` fails closed).
        sender: if channel == WIDGET_CHANNEL || channel == ODOO_LIVECHAT_CHANNEL {
            String::new()
        } else {
            sender.to_string()
        },
        user_id: None,
    }
}

fn system() -> RequesterIdentity {
    RequesterIdentity::System {
        kind: "internal".to_string(),
        on_behalf_of: None,
    }
}

/// [`map_requester`] with the real lookups: the WebChat principal from the
/// task-local (read here, on the turn's task), the verified binding from
/// `users.db` (off the reactor). A failed lookup task serves nobody.
pub async fn requester_for_reply(home: &Path, session_id: &str, sender: &str) -> RequesterIdentity {
    let principal = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
        .try_with(Clone::clone)
        .ok();
    let (home, session, sender) = (
        home.to_path_buf(),
        session_id.to_string(),
        sender.to_string(),
    );
    tokio::task::spawn_blocking(move || {
        map_requester(&session, &sender, principal.as_deref(), |channel, id| {
            crate::trigger_route::access::verified_user_for_channel_sender(&home, channel, id)
        })
    })
    .await
    .unwrap_or(RequesterIdentity::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_knowledge::acl::HumanMode;
    use duduclaw_knowledge::requester::{ResolvedUser, UserDirectory, human_ctx};

    struct Dir;
    impl UserDirectory for Dir {
        fn lookup(&self, user_id: &str) -> Result<Option<ResolvedUser>, String> {
            Ok((user_id == "u-hr").then(|| ResolvedUser {
                user_id: "u-hr".into(),
                department: Some("人資".into()),
                is_admin: false,
            }))
        }
    }

    fn bound(channel: &str, id: &str) -> Option<String> {
        (channel == "line" && id == "Ubound").then(|| "u-hr".to_string())
    }

    #[test]
    fn webchat_account_is_a_dashboard_user() {
        let r = map_requester("webchat:abc:x", "u-hr", Some("u-hr"), bound);
        assert_eq!(
            r,
            RequesterIdentity::DashboardUser {
                user_id: "u-hr".into()
            }
        );
        let h = human_ctx(&r, &Dir);
        assert_eq!(h.mode, HumanMode::Full);
        assert_eq!(h.user_department.as_deref(), Some("人資"));
        assert!(h.is_private);
        assert_eq!(
            map_requester("webchat:x", "", Some(" "), bound),
            RequesterIdentity::Unknown
        );
    }

    #[test]
    fn widget_and_livechat_visitors_get_channel_grants_only() {
        let r = map_requester("webchat:t:x", "v", Some("widget-visitor:1234"), bound);
        let h = human_ctx(&r, &Dir);
        assert_eq!(h.mode, HumanMode::ChannelOnly);
        assert_eq!(h.channel.as_deref(), Some(WIDGET_CHANNEL));
        assert!(h.user_id.is_none());
        assert!(!h.is_private, "a visitor is never a 1:1 session");
        let r = map_requester("odoolivechat:42", "visitor-42", None, |_, _| {
            Some("u-hr".into())
        });
        let h = human_ctx(&r, &Dir);
        assert_eq!(h.mode, HumanMode::ChannelOnly);
        assert_eq!(h.channel.as_deref(), Some(ODOO_LIVECHAT_CHANNEL));
        assert!(h.user_id.is_none() && !h.is_private);
    }

    #[test]
    fn line_senders_bound_and_unbound() {
        let r = map_requester("line:Ubound", "Ubound", None, bound);
        let h = human_ctx(&r, &Dir);
        assert_eq!(h.mode, HumanMode::Full);
        assert_eq!(h.user_id.as_deref(), Some("u-hr"));
        assert!(h.is_private, "1:1 LINE chat");
        let r = map_requester("line:Ustranger", "Ustranger", None, bound);
        let h = human_ctx(&r, &Dir);
        assert_eq!(h.mode, HumanMode::ChannelOnly);
        assert!(h.user_id.is_none());
        assert_eq!(h.channel_chat.as_deref(), Some("line:Ustranger"));
    }

    #[test]
    fn group_chat_is_not_private_even_for_a_bound_sender() {
        let r = map_requester("line:Cgroup1", "Ubound", None, bound);
        let h = human_ctx(&r, &Dir);
        assert_eq!(h.mode, HumanMode::Full);
        assert!(!h.is_private);
        let r = map_requester("telegram:-100", "Ubound", None, bound);
        assert!(!human_ctx(&r, &Dir).is_private);
    }

    #[test]
    fn internal_sessions_are_system_turns() {
        for sid in ["default", "cron:bot", "dispatch:bot"] {
            let r = map_requester(sid, "", None, bound);
            assert!(
                matches!(
                    r,
                    RequesterIdentity::System {
                        on_behalf_of: None,
                        ..
                    }
                ),
                "{sid}: {r:?}"
            );
            assert_eq!(human_ctx(&r, &Dir).mode, HumanMode::InternalOnly);
        }
    }
}
