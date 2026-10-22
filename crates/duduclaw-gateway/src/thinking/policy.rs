//! Who sees what of a turn's thinking.
//!
//! `config.toml`:
//! ```toml
//! [thinking]
//! display = "summary"              # summary | status | off  (gateway ceiling)
//! max_chars = 4000                 # thinking text per turn, all blocks
//! anthropic_display = "summarized" # summarized | updates (beta, few models)
//! ```
//! `agent.toml [model] show_thinking` narrows it per employee
//! ([`duduclaw_core::show_thinking`]); the stricter value wins.
//!
//! Text is only ever produced for a signed-in dashboard WebChat viewer.
//! External channels and other sessions get [`SinkLevel::Signal`] (text-less
//! events, used only so the interim status line can say "thinking"). Public
//! visitors (Odoo Live Chat) get no sink at all, and WebChat widget visitors
//! are held at `Signal` here and dropped again by the WebChat socket.

use std::path::Path;

use duduclaw_core::show_thinking::ShowThinking;
use duduclaw_llm::ThinkingDisplay;
use tracing::warn;

/// Default thinking-text budget per turn (chars, all blocks together).
pub const DEFAULT_MAX_CHARS: usize = 4_000;
const MIN_MAX_CHARS: usize = 200;
const MAX_MAX_CHARS: usize = 20_000;

/// What the turn's sink forwards. Ordered from strictest, so `a.min(b)` is
/// the stricter level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SinkLevel {
    /// Nothing reaches the callback (WebChat with display `off`).
    Off,
    /// Text-less events; no masking work is done. External channels, ACP,
    /// internal sessions, widget visitors.
    Signal,
    /// Text-less events for a WebChat viewer (timer + tokens).
    Status,
    /// Masked, truncated text for a WebChat viewer.
    Summary,
}

/// `config.toml [thinking]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingConfig {
    pub display: ShowThinking,
    pub max_chars: usize,
    pub anthropic_display: ThinkingDisplay,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            display: ShowThinking::Summary,
            max_chars: DEFAULT_MAX_CHARS,
            anthropic_display: ThinkingDisplay::Summarized,
        }
    }
}

impl ThinkingConfig {
    /// Read per turn. No `config.toml` or no `[thinking]` ⇒ defaults. A file
    /// that exists but cannot be read or parsed, a non-table `[thinking]`, or
    /// an unknown `display` value reads as `status` (no text): a display
    /// setting the gateway cannot understand never shows more.
    pub fn from_home(home: &Path) -> Self {
        let defaults = Self::default();
        let restricted = Self {
            display: ShowThinking::Status,
            ..defaults
        };
        let text = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return defaults,
            Err(e) => {
                warn!(error = %e, "config.toml unreadable — thinking text off for this turn");
                return restricted;
            }
        };
        let Ok(table) = text.parse::<toml::Table>() else {
            return restricted;
        };
        let sec = match table.get("thinking") {
            None => return defaults,
            Some(v) => match v.as_table() {
                Some(sec) => sec,
                None => return restricted,
            },
        };
        Self {
            display: read_display(sec.get("display")),
            max_chars: read_max_chars(sec.get("max_chars")),
            anthropic_display: read_anthropic_display(sec.get("anthropic_display")),
        }
    }
}

fn read_display(v: Option<&toml::Value>) -> ShowThinking {
    let Some(v) = v else {
        return ShowThinking::Summary;
    };
    match v.as_str().and_then(|s| s.parse::<ShowThinking>().ok()) {
        Some(level) => level,
        None => {
            warn!(value = %v, "[thinking] display is not summary/status/off — using status");
            ShowThinking::Status
        }
    }
}

fn read_max_chars(v: Option<&toml::Value>) -> usize {
    let Some(v) = v else {
        return DEFAULT_MAX_CHARS;
    };
    match v.as_integer() {
        Some(n) if n >= 0 => (n as usize).clamp(MIN_MAX_CHARS, MAX_MAX_CHARS),
        _ => {
            warn!(value = %v, "[thinking] max_chars is not a non-negative integer — ignoring");
            DEFAULT_MAX_CHARS
        }
    }
}

