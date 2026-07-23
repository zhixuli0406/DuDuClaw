//! Sensitivity labels + context-collapse privacy primitives (P3-2).
//!
//! Methodology: *Local Is Not a Sufficient Privacy Boundary* (arXiv:2606.10173)
//! §5.1 five control points. "Computation is local" answers only *where* — it
//! does not stop **context collapse**: an agent stitching a user's personal
//! context (persona, calendar, screen, clipboard) into a prompt that is shared
//! with other people. The defence has two halves, both here:
//!
//! 1. [`Sensitivity`] — a monotone label attached to perception sources and to
//!    memory writes (additive; carried in the memory metadata JSON blob, never a
//!    schema change — same convention as [`crate`]'s origin binding).
//! 2. [`is_private_session`] — the deterministic gate deciding whether a chat
//!    session is a 1:1 private conversation (personal context may be injected)
//!    or a group/shared session (personal context must be stripped). It is
//!    **fail-closed**: anything it cannot positively prove to be 1:1 is treated
//!    as shared.

use serde::{Deserialize, Serialize};

/// How sensitive a piece of context is. Ordered least → most sensitive, so
/// `>=` comparisons express "at least this sensitive".
///
/// - `Public`     — safe to surface anywhere.
/// - `Internal`   — workspace/operational (file paths, spotlight hits).
/// - `Personal`   — tied to one identity (persona, calendar, frontmost window).
/// - `Restricted` — highest exposure (clipboard, screen capture — future P4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Public,
    Internal,
    Personal,
    Restricted,
}

impl Sensitivity {
    /// Stable lowercase string (matches the serde representation and the value
    /// written into memory metadata / `.scope.toml`).
    pub fn as_str(self) -> &'static str {
        match self {
            Sensitivity::Public => "public",
            Sensitivity::Internal => "internal",
            Sensitivity::Personal => "personal",
            Sensitivity::Restricted => "restricted",
        }
    }

    /// Parse a label. Unknown / empty → `None` (callers pick a fail-safe
    /// default appropriate to their context).
    pub fn parse(s: &str) -> Option<Sensitivity> {
        match s.trim().to_ascii_lowercase().as_str() {
            "public" => Some(Sensitivity::Public),
            "internal" => Some(Sensitivity::Internal),
            "personal" => Some(Sensitivity::Personal),
            "restricted" => Some(Sensitivity::Restricted),
            _ => None,
        }
    }

    /// Whether this level is `Personal` or above — the threshold at which
    /// context must be stripped from a shared/group session (context-collapse
    /// defence).
    pub fn is_personal_or_higher(self) -> bool {
        self >= Sensitivity::Personal
    }

    /// Whether context at this level must be withheld from a session with the
    /// given privacy. Personal+ is withheld from shared sessions; everything is
    /// allowed in a private 1:1 session.
    pub fn allowed_in_session(self, is_private: bool) -> bool {
        is_private || !self.is_personal_or_higher()
    }
}

/// Sensitivity of a perception source, by its stable source name (the same
/// snake_case names used for autopilot event kinds / MCP tools).
///
/// This is the constant classification table from P3-2. Both the underscore
/// (`os_file`) and dotted (`os.file`) event keys map identically, mirroring the
/// autopilot dual-key convention.
///
/// **Fail-closed for privacy:** an *unrecognized* source name resolves to
/// [`Sensitivity::Personal`], so a perception source we forgot to classify is
/// withheld from shared sessions rather than leaking. (This differs from the
/// memory-metadata read default, which is `Internal` for backward compatibility
/// with pre-label memories — see `duduclaw_memory::sensitivity`.)
pub fn perception_source_sensitivity(source: &str) -> Sensitivity {
    match source {
        // Workspace/operational — file paths & names, spotlight index hits.
        "os_file" | "os.file" | "spotlight" | "os_spotlight" | "os_spotlight_search" => {
            Sensitivity::Internal
        }
        // Identity-bound — foreground window title, calendar events.
        "frontmost" | "os_frontmost" | "calendar" | "os_calendar" | "os_calendar_today" => {
            Sensitivity::Personal
        }
        // Highest exposure — not captured yet (P4), classified ahead of use.
        "clipboard" | "os_clipboard" | "screen" | "screenshot" | "os_screen" => {
            Sensitivity::Restricted
        }
        // Fail-closed: unknown perception source → Personal (stripped in groups).
        _ => Sensitivity::Personal,
    }
}

