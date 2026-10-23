//! Where a conversation comes from and, for a signed-in WebChat account, who
//! owns it. Stored on the `sessions` row (`origin_kind`, `owner_user_id`,
//! `origin_at`) by the host when the row is created; the first stamp wins and
//! is never overwritten.
//!
//! Why a stored column instead of reading the session id: a WebChat session
//! of a signed-in employee and one of a public website-widget visitor have
//! the same id shape (`webchat:webchat:<owner_tag>:<conn>`), so only the
//! creating turn (which knows the authenticated principal) can tell them
//! apart. The id rules are used once, for the [`backfill`] of rows written
//! before these columns existed.
//!
//! | turn | `origin_kind` | `owner_user_id` |
//! |---|---|---|
//! | WebChat, signed-in dashboard account | `webchat_user` | the account id |
//! | WebChat public widget visitor (`widget-visitor:…`) | `webchat_visitor` | — |
//! | Odoo website Live Chat (`odoolivechat:…`) | `public_visitor` | — |
//! | messaging app (LINE, Telegram, … incl. `#agent:` buckets) | `channel` | — |
//! | gateway-internal (`cron:`, `dispatch:`, `goal:` …) | `internal` | — |
//! | anything else, or no origin known | `unknown` | — |
//!
//! The origin is decided by the host from the authenticated WebChat principal
//! (the `AUTHENTICATED_WEBCHAT_PRINCIPAL` task-local) and the host-built
//! session id; never from model output or tool arguments. Nothing reads these
//! columns yet (B5 PR 1 is data only); later readers must treat `NULL` and
//! `unknown` as "show to nobody but the existing readers".

pub mod backfill;
pub mod store;

#[cfg(test)]
mod tests;

use duduclaw_knowledge::requester::RequesterIdentity;

use crate::knowledge::requester_map::{self, ODOO_LIVECHAT_CHANNEL, WIDGET_CHANNEL};
use crate::session::{Session, SessionManager};

/// What kind of conversation a session is (the stored `origin_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OriginKind {
    /// A signed-in dashboard account talking in WebChat.
    WebchatUser,
    /// An anonymous public website-widget visitor (WebChat `widget:` key).
    WebchatVisitor,
    /// An external messaging app (LINE, Telegram, Discord, Slack, …).
    Channel,
    /// An Odoo website Live Chat visitor.
    PublicVisitor,
    /// A gateway-internal session with no person in the loop.
    Internal,
    /// Could not be determined. Never widened into another kind.
    Unknown,
}

impl OriginKind {
    /// Every kind, in a stable order (tests, backfill counters).
    pub const ALL: [OriginKind; 6] = [
        OriginKind::WebchatUser,
        OriginKind::WebchatVisitor,
        OriginKind::Channel,
        OriginKind::PublicVisitor,
        OriginKind::Internal,
        OriginKind::Unknown,
    ];

    /// The stored column value.
    pub fn as_str(self) -> &'static str {
        match self {
            OriginKind::WebchatUser => "webchat_user",
            OriginKind::WebchatVisitor => "webchat_visitor",
            OriginKind::Channel => "channel",
            OriginKind::PublicVisitor => "public_visitor",
            OriginKind::Internal => "internal",
            OriginKind::Unknown => "unknown",
        }
    }

    /// Parse a stored value, exact match. Anything unrecognised is
    /// `Unknown` (fail closed: a value written by a newer version never
    /// reads as a more visible kind).
    pub fn parse(raw: &str) -> OriginKind {
        OriginKind::ALL
            .into_iter()
            .find(|k| k.as_str() == raw)
            .unwrap_or(OriginKind::Unknown)
    }
}

/// The origin of one session. `owner_user_id` is set only for
/// [`OriginKind::WebchatUser`] (constructors enforce it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOrigin {
    kind: OriginKind,
    owner_user_id: Option<String>,
}

