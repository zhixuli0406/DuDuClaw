//! Per-call reasoning **effort** — the one knob every vendor CLI spells
//! differently.
//!
//! P1/WP-3 ("effort plumbing"): effort becomes a first-class per-call
//! parameter so a team role spec `{runtime, model, effort}` can set it, the
//! same way `[model] preferred` already sets the model. This module owns the
//! closed value set, the per-runtime clamp, and the `agent.toml` reader; every
//! spawn site translates [`Effort`] into its own CLI's flag.
//!
//! ## Verified flag mapping (local probe 2026-09-24,
//! `research/multi-model-routing-2026-09/17-P0-cli-flag-probe.md` §4 + §6)
//!
//! | runtime | version | flag | accepted values |
//! |---|---|---|---|
//! | claude | 2.1.258 | `--effort <level>` | `low medium high xhigh max` |
//! | codex | 0.156.1 | `-c model_reasoning_effort=<v>` | `low medium high xhigh` |
//! | antigravity (`agy`) | 1.2.10 | `--effort <v>` | `low medium high` |
//! | grok | 1.0.41 | `--reasoning-effort <v>` (alias `--effort`) | *not enumerated by `--help`* |
//! | gemini | — | **no flag exists** | — |
//!
//! ## Cache note
//!
//! Changing effort mid-conversation invalidates the prompt cache on most
//! models (Claude Code docs: effort participates in the cached prefix). Hold
//! an agent's effort steady across a conversation unless you mean to pay the
//! cache rebuild.

use std::path::Path;
use std::str::FromStr;

use crate::types::RuntimeType;

/// Reasoning effort for one call.
///
/// Ordered weakest → strongest; [`Effort::clamp_for`] relies on that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// Canonical lowercase spelling — the literal every CLI accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// Every variant, weakest first.
    pub const ALL: &'static [Effort] = &[
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    /// The strongest effort `runtime` is known to accept, or `None` when the
    /// runtime has no effort knob at all (gemini, and every runtime driven
    /// through the generic print-mode path).
    ///
    /// Grok is deliberately capped at [`Effort::High`]: `grok --help` names the
    /// flag (`--reasoning-effort <EFFORT>`) but does **not** enumerate its
    /// accepted values, so sending `xhigh`/`max` could be rejected. Capping at
    /// the universally-accepted trio fails safe — see the module table.
    pub fn ceiling_for(runtime: RuntimeType) -> Option<Effort> {
        match runtime {
            RuntimeType::Claude => Some(Effort::Max),
            RuntimeType::Codex => Some(Effort::XHigh),
            RuntimeType::Antigravity => Some(Effort::High),
            RuntimeType::Grok => Some(Effort::High),
            // API-level compat surface (`reasoning_effort` on
            // chat/completions), spanning 8 heterogeneous presets
            // (deepseek/minimax/groq/together/mistral/openrouter/xai/qwen).
            // `low|medium|high` is the set they broadly agree on, so cap there
            // rather than 400-ing a preset with `xhigh`/`max`.
            RuntimeType::OpenAiCompat => Some(Effort::High),
            // Gemini CLI has no thinking/reasoning flag (probe §2: the full
            // 25-flag reference lists none), and the generic print-mode
            // runtimes (qwen/kimi/copilot/…) were never probed. Both ignore
            // effort rather than guessing a flag.
            _ => None,
        }
    }

    /// Clamp this effort down to what `runtime` actually accepts.
    ///
    /// A runtime with no effort knob clamps to itself — the caller decides
    /// what to do with that (every such runtime simply drops the value).
    pub fn clamp_for(self, runtime: RuntimeType) -> Effort {
        match Effort::ceiling_for(runtime) {
            Some(ceiling) => self.min(ceiling),
            None => self,
        }
    }

    /// Does `runtime` have any way to express effort?
    pub fn is_supported_by(runtime: RuntimeType) -> bool {
        Effort::ceiling_for(runtime).is_some()
    }
}

/// Lenient parse: case-insensitive, surrounding whitespace ignored, and the
/// common `x-high` / `x_high` spellings accepted for [`Effort::XHigh`].
impl FromStr for Effort {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Effort::Low),
            "medium" | "med" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            "xhigh" | "x-high" | "x_high" => Ok(Effort::XHigh),
            "max" => Ok(Effort::Max),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Read `agent.toml [model] effort` for the agent rooted at `agent_dir`.
