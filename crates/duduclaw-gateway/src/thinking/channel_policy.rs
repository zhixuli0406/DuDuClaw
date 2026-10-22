//! Who sees what of an employee's thinking in a messaging app.
//!
//! `config.toml`:
//! ```toml
//! [thinking]
//! channel_audience = "internal"    # internal | all | none
//! channel_group_display = "status" # summary | status | off (group chats)
//! ```
//!
//! - `internal` (default): a sender bound to an active dashboard account
//!   (a verified channel identity) sees the timer and the masked summary;
//!   anyone else sees the timer only.
//! - `all`: every sender of a 1:1 chat sees the summary.
//! - `none`: nothing about thinking is shown in messaging apps.
//! - Group chats show at most `channel_group_display` (default `status`:
//!   the timer only), whatever the sender.
//! - The stricter of `[thinking] display` and the answering employee's
//!   `[model] show_thinking` caps all of it.
//! - Public visitors (Odoo Live Chat, the WebChat widget) never get a
//!   thinking sink at all, so nothing here applies to them.
//!
//! Every read fails closed: an unreadable or unparsable `config.toml`, or an
//! unknown value, shows less, never more.

use std::path::Path;

use duduclaw_core::show_thinking::ShowThinking;
use tracing::warn;

/// `[thinking] channel_audience`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelAudience {
    /// Summary for senders bound to a dashboard account, timer for others.
    Internal,
    /// Summary for every sender of a 1:1 chat.
    All,
    /// Nothing.
    None,
}

/// The `[thinking]` keys that concern messaging apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelThinkingConfig {
    pub audience: ChannelAudience,
    pub group_display: ShowThinking,
}

impl Default for ChannelThinkingConfig {
    fn default() -> Self {
        Self {
            audience: ChannelAudience::Internal,
            group_display: ShowThinking::Status,
        }
    }
}

impl ChannelThinkingConfig {
    /// Read per turn. No file or no `[thinking]` ⇒ defaults; a file that
    /// exists but cannot be read or parsed, or a non-table `[thinking]` ⇒
    /// `none`.
    pub fn from_home(home: &Path) -> Self {
        let closed = Self {
            audience: ChannelAudience::None,
            group_display: ShowThinking::Off,
        };
        let text = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(_) => return closed,
        };
        let Ok(table) = text.parse::<toml::Table>() else {
            return closed;
        };
        let sec = match table.get("thinking") {
            None => return Self::default(),
            Some(v) => match v.as_table() {
                Some(sec) => sec,
                None => return closed,
            },
        };
        Self {
            audience: read_audience(sec.get("channel_audience")),
            group_display: read_group_display(sec.get("channel_group_display")),
        }
    }
}

fn read_audience(v: Option<&toml::Value>) -> ChannelAudience {
    let Some(v) = v else {
        return ChannelAudience::Internal;
    };
    match v.as_str().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("internal") => ChannelAudience::Internal,
        Some("all") => ChannelAudience::All,
        Some("none") => ChannelAudience::None,
        _ => {
            warn!(value = %v, "[thinking] channel_audience is not internal/all/none — using none");
            ChannelAudience::None
        }
    }
}

fn read_group_display(v: Option<&toml::Value>) -> ShowThinking {
    let Some(v) = v else {
        return ShowThinking::Status;
    };
    match v.as_str().and_then(|s| s.parse::<ShowThinking>().ok()) {
        Some(level) => level,
        None => {
            warn!(value = %v, "[thinking] channel_group_display is not summary/status/off — using off");
            ShowThinking::Off
        }
    }
}

/// What one messaging-app turn shows: `Off` (nothing), `Status` (the
/// timer) or `Summary` (timer + masked summary).
pub fn channel_show(
    cfg: &ChannelThinkingConfig,
    sender_internal: bool,
    is_group: bool,
    global: ShowThinking,
    agent: ShowThinking,
) -> ShowThinking {
    let by_audience = match cfg.audience {
        ChannelAudience::None => ShowThinking::Off,
        ChannelAudience::All => ShowThinking::Summary,
        ChannelAudience::Internal if sender_internal => ShowThinking::Summary,
        ChannelAudience::Internal => ShowThinking::Status,
    };
    let by_chat = if is_group {
        by_audience.strictest(cfg.group_display)
    } else {
        by_audience
    };
    by_chat.strictest(global).strictest(agent)
}

/// Is this messaging-app sender bound to an active dashboard account
/// (a verified channel identity)? Any lookup failure ⇒ `false`.
pub fn sender_is_internal(home: &Path, channel: &str, channel_user_id: &str) -> bool {
    use crate::trigger_route::access::{Sender, for_channel_sender};
    matches!(
        for_channel_sender(home, channel, channel_user_id).sender,
        Sender::Admin | Sender::Account { .. }
    )
}

