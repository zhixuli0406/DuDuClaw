//! Who a channel reply turn is for, resolved once per turn before the reply
//! pipeline runs.
//!
//! | turn | requester |
//! |---|---|
//! | WebChat, signed-in dashboard account | `DashboardUser` (the authenticated account, never the session id) |
//! | WebChat public widget visitor (`widget-visitor:…`) | `ChannelUser` of channel `widget`, unbound |
//! | Odoo website Live Chat visitor (`odoolivechat:…`) | `ChannelUser` of channel `odoolivechat`, unbound |
//! | messaging app (LINE, Telegram, …) | `ChannelUser`; `user_id` only from a **verified** `channel_identities` binding of an active account |
//! | Google Chat 1:1 DM | as above, plus `attested_private` when the adapter's `ATTESTED_ONE_TO_ONE` names exactly this space and sender |
//! | gateway-internal sessions (`cron:`, `dispatch:`, `heartbeat:` …, [`INTERNAL_SESSION_PREFIXES`]) | `System`, on behalf of nobody |
//! | anything else | `Unknown` (reads nothing) |
//!
//! Public visitors are never mapped to an account and read only `public`
//! collections opened to their channel (never internal, personal or
//! restricted ones, whatever is granted). A failed identity
//! lookup means unbound (the least a sender can have).

use std::path::Path;

use duduclaw_knowledge::requester::RequesterIdentity;

/// Channel name for public WebChat widget visitors.
pub const WIDGET_CHANNEL: &str = duduclaw_knowledge::requester::PUBLIC_VISITOR_CHANNELS[0];
/// Channel name for Odoo website Live Chat visitors.
pub const ODOO_LIVECHAT_CHANNEL: &str = duduclaw_knowledge::requester::PUBLIC_VISITOR_CHANNELS[1];
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
    map_requester_attested(session_id, sender, webchat_principal, None, verified)
}

/// [`map_requester`] with the authenticated adapter's 1:1 attestation (the
/// `ATTESTED_ONE_TO_ONE` task-local). It counts only when it names exactly
/// this conversation's channel and chat and this sender; it never adds a
/// binding or a grant.
pub fn map_requester_attested(
    session_id: &str,
    sender: &str,
    webchat_principal: Option<&str>,
    attestation: Option<&crate::ccr_runtime::OneToOneAttestation>,
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
    // A called employee's bucket (`<session>#agent:<id>`) is the same
    // conversation: privacy and grants follow the base session.
    let session_id = super::chat_attachments::base_session(session_id);
    let Some((channel, chat)) = session_id.split_once(':') else {
        return RequesterIdentity::Unknown;
    };
    if chat.is_empty() || session_id.chars().any(char::is_control) {
        return RequesterIdentity::Unknown;
    }
    if crate::odoo_livechat::is_public_visitor_session(session_id) {
        return unbound(ODOO_LIVECHAT_CHANNEL, chat, sender);
    }
    if !crate::channel_reply::is_messaging_session(session_id) {
        // Only gateway-internal sessions are system turns; any other prefix
        // (a channel added later, a test session) reads nothing.
        return if INTERNAL_SESSION_PREFIXES.contains(&channel) {
            system()
        } else {
            RequesterIdentity::Unknown
        };
    }
    let user_id = (!sender.is_empty())
        .then(|| verified(channel, sender))
        .flatten();
    let attested_private = attestation.is_some_and(|a| a.matches(channel, chat, sender));
    RequesterIdentity::ChannelUser {
        channel: channel.to_string(),
        chat: chat.to_string(),
        sender: sender.to_string(),
        user_id,
        attested_private,
    }
}