///
/// Deliberately a standalone minimal reader rather than a field on
/// `agent_toml::ModelSectionView`: effort is the only key this needs, and a
/// lenient read keeps a typo'd or wrong-typed value from breaking the agent —
/// an unparseable value yields `None` (provider default), never an error.
pub fn read_agent_effort(agent_dir: &Path) -> Option<Effort> {
    let raw = std::fs::read_to_string(agent_dir.join("agent.toml")).ok()?;
    let value: toml::Value = raw.parse().ok()?;
    let effort = value.get("model")?.get("effort")?.as_str()?;
    match Effort::from_str(effort) {
        Ok(e) => Some(e),
        Err(()) => {
            tracing::warn!(
                agent_dir = %agent_dir.display(),
                value = %effort,
                "agent.toml [model] effort is not one of low/medium/high/xhigh/max — ignoring"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_canonical_spelling() {
        for e in Effort::ALL {
            assert_eq!(Effort::from_str(e.as_str()), Ok(*e));
        }
    }

    #[test]
    fn parse_is_lenient_about_case_whitespace_and_xhigh_spellings() {
        assert_eq!(Effort::from_str("  HIGH "), Ok(Effort::High));
        assert_eq!(Effort::from_str("XHigh"), Ok(Effort::XHigh));
        assert_eq!(Effort::from_str("x-high"), Ok(Effort::XHigh));
        assert_eq!(Effort::from_str("x_high"), Ok(Effort::XHigh));
        assert_eq!(Effort::from_str("med"), Ok(Effort::Medium));
    }

    #[test]
    fn parse_rejects_unknown_values() {
        for bad in ["", "ultra", "ultracode", "auto", "none", "highest", "1"] {
            assert_eq!(Effort::from_str(bad), Err(()), "{bad} should not parse");
        }
    }

    /// The clamp table from the module doc, asserted literally.
    #[test]
    fn clamp_matches_the_probed_per_runtime_ceilings() {
        // Claude accepts the full range — identity.
        for e in Effort::ALL {
            assert_eq!(e.clamp_for(RuntimeType::Claude), *e);
        }
        // Codex tops out at xhigh.
        assert_eq!(Effort::Max.clamp_for(RuntimeType::Codex), Effort::XHigh);
        assert_eq!(Effort::XHigh.clamp_for(RuntimeType::Codex), Effort::XHigh);
        assert_eq!(Effort::Low.clamp_for(RuntimeType::Codex), Effort::Low);
        // Antigravity and Grok top out at high.
        for rt in [RuntimeType::Antigravity, RuntimeType::Grok] {
            assert_eq!(Effort::Max.clamp_for(rt), Effort::High);
            assert_eq!(Effort::XHigh.clamp_for(rt), Effort::High);
            assert_eq!(Effort::Medium.clamp_for(rt), Effort::Medium);
        }
    }

    #[test]
    fn runtimes_without_an_effort_knob_report_unsupported() {
        assert!(!Effort::is_supported_by(RuntimeType::Gemini));
        assert!(Effort::ceiling_for(RuntimeType::Gemini).is_none());
        for rt in [
            RuntimeType::Claude,
            RuntimeType::Codex,
            RuntimeType::Antigravity,
            RuntimeType::Grok,
        ] {
            assert!(Effort::is_supported_by(rt), "{rt:?} should support effort");
        }
    }

    #[test]
    fn reads_effort_from_agent_toml() {
        let dir = std::env::temp_dir().join(format!("ddc-effort-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("agent.toml"),
            "[model]\npreferred = \"x\"\neffort = \"xhigh\"\n",
        )
        .unwrap();
        assert_eq!(read_agent_effort(&dir), Some(Effort::XHigh));

        // Missing key, missing section, wrong type, and garbage all degrade to
        // `None` (provider default) rather than erroring.
        std::fs::write(dir.join("agent.toml"), "[model]\npreferred = \"x\"\n").unwrap();
        assert_eq!(read_agent_effort(&dir), None);
        std::fs::write(dir.join("agent.toml"), "[runtime]\nprovider = \"codex\"\n").unwrap();
        assert_eq!(read_agent_effort(&dir), None);
        std::fs::write(dir.join("agent.toml"), "[model]\neffort = 3\n").unwrap();
        assert_eq!(read_agent_effort(&dir), None);
        std::fs::write(dir.join("agent.toml"), "[model]\neffort = \"turbo\"\n").unwrap();
        assert_eq!(read_agent_effort(&dir), None);
        std::fs::write(dir.join("agent.toml"), "not : valid = toml [[[").unwrap();
        assert_eq!(read_agent_effort(&dir), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_agent_toml_is_none_not_a_panic() {
        assert_eq!(read_agent_effort(Path::new("/nonexistent/ddc/agent")), None);
    }
}