/// Resolve one turn: both config layers, the employee's ceiling (a missing
/// agent directory reads as `status`) and the sender.
pub fn resolve_channel(
    home: &Path,
    agent_dir: Option<&Path>,
    channel: &str,
    channel_user_id: &str,
    is_group: bool,
) -> ShowThinking {
    let cfg = ChannelThinkingConfig::from_home(home);
    if cfg.audience == ChannelAudience::None {
        return ShowThinking::Off;
    }
    let global = super::policy::ThinkingConfig::from_home(home).display;
    let agent = agent_dir.map_or(ShowThinking::Status, super::policy::agent_ceiling);
    let internal = sender_is_internal(home, channel, channel_user_id);
    channel_show(&cfg, internal, is_group, global, agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShowThinking::*;

    #[test]
    fn audience_matrix() {
        let internal = ChannelThinkingConfig::default();
        let all = ChannelThinkingConfig {
            audience: ChannelAudience::All,
            ..internal
        };
        let none = ChannelThinkingConfig {
            audience: ChannelAudience::None,
            ..internal
        };
        let open_groups = ChannelThinkingConfig {
            group_display: Summary,
            ..internal
        };
        // (config, bound, group, global, agent) → shown
        let cases = [
            (internal, true, false, Summary, Summary, Summary),
            (internal, false, false, Summary, Summary, Status),
            (internal, true, true, Summary, Summary, Status),
            (internal, false, true, Summary, Summary, Status),
            (open_groups, true, true, Summary, Summary, Summary),
            (open_groups, false, true, Summary, Summary, Status),
            (all, false, false, Summary, Summary, Summary),
            (all, false, true, Summary, Summary, Status),
            (none, true, false, Summary, Summary, Off),
            // The stricter of the global display and the employee caps it.
            (internal, true, false, Status, Summary, Status),
            (internal, true, false, Summary, Status, Status),
            (internal, true, false, Summary, Off, Off),
            (all, false, false, Off, Summary, Off),
        ];
        for (cfg, bound, group, global, agent, want) in cases {
            assert_eq!(
                channel_show(&cfg, bound, group, global, agent),
                want,
                "{cfg:?} bound={bound} group={group} {global:?}/{agent:?}"
            );
        }
    }

    #[test]
    fn config_reads_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            ChannelThinkingConfig::from_home(dir.path()),
            ChannelThinkingConfig::default()
        );
        for (cfg, audience, group) in [
            ("[gateway]\n", ChannelAudience::Internal, Status),
            ("[thinking]\nchannel_audience = \"ALL\"\n", ChannelAudience::All, Status),
            ("[thinking]\nchannel_audience = \"none\"\n", ChannelAudience::None, Status),
            ("[thinking]\nchannel_audience = \"everyone\"\n", ChannelAudience::None, Status),
            ("[thinking]\nchannel_group_display = \"summary\"\n", ChannelAudience::Internal, Summary),
            ("[thinking]\nchannel_group_display = 1\n", ChannelAudience::Internal, Off),
            ("thinking = 1\n", ChannelAudience::None, Off),
            ("not = [valid", ChannelAudience::None, Off),
        ] {
            std::fs::write(dir.path().join("config.toml"), cfg).unwrap();
            let c = ChannelThinkingConfig::from_home(dir.path());
            assert_eq!((c.audience, c.group_display), (audience, group), "{cfg}");
        }
    }

    #[test]
    fn resolve_uses_the_identity_store_and_fails_closed() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents").join("a");
        std::fs::create_dir_all(&agent).unwrap();
        // No identity store: every sender is external ⇒ timer only.
        assert_eq!(
            resolve_channel(home.path(), Some(&agent), "telegram", "42", false),
            Status
        );
        // Missing employee directory ⇒ status ceiling.
        assert_eq!(
            resolve_channel(home.path(), None, "telegram", "42", false),
            Status
        );
        // A verified identity bound to an active account ⇒ summary; an
        // unverified one is still external.
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db
            .create_user(
                "emp@test.local",
                "Emp",
                "EmpPass-12345",
                duduclaw_auth::UserRole::Employee,
            )
            .unwrap();
        db.bind_channel_identity(&user.id, "telegram", "7", true)
            .unwrap();
        db.bind_channel_identity(&user.id, "telegram", "8", false)
            .unwrap();
        assert_eq!(
            resolve_channel(home.path(), Some(&agent), "telegram", "7", false),
            Summary
        );
        assert_eq!(
            resolve_channel(home.path(), Some(&agent), "telegram", "7", true),
            Status,
            "group chats show the timer only by default"
        );
        assert_eq!(
            resolve_channel(home.path(), Some(&agent), "telegram", "8", false),
            Status
        );
        std::fs::write(
            home.path().join("config.toml"),
            "[thinking]\nchannel_audience = \"all\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve_channel(home.path(), Some(&agent), "telegram", "42", false),
            Summary
        );
        assert_eq!(
            resolve_channel(home.path(), Some(&agent), "telegram", "42", true),
            Status
        );
        std::fs::write(
            agent.join("agent.toml"),
            "[model]\nshow_thinking = \"off\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve_channel(home.path(), Some(&agent), "telegram", "42", false),
            Off
        );
    }
}