/// Session prefixes the gateway itself uses for turns with no person in the
/// loop (exact match).
pub const INTERNAL_SESSION_PREFIXES: &[&str] = &[
    "cron",
    "dispatch",
    "heartbeat",
    "reminder",
    "goal",
    "autopilot",
    "responsibility",
];

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
        attested_private: false,
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
    // Read on the turn's task too: task-locals do not reach the blocking pool.
    let attestation = crate::ccr_runtime::current_one_to_one_attestation();
    let (home, session, sender) = (
        home.to_path_buf(),
        session_id.to_string(),
        sender.to_string(),
    );
    tokio::task::spawn_blocking(move || {
        map_requester_attested(
            &session,
            &sender,
            principal.as_deref(),
            attestation.as_ref(),
            |channel, id| {
                crate::trigger_route::access::verified_user_for_channel_sender(&home, channel, id)
            },
        )
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
        for sid in ["cron:bot", "dispatch:bot", "heartbeat:bot"] {
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

    #[test]
    fn unknown_sessions_read_nothing() {
        for sid in ["default", "anonymous", "newchat:abc", "cronx:bot", "line"] {
            assert_eq!(
                map_requester(sid, "u", None, bound),
                RequesterIdentity::Unknown,
                "{sid}"
            );
        }
    }

    #[test]
    fn a_called_employee_in_a_one_to_one_chat_is_still_one_to_one() {
        let r = map_requester("line:Ubound#agent:hr-bot", "Ubound", None, bound);
        let h = human_ctx(&r, &Dir);
        assert!(h.is_private);
        assert_eq!(h.channel_chat.as_deref(), Some("line:Ubound"));
        let r = map_requester("line:Cgroup1#agent:hr-bot", "Ubound", None, bound);
        assert!(!human_ctx(&r, &Dir).is_private);
    }

    fn gchat_bound(channel: &str, id: &str) -> Option<String> {
        (channel == "googlechat" && id == "users/111").then(|| "u-hr".to_string())
    }

    fn dm_attestation(space: &str, sender: &str) -> crate::ccr_runtime::OneToOneAttestation {
        crate::ccr_runtime::OneToOneAttestation::googlechat_dm(space, sender).unwrap()
    }

    fn attested(r: &RequesterIdentity) -> bool {
        matches!(r, RequesterIdentity::ChannelUser { attested_private: true, .. })
    }

    #[test]
    fn attested_googlechat_dm_of_a_bound_sender_is_one_to_one() {
        let a = dm_attestation("spaces/DM1", "users/111");
        let r = map_requester_attested("googlechat:spaces/DM1", "users/111", None, Some(&a), gchat_bound);
        assert_eq!(
            r,
            RequesterIdentity::ChannelUser {
                channel: "googlechat".into(),
                chat: "spaces/DM1".into(),
                sender: "users/111".into(),
                user_id: Some("u-hr".into()),
                attested_private: true,
            }
        );
        let h = human_ctx(&r, &Dir);
        assert_eq!(h.mode, HumanMode::Full);
        assert!(h.is_private);
        // A called employee's bucket of the same DM.
        let r = map_requester_attested(
            "googlechat:spaces/DM1#agent:hr-bot",
            "users/111",
            None,
            Some(&a),
            gchat_bound,
        );
        assert!(attested(&r) && human_ctx(&r, &Dir).is_private);
    }

    #[test]
    fn googlechat_attestation_must_name_this_space_and_sender() {
        // Same sender in a shared space: no attestation for it.
        let r = map_requester_attested("googlechat:spaces/TEAM", "users/111", None, None, gchat_bound);
        assert!(!attested(&r));
        assert!(!human_ctx(&r, &Dir).is_private);
        // An attestation for another space.
        let other_space = dm_attestation("spaces/DM1", "users/111");
        let r = map_requester_attested(
            "googlechat:spaces/TEAM",
            "users/111",
            None,
            Some(&other_space),
            gchat_bound,
        );
        assert!(!attested(&r) && !human_ctx(&r, &Dir).is_private);
        // An attestation for another sender in the same space.
        let other_sender = dm_attestation("spaces/DM1", "users/222");
        let r = map_requester_attested(
            "googlechat:spaces/DM1",
            "users/111",
            None,
            Some(&other_sender),
            gchat_bound,
        );
        assert!(!attested(&r) && !human_ctx(&r, &Dir).is_private);
        // No attestation at all: the session id shape alone never proves it.
        let r = map_requester("googlechat:spaces/DM1", "users/111", None, gchat_bound);
        assert!(!attested(&r) && !human_ctx(&r, &Dir).is_private);
        // A Google Chat attestation does not carry over to another channel.
        let r = map_requester_attested("line:spaces/DM1", "users/111", None, Some(&other_space), bound);
        assert!(!attested(&r));
        // WebChat and visitors ignore it.
        let r = map_requester_attested("webchat:t:x", "v", Some("widget-visitor:1"), Some(&other_space), bound);
        assert!(!attested(&r) && !human_ctx(&r, &Dir).is_private);
    }

    #[test]
    fn attestation_only_accepts_a_well_formed_space_and_human_user() {
        use crate::ccr_runtime::OneToOneAttestation as A;
        assert!(A::googlechat_dm("spaces/DM1", "users/111").is_some());
        for (space, sender) in [
            ("spaces/DM1", ""),
            ("spaces/DM1", "111"),
            ("spaces/DM1", "users/app"),
            ("spaces/DM1", "users/a@example.com"),
            ("spaces/DM1", "unknown"),
            ("", "users/111"),
            ("spaces/../x", "users/111"),
            ("users/111", "users/111"),
        ] {
            assert!(A::googlechat_dm(space, sender).is_none(), "{space} {sender}");
        }
    }

    #[tokio::test]
    async fn the_task_local_attestation_reaches_conversation_is_one_to_one() {
        use crate::ccr_runtime::{ATTESTED_ONE_TO_ONE, conversation_is_one_to_one};
        assert!(!conversation_is_one_to_one("googlechat:spaces/DM1", "users/111"));
        let a = dm_attestation("spaces/DM1", "users/111");
        let (dm, other_space, other_sender) = ATTESTED_ONE_TO_ONE
            .scope(Some(a), async {
                (
                    conversation_is_one_to_one("googlechat:spaces/DM1", "users/111"),
                    conversation_is_one_to_one("googlechat:spaces/TEAM", "users/111"),
                    conversation_is_one_to_one("googlechat:spaces/DM1", "users/222"),
                )
            })
            .await;
        assert!(dm && !other_space && !other_sender);
        let none = ATTESTED_ONE_TO_ONE
            .scope(None, async { conversation_is_one_to_one("googlechat:spaces/DM1", "users/111") })
            .await;
        assert!(!none);
        // LINE keeps its session-shape rule.
        assert!(conversation_is_one_to_one("line:Ubound", "Ubound"));
    }
}