/// Whether `session_id` denotes a **1:1 private** conversation with `user_id`.
///
/// Personal-or-higher context (persona blocks, calendar, screen, clipboard) may
/// be injected only when this returns `true`. It is deliberately conservative —
/// **fail-closed**: any session it cannot positively prove to be 1:1 (unknown
/// channel, group markers, empty identity, structural mismatch) is treated as
/// shared and returns `false`.
///
/// ## Decision rule
///
/// Session ids are `"<channel>:<rest>"` (see `channel_reply::parse_session_id_parts`).
/// - Empty `user_id` or `session_id`, or a session with no channel prefix →
///   `false` (cannot prove privacy).
/// - Explicit shared markers → `false`: `slack:group:<c>`, `discord:thread:<t>`,
///   or a telegram negative chat id (`telegram:-100…`, groups/supergroups).
/// - **Identity test** (channel-agnostic, "群組 id 與 user id 不同構"): in a 1:1
///   chat the conversation is keyed by the sender, so the post-prefix remainder
///   equals `user_id`. In a group the remainder is a group/room/space id,
///   structurally distinct. `rest == user_id` → `true`.
/// - WebChat sessions are single-viewer browser connections; `compose_session_id`
///   may append `#agent:…#conv:…` suffixes, so `webchat` also accepts
///   `user_id` followed by a `#`-delimited suffix.
/// - Everything else (discord non-thread channels, feishu, googlechat, teams —
///   whose ids are not derivable from the sender id) → `false`.
pub fn is_private_session(session_id: &str, user_id: &str) -> bool {
    // Fail-closed: without a sender identity we cannot prove a 1:1 chat.
    if user_id.is_empty() || session_id.is_empty() {
        return false;
    }
    let Some((channel, rest)) = session_id.split_once(':') else {
        // No channel prefix — unclassifiable. Fail-closed.
        return false;
    };
    if channel.is_empty() || rest.is_empty() {
        return false;
    }
    // Explicit shared-conversation markers (never a 1:1 chat).
    // slack:group:<channel>, discord:thread:<id>.
    if rest.starts_with("group:") || rest.starts_with("thread:") {
        return false;
    }
    // Telegram private chats have a positive chat.id == the user's id; groups /
    // supergroups use a negative chat id.
    if channel == "telegram" && rest.starts_with('-') {
        return false;
    }
    // Identity test: 1:1 conversations are keyed by the sender themselves.
    if rest == user_id {
        return true;
    }
    // WebChat: single-viewer session, id = "webchat:" + user_id, optionally with
    // compose_session_id's "#agent:…#conv:…" suffix appended.
    if channel == "webchat" {
        if let Some(suffix) = rest.strip_prefix(user_id) {
            if suffix.starts_with('#') {
                return true;
            }
        }
    }
    // Cannot positively classify as 1:1 → treat as shared (fail-closed).
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_is_ordered_least_to_most() {
        assert!(Sensitivity::Public < Sensitivity::Internal);
        assert!(Sensitivity::Internal < Sensitivity::Personal);
        assert!(Sensitivity::Personal < Sensitivity::Restricted);
    }

    #[test]
    fn sensitivity_roundtrips_via_str_and_parse() {
        for s in [
            Sensitivity::Public,
            Sensitivity::Internal,
            Sensitivity::Personal,
            Sensitivity::Restricted,
        ] {
            assert_eq!(Sensitivity::parse(s.as_str()), Some(s));
        }
        // Case-insensitive + trimmed.
        assert_eq!(
            Sensitivity::parse("  PERSONAL "),
            Some(Sensitivity::Personal)
        );
        // Unknown → None.
        assert_eq!(Sensitivity::parse("secret"), None);
        assert_eq!(Sensitivity::parse(""), None);
    }

    #[test]
    fn sensitivity_serde_is_lowercase() {
        let json = serde_json::to_string(&Sensitivity::Personal).unwrap();
        assert_eq!(json, "\"personal\"");
        let back: Sensitivity = serde_json::from_str("\"restricted\"").unwrap();
        assert_eq!(back, Sensitivity::Restricted);
    }

    #[test]
    fn personal_or_higher_threshold() {
        assert!(!Sensitivity::Public.is_personal_or_higher());
        assert!(!Sensitivity::Internal.is_personal_or_higher());
        assert!(Sensitivity::Personal.is_personal_or_higher());
        assert!(Sensitivity::Restricted.is_personal_or_higher());
    }

    #[test]
    fn allowed_in_session_strips_personal_from_groups_only() {
        // Private session: everything allowed.
        for s in [
            Sensitivity::Public,
            Sensitivity::Internal,
            Sensitivity::Personal,
            Sensitivity::Restricted,
        ] {
            assert!(
                s.allowed_in_session(true),
                "{s:?} should be allowed privately"
            );
        }
        // Shared session: only Public/Internal allowed.
        assert!(Sensitivity::Public.allowed_in_session(false));
        assert!(Sensitivity::Internal.allowed_in_session(false));
        assert!(!Sensitivity::Personal.allowed_in_session(false));
        assert!(!Sensitivity::Restricted.allowed_in_session(false));
    }

    #[test]
    fn perception_table_matches_spec() {
        assert_eq!(
            perception_source_sensitivity("os_file"),
            Sensitivity::Internal
        );
        assert_eq!(
            perception_source_sensitivity("os.file"),
            Sensitivity::Internal
        );
        assert_eq!(
            perception_source_sensitivity("spotlight"),
            Sensitivity::Internal
        );
        assert_eq!(
            perception_source_sensitivity("frontmost"),
            Sensitivity::Personal
        );
        assert_eq!(
            perception_source_sensitivity("calendar"),
            Sensitivity::Personal
        );
        assert_eq!(
            perception_source_sensitivity("clipboard"),
            Sensitivity::Restricted
        );
        assert_eq!(
            perception_source_sensitivity("screen"),
            Sensitivity::Restricted
        );
    }

    #[test]
    fn perception_unknown_source_fails_closed_to_personal() {
        assert_eq!(
            perception_source_sensitivity("who_knows"),
            Sensitivity::Personal
        );
        assert_eq!(perception_source_sensitivity(""), Sensitivity::Personal);
    }

    // ── is_private_session ────────────────────────────────────────────

    #[test]
    fn telegram_private_chat_is_private() {
        // Private chat: chat.id == user id, positive.
        assert!(is_private_session("telegram:123", "123"));
    }

    #[test]
    fn telegram_group_is_not_private() {
        // Supergroup: negative chat id, distinct from the member's user id.
        assert!(!is_private_session("telegram:-100456", "123"));
        // Even if a group member's id somehow appeared, the '-' marker wins.
        assert!(!is_private_session("telegram:-100456", "-100456"));
    }

    #[test]
    fn telegram_supergroup_thread_is_not_private() {
        // telegram:<negative chat>:<thread> — group, stripped.
        assert!(!is_private_session("telegram:-100456:7", "123"));
    }

    #[test]
    fn slack_dm_is_private_group_is_not() {
        assert!(is_private_session("slack:U123", "U123"));
        assert!(!is_private_session("slack:group:C999", "U123"));
    }

    #[test]
    fn line_dm_is_private_group_is_not() {
        assert!(is_private_session("line:U123", "U123"));
        // Group id (C…) / room id (R…) differ from the user id.
        assert!(!is_private_session("line:C999", "U123"));
        assert!(!is_private_session("line:R999", "U123"));
    }

    #[test]
    fn whatsapp_dm_is_private() {
        assert!(is_private_session("whatsapp:886900123", "886900123"));
    }

    #[test]
    fn webchat_single_viewer_is_private() {
        // session_id = "webchat:" + user_id.
        let uid = "webchat:1.2.3.4:abc";
        let sid = format!("webchat:{uid}");
        assert!(is_private_session(&sid, uid));
        // With compose_session_id's #agent/#conv suffix appended.
        let composed = format!("webchat:{uid}#agent:agnes#conv:nonce");
        assert!(is_private_session(&composed, uid));
    }

    #[test]
    fn discord_channel_cannot_be_proven_private() {
        // Discord channel/DM ids are not derivable from the user id → fail-closed.
        assert!(!is_private_session("discord:112233", "user-77"));
        assert!(!is_private_session("discord:thread:445566", "user-77"));
    }

    #[test]
    fn feishu_googlechat_teams_fail_closed() {
        assert!(!is_private_session("feishu:oc_abc", "ou_user"));
        assert!(!is_private_session("googlechat:spaces/AAA", "users/111"));
        assert!(!is_private_session("teams:conv-1", "29:user"));
    }

    #[test]
    fn empty_identity_or_session_fails_closed() {
        assert!(!is_private_session("telegram:123", ""));
        assert!(!is_private_session("", "123"));
        assert!(!is_private_session("no-colon", "no-colon"));
    }
}