impl SessionOrigin {
    /// A signed-in account's WebChat conversation. An empty id or one with
    /// control characters cannot name an owner and yields `Unknown`.
    pub fn webchat_user(user_id: &str) -> SessionOrigin {
        let id = user_id.trim();
        if id.is_empty() || id.chars().any(char::is_control) {
            return SessionOrigin::unknown();
        }
        SessionOrigin {
            kind: OriginKind::WebchatUser,
            owner_user_id: Some(id.to_string()),
        }
    }

    /// An ownerless origin of `kind`. `WebchatUser` without an owner is not
    /// a valid origin and yields `Unknown`.
    pub fn of_kind(kind: OriginKind) -> SessionOrigin {
        let kind = if kind == OriginKind::WebchatUser {
            OriginKind::Unknown
        } else {
            kind
        };
        SessionOrigin {
            kind,
            owner_user_id: None,
        }
    }

    pub fn unknown() -> SessionOrigin {
        SessionOrigin::of_kind(OriginKind::Unknown)
    }

    pub fn kind(&self) -> OriginKind {
        self.kind
    }

    pub fn owner_user_id(&self) -> Option<&str> {
        self.owner_user_id.as_deref()
    }

    /// From the person a turn is for (`requester_map::map_requester`).
    pub fn from_requester(who: &RequesterIdentity) -> SessionOrigin {
        match who {
            RequesterIdentity::DashboardUser { user_id } => SessionOrigin::webchat_user(user_id),
            RequesterIdentity::ChannelUser { channel, .. } if channel == WIDGET_CHANNEL => {
                SessionOrigin::of_kind(OriginKind::WebchatVisitor)
            }
            RequesterIdentity::ChannelUser { channel, .. } if channel == ODOO_LIVECHAT_CHANNEL => {
                SessionOrigin::of_kind(OriginKind::PublicVisitor)
            }
            RequesterIdentity::ChannelUser { .. } => SessionOrigin::of_kind(OriginKind::Channel),
            RequesterIdentity::System { .. } => SessionOrigin::of_kind(OriginKind::Internal),
            RequesterIdentity::Unknown => SessionOrigin::unknown(),
        }
    }
}

/// The origin of `session_id` for a turn whose authenticated WebChat
/// principal is `webchat_principal` (`None` outside WebChat). Pure: the same
/// rules as the knowledge-base requester mapping, without the channel
/// identity lookup (the origin never stores a channel account).
///
/// The principal only speaks for WebChat session ids (`webchat:…`, the ids
/// the WebChat socket builds): any other session touched while a principal is
/// in scope is classified from its id alone, so a WebChat account can never
/// be stamped as the owner of a channel or internal conversation.
pub fn origin_of(session_id: &str, webchat_principal: Option<&str>) -> SessionOrigin {
    let principal = webchat_principal.filter(|_| is_webchat_session(session_id));
    let who = requester_map::map_requester(session_id, "", principal, |_, _| None);
    SessionOrigin::from_requester(&who)
}

/// Session ids the WebChat socket builds (exact `webchat` prefix).
fn is_webchat_session(session_id: &str) -> bool {
    session_id
        .split_once(':')
        .is_some_and(|(prefix, rest)| prefix == WEBCHAT_PREFIX && !rest.is_empty())
}

const WEBCHAT_PREFIX: &str = "webchat";

/// [`origin_of`] for the turn running on the current task: reads the
/// authenticated WebChat principal the WebChat socket scopes around each
/// turn and chat command.
pub fn origin_for_current_turn(session_id: &str) -> SessionOrigin {
    let principal = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
        .try_with(Clone::clone)
        .ok();
    origin_of(session_id, principal.as_deref())
}

/// Create or load the session a live turn writes into, stamping the turn's
/// origin (first stamp wins). Every production creation point calls this
/// instead of [`SessionManager::get_or_create`].
pub async fn ensure_turn_session(
    sessions: &SessionManager,
    session_id: &str,
    agent_id: &str,
) -> duduclaw_core::error::Result<Session> {
    let origin = origin_for_current_turn(session_id);
    sessions
        .get_or_create_with_origin(session_id, agent_id, &origin)
        .await
}