fn read_anthropic_display(v: Option<&toml::Value>) -> ThinkingDisplay {
    match v.map(|v| v.as_str()) {
        None | Some(Some("summarized")) => ThinkingDisplay::Summarized,
        Some(Some("updates")) => ThinkingDisplay::Updates,
        Some(_) => {
            warn!("[thinking] anthropic_display is not summarized/updates — using summarized");
            ThinkingDisplay::Summarized
        }
    }
}

/// The resolved policy for one turn, as producers see it through
/// [`super::policy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingPolicy {
    pub level: SinkLevel,
    pub max_chars: usize,
    pub anthropic_display: ThinkingDisplay,
}

impl ThinkingPolicy {
    /// No sink in scope: producers change nothing about their request.
    pub const INACTIVE: ThinkingPolicy = ThinkingPolicy {
        level: SinkLevel::Off,
        max_chars: 0,
        anthropic_display: ThinkingDisplay::Summarized,
    };

    /// Should a producer stream thinking as it happens (e.g. Claude CLI
    /// `--include-partial-messages`)? Only when someone will read the text.
    pub fn wants_live(&self) -> bool {
        self.level == SinkLevel::Summary
    }

    /// Should a producer ask the vendor for thinking text (summaries)?
    pub fn wants_text(&self) -> bool {
        self.level == SinkLevel::Summary
    }

    /// The display to request from a provider that takes one, or `None` to
    /// leave the request unchanged.
    pub fn requested_display(&self) -> Option<ThinkingDisplay> {
        self.wants_text().then_some(self.anthropic_display)
    }
}

/// Is `session_id` a WebChat session (`webchat:` prefix, exact)?
pub fn is_webchat_session(session_id: &str) -> bool {
    session_id
        .split_once(':')
        .is_some_and(|(prefix, rest)| prefix == "webchat" && !rest.is_empty())
}

/// The sink level for a turn, or `None` when no sink may be installed at all
/// (public visitors).
pub fn resolve_level(
    session_id: &str,
    global: ShowThinking,
    agent: Option<ShowThinking>,
    widget_visitor: bool,
) -> Option<SinkLevel> {
    if crate::odoo_livechat::is_public_visitor_session(session_id) {
        return None;
    }
    if !is_webchat_session(session_id) || widget_visitor {
        return Some(SinkLevel::Signal);
    }
    Some(match global.strictest(agent.unwrap_or(global)) {
        ShowThinking::Off => SinkLevel::Off,
        ShowThinking::Status => SinkLevel::Status,
        ShowThinking::Summary => SinkLevel::Summary,
    })
}

/// Read both layers and resolve the policy for one turn. `agent_dir` is only
/// read for WebChat sessions (the only place the per-agent value matters).
pub fn resolve(
    home: &Path,
    agent_dir: Option<&Path>,
    session_id: &str,
    widget_visitor: bool,
) -> Option<ThinkingPolicy> {
    let cfg = ThinkingConfig::from_home(home);
    let agent = if is_webchat_session(session_id) && !widget_visitor {
        Some(agent_dir.map_or(ShowThinking::Status, agent_ceiling))
    } else {
        None
    };
    let level = resolve_level(session_id, cfg.display, agent, widget_visitor)?;
    Some(ThinkingPolicy {
        level,
        max_chars: cfg.max_chars,
        anthropic_display: cfg.anthropic_display,
    })
}

