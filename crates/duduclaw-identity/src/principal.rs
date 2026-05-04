//! Channel kinds and resolved-person value types.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Identifies which channel an external_id belongs to.
///
/// Serialised as snake_case strings so they're human-friendly in YAML
/// frontmatter and HTTP payloads (`"discord"`, `"line"`, `"telegram"`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Discord,
    Line,
    Telegram,
    Slack,
    Whatsapp,
    Feishu,
    Webchat,
    Email,
    /// Catch-all for self-hosted webhooks or future channels. The string is
    /// the channel's stable identifier, not free-form.
    Other(String),
}

impl ChannelKind {
    /// Stable wire-format identifier (matches the YAML/JSON serialisation).
    pub fn as_wire(&self) -> String {
        match self {
            ChannelKind::Discord => "discord".into(),
            ChannelKind::Line => "line".into(),
            ChannelKind::Telegram => "telegram".into(),
            ChannelKind::Slack => "slack".into(),
            ChannelKind::Whatsapp => "whatsapp".into(),
            ChannelKind::Feishu => "feishu".into(),
            ChannelKind::Webchat => "webchat".into(),
            ChannelKind::Email => "email".into(),
            ChannelKind::Other(s) => s.clone(),
        }
    }

    /// Parse a wire identifier back into a kind. Unknown identifiers map to
    /// `ChannelKind::Other(_)` so this never fails.
    pub fn parse_wire(s: &str) -> Self {
        match s {
            "discord" => ChannelKind::Discord,
            "line" => ChannelKind::Line,
            "telegram" => ChannelKind::Telegram,
            "slack" => ChannelKind::Slack,
            "whatsapp" => ChannelKind::Whatsapp,
            "feishu" => ChannelKind::Feishu,
            "webchat" => ChannelKind::Webchat,
            "email" => ChannelKind::Email,
            other => ChannelKind::Other(other.to_string()),
        }
    }
}

/// Canonical person record returned by an [`crate::IdentityProvider`].
///
/// Only the upstream provider may produce these; downstream callers receive
/// them as immutable lookup results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPerson {
    /// Stable canonical identifier from the source of truth (e.g. Notion
    /// page id `person_2f9…`). Treat as opaque.
    pub person_id: String,

    /// Human-readable display name (e.g. "Ruby Lin").
    pub display_name: String,

    /// Domain roles assigned to this person (e.g. ["customer-pm", "engineer"]).
    #[serde(default)]
    pub roles: Vec<String>,

    /// Project memberships — used by SOUL.md "reject non-project members"
    /// rules. Empty means the person is not bound to any project.
    #[serde(default)]
    pub project_ids: Vec<String>,

    /// Email addresses associated with the person. May be empty.
    #[serde(default)]
    pub emails: Vec<String>,

    /// Channel handle table — `{channel-wire-name: external_id}`.
    /// Stored as a `BTreeMap` for deterministic serialisation order and to
    /// keep round-trip equality stable in tests.
    #[serde(default)]
    pub channel_handles: BTreeMap<String, String>,

    /// Provider that produced this record (e.g. `"notion"`, `"wiki-cache"`).
    /// Surfaced into audit logs.
    pub source: String,

    /// When this record was fetched. Cached records carry the cache write
    /// time; live records carry the upstream fetch time.
    pub fetched_at: DateTime<Utc>,
}

impl ResolvedPerson {
    /// Look up the external id this person uses on a particular channel.
    pub fn handle_for(&self, channel: &ChannelKind) -> Option<&str> {
        self.channel_handles
            .get(&channel.as_wire())
            .map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_kind_round_trips_through_wire_format() {
        for kind in [
            ChannelKind::Discord,
            ChannelKind::Line,
            ChannelKind::Telegram,
            ChannelKind::Slack,
            ChannelKind::Whatsapp,
            ChannelKind::Feishu,
            ChannelKind::Webchat,
            ChannelKind::Email,
        ] {
            let wire = kind.as_wire();
            assert_eq!(ChannelKind::parse_wire(&wire), kind);
        }
    }

    #[test]
    fn channel_kind_other_preserves_identifier() {
        let custom = ChannelKind::Other("matrix".into());
        assert_eq!(custom.as_wire(), "matrix");
        assert_eq!(ChannelKind::parse_wire("matrix"), custom);
    }

    #[test]
    fn resolved_person_handle_lookup_uses_channel_wire_name() {
        let mut handles = BTreeMap::new();
        handles.insert("discord".into(), "1234567890".into());
        handles.insert("line".into(), "Uabc".into());

        let person = ResolvedPerson {
            person_id: "person_2f9".into(),
            display_name: "Ruby Lin".into(),
            roles: vec![],
            project_ids: vec![],
            emails: vec![],
            channel_handles: handles,
            source: "wiki-cache".into(),
            fetched_at: Utc::now(),
        };

        assert_eq!(person.handle_for(&ChannelKind::Discord), Some("1234567890"));
        assert_eq!(person.handle_for(&ChannelKind::Line), Some("Uabc"));
        assert_eq!(person.handle_for(&ChannelKind::Telegram), None);
    }
}
