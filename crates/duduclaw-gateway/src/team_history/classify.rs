//! What a viewer may see of one session, from its stored origin alone.
//!
//! | session | class |
//! |---|---|
//! | the viewer's own WebChat (`webchat_user`, owner = viewer) | [`HistoryClass::ViewerOwn`] |
//! | WebChat of a person in `full_text_owners` | [`HistoryClass::ReportOwnWebchat`] |
//! | anyone else's WebChat | [`HistoryClass::Hidden`] |
//! | messaging app, website widget visitor, Odoo Live Chat | [`HistoryClass::External`] (masked by the caller) |
//! | internal, unknown, not stamped yet (`None`) | [`HistoryClass::Hidden`] |
//!
//! The session id is only used for the channel label of an external
//! conversation, never for ownership; a `channel` row whose id is not a
//! messaging-app conversation is inconsistent and hidden.

use std::collections::BTreeSet;

use crate::knowledge::requester_map::{ODOO_LIVECHAT_CHANNEL, WIDGET_CHANNEL};
use crate::session_origin::{OriginKind, SessionOrigin};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryClass {
    /// The viewer's own WebChat conversation.
    ViewerOwn,
    /// The WebChat conversation of `report`, a person whose WebChat the
    /// viewer may read in full.
    ReportOwnWebchat { report: String },
    /// A conversation with someone outside the company (`channel`: `line`,
    /// `telegram`, …, `widget`, `odoolivechat`).
    External { channel: String },
    /// Not listed and not readable.
    Hidden,
}

/// Classify one session for `viewer_user_id`. `full_text_owners` are the
/// people whose WebChat this viewer may read in full (for B5: the manager's
/// in-scope reports). Fails closed: anything not positively matched is
/// [`HistoryClass::Hidden`].
pub fn classify(
    session_id: &str,
    origin: Option<&SessionOrigin>,
    viewer_user_id: &str,
    full_text_owners: &BTreeSet<String>,
) -> HistoryClass {
    let Some(origin) = origin else {
        return HistoryClass::Hidden;
    };
    match origin.kind() {
        OriginKind::WebchatUser => match origin.owner_user_id() {
            Some(owner) if !viewer_user_id.is_empty() && owner == viewer_user_id => {
                HistoryClass::ViewerOwn
            }
            Some(owner) if full_text_owners.contains(owner) => HistoryClass::ReportOwnWebchat {
                report: owner.to_string(),
            },
            _ => HistoryClass::Hidden,
        },
        OriginKind::Channel => match messaging_channel(session_id) {
            Some(channel) => HistoryClass::External {
                channel: channel.to_string(),
            },
            None => HistoryClass::Hidden,
        },
        OriginKind::WebchatVisitor => HistoryClass::External {
            channel: WIDGET_CHANNEL.to_string(),
        },
        OriginKind::PublicVisitor => HistoryClass::External {
            channel: ODOO_LIVECHAT_CHANNEL.to_string(),
        },
        OriginKind::Internal | OriginKind::Unknown => HistoryClass::Hidden,
    }
}

/// The messaging-app prefix of a conversation (a called employee's
/// `#agent:` bucket counts as its conversation).
fn messaging_channel(session_id: &str) -> Option<&str> {
    let base = crate::knowledge::chat_attachments::base_session(session_id);
    if !crate::channel_reply::is_messaging_session(base) {
        return None;
    }
    base.split_once(':').map(|(channel, _)| channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reports(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn webchat(owner: &str) -> SessionOrigin {
        SessionOrigin::webchat_user(owner)
    }

    #[test]
    fn webchat_rows_by_owner() {
        let team = reports(&["u-emp1"]);
        let sid = "webchat:webchat:0123456789ab:c1";
        assert_eq!(
            classify(sid, Some(&webchat("u-mgr")), "u-mgr", &team),
            HistoryClass::ViewerOwn
        );
        assert_eq!(
            classify(sid, Some(&webchat("u-emp1")), "u-mgr", &team),
            HistoryClass::ReportOwnWebchat {
                report: "u-emp1".into()
            }
        );
        // A colleague, a peer, the viewer's own manager, an out-of-scope report.
        for other in ["u-other", "u-peer", "u-boss", "u-emp9"] {
            assert_eq!(
                classify(sid, Some(&webchat(other)), "u-mgr", &team),
                HistoryClass::Hidden,
                "{other}"
            );
        }
    }

    #[test]
    fn an_empty_viewer_owns_nothing() {
        let origin = SessionOrigin::webchat_user("");
        assert_eq!(origin.kind(), OriginKind::Unknown);
        assert_eq!(
            classify("webchat:x", Some(&origin), "", &reports(&[])),
            HistoryClass::Hidden
        );
    }

    #[test]
    fn external_conversations_carry_their_channel() {
        let none = reports(&[]);
        let channel = SessionOrigin::of_kind(OriginKind::Channel);
        for (sid, label) in [
            ("line:Cgroup1", "line"),
            ("telegram:-100:7", "telegram"),
            ("discord:thread:42", "discord"),
            ("slack:group:C1", "slack"),
            ("telegram:555#agent:sales", "telegram"),
            ("line:Ubound#agent:hr-bot#conv:2", "line"),
        ] {
            assert_eq!(
                classify(sid, Some(&channel), "u-mgr", &none),
                HistoryClass::External {
                    channel: label.into()
                },
                "{sid}"
            );
        }
        assert_eq!(
            classify(
                "webchat:webchat:0123456789ab:c1",
                Some(&SessionOrigin::of_kind(OriginKind::WebchatVisitor)),
                "u-mgr",
                &none
            ),
            HistoryClass::External {
                channel: "widget".into()
            }
        );
        assert_eq!(
            classify(
                "odoolivechat:1:42",
                Some(&SessionOrigin::of_kind(OriginKind::PublicVisitor)),
                "u-mgr",
                &none
            ),
            HistoryClass::External {
                channel: "odoolivechat".into()
            }
        );
    }

    #[test]
    fn inconsistent_channel_rows_are_hidden() {
        let channel = SessionOrigin::of_kind(OriginKind::Channel);
        for sid in [
            "webchat:webchat:0123456789ab:c1",
            "cron:bot",
            "default",
            "lineX:1",
        ] {
            assert_eq!(
                classify(sid, Some(&channel), "u-mgr", &reports(&[])),
                HistoryClass::Hidden,
                "{sid}"
            );
        }
    }

    #[test]
    fn internal_unknown_and_unstamped_are_hidden() {
        let team = reports(&["u-emp1"]);
        for origin in [
            Some(SessionOrigin::of_kind(OriginKind::Internal)),
            Some(SessionOrigin::unknown()),
            None,
        ] {
            assert_eq!(
                classify("dispatch:run-1", origin.as_ref(), "u-mgr", &team),
                HistoryClass::Hidden
            );
            assert_eq!(
                classify(
                    "webchat:webchat:0123456789ab:c1",
                    origin.as_ref(),
                    "u-mgr",
                    &team
                ),
                HistoryClass::Hidden
            );
        }
    }
}