/// The most one employee allows to be shown, failing closed: a missing
/// agent directory, an `agent.toml` or `CONTRACT.toml` that exists but
/// cannot be read or parsed, or an unknown `show_thinking` value all mean
/// `status` (timing only — the CONTRACT check on thinking text could not
/// run reliably). No `show_thinking` key ⇒ no narrowing (`summary`).
pub fn agent_ceiling(agent_dir: &Path) -> ShowThinking {
    if !agent_dir.is_dir() {
        return ShowThinking::Status;
    }
    let parse = |name: &str| -> Result<Option<toml::Table>, ()> {
        match std::fs::read_to_string(agent_dir.join(name)) {
            Ok(text) => text.parse::<toml::Table>().map(Some).map_err(|_| ()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        }
    };
    let (Ok(agent), Ok(_contract)) = (parse("agent.toml"), parse("CONTRACT.toml")) else {
        warn!(agent_dir = %agent_dir.display(), "agent.toml / CONTRACT.toml unreadable — thinking text off");
        return ShowThinking::Status;
    };
    let Some(value) = agent
        .as_ref()
        .and_then(|t| t.get("model"))
        .and_then(|m| m.get("show_thinking"))
    else {
        return ShowThinking::Summary;
    };
    match value.as_str().and_then(|s| s.parse::<ShowThinking>().ok()) {
        Some(level) => level,
        None => {
            warn!(value = %value, "[model] show_thinking is not summary/status/off — using status");
            ShowThinking::Status
        }
    }
}

/// The sink level one employee's ceiling allows.
pub fn level_for(show: ShowThinking) -> SinkLevel {
    match show {
        ShowThinking::Off => SinkLevel::Off,
        ShowThinking::Status => SinkLevel::Status,
        ShowThinking::Summary => SinkLevel::Summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShowThinking::*;

    #[test]
    fn agent_ceiling_fails_closed() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("a");
        // Missing directory.
        assert_eq!(agent_ceiling(&dir), Status);
        std::fs::create_dir_all(&dir).unwrap();
        // Directory with no files: no narrowing.
        assert_eq!(agent_ceiling(&dir), Summary);
        for (agent, contract, want) in [
            (Some("[model]\nshow_thinking = \"off\"\n"), None, Off),
            (Some("[model]\npreferred = \"x\"\n"), None, Summary),
            (Some("[model]\nshow_thinking = \"loud\"\n"), None, Status),
            (Some("not = [valid"), None, Status),
            (Some("[model]\n"), Some("[boundaries\nmust_not = 1"), Status),
            (None, Some("[boundaries]\nmust_not = [\"x\"]\n"), Summary),
        ] {
            let _ = std::fs::remove_file(dir.join("agent.toml"));
            let _ = std::fs::remove_file(dir.join("CONTRACT.toml"));
            if let Some(a) = agent {
                std::fs::write(dir.join("agent.toml"), a).unwrap();
            }
            if let Some(c) = contract {
                std::fs::write(dir.join("CONTRACT.toml"), c).unwrap();
            }
            assert_eq!(agent_ceiling(&dir), want, "{agent:?} / {contract:?}");
        }
        assert!(SinkLevel::Off < SinkLevel::Status && SinkLevel::Status < SinkLevel::Summary);
    }

    #[test]
    fn missing_agent_dir_means_no_text_for_webchat() {
        let home = tempfile::tempdir().unwrap();
        let ghost = home.path().join("agents").join("ghost");
        let p = resolve(home.path(), Some(&ghost), "webchat:x:1", false).unwrap();
        assert_eq!(p.level, SinkLevel::Status);
        let p = resolve(home.path(), None, "webchat:x:1", false).unwrap();
        assert_eq!(p.level, SinkLevel::Status);
    }

    #[test]
    fn config_defaults_and_fail_closed_reads() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            ThinkingConfig::from_home(dir.path()),
            ThinkingConfig::default()
        );
        for (cfg, display) in [
            ("[gateway]\n", Summary),
            ("[thinking]\ndisplay = \"status\"\n", Status),
            ("[thinking]\ndisplay = \"OFF\"\n", Off),
            ("[thinking]\ndisplay = \"verbose\"\n", Status),
            ("[thinking]\ndisplay = 3\n", Status),
            ("thinking = 1\n", Status),
            ("not = [valid", Status),
        ] {
            std::fs::write(dir.path().join("config.toml"), cfg).unwrap();
            assert_eq!(
                ThinkingConfig::from_home(dir.path()).display,
                display,
                "{cfg}"
            );
        }
        std::fs::write(
            dir.path().join("config.toml"),
            "[thinking]\nmax_chars = 50\nanthropic_display = \"updates\"\n",
        )
        .unwrap();
        let c = ThinkingConfig::from_home(dir.path());
        assert_eq!(c.max_chars, MIN_MAX_CHARS);
        assert_eq!(c.anthropic_display, ThinkingDisplay::Updates);
        std::fs::write(
            dir.path().join("config.toml"),
            "[thinking]\nmax_chars = \"x\"\nanthropic_display = \"loud\"\n",
        )
        .unwrap();
        let c = ThinkingConfig::from_home(dir.path());
        assert_eq!(c.max_chars, DEFAULT_MAX_CHARS);
        assert_eq!(c.anthropic_display, ThinkingDisplay::Summarized);
    }

    #[test]
    fn levels_by_session_kind() {
        // Public visitors: no sink at all.
        assert_eq!(resolve_level("odoolivechat:42", Summary, None, false), None);
        // External channels and internal sessions: signal only.
        for sid in [
            "telegram:1",
            "line:u1",
            "discord:9",
            "acp:s1",
            "cron",
            "default",
        ] {
            assert_eq!(
                resolve_level(sid, Summary, None, false),
                Some(SinkLevel::Signal),
                "{sid}"
            );
        }
        // Prefix match is exact.
        assert_eq!(
            resolve_level("webchatx:1", Summary, None, false),
            Some(SinkLevel::Signal)
        );
        // Widget visitors never get more than a signal.
        assert_eq!(
            resolve_level("webchat:abc:1", Summary, Some(Summary), true),
            Some(SinkLevel::Signal)
        );
        // Signed-in WebChat: the stricter of global and agent.
        assert_eq!(
            resolve_level("webchat:abc:1", Summary, None, false),
            Some(SinkLevel::Summary)
        );
        assert_eq!(
            resolve_level("webchat:abc:1", Summary, Some(Status), false),
            Some(SinkLevel::Status)
        );
        assert_eq!(
            resolve_level("webchat:abc:1", Status, Some(Summary), false),
            Some(SinkLevel::Status)
        );
        assert_eq!(
            resolve_level("webchat:abc:1", Off, Some(Summary), false),
            Some(SinkLevel::Off)
        );
        assert_eq!(
            resolve_level("webchat:abc:1", Summary, Some(Off), false),
            Some(SinkLevel::Off)
        );
    }

    #[test]
    fn producer_flags_follow_the_level() {
        let at = |level| ThinkingPolicy {
            level,
            max_chars: DEFAULT_MAX_CHARS,
            anthropic_display: ThinkingDisplay::Updates,
        };
        assert!(at(SinkLevel::Summary).wants_live() && at(SinkLevel::Summary).wants_text());
        assert_eq!(
            at(SinkLevel::Summary).requested_display(),
            Some(ThinkingDisplay::Updates)
        );
        for level in [SinkLevel::Off, SinkLevel::Signal, SinkLevel::Status] {
            assert!(
                !at(level).wants_live() && !at(level).wants_text(),
                "{level:?}"
            );
            assert_eq!(at(level).requested_display(), None);
        }
        assert!(!ThinkingPolicy::INACTIVE.wants_live());
        assert_eq!(ThinkingPolicy::INACTIVE.requested_display(), None);
    }

    #[test]
    fn resolve_reads_the_agent_only_for_webchat() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents").join("a");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("agent.toml"),
            "[model]\nshow_thinking = \"status\"\n",
        )
        .unwrap();
        let p = resolve(home.path(), Some(&agent), "webchat:x:1", false).unwrap();
        assert_eq!(p.level, SinkLevel::Status);
        let p = resolve(home.path(), Some(&agent), "telegram:1", false).unwrap();
        assert_eq!(p.level, SinkLevel::Signal);
        assert!(resolve(home.path(), Some(&agent), "odoolivechat:1", false).is_none());
    }
}
