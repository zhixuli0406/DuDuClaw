//! Who sees what of an employee's thinking in a messaging app.
//!
//! `config.toml`:
//! ```toml
//! [thinking]
//! channel_audience = "internal"    # internal | all | none
//! channel_group_display = "status" # summary | status | off (group chats)
//! ```
//!
//! - `internal` (default): a sender whose verified channel identity maps to
//!   an active dashboard account that is an Admin or is bound to the
//!   answering employee sees the timer and the masked summary; anyone else
//!   (including accounts bound only to other employees) sees the timer only.
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

/// May this messaging-app sender read `agent_id`'s thinking summary? Only
/// when a **verified** channel identity maps to an active dashboard account
/// that is an Admin or is bound to this very employee (the same binding the
/// dashboard's agent ACL uses). Read live from `users.db` (no cache), so a
/// revoked binding takes effect on the next message. Any lookup failure ⇒
/// `false`.
pub fn sender_is_internal(
    home: &Path,
    channel: &str,
    channel_user_id: &str,
    agent_id: &str,
) -> bool {
    use crate::trigger_route::access::{Sender, lookup_channel_sender};
    if channel_user_id.is_empty() || agent_id.is_empty() {
        return false;
    }
    match lookup_channel_sender(home, channel, channel_user_id).sender {
        Sender::Admin { .. } => true,
        // Bound to THIS employee directly; a team grant (a manager calling a
        // report's employee) does not make the sender internal here.
        Sender::Account { access, .. } => access.direct.contains_key(agent_id),
        Sender::Unmapped => false,
    }
}

/// Resolve one turn for the employee `agent_id`: both config layers, the
/// employee's ceiling (a missing agent directory reads as `status`) and the
/// sender's access to that employee.
pub fn resolve_channel(
    home: &Path,
    agent_id: &str,
    channel: &str,
    channel_user_id: &str,
    is_group: bool,
) -> ShowThinking {
    let cfg = ChannelThinkingConfig::from_home(home);
    if cfg.audience == ChannelAudience::None {
        return ShowThinking::Off;
    }
    let global = super::policy::ThinkingConfig::from_home(home).display;
    let agent = if agent_id.is_empty() {
        ShowThinking::Status
    } else {
        super::policy::agent_ceiling(&home.join("agents").join(agent_id))
    };
    let internal = sender_is_internal(home, channel, channel_user_id, agent_id);
    channel_show(&cfg, internal, is_group, global, agent)
}

/// Feishu, for thinking display: fail closed — only an explicit 1:1 chat
/// (`chat_type = "p2p"`) is one-to-one; `group`, `topic_group`, a missing
/// or an unknown value all count as a group chat.
pub fn feishu_is_group(message: &serde_json::Value) -> bool {
    message.get("chat_type").and_then(|v| v.as_str()) != Some("p2p")
}

/// DingTalk, for thinking display: fail closed — only an explicit 1:1
/// conversation (`conversationType = "1"`) is one-to-one.
pub fn dingtalk_is_group(payload: &serde_json::Value) -> bool {
    payload.get("conversationType").and_then(|v| v.as_str()) != Some("1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShowThinking::*;

    #[test]
    fn group_detection_fails_closed() {
        use serde_json::json;
        assert!(!feishu_is_group(&json!({"chat_type": "p2p"})));
        for m in [json!({"chat_type": "group"}), json!({"chat_type": "topic_group"}), json!({}), json!({"chat_type": 1})] {
            assert!(feishu_is_group(&m), "{m}");
        }
        assert!(!dingtalk_is_group(&json!({"conversationType": "1"})));
        for p in [json!({"conversationType": "2"}), json!({}), json!({"conversationType": 1})] {
            assert!(dingtalk_is_group(&p), "{p}");
        }
    }

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
    fn resolve_requires_admin_or_a_binding_to_this_employee() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        for a in ["finance", "sales"] {
            std::fs::create_dir_all(h.join("agents").join(a)).unwrap();
        }
        // No identity store: every sender is external ⇒ timer only.
        assert_eq!(resolve_channel(h, "finance", "telegram", "42", false), Status);
        // Missing employee directory ⇒ status ceiling.
        assert_eq!(resolve_channel(h, "ghost", "telegram", "42", false), Status);
        let db = duduclaw_auth::UserDb::new(&h.join("users.db")).unwrap();
        let emp = db
            .create_user("emp@test.local", "Emp", "EmpPass-12345", duduclaw_auth::UserRole::Employee)
            .unwrap();
        db.bind_agent(&emp.id, "finance", duduclaw_auth::AccessLevel::Operator)
            .unwrap();
        db.bind_channel_identity(&emp.id, "telegram", "7", true).unwrap();
        db.bind_channel_identity(&emp.id, "telegram", "8", false).unwrap();
        let admin = db
            .create_user("adm@test.local", "Adm", "AdmPass-12345", duduclaw_auth::UserRole::Admin)
            .unwrap();
        db.bind_channel_identity(&admin.id, "telegram", "9", true).unwrap();
        // Bound to this employee ⇒ summary; bound only to another ⇒ timer.
        assert_eq!(resolve_channel(h, "finance", "telegram", "7", false), Summary);
        assert_eq!(resolve_channel(h, "sales", "telegram", "7", false), Status);
        // Unverified identity ⇒ external.
        assert_eq!(resolve_channel(h, "finance", "telegram", "8", false), Status);
        // Admin ⇒ summary for any employee.
        assert_eq!(resolve_channel(h, "sales", "telegram", "9", false), Summary);
        // Group chats: timer only by default.
        assert_eq!(resolve_channel(h, "finance", "telegram", "7", true), Status);
        // Read live: revoking the binding takes effect at once.
        db.unbind_agent(&emp.id, "finance").unwrap();
        assert_eq!(resolve_channel(h, "finance", "telegram", "7", false), Status);
        std::fs::write(h.join("config.toml"), "[thinking]\nchannel_audience = \"all\"\n").unwrap();
        assert_eq!(resolve_channel(h, "sales", "telegram", "42", false), Summary);
        assert_eq!(resolve_channel(h, "sales", "telegram", "42", true), Status);
        std::fs::write(h.join("agents/sales/agent.toml"), "[model]\nshow_thinking = \"off\"\n").unwrap();
        assert_eq!(resolve_channel(h, "sales", "telegram", "42", false), Off);
    }
}
