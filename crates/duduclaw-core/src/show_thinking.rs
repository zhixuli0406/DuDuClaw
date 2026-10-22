//! How much of an employee's thinking the dashboard chat may show.
//!
//! Display only — it never changes how much the model thinks or what a turn
//! costs (`[model] effort` stays the one cost knob). Two layers, and the
//! stricter one wins:
//!
//! - `config.toml [thinking] display` — the gateway-wide ceiling (read by the
//!   gateway's thinking policy);
//! - `agent.toml [model] show_thinking` — per employee; unset follows the
//!   global value.
//!
//! Order from strictest: `off` < `status` < `summary`.

use std::path::Path;
use std::str::FromStr;

/// One display level. `Ord` follows strictness: the smaller value is the
/// stricter one, so `a.min(b)` is the effective level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShowThinking {
    /// No thinking frames at all (the browser's own timer still runs).
    Off,
    /// Elapsed time and token counts only, never text.
    Status,
    /// Elapsed time, token counts and a masked, truncated summary.
    Summary,
}

impl ShowThinking {
    pub const ALL: &'static [ShowThinking] = &[
        ShowThinking::Off,
        ShowThinking::Status,
        ShowThinking::Summary,
    ];

    /// Canonical spelling (also the config value).
    pub fn as_str(self) -> &'static str {
        match self {
            ShowThinking::Off => "off",
            ShowThinking::Status => "status",
            ShowThinking::Summary => "summary",
        }
    }

    /// The stricter of two levels.
    pub fn strictest(self, other: ShowThinking) -> ShowThinking {
        self.min(other)
    }
}

impl FromStr for ShowThinking {
    type Err = ();

    /// Case-insensitive, surrounding whitespace ignored.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(ShowThinking::Off),
            "status" => Ok(ShowThinking::Status),
            "summary" => Ok(ShowThinking::Summary),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for ShowThinking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Read `agent.toml [model] show_thinking` for the agent rooted at
/// `agent_dir`. Same leniency as [`crate::effort::read_agent_effort`]: a
/// missing file/key yields `None` (follow the global value); a wrong-typed or
/// unknown value is warned about and yields `None` as well. Because the
/// effective level is the stricter of this and the global ceiling, ignoring a
/// bad per-agent value can never show more than the operator allowed.
pub fn read_agent_show_thinking(agent_dir: &Path) -> Option<ShowThinking> {
    let raw = std::fs::read_to_string(agent_dir.join("agent.toml")).ok()?;
    let value: toml::Value = raw.parse().ok()?;
    let entry = value.get("model")?.get("show_thinking")?;
    let parsed = entry.as_str().and_then(|s| ShowThinking::from_str(s).ok());
    if parsed.is_none() {
        tracing::warn!(
            agent_dir = %agent_dir.display(),
            value = %entry,
            "agent.toml [model] show_thinking is not one of summary/status/off — ignoring"
        );
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_by_strictness() {
        for level in ShowThinking::ALL {
            assert_eq!(ShowThinking::from_str(level.as_str()), Ok(*level));
        }
        assert_eq!(
            ShowThinking::from_str(" Summary "),
            Ok(ShowThinking::Summary)
        );
        for bad in ["", "full", "on", "raw", "1"] {
            assert_eq!(ShowThinking::from_str(bad), Err(()), "{bad}");
        }
        use ShowThinking::*;
        assert_eq!(Summary.strictest(Status), Status);
        assert_eq!(Status.strictest(Off), Off);
        assert_eq!(Off.strictest(Summary), Off);
        assert_eq!(Summary.strictest(Summary), Summary);
    }

    #[test]
    fn reads_the_agent_value_leniently() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert_eq!(read_agent_show_thinking(p), None, "no agent.toml");
        for (body, want) in [
            (
                "[model]\nshow_thinking = \"status\"\n",
                Some(ShowThinking::Status),
            ),
            (
                "[model]\nshow_thinking = \"OFF\"\n",
                Some(ShowThinking::Off),
            ),
            ("[model]\npreferred = \"x\"\n", None),
            ("[model]\nshow_thinking = 1\n", None),
            ("[model]\nshow_thinking = \"verbose\"\n", None),
            ("[runtime]\nprovider = \"codex\"\n", None),
            ("not = [valid", None),
        ] {
            std::fs::write(p.join("agent.toml"), body).unwrap();
            assert_eq!(read_agent_show_thinking(p), want, "{body}");
        }
    }
}
